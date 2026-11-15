"""Cross-check the markdown docs against the actual Rust/JS source.

Checks:
  1. every `.route()` in api.rs, every `PluginRoute` in src/plugins/ and every entry in
     a guest crate's ROUTES table appears in
     API.md and ARCHITECTURE.md
  2. every registered tauri command appears in both docs
  3. every event type emitted on the SSE bus is documented
  4. every command the UI actually invokes is registered
  5. no doc mentions a route/command/event that does not exist
"""
import re
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
api_rs = (ROOT / "src-tauri/src/api.rs").read_text(encoding="utf-8")
lib_rs = (ROOT / "src-tauri/src/lib.rs").read_text(encoding="utf-8")
api_md = (ROOT / "API.md").read_text(encoding="utf-8")
arch_md = (ROOT / "ARCHITECTURE.md").read_text(encoding="utf-8")
readme = (ROOT / "README.md").read_text(encoding="utf-8")

problems = []

# Filled in by the MCP section (9); declared here so the rules-file checks can
# use it even if the bridge source is missing.
lib_expressions = []

# ---------- 1. routes ----------
routes = set(re.findall(r'\.route\(\s*"([^"]+)"', api_rs))
# Plugins contribute their own endpoints, so those are not in api.rs at all:
# `PluginRoute::get("/x", ..)` / `::post` / `::get_post` / `::new` are the ways a
# route reaches the router now. Scan every plugin file for them.
plugin_routes = set()
for plugin_path in sorted((ROOT / "src-tauri/src/plugins").rglob("*.rs")):
    # Test stubs are not routes the app serves.
    if plugin_path.name == "tests.rs" or plugin_path.name.endswith("_tests.rs"):
        continue
    plugin_routes |= set(
        re.findall(
            r'PluginRoute::(?:get|post|get_post|new)\(\s*"([^"]+)"',
            plugin_path.read_text(encoding="utf-8"),
        )
    )
# Guest plugins live outside the app crate (plugins/<name>/src/*.rs) and declare
# their endpoints as data: ("/path", &["GET", ...]). Read that table too — it is
# the same one the Wasm host asks the instance for at startup.
wasm_routes = set()
for guest_path in sorted((ROOT / "plugins").rglob("src/*.rs")):
    wasm_routes |= set(
        re.findall(r'\("(/[A-Za-z0-9_/\-]+)",\s*&\[', guest_path.read_text(encoding="utf-8"))
    )
routes |= wasm_routes
routes |= plugin_routes
print(f"routes in api.rs + plugins ({len(routes)}): {', '.join(sorted(routes))}")
if wasm_routes:
    print(f"  of which from guest wasm crates ({len(wasm_routes)}): {', '.join(sorted(wasm_routes))}")
print(f"  of which contributed by plugins ({len(plugin_routes)}): {', '.join(sorted(plugin_routes))}")


def doc_routes(text):
    found = set()
    # `GET /health`, `POST /rig/reset`, `/emote`, `/pose` (in prose/headings)
    for m in re.finditer(r"`(?:GET|POST|DELETE|PUT)\s+(/[A-Za-z0-9_/\-]+)`", text):
        found.add(m.group(1))
    # heading style:  ### `GET /context`   /  ### `/rig`, `/rig/reset`
    for m in re.finditer(r"^#{2,4}\s+`(/[A-Za-z0-9_/\-]+)`", text, re.M):
        found.add(m.group(1))
    # table cells / prose code spans that are exactly a path
    for m in re.finditer(r"`(/[A-Za-z0-9_/\-]+)`", text):
        found.add(m.group(1))
    return found


for name, text in (("API.md", api_md), ("ARCHITECTURE.md", arch_md)):
    documented = doc_routes(text)
    missing = {r for r in routes if r not in documented}
    extra = {r for r in documented if r not in routes}
    if missing:
        problems.append(f"{name}: routes not documented: {sorted(missing)}")
    if extra:
        problems.append(f"{name}: documents non-existent routes: {sorted(extra)}")
    print(f"{name}: {len(documented)} route mentions, missing={sorted(missing)} extra={sorted(extra)}")

# ---------- 2. tauri commands ----------
handler = re.search(r"generate_handler!\[(.*?)\]", lib_rs, re.S).group(1)
commands = set(re.findall(r"\b([a-z][a-z0-9_]+),", handler))
print(f"\nregistered commands ({len(commands)})")

for name, text in (("API.md", api_md), ("ARCHITECTURE.md", arch_md)):
    missing = {c for c in commands if c not in text}
    if missing:
        problems.append(f"{name}: commands not documented: {sorted(missing)}")

# commands mentioned in docs that don't exist — only look inside the
# "Tauri command surface" section of ARCHITECTURE.md (the rest of the document
# legitimately mentions SDK methods like `primary_monitor()`).
arch_surface = arch_md.split("## 6.")[1].split("## 7.")[0]
mentioned = set(re.findall(r"`([a-z][a-z0-9_]+)\(", arch_surface))
bogus = {c for c in mentioned if c not in commands}
if bogus:
    problems.append(f"ARCHITECTURE.md §6: mentions commands that do not exist: {sorted(bogus)}")

