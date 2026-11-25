#!/usr/bin/env python3
"""Smoke-test the desktop-buddy MCP stdio server.

Spawns `desktop-buddy-mcp.exe`, speaks JSON-RPC 2.0 over its stdin/stdout exactly the
way an MCP client does, and asserts the protocol surface and the error handling.

The mode is auto-detected: if the avatar HTTP API is up, tool calls must
succeed; if it is down, they must come back as structured `isError` results
(never a crash, never a closed stream).

    python tools/mcp_smoke_test.py [path-to-desktop-buddy-mcp.exe]
"""
import json
import os
import queue
import subprocess
import sys
import threading
import urllib.error
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEFAULT_BIN = os.path.join(ROOT, "src-tauri", "target", "debug", "desktop-buddy-mcp.exe")
API = os.environ.get("DESKTOP_BUDDY_API_URL", "http://127.0.0.1:8765")

EXPECTED_TOOLS = {
    "avatar_speak": ["text"],
    "avatar_emote": ["expression"],
    "avatar_move": ["x", "y"],
    "avatar_perch": ["edge"],
    "avatar_wave": [],
    "avatar_pose": ["joints"],
    "avatar_get_context": [],
    "avatar_get_history": [],
    "avatar_get_creature": [],
    "avatar_get_clipboard": [],
    "avatar_set_clipboard": ["text"],
    "avatar_list_models": [],
    "avatar_switch_model": ["name"],
    "avatar_list_animations": [],
    "avatar_animate": ["name"],
    "avatar_switch_ai": ["name"],
    "avatar_send_alert": ["source", "summary"],
    "avatar_clear_speech": [],
    "avatar_clear_pose": [],
}
EXPECTED_TOOL_COUNT = 19

failures = []
checks = 0


def check(condition, label, detail=""):
    global checks
    checks += 1
    if condition:
        print(f"  ok   {label}")
    else:
        print(f"  FAIL {label}" + (f"  → {detail}" if detail else ""))
        failures.append(label)


class Mcp:
    """Minimal MCP client over a child process's stdio."""

    def __init__(self, binary):
        self.proc = subprocess.Popen(
            [binary],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=0,
        )
        self.lines = queue.Queue()
        self.stderr = []
        threading.Thread(target=self._pump_stdout, daemon=True).start()
        threading.Thread(target=self._pump_stderr, daemon=True).start()
        self.next_id = 0

    def _pump_stdout(self):
        for raw in self.proc.stdout:
            text = raw.decode("utf-8", "replace").strip()
            if text:
                self.lines.put(text)

    def _pump_stderr(self):
        for raw in self.proc.stderr:
            self.stderr.append(raw.decode("utf-8", "replace").rstrip())

    def send(self, payload):
        self.proc.stdin.write((json.dumps(payload) + "\n").encode("utf-8"))
        self.proc.stdin.flush()

    def request(self, method, params=None, timeout=20):
        self.next_id += 1
        req_id = self.next_id
        message = {"jsonrpc": "2.0", "id": req_id, "method": method}
        if params is not None:
            message["params"] = params
        self.send(message)
        try:
            line = self.lines.get(timeout=timeout)
        except queue.Empty:
            raise AssertionError(f"no response to {method} within {timeout}s")
        response = json.loads(line)  # must be valid JSON, one object per line
        assert response.get("jsonrpc") == "2.0", f"bad jsonrpc version: {response}"
        assert response.get("id") == req_id, f"id mismatch: sent {req_id}, got {response.get('id')}"
        return response

    def notify(self, method, params=None):
        message = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            message["params"] = params
        self.send(message)

    def call(self, name, arguments=None):
        return self.request("tools/call", {"name": name, "arguments": arguments or {}})

    def close(self):
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        code = self.proc.wait(timeout=10)
        return code


def api_reachable():
    try:
        with urllib.request.urlopen(f"{API}/health", timeout=2) as response:
            return response.status == 200
    except (urllib.error.URLError, TimeoutError, OSError):
        return False


