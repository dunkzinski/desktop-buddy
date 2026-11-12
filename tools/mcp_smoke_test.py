#!/usr/bin/env python3
"""Smoke-test the char-rig MCP stdio server.

Spawns `char-rig-mcp.exe`, speaks JSON-RPC 2.0 over its stdin/stdout exactly the
way an MCP client does, and asserts the protocol surface and the error handling.

The mode is auto-detected: if the avatar HTTP API is up, tool calls must
succeed; if it is down, they must come back as structured `isError` results
(never a crash, never a closed stream).

    python tools/mcp_smoke_test.py [path-to-char-rig-mcp.exe]
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
DEFAULT_BIN = os.path.join(ROOT, "src-tauri", "target", "debug", "char-rig-mcp.exe")
API = os.environ.get("CHAR_RIG_API_URL", "http://127.0.0.1:8765")

EXPECTED_TOOLS = {
    "avatar_speak": ["text"],
    "avatar_emote": ["expression"],
    "avatar_move": ["x", "y"],
    "avatar_wave": [],
    "avatar_pose": ["joints"],
    "avatar_get_context": [],
    "avatar_get_clipboard": [],
    "avatar_list_models": [],
    "avatar_switch_model": ["name"],
    "avatar_list_animations": [],
    "avatar_animate": ["name"],
}
EXPECTED_TOOL_COUNT = 11

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
                "clientInfo": {"name": "char-rig-smoke-test", "version": "1"},
            },
        )
        result = init["result"]
        check(result["protocolVersion"] == "2025-06-18", "negotiates the requested protocol version")
        check(result["serverInfo"]["name"] == "char-rig", "serverInfo.name is char-rig")
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
        no_clip = mcp.call("avatar_animate", {})
        check(json.loads(text_of(no_clip))["kind"] == "invalid_arguments", "animate without a clip name rejected locally")
        bad_loop = mcp.call("avatar_animate", {"name": "stop", "loop": True})
        check(json.loads(text_of(bad_loop))["kind"] == "invalid_arguments", "loop overrides rejected when stopping")

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
            ("avatar_speak", {"text": "Hello from the MCP smoke test.", "expression": "happy", "duration_ms": 3000}),
            ("avatar_emote", {"expression": "surprised", "duration_ms": 1500}),
            ("avatar_pose", {"joints": {"root": {"rotation": 0.1}}}),
            ("avatar_get_context", {}),
            ("avatar_get_clipboard", {}),
            ("avatar_list_models", {}),
            ("avatar_list_animations", {}),
            ("avatar_animate", {"name": "stop"}),
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

        if online:
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
            check("Recent events" in context_body, "context summary mentions the recent events")
            check(context_body.startswith("User is in"), "context opens with a human summary")

            print("\nclipboard payload")
            clipboard_body = text_of(mcp.call("avatar_get_clipboard", {}))
            clipboard = json.loads(clipboard_body)
            check(clipboard.get("ok") is True, "clipboard reports ok")
            check(isinstance(clipboard.get("text"), str), "clipboard text is a string")
            check("truncated" in clipboard, "clipboard reports truncation")
            check(len(clipboard["text"]) <= 2000, "clipboard text is capped at 2000 chars")

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