# ---------- 3. event types ----------
# ignore the unit-test module: it uses placeholder types "a"/"b"
lib_prod = lib_rs.split("#[cfg(test)]")[0]
events = set(re.findall(r'emit_event\(\s*"([a-z_]+)"', lib_prod))
events |= set(re.findall(r'emit_event\(\s*"([a-z_]+)"', api_rs))
events.add("hello")  # emitted inline by the SSE handler
print(f"\nSSE event types ({len(events)}): {', '.join(sorted(events))}")
for name, text in (("API.md", api_md), ("ARCHITECTURE.md", arch_md)):
    missing = {e for e in events if f"`{e}`" not in text}
    if missing:
        problems.append(f"{name}: event types not documented: {sorted(missing)}")

# event_name() in api.rs must cover every emitted type
names_fn = set(re.findall(r'"([a-z_]+)"\s*=>\s*"[a-z_]+"', api_rs))
unmapped = {e for e in events if e not in names_fn and e != "hello"}
if unmapped:
    problems.append(f"api.rs event_name() has no arm for: {sorted(unmapped)}")

# ---------- 4. UI ↔ commands ----------
ui_calls = set()
for js in (ROOT / "ui/js").glob("*.js"):
    ui_calls |= set(re.findall(r'call\(\s*"([a-z_]+)"', js.read_text(encoding="utf-8")))
unknown = {c for c in ui_calls if c not in commands}
if unknown:
    problems.append(f"UI invokes unregistered commands: {sorted(unknown)}")
else:
    print(f"\nUI invokes {len(ui_calls)} commands, all registered.")

# ---------- 5. /context JSON keys ----------
ctx_block = re.search(r"async fn context.*?\n\}", api_rs, re.S).group(0)
ctx_keys = set(re.findall(r'"([a-z_]+)":', ctx_block))
print(f"\n/context keys in api.rs: {sorted(ctx_keys)}")
for name, text in (("API.md", api_md), ("ARCHITECTURE.md", arch_md)):
    for key in sorted(ctx_keys):
        if f'"{key}"' not in text:
            problems.append(f"{name}: /context key {key!r} not shown")

# ---------- 6. interaction kinds ----------
kinds = set(re.findall(r'INTERACTION_KINDS: \[&str; 3\] = \[(.*?)\];', lib_rs, re.S))
kinds = set(re.findall(r'"([a-z_]+)"', list(kinds)[0])) if kinds else set()
print(f"interaction kinds: {sorted(kinds)}")
for name, text in (("API.md", api_md), ("ARCHITECTURE.md", arch_md)):
    missing = {k for k in kinds if f"`{k}`" not in text}
    if missing:
        problems.append(f"{name}: interaction kinds not documented: {sorted(missing)}")

# ---------- 7. UI kinds actually sent ----------
main_js = (ROOT / "ui/js/main.js").read_text(encoding="utf-8")
ui_kinds = set(re.findall(r'notifyInteraction\(\s*"([a-z_]+)"', main_js))
ui_kinds |= set(re.findall(r'"(double_click|click|drag_end)"', main_js))
ui_kinds &= kinds
print(f"kinds the UI sends: {sorted(ui_kinds)}")
if ui_kinds != kinds:
    problems.append(f"UI never sends: {sorted(kinds - ui_kinds)}")

# ---------- 8. README endpoints ----------
for route in ("/context", "/events", "/health", "/history"):
    if route not in readme:
        problems.append(f"README.md: {route} not mentioned")

# ---------- 8b. chat history + global cursor constants ----------
#
# The History drawer and the `cursor-pos` throttle are described with hard
# numbers in the docs (50 turns, 3 CSS px, 50 ms). Those numbers live in lib.rs
# and are mirrored in ui/js — pin all three so a bump cannot leave a doc lie.
history_cap = re.search(r'CHAT_HISTORY_CAPACITY: usize = (\d+);', lib_rs)
cursor_deadband = re.search(r'CURSOR_EMIT_DEADBAND: f64 = ([\d.]+);', lib_rs)
cursor_interval = re.search(r'CURSOR_EMIT_INTERVAL_MS: u64 = (\d+);', lib_rs)
print("\nchat/gaze constants:",
      "capacity", history_cap.group(1) if history_cap else "?",
      "deadband", cursor_deadband.group(1) if cursor_deadband else "?",
      "interval_ms", cursor_interval.group(1) if cursor_interval else "?")

for label, match in (
    ("CHAT_HISTORY_CAPACITY", history_cap),
    ("CURSOR_EMIT_DEADBAND", cursor_deadband),
    ("CURSOR_EMIT_INTERVAL_MS", cursor_interval),
):
    if match is None:
        problems.append(f"src-tauri/src/lib.rs: {label} not found")
        continue
    pinned = f"{label} = {match.group(1)}"
    for name, text in (("ARCHITECTURE.md", arch_md),):
        if pinned not in text:
            problems.append(f"{name}: {pinned} is not documented as written in lib.rs")