def text_of(response):
    result = response.get("result") or {}
    blocks = result.get("content") or []
    return "\n".join(b.get("text", "") for b in blocks if b.get("type") == "text")


def main():
    binary = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_BIN
    if not os.path.exists(binary):
        print(f"missing binary: {binary}\nbuild it with: cd src-tauri && cargo build")
        return 2

    online = api_reachable()
    print(f"binary:  {binary}")
    print(f"api:     {API}  ({'reachable' if online else 'down'})")
    print()

    mcp = Mcp(binary)
    try:
        print("initialize")
        init = mcp.request(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "desktop-buddy-smoke-test", "version": "1"},
            },
        )
        result = init["result"]
        check(result["protocolVersion"] == "2025-06-18", "negotiates the requested protocol version")
        check(result["serverInfo"]["name"] == "desktop-buddy", "serverInfo.name is desktop-buddy")
        check("tools" in result["capabilities"], "advertises the tools capability")
        check(bool(result.get("instructions")), "ships instructions for the agent")

        mcp.notify("notifications/initialized")
        # a notification must produce no reply: the next line belongs to ping
        pong = mcp.request("ping")
        check(pong["result"] == {}, "ping replies {} (notification produced no frame)")

        print("\ntools/list")
        listing = mcp.request("tools/list")["result"]["tools"]
        names = {tool["name"] for tool in listing}
        check(len(listing) == EXPECTED_TOOL_COUNT, f"{EXPECTED_TOOL_COUNT} tools advertised (got {len(listing)})")
        check(names == set(EXPECTED_TOOLS), "tool names match the spec", sorted(names ^ set(EXPECTED_TOOLS)))
        for tool in listing:
            schema = tool.get("inputSchema") or {}
            required = (schema.get("required") or [])
            expected = EXPECTED_TOOLS.get(tool["name"], [])
            check(
                sorted(required) == sorted(expected),
                f"{tool['name']}: required = {expected}",
                f"got {required}",
            )
            check(bool(tool.get("description")), f"{tool['name']}: has a description")
            check(schema.get("type") == "object", f"{tool['name']}: inputSchema is an object schema")

        print("\nargument validation (before any HTTP call)")
        empty = mcp.call("avatar_speak", {"text": "   "})
        payload = json.loads(text_of(empty))
        check(empty["result"]["isError"] is True, "empty text is an isError tool result")
        check(payload["kind"] == "invalid_arguments", "empty text reports invalid_arguments")
        bad_expr = mcp.call("avatar_emote", {"expression": "amused"})
        check(json.loads(text_of(bad_expr))["kind"] == "invalid_arguments", "unknown expression rejected locally")
        no_name = mcp.call("avatar_switch_model", {})
        check(json.loads(text_of(no_name))["kind"] == "invalid_arguments", "missing name rejected locally")
        no_ai = mcp.call("avatar_switch_ai", {})
        check(json.loads(text_of(no_ai))["kind"] == "invalid_arguments", "switch_ai without a name rejected locally")
        blank_ai = mcp.call("avatar_switch_ai", {"name": "   "})
        check(json.loads(text_of(blank_ai))["kind"] == "invalid_arguments", "blank AI profile name rejected locally")
        no_clip = mcp.call("avatar_animate", {})
        check(json.loads(text_of(no_clip))["kind"] == "invalid_arguments", "animate without a clip name rejected locally")
        bad_loop = mcp.call("avatar_animate", {"name": "stop", "loop": True})
        check(json.loads(text_of(bad_loop))["kind"] == "invalid_arguments", "loop overrides rejected when stopping")
        fail_perch = mcp.call("avatar_perch", {"edge": "middle"})
        perch_payload = json.loads(text_of(fail_perch))
        check(fail_perch["result"]["isError"] is True, "avatar_perch with a bad edge is an isError result")
        check(perch_payload["kind"] == "invalid_arguments", "bad perch edge reports invalid_arguments")
        check(
            "top_left" in json.dumps(perch_payload),
            "the perch error lists the accepted edges",
            perch_payload.get("error"),
        )
        no_edge = mcp.call("avatar_perch", {})
        check(json.loads(text_of(no_edge))["kind"] == "invalid_arguments", "perch without an edge rejected locally")
        bad_offset = mcp.call("avatar_perch", {"edge": "left", "offset": "12"})
        check(json.loads(text_of(bad_offset))["kind"] == "invalid_arguments", "non-numeric perch offset rejected locally")
        bad_look = mcp.call("avatar_move", {"x": 1, "y": 2, "look_at": [1, 2, 3]})
        check(json.loads(text_of(bad_look))["kind"] == "invalid_arguments", "malformed look_at rejected locally")
        no_text = mcp.call("avatar_set_clipboard", {})
        check(json.loads(text_of(no_text))["kind"] == "invalid_arguments", "set_clipboard without text rejected locally")
        long_text = mcp.call("avatar_speak", {"text": "x" * 2001})
        check(
            json.loads(text_of(long_text))["kind"] == "invalid_arguments",
            "speak rejects text past the 2000-character cap locally",
        )
        no_source = mcp.call("avatar_send_alert", {"summary": "y"})
        check(json.loads(text_of(no_source))["kind"] == "invalid_arguments", "alert without a source rejected locally")
        bad_severity = mcp.call("avatar_send_alert", {"source": "smoke", "summary": "y", "severity": "critical"})
        severity_payload = json.loads(text_of(bad_severity))
        check(severity_payload["kind"] == "invalid_arguments", "unknown alert severity rejected locally")
        check("warn" in json.dumps(severity_payload), "the severity error lists the accepted values", severity_payload.get("error"))
        hwnd = mcp.call("avatar_perch", {"edge": "left", "hwnd": 1234})
        hwnd_payload = json.loads(text_of(hwnd))
        check(hwnd_payload["kind"] == "invalid_arguments", "perch refuses hwnd instead of silently ignoring it")
        check("window_process" in json.dumps(hwnd_payload), "the hwnd error points at window_process", hwnd_payload.get("error"))
        bad_arguments = mcp.request("tools/call", {"name": "avatar_wave", "arguments": "nope"})
        check(
            bad_arguments.get("error", {}).get("code") == -32602,
            "non-object arguments → JSON-RPC -32602",
            json.dumps(bad_arguments.get("error")),
        )

        print("\nprotocol errors")
        unknown = mcp.call("avatar_teleport", {})
        check(
            unknown.get("error", {}).get("code") == -32602,
            "unknown tool → JSON-RPC -32602",
            json.dumps(unknown.get("error")),
        )
        bad_method = mcp.request("resources/list")
        check(bad_method.get("error", {}).get("code") == -32601, "unsupported method → -32601")

        print("\ngarbage on stdin must not kill the transport")
        mcp.proc.stdin.write(b"this is not json\n")
        mcp.proc.stdin.flush()
        raw = mcp.lines.get(timeout=10)
        parsed = json.loads(raw)
        check(parsed.get("error", {}).get("code") == -32700, "unparseable line → -32700 parse error")
        check(mcp.request("ping")["result"] == {}, "still serving after a parse error")

        print("\ntool calls")
        cases = [
            ("avatar_wave", {}),
            ("avatar_move", {"x": 700, "y": 500}),
            ("avatar_perch", {"edge": "top_left"}),
            ("avatar_speak", {"text": "Hello from the MCP smoke test.", "expression": "happy", "duration_ms": 3000}),
            ("avatar_emote", {"expression": "surprised", "duration_ms": 1500}),
            ("avatar_pose", {"joints": {"root": {"rotation": 0.1}}}),
            ("avatar_get_context", {}),
            ("avatar_get_clipboard", {}),
            ("avatar_list_models", {}),
            ("avatar_list_animations", {}),
            ("avatar_animate", {"name": "stop"}),
            ("avatar_switch_ai", {"name": "hermes"}),
            ("avatar_get_history", {}),
            ("avatar_clear_speech", {}),
            ("avatar_clear_pose", {}),
            # `avatar_send_alert` is not called live: it broadcasts an `alert`
            # frame and can wake the watcher's AI turn, which a smoke test must
            # not do. Its local validation is covered above.
            # `avatar_get_creature` is checked separately: it 404s when the
            # metamorphosis guest is not built, and that is a valid answer.
        ]
        for name, arguments in cases:
            response = mcp.call(name, arguments)
            result = response["result"]
            body = text_of(response)
            if online:
                check(result.get("isError") is False, f"{name} succeeded", body[:300])
            else:
                check(result.get("isError") is True, f"{name} reports isError when the avatar is down")
                payload = json.loads(body)
                check(
                    payload["kind"] == "avatar_unreachable",
                    f"{name} says avatar_unreachable",
                    payload.get("kind"),
                )
                check(bool(payload.get("hint")), f"{name} includes a hint")

        print("\ncreature status (needs the metamorphosis plugin)")
        creature = mcp.call("avatar_get_creature", {})
        creature_body = text_of(creature)
        if creature["result"].get("isError") is False:
            status = json.loads(creature_body)
            check(status.get("ok") is True, "get_creature reports ok")
            check("stage" in status, "get_creature carries the stage", creature_body[:200])
        else:
            payload = json.loads(creature_body)
            if online:
                check(
                    payload.get("kind") == "avatar_route_unavailable",
                    "a missing metamorphosis plugin reports avatar_route_unavailable",
                    creature_body[:300],
                )
                check("metamorphosis" in json.dumps(payload), "the hint names the plugin to build")
            else:
                check(payload.get("kind") == "avatar_unreachable", "offline creature status is avatar_unreachable")

        if online:
            print("\nclipboard write round-trip")
            marker = "desktop-buddy clipboard round-trip\n  line two"
            wrote = mcp.call("avatar_set_clipboard", {"text": marker})
            check(wrote["result"].get("isError") is False, "set_clipboard succeeded", text_of(wrote)[:200])
            if wrote["result"].get("isError") is False:
                body = json.loads(text_of(wrote))
                check(body.get("chars") == len(marker), "set_clipboard reports the character count", body.get("chars"))
                check(body.get("truncated") is False, "a short clipboard text is not truncated")
                read_back = json.loads(text_of(mcp.call("avatar_get_clipboard")))
                check(read_back.get("text") == marker, "GET /clipboard reads back what was written", read_back.get("text"))

            print("\nmodel switching")
            models = json.loads(text_of(mcp.call("avatar_list_models")))
            check(bool(models.get("models")), "list_models returns entries")
            current = models["models"][0]["name"]
            other = next((m["name"] for m in models["models"] if m["name"] != current), current)
            switched = mcp.call("avatar_switch_model", {"name": other})
            check(switched["result"].get("isError") is False, f"switch_model({other}) succeeded")
            mcp.call("avatar_switch_model", {"name": current})

            print("\ncustom animations")
            clips = json.loads(text_of(mcp.call("avatar_list_animations")))
            check(clips.get("ok") is True, "list_animations reports ok")
            animations = clips.get("animations") or []
            check(isinstance(animations, list), "list_animations returns an array")
            for clip in animations:
                check("name" in clip and "duration_ms" in clip, f"clip {clip.get('name')}: has name + duration")
                check(float(clip.get("duration_ms") or 0) > 0, f"clip {clip.get('name')}: duration_ms > 0")
                check("loop" in clip, f"clip {clip.get('name')}: reports the loop flag")
                check("keyframes" in clip, f"clip {clip.get('name')}: reports its keyframe count")
            if animations:
                first = animations[0]["name"]
                played = mcp.call("avatar_animate", {"name": first, "loop": False})
                check(played["result"].get("isError") is False, f"animate({first}) succeeded")
                stopped = mcp.call("avatar_animate", {"name": "stop"})
                check(stopped["result"].get("isError") is False, "animate(stop) succeeded")

            print("\ncontext payload")
            context_body = text_of(mcp.call("avatar_get_context", {}))
            check("window_title" in context_body, "context includes window_title")
            check("process_name" in context_body, "context includes process_name")
            check("idle_secs" in context_body, "context includes idle_secs")
            check("avatar_state" in context_body, "context includes avatar_state")
            check("recent_events" in context_body, "context includes the recent event buffer")
            check("active_profile" in context_body, "context includes the active AI profile")
            check("ai_profiles" in context_body, "context lists the available AI profiles")
            context_json = json.loads(context_body[context_body.index("{"):])
            check("window_rect" in context_json, "context includes the foreground window_rect")
            check("visible_windows" in context_json, "context includes visible_windows")
            check(isinstance(context_json["visible_windows"], list), "visible_windows is an array")
            check(len(context_json["visible_windows"]) <= 5, "visible_windows is capped at 5")
            check("Foreground window rect" in context_body, "the summary names the foreground rect")
            check("Visible windows" in context_body, "the summary lists the visible windows")
            if isinstance(context_json.get("visible_windows"), list) and context_json["visible_windows"]:
                first = context_json["visible_windows"][0]
                check(
                    {"title", "process_name", "rect"} <= set(first),
                    "a visible window carries title/process_name/rect",
                    sorted(first),
                )
            check("Recent events" in context_body, "context summary mentions the recent events")
            check(context_body.startswith("User is in"), "context opens with a human summary")

            print("\nclipboard payload")
            clipboard_body = text_of(mcp.call("avatar_get_clipboard", {}))
            clipboard = json.loads(clipboard_body)
            check(clipboard.get("ok") is True, "clipboard reports ok")
            check(isinstance(clipboard.get("text"), str), "clipboard text is a string")
            check("truncated" in clipboard, "clipboard reports truncation")
            check(len(clipboard["text"]) <= 2000, "clipboard text is capped at 2000 chars")

            print("\nAI profiles")
            try:
                with urllib.request.urlopen(f"{API}/ai-profiles", timeout=10) as response:
                    profiles = json.load(response)
                entries = profiles.get("profiles") or []
                names = {entry.get("name") for entry in entries}
                check(profiles.get("ok") is True, "GET /ai-profiles reports ok")
                check(bool(entries), "GET /ai-profiles lists the configured profiles")
                check(profiles.get("active_profile") in names, "the active profile is one of them")
                for entry in entries:
                    check(
                        {"name", "label", "kind", "model"} <= set(entry),
                        f"profile {entry.get('name')}: has name/label/kind/model",
                        str(sorted(entry)),
                    )
                    check(
                        entry.get("kind") in {"openai_compat", "hermes_cli"},
                        f"profile {entry.get('name')}: kind is a known backend",
                        entry.get("kind"),
                    )
                health = profiles.get("active_health") or {}
                check(
                    health.get("status") in {"online", "offline", "cli"},
                    "active_health.status is online|offline|cli",
                    str(health),
                )
                check("reachable" in health, "active_health reports reachable")
                check("detail" in health, "active_health carries a detail string")
                # Switching to the profile that is already active is a no-op that
                # still exercises persistence + the broadcast.
                same = json.loads(text_of(mcp.call("avatar_switch_ai", {"name": profiles["active_profile"]})))
                check(
                    same.get("active_profile") == profiles["active_profile"],
                    "avatar_switch_ai echoes the persisted active profile",
                    same,
                )
                unknown = mcp.call("avatar_switch_ai", {"name": "definitely-not-a-profile"})
                check(unknown["result"].get("isError") is True, "an unknown AI profile is an isError result")
                check(
                    json.loads(text_of(unknown)).get("kind") == "avatar_unreachable"
                    or "unknown AI profile" in text_of(unknown),
                    "the unknown-profile error explains itself",
                    text_of(unknown)[:200],
                )
            except (urllib.error.URLError, TimeoutError, ValueError) as err:
                check(False, "GET /ai-profiles answered", str(err))

        print("\nshutdown")
        code = mcp.close()
        check(code == 0, f"clean exit on stdin EOF (exit {code})")

    finally:
        if mcp.proc.poll() is None:
            mcp.proc.kill()

    print()
    if failures:
        print(f"{len(failures)} of {checks} checks FAILED:")
        for failure in failures:
            print(f"  - {failure}")
        return 1
    print(f"all {checks} checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
