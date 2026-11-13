"""Cross-check the markdown docs against the actual Rust/JS source.

Checks:
  1. every `.route()` in api.rs appears in API.md and ARCHITECTURE.md
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
print(f"routes in api.rs ({len(routes)}): {', '.join(sorted(routes))}")


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
for route in ("/context", "/events", "/health"):
    if route not in readme:
        problems.append(f"README.md: {route} not mentioned")

# ---------- 9. MCP bridge ----------
mcp_path = ROOT / "src-tauri/src/bin/char-rig-mcp.rs"
if not mcp_path.exists():
    problems.append("src-tauri/src/bin/char-rig-mcp.rs is missing")
else:
    mcp_rs = mcp_path.read_text(encoding="utf-8")

    tool_block = re.search(r"const TOOL_NAMES: \[&str; \d+\] = \[(.*?)\];", mcp_rs, re.S)
    tool_names = set(re.findall(r'"([a-z_]+)"', tool_block.group(1))) if tool_block else set()
    print(f"\nMCP tools ({len(tool_names)}): {', '.join(sorted(tool_names))}")
    if len(tool_names) != 13:
        problems.append(f"char-rig-mcp.rs: expected 13 tools, found {len(tool_names)}")

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
    mcp_paths = set(re.findall(r'api\.(?:get|post)\(\s*"(/[a-z_/]+)"', mcp_rs))
    print(f"paths used by the MCP bridge: {', '.join(sorted(mcp_paths))}")
    unknown_paths = {p for p in mcp_paths if p not in routes}
    if unknown_paths:
        problems.append(f"char-rig-mcp.rs calls non-existent routes: {sorted(unknown_paths)}")
    if len(mcp_paths) < 13:
        problems.append(f"char-rig-mcp.rs only maps {len(mcp_paths)} routes; expected 13")

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
            }
            unknown = set(shipped) - known
            if unknown:
                problems.append(f"watcher_rules.json: unknown keys {sorted(unknown)}")

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
            quick_task_keys = {"label", "prompt"}
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
            app_rule_keys = {"process", "title_contains", "animation", "prompt_hint"}
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