if history_cap:
    # ui/js/history.js keeps its own copy of the capacity.
    history_js = (ROOT / "ui/js/history.js").read_text(encoding="utf-8")
    ui_limit = re.search(r"const CHAT_LIMIT = (\d+);", history_js)
    if ui_limit is None:
        problems.append("ui/js/history.js: CHAT_LIMIT not found")
    elif ui_limit.group(1) != history_cap.group(1):
        problems.append(
            f"ui/js/history.js: CHAT_LIMIT = {ui_limit.group(1)} but "
            f"lib.rs CHAT_HISTORY_CAPACITY = {history_cap.group(1)}"
        )

# ---------- 9. MCP bridge ----------
mcp_path = ROOT / "src-tauri/src/bin/char-rig-mcp.rs"
if not mcp_path.exists():
    problems.append("src-tauri/src/bin/char-rig-mcp.rs is missing")
else:
    mcp_rs = mcp_path.read_text(encoding="utf-8")

    tool_block = re.search(r"const TOOL_NAMES: \[&str; \d+\] = \[(.*?)\];", mcp_rs, re.S)
    tool_names = set(re.findall(r'"([a-z_]+)"', tool_block.group(1))) if tool_block else set()
    print(f"\nMCP tools ({len(tool_names)}): {', '.join(sorted(tool_names))}")
    if len(tool_names) != 14:
        problems.append(f"char-rig-mcp.rs: expected 14 tools, found {len(tool_names)}")

    # every declared tool must have a schema entry in tools()
    for name in sorted(tool_names):
        if mcp_rs.count(f'"name": "{name}"') < 1:
            problems.append(f"char-rig-mcp.rs: tool {name} has no tools() entry")

    # each tool must be dispatched
    for name in sorted(tool_names):
        if f'"{name}" =>' not in mcp_rs:
            problems.append(f"char-rig-mcp.rs: tool {name} is not dispatched in run_tool")

    for doc_name, text in (("API.md", api_md), ("ARCHITECTURE.md", arch_md), ("README.md", readme)):
        missing = {t for t in tool_names if f"`{t}`" not in text and t not in text}
        if missing:
            problems.append(f"{doc_name}: MCP tools not documented: {sorted(missing)}")

    # expression enum must match lib.rs exactly
    lib_exp = re.search(r"AVATAR_EXPRESSIONS: \[&str; \d+\] = \[(.*?)\];", lib_prod, re.S)
    lib_expressions = re.findall(r'"([a-z]+)"', lib_exp.group(1)) if lib_exp else []
    mcp_exp = re.search(r"const EXPRESSIONS: \[&str; \d+\] = \[(.*?)\];", mcp_rs, re.S)
    mcp_expressions = re.findall(r'"([a-z]+)"', mcp_exp.group(1)) if mcp_exp else []
    if lib_expressions != mcp_expressions:
        problems.append(
            "EXPRESSIONS in char-rig-mcp.rs drifted from AVATAR_EXPRESSIONS in lib.rs: "
            f"{mcp_expressions} vs {lib_expressions}"
        )
    else:
        print(f"expression enums match lib.rs: {mcp_expressions}")

    # every path the bridge calls must be a real route
    mcp_paths = set(re.findall(r'api\.(?:get|post)\(\s*"(/[a-z_/-]+)"', mcp_rs))
    print(f"paths used by the MCP bridge: {', '.join(sorted(mcp_paths))}")
    unknown_paths = {p for p in mcp_paths if p not in routes}
    if unknown_paths:
        problems.append(f"char-rig-mcp.rs calls non-existent routes: {sorted(unknown_paths)}")
    if len(mcp_paths) < 14:
        problems.append(f"char-rig-mcp.rs only maps {len(mcp_paths)} routes; expected 14")

    # the documented Windows path must match where cargo puts the binary
    if r"src-tauri\target\debug\char-rig-mcp.exe" not in readme:
        problems.append("README.md: missing the char-rig-mcp.exe path in the MCP config snippet")

# ---------- 10. watcher binary + rules file ----------
watcher_path = ROOT / "src-tauri/src/bin/char-rig-watcher.rs"
rules_path = ROOT / "watcher_rules.json"

if not watcher_path.exists():
    problems.append("src-tauri/src/bin/char-rig-watcher.rs is missing")
else:
    watcher_rs = watcher_path.read_text(encoding="utf-8")

    for doc_name, text in (("README.md", readme), ("ARCHITECTURE.md", arch_md)):
        for needle in ("char-rig-watcher.exe", "watcher_rules.json"):
            if needle not in text:
                problems.append(f"{doc_name}: does not mention {needle}")

    # every flag the parser accepts must be documented in README
    flags = set(re.findall(r'"(--[a-z][a-z-]+)"\s*(?:\||=>)', watcher_rs))
    flags.discard("--help")
    undocumented = {f for f in flags if f not in readme}
    if undocumented:
        problems.append(f"README.md: watcher flags not documented: {sorted(undocumented)}")
    print(f"\nwatcher flags ({len(flags)}): {', '.join(sorted(flags))}")

    # documented defaults must match the constants
    def const_u64(name):
        m = re.search(rf"const {name}: u64 = (\d+);", watcher_rs)
        return int(m.group(1)) if m else None

    defaults = {
        "DEFAULT_CONTEXT_INTERVAL_SECS": const_u64("DEFAULT_CONTEXT_INTERVAL_SECS"),
        "DEFAULT_INTERACTION_COOLDOWN_SECS": const_u64("DEFAULT_INTERACTION_COOLDOWN_SECS"),
        "DEFAULT_CONTEXT_COOLDOWN_SECS": const_u64("DEFAULT_CONTEXT_COOLDOWN_SECS"),
        "DEFAULT_ALERT_COOLDOWN_SECS": const_u64("DEFAULT_ALERT_COOLDOWN_SECS"),
        "DEFAULT_IDLE_THRESHOLD_SECS": const_u64("DEFAULT_IDLE_THRESHOLD_SECS"),
        "DEFAULT_HERMES_TIMEOUT_SECS": const_u64("DEFAULT_HERMES_TIMEOUT_SECS"),
    }
    for name, value in defaults.items():
        if value is None:
            problems.append(f"char-rig-watcher.rs: {name} not found")
        elif name not in arch_md:
            problems.append(f"ARCHITECTURE.md: {name} not documented")

    m = re.search(r'const DEFAULT_ACK_EMOTE: &str = "([a-z]+)";', watcher_rs)
    ack = m.group(1) if m else None
    if ack is None:
        problems.append("char-rig-watcher.rs: DEFAULT_ACK_EMOTE not found")
    elif f"`{ack}`" not in readme:
        problems.append(f"README.md: default ack emote {ack!r} not documented")

    # trigger event types must be documented
    trigger_types = set(re.findall(r'event_type: "([a-z_]+)"\.to_string\(\)', watcher_rs))
    print(f"watcher trigger types: {', '.join(sorted(trigger_types))}")
    for doc_name, text in (("README.md", readme), ("ARCHITECTURE.md", arch_md)):
        missing = {t for t in trigger_types if f"`{t}`" not in text}
        if missing:
            problems.append(f"{doc_name}: watcher trigger types not documented: {sorted(missing)}")

    # shipped rules file must agree with the built-in defaults
    if not rules_path.exists():
        problems.append("watcher_rules.json is missing")
    else:
        try:
            shipped = json.loads(rules_path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as err:
            problems.append(f"watcher_rules.json is not valid JSON: {err}")
            shipped = None
        if shipped is not None:
            checks = [
                ("interactions", "cooldown_secs", defaults["DEFAULT_INTERACTION_COOLDOWN_SECS"]),
                ("context", "cooldown_secs", defaults["DEFAULT_CONTEXT_COOLDOWN_SECS"]),
                ("context", "idle_threshold_secs", defaults["DEFAULT_IDLE_THRESHOLD_SECS"]),
            ]
            for section, key, expected in checks:
                actual = shipped.get(section, {}).get(key)
                if expected is not None and actual != expected:
                    problems.append(
                        f"watcher_rules.json: {section}.{key} = {actual}, "
                        f"but the code default is {expected}"
                    )
            if shipped.get("ack_emote") != ack:
                problems.append(
                    f"watcher_rules.json: ack_emote = {shipped.get('ack_emote')!r}, "
                    f"but the code default is {ack!r}"
                )
            kw_block = re.search(r"window_keywords: vec!\[(.*?)\]", watcher_rs, re.S)
            default_keywords = re.findall(r'"([^"]+)"', kw_block.group(1)) if kw_block else []
            if shipped.get("context", {}).get("window_keywords") != default_keywords:
                problems.append(
                    "watcher_rules.json: window_keywords drifted from the built-in defaults: "
                    f"{shipped.get('context', {}).get('window_keywords')} vs {default_keywords}"
                )
            # unknown keys in the rules file would be silently ignored by serde
            known = {
                "interactions",
                "context",
                "app_rules",
                "alerts",
                "quick_tasks",
                "max_session_turns",
                "session_idle_reset_secs",
                "session_base",
                "ack_emote",
                "hermes_cmd",
                "hermes_timeout_secs",
                "webhook_url",
                "hotkey",
                "tts",
                "stt",
                "vision",
                "active_profile",
                "ai_profiles",
                "survival",
            }
            unknown = set(shipped) - known
            if unknown:
                problems.append(f"watcher_rules.json: unknown keys {sorted(unknown)}")

            # ai_profiles: tagged by `kind`, and every field the watcher reads
            # must be present for that kind (the serde enum is deny-nothing, so
            # drift here would only fail at runtime).
            ai_profiles = shipped.get("ai_profiles")
            active = shipped.get("active_profile")
            if not isinstance(ai_profiles, dict) or not ai_profiles:
                problems.append("watcher_rules.json: ai_profiles must be a non-empty object")
            else:
                if not isinstance(active, str) or active not in ai_profiles:
                    problems.append(
                        "watcher_rules.json: active_profile "
                        f"{active!r} is not one of {sorted(ai_profiles)}"
                    )
                kinds = {"openai_compat", "hermes_cli"}
                for name, profile in ai_profiles.items():
                    if not isinstance(profile, dict):
                        problems.append(f"watcher_rules.json: ai_profiles.{name} is not an object")
                        continue
                    kind = profile.get("kind")
                    if kind not in kinds:
                        problems.append(
                            f"watcher_rules.json: ai_profiles.{name}.kind {kind!r} "
                            f"is not one of {sorted(kinds)}"
                        )
                        continue
                    allowed = {
                        "openai_compat": {"kind", "label", "endpoint", "model", "api_key",
                                          "max_tokens", "timeout_secs", "system_prompt"},
                        "hermes_cli": {"kind", "label", "cmd", "timeout_secs"},
                    }[kind]
                    stray = set(profile) - allowed
                    if stray:
                        problems.append(
                            f"watcher_rules.json: ai_profiles.{name} has unknown keys {sorted(stray)}"
                        )
                    needed = ("endpoint", "model") if kind == "openai_compat" else ("cmd",)
                    for field in needed:
                        value = profile.get(field)
                        if not isinstance(value, str) or not value.strip():
                            problems.append(
                                f"watcher_rules.json: ai_profiles.{name}.{field} must be a non-empty string"
                            )
                if not any(p.get("kind") == "hermes_cli" for p in ai_profiles.values() if isinstance(p, dict)):
                    problems.append(
                        "watcher_rules.json: no hermes_cli profile — an openai_compat profile "
                        "could never escalate, and New chat would not reach an agent"
                    )

            # The hotkey is registered from this file at startup, so a value the
            # parser cannot read would silently fall back to Alt+Space.
            hotkey = shipped.get("hotkey")
            if hotkey is not None:
                if not isinstance(hotkey, str) or not hotkey.strip():
                    problems.append("watcher_rules.json: hotkey must be a non-empty string")
                elif "hotkey" not in arch_md:
                    problems.append(
                        "ARCHITECTURE.md: watcher_rules.json carries `hotkey` but it is not documented"
                    )

            # quick_tasks: the command bar reads these straight out of the file,
            # so a typo there means a silently missing chip.
            quick_task_keys = {"label", "prompt", "profile", "include_screen"}
            chips = shipped.get("quick_tasks")
            if chips is not None:
                if not isinstance(chips, list):
                    problems.append("watcher_rules.json: quick_tasks is not an array")
                    chips = []
                if len(chips) > 6:
                    problems.append(
                        f"watcher_rules.json: quick_tasks has {len(chips)} entries; the bar shows at most 6"
                    )
                for index, chip in enumerate(chips):
                    if not isinstance(chip, dict):
                        problems.append(f"watcher_rules.json: quick_tasks[{index}] is not an object")
                        continue
                    stray = set(chip) - quick_task_keys
                    if stray:
                        problems.append(
                            f"watcher_rules.json: quick_tasks[{index}] has unknown keys {sorted(stray)}"
                        )
                    for field in ("label", "prompt"):
                        value = chip.get(field)
                        if not isinstance(value, str) or not value.strip():
                            problems.append(
                                f"watcher_rules.json: quick_tasks[{index}].{field} must be a non-empty string"
                            )
                    if "include_screen" in chip and not isinstance(chip["include_screen"], bool):
                        problems.append(
                            f"watcher_rules.json: quick_tasks[{index}].include_screen must be a boolean"
                        )

            # The watcher substitutes {session}; without the placeholder a
            # rotation (New chat) cannot reach the CLI at all.
            command = shipped.get("hermes_cmd")
            if isinstance(command, str) and command.strip() and "{session}" not in command:
                problems.append(
                    "watcher_rules.json: hermes_cmd has no {session} placeholder — "
                    "POST /session/reset would only rotate the watcher's own state"
                )

            # per-app rules: the schema is `{process, title_contains, animation,
            # prompt_hint}` — a typo there would silently never match.
            app_rule_keys = {"process", "title_contains", "animation", "prompt_hint", "profile"}
            for index, rule in enumerate(shipped.get("app_rules") or []):
                if not isinstance(rule, dict):
                    problems.append(f"watcher_rules.json: app_rules[{index}] is not an object")
                    continue
                stray = set(rule) - app_rule_keys
                if stray:
                    problems.append(f"watcher_rules.json: app_rules[{index}] has unknown keys {sorted(stray)}")
                if not rule.get("process") and not rule.get("title_contains"):
                    problems.append(
                        f"watcher_rules.json: app_rules[{index}] matches every window "
                        "(both matchers are blank)"
                    )
                animation = rule.get("animation")
                if animation and animation not in lib_expressions and animation not in arch_md:
                    problems.append(
                        f"watcher_rules.json: app_rules[{index}].animation {animation!r} is neither an "
                        "expression nor documented as a clip name"
                    )

            # alerts: only `enabled` + `cooldown_secs`, and the cooldown must be
            # the documented default unless deliberately tuned.
            alerts = shipped.get("alerts") or {}
            stray_alerts = set(alerts) - {"enabled", "cooldown_secs"}
            if stray_alerts:
                problems.append(f"watcher_rules.json: alerts has unknown keys {sorted(stray_alerts)}")
            if alerts.get("cooldown_secs") != defaults["DEFAULT_ALERT_COOLDOWN_SECS"]:
                problems.append(
                    f"watcher_rules.json: alerts.cooldown_secs = {alerts.get('cooldown_secs')}, "
                    f"but the code default is {defaults['DEFAULT_ALERT_COOLDOWN_SECS']}"
                )

            # tts: the desktop app spawns Piper from these paths, and a wrong
            # one makes the avatar silently mute — check the schema and the
            # shipped defaults against the constants in src/tts.rs.
            tts_block = shipped.get("tts")
            if tts_block is not None:
                tts_keys = {
                    "enabled",
                    "piper_exe_path",
                    "model_path",
                    "espeak_data_path",
                    "speaker_id",
                    "length_scale",
                }
                if not isinstance(tts_block, dict):
                    problems.append("watcher_rules.json: tts is not an object")
                else:
                    stray = set(tts_block) - tts_keys
                    if stray:
                        problems.append(f"watcher_rules.json: tts has unknown keys {sorted(stray)}")
                    if not isinstance(tts_block.get("enabled"), bool):
                        problems.append("watcher_rules.json: tts.enabled must be a boolean")
                    for field in ("piper_exe_path", "model_path"):
                        value = tts_block.get(field)
                        if not isinstance(value, str) or not value.strip():
                            problems.append(
                                f"watcher_rules.json: tts.{field} must be a non-empty string"
                            )
                    espeak = tts_block.get("espeak_data_path")
                    if espeak is not None and (not isinstance(espeak, str) or not espeak.strip()):
                        problems.append(
                            "watcher_rules.json: tts.espeak_data_path must be a path string or null"
                        )
                    speaker = tts_block.get("speaker_id")
                    if speaker is not None and (
                        not isinstance(speaker, int) or isinstance(speaker, bool) or speaker < 0
                    ):
                        problems.append(
                            "watcher_rules.json: tts.speaker_id must be a non-negative integer or null"
                        )
                    scale = tts_block.get("length_scale")
                    if scale is not None and (
                        not isinstance(scale, (int, float)) or isinstance(scale, bool)
                    ):
                        problems.append("watcher_rules.json: tts.length_scale must be a number or null")

                    tts_rs_path = ROOT / "src-tauri/src/tts.rs"
                    if not tts_rs_path.exists():
                        problems.append("src-tauri/src/tts.rs is missing")
                    else:
                        tts_rs = tts_rs_path.read_text(encoding="utf-8")

                        def tts_const(name):
                            found = re.search(rf'pub const {name}: &str =\s*r?"([^"]+)";', tts_rs)
                            return found.group(1) if found else None

                        for field, constant in (
                            ("piper_exe_path", "DEFAULT_PIPER_EXE_PATH"),
                            ("model_path", "DEFAULT_PIPER_MODEL_PATH"),
                            ("espeak_data_path", "DEFAULT_ESPEAK_DATA_PATH"),
                        ):
                            default = tts_const(constant)
                            if default is None:
                                problems.append(f"src-tauri/src/tts.rs: {constant} not found")
                            elif tts_block.get(field) != default:
                                problems.append(
                                    f"watcher_rules.json: tts.{field} drifted from {constant}: "
                                    f"{tts_block.get(field)!r} vs {default!r}"
                                )
                    for key in sorted(tts_keys):
                        if f"`{key}`" not in arch_md:
                            problems.append(f"ARCHITECTURE.md: tts.{key} is not documented")

            # stt: push-to-talk. Same deal as tts — the app spawns whisper-cli
            # from these paths, so a typo means a silently dead microphone.
            stt_block = shipped.get("stt")
            if stt_block is not None:
                stt_keys = {
                    "enabled",
                    "hotkey",
                    "whisper_exe_path",
                    "whisper_model_path",
                    "silero_vad_model_path",
                    "vad_threshold",
                    "language",
                    "min_speech_ms",
                }
                if not isinstance(stt_block, dict):
                    problems.append("watcher_rules.json: stt is not an object")
                else:
                    stray = set(stt_block) - stt_keys
                    if stray:
                        problems.append(f"watcher_rules.json: stt has unknown keys {sorted(stray)}")
                    if not isinstance(stt_block.get("enabled"), bool):
                        problems.append("watcher_rules.json: stt.enabled must be a boolean")
                    for field in ("whisper_exe_path", "whisper_model_path"):
                        value = stt_block.get(field)
                        if not isinstance(value, str) or not value.strip():
                            problems.append(
                                f"watcher_rules.json: stt.{field} must be a non-empty string"
                            )
                    vad = stt_block.get("silero_vad_model_path")
                    if vad is not None and (not isinstance(vad, str) or not vad.strip()):
                        problems.append(
                            "watcher_rules.json: stt.silero_vad_model_path must be a path string or null"
                        )
                    threshold = stt_block.get("vad_threshold")
                    if not isinstance(threshold, (int, float)) or isinstance(threshold, bool):
                        problems.append("watcher_rules.json: stt.vad_threshold must be a number")
                    elif not 0.0 < threshold < 1.0:
                        problems.append(
                            f"watcher_rules.json: stt.vad_threshold {threshold} is outside 0..1"
                        )
                    language = stt_block.get("language")
                    if not isinstance(language, str) or not language.strip():
                        problems.append("watcher_rules.json: stt.language must be a non-empty string")
                    min_speech = stt_block.get("min_speech_ms")
                    if not isinstance(min_speech, int) or isinstance(min_speech, bool) or min_speech < 0:
                        problems.append(
                            "watcher_rules.json: stt.min_speech_ms must be a non-negative integer"
                        )
                    # The push-to-talk shortcut is parsed by parse_hotkey; it must
                    # at least look like the documented grammar.
                    hotkey = stt_block.get("hotkey")
                    if not isinstance(hotkey, str) or not hotkey.strip():
                        problems.append("watcher_rules.json: stt.hotkey must be a non-empty string")
                    elif not re.fullmatch(r"[A-Za-z+ ]+", hotkey):
                        problems.append(
                            f"watcher_rules.json: stt.hotkey {hotkey!r} is not a hotkey spec"
                        )

                    stt_rs_path = ROOT / "src-tauri/src/stt.rs"
                    if not stt_rs_path.exists():
                        problems.append("src-tauri/src/stt.rs is missing")
                    else:
                        stt_rs = stt_rs_path.read_text(encoding="utf-8")

                        def stt_const(name):
                            found = re.search(rf'pub const {name}: &str =\s*r?"([^"]+)";', stt_rs)
                            return found.group(1) if found else None

                        for field, constant in (
                            ("hotkey", "DEFAULT_STT_HOTKEY"),
                            ("whisper_exe_path", "DEFAULT_WHISPER_EXE_PATH"),
                            ("whisper_model_path", "DEFAULT_WHISPER_MODEL_PATH"),
                            ("silero_vad_model_path", "DEFAULT_SILERO_VAD_MODEL_PATH"),
                        ):
                            default = stt_const(constant)
                            if default is None:
                                problems.append(f"src-tauri/src/stt.rs: {constant} not found")
                            elif stt_block.get(field) != default:
                                problems.append(
                                    f"watcher_rules.json: stt.{field} drifted from {constant}: "
                                    f"{stt_block.get(field)!r} vs {default!r}"
                                )
                    for key in sorted(stt_keys):
                        if f"`{key}`" not in arch_md:
                            problems.append(f"ARCHITECTURE.md: stt.{key} is not documented")


            # survival: the plugin config the Settings modal writes. Friendly
            # names (`metabolic_drain_rate`, `starting_hunger`, `notifications`)
            # are NOT serde field names — they were silently dropped once and the
            # app ran on defaults while the file looked configured. Pin the real
            # schema so that cannot come back.
            survival_block = shipped.get("survival")
            if survival_block is not None:
                survival_keys = {
                    "enabled",
                    "start_hunger",
                    "start_saturation",
                    "metabolism",
                    "notify",
                    "telemetry_secs",
                }
                if not isinstance(survival_block, dict):
                    problems.append("watcher_rules.json: survival is not an object")
                else:
                    stray = set(survival_block) - survival_keys
                    if stray:
                        problems.append(
                            f"watcher_rules.json: survival has unknown keys {sorted(stray)} "
                            "(these are silently ignored by the loader)"
                        )
                    if not isinstance(survival_block.get("enabled"), bool):
                        problems.append("watcher_rules.json: survival.enabled must be a boolean")
                    if not isinstance(survival_block.get("notify"), bool):
                        problems.append("watcher_rules.json: survival.notify must be a boolean")
                    for field in ("start_hunger", "start_saturation"):
                        value = survival_block.get(field)
                        if isinstance(value, bool) or not isinstance(value, (int, float)):
                            problems.append(f"watcher_rules.json: survival.{field} must be a number")
                        elif not 0.0 <= value <= 100.0:
                            problems.append(
                                f"watcher_rules.json: survival.{field} {value} is outside 0..100"
                            )
                    telemetry = survival_block.get("telemetry_secs")
                    if isinstance(telemetry, bool) or not isinstance(telemetry, int) or telemetry < 0:
                        problems.append(
                            "watcher_rules.json: survival.telemetry_secs must be a non-negative integer"
                        )
                    metabolism = survival_block.get("metabolism")
                    if not isinstance(metabolism, dict):
                        problems.append("watcher_rules.json: survival.metabolism must be an object")
                    else:
                        stray = set(metabolism) - {"hunger_per_min", "saturation_per_min"}
                        if stray:
                            problems.append(
                                f"watcher_rules.json: survival.metabolism has unknown keys {sorted(stray)}"
                            )
                        for field in ("hunger_per_min", "saturation_per_min"):
                            value = metabolism.get(field)
                            if isinstance(value, bool) or not isinstance(value, (int, float)):
                                problems.append(
                                    f"watcher_rules.json: survival.metabolism.{field} must be a number"
                                )
                            elif not 0.0 < value <= 100.0:
                                problems.append(
                                    f"watcher_rules.json: survival.metabolism.{field} {value} is outside 0..100"
                                )
                    for key in sorted(survival_keys):
                        if f"`{key}`" not in arch_md:
                            problems.append(f"ARCHITECTURE.md: survival.{key} is not documented")
                    for key in ("hunger_per_min", "saturation_per_min"):
                        if f"`{key}`" not in arch_md:
                            problems.append(
                                f"ARCHITECTURE.md: survival.metabolism.{key} is not documented"
                            )

            # vision: screen capture + a named local VL profile. The profile name
            # must resolve, or a screenshot would silently fall back to the text
            # model and the avatar would "look" without seeing anything.
            vision = shipped.get("vision")
            if vision is not None:
                vision_keys = {
                    "enabled",
                    "profile_name",
                    "capture_mode",
                    "max_image_dim",
                    "jpeg_quality",
                    "trigger_phrases",
                }
                if not isinstance(vision, dict):
                    problems.append("watcher_rules.json: vision is not an object")
                else:
                    stray = set(vision) - vision_keys
                    if stray:
                        problems.append(f"watcher_rules.json: vision has unknown keys {sorted(stray)}")
                    if not isinstance(vision.get("enabled"), bool):
                        problems.append("watcher_rules.json: vision.enabled must be a boolean")
                    profile_name = vision.get("profile_name")
                    if not isinstance(profile_name, str) or not profile_name.strip():
                        problems.append(
                            "watcher_rules.json: vision.profile_name must be a non-empty string"
                        )
                    elif isinstance(ai_profiles, dict):
                        chosen = ai_profiles.get(profile_name)
                        if not isinstance(chosen, dict):
                            problems.append(
                                f"watcher_rules.json: vision.profile_name {profile_name!r} "
                                f"is not one of {sorted(ai_profiles)}"
                            )
                        elif chosen.get("kind") != "openai_compat":
                            problems.append(
                                "watcher_rules.json: vision.profile_name must point at an "
                                "openai_compat profile (the multimodal request is OpenAI-shaped)"
                            )
                    mode = vision.get("capture_mode")
                    if mode not in ("active_window", "full_screen"):
                        problems.append(
                            f"watcher_rules.json: vision.capture_mode {mode!r} "
                            "is not one of ['active_window', 'full_screen']"
                        )
                    max_dim = vision.get("max_image_dim")
                    if not isinstance(max_dim, int) or isinstance(max_dim, bool) or max_dim < 64:
                        problems.append(
                            "watcher_rules.json: vision.max_image_dim must be an integer >= 64"
                        )
                    quality = vision.get("jpeg_quality")
                    if not isinstance(quality, int) or isinstance(quality, bool) or not 1 <= quality <= 100:
                        problems.append(
                            "watcher_rules.json: vision.jpeg_quality must be an integer 1..100"
                        )
                    phrases = vision.get("trigger_phrases")
                    if not isinstance(phrases, list) or not phrases:
                        problems.append(
                            "watcher_rules.json: vision.trigger_phrases must be a non-empty array"
                        )
                    elif not all(isinstance(p, str) and p.strip() for p in phrases):
                        problems.append(
                            "watcher_rules.json: vision.trigger_phrases entries must be non-empty strings"
                        )

                    watcher_rs_path = ROOT / "src-tauri/src/bin/char-rig-watcher.rs"
                    if not watcher_rs_path.exists():
                        problems.append("src-tauri/src/bin/char-rig-watcher.rs is missing")
                    else:
                        watcher_rs = watcher_rs_path.read_text(encoding="utf-8")
                        profile_const = re.search(
                            r'const DEFAULT_VISION_PROFILE: &str =\s*r?"([^"]+)";', watcher_rs
                        )
                        if profile_const is None:
                            problems.append(
                                "src-tauri/src/bin/char-rig-watcher.rs: DEFAULT_VISION_PROFILE not found"
                            )
                        elif profile_name != profile_const.group(1):
                            problems.append(
                                "watcher_rules.json: vision.profile_name drifted from "
                                f"DEFAULT_VISION_PROFILE: {profile_name!r} vs {profile_const.group(1)!r}"
                            )
                    for key in sorted(vision_keys):
                        if f"`{key}`" not in arch_md:
                            problems.append(f"ARCHITECTURE.md: vision.{key} is not documented")

            shipped_timeout = shipped.get("hermes_timeout_secs")
            if shipped_timeout is not None:
                expected_timeout = defaults["DEFAULT_HERMES_TIMEOUT_SECS"]
                if shipped_timeout != expected_timeout:
                    problems.append(
                        f"watcher_rules.json: hermes_timeout_secs = {shipped_timeout}, "
                        f"but the code default is {expected_timeout}"
                    )

print("\n" + "=" * 60)
if problems:
    for p in problems:
        print("PROBLEM:", p)
    sys.exit(1)
print("Docs and source agree on routes, commands, events, kinds and keys.")
