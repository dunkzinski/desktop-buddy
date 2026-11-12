//! `char-rig-watcher` — a headless watcher that wakes an agent when the desktop
//! or the avatar does something interesting, at **zero idle LLM cost**.
//!
//! It is the third binary built by `cargo build` (next to `avatar-desktop.exe`
//! and `char-rig-mcp.exe`) and it owns no UI: it subscribes to the running
//! avatar's `GET /events` stream and polls `GET /context`, and only when a
//! configured rule fires does it reach out — a visual ack on the avatar, an
//! HTTP webhook, or a Hermes command with a concise prompt.
//!
//! ```text
//!   avatar-desktop.exe ──SSE /events────┐
//!                        ──GET /context─┴──▶ char-rig-watcher ──▶ POST /emote (ack)
//!                                                          ──▶ --webhook-url
//!                                                          ──▶ --hermes-cmd (spawn)
//! ```
//!
//! Design rules that matter:
//!
//! * **Idle costs nothing.** With no triggers firing, the process only reads a
//!   keep-alive SSE stream and one small JSON poll every 20 s. No model is
//!   called, no tokens are spent.
//! * **The avatar may be down.** Every connection is retried with exponential
//!   backoff (1 s → 30 s); a restart reconnects and keeps watching.
//! * **Rules are data, not code.** `watcher_rules.json` (or `--rules`) tunes
//!   which interactions matter, the keywords that are worth interrupting for,
//!   and the cooldowns. A missing file means built-in defaults.
//! * **Dry runs are side-effect free.** `--dry-run` prints the trigger payloads
//!   on stdout and touches neither the avatar nor Hermes.
//!
//! Diagnostics always go to stderr; stdout carries only `--dry-run` payloads.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

/* ------------------------------------------------------------------ *
 * constants
 * ------------------------------------------------------------------ */

const DEFAULT_API_URL: &str = "http://127.0.0.1:8765";
const ENV_API_URL: &str = "CHAR_RIG_API_URL";
const ENV_HERMES_CMD: &str = "CHAR_RIG_HERMES_CMD";
const DEFAULT_RULES_FILE: &str = "watcher_rules.json";

const DEFAULT_CONTEXT_INTERVAL_SECS: u64 = 20;
const DEFAULT_INTERACTION_COOLDOWN_SECS: u64 = 15;
const DEFAULT_CONTEXT_COOLDOWN_SECS: u64 = 300;
const DEFAULT_ALERT_COOLDOWN_SECS: u64 = 60;
const DEFAULT_IDLE_THRESHOLD_SECS: u64 = 900;
const DEFAULT_ACK_EMOTE: &str = "thinking";
const ACK_EMOTE_DURATION_MS: u64 = 4000;
/// Prefix for generated session ids (`char-rig-<seed>`); override with
/// `session_base` in the rules file.
const DEFAULT_SESSION_BASE: &str = "char-rig";
/// How long a spawned `--hermes-cmd` may run before it is killed.
///
/// Generous on purpose: a local model answering a trigger often needs well over
/// a minute before it emits its `avatar_*` tool call.
const DEFAULT_HERMES_TIMEOUT_SECS: u64 = 120;

/// Expressions the avatar understands — mirrors `AVATAR_EXPRESSIONS` in
/// `lib.rs`. A rule's `animation` naming one of these is shown with `POST
/// /emote`; anything else is treated as a custom keyframe clip name and played
/// with `POST /animate`.
const EXPRESSIONS: [&str; 6] = ["neutral", "happy", "sad", "surprised", "thinking", "angry"];

/// Appended to every `--hermes-cmd` prompt.
///
/// The child gets `Stdio::null()` on stdin, so an agent that asks a follow-up
/// question reads EOF (or hangs on a Windows console that has no input) instead
/// of answering. The directive therefore spells out the MCP call signatures,
/// so a trigger costs one model turn and no `tool_describe` round-trip. It also
/// tells the model that chaining calls in one turn is expected, and caps the
/// bubble text: the avatar's bubble is a fixed-width card, so a paragraph would
/// run off the screen.
const HEADLESS_DIRECTIVE: &str = "[Headless desktop avatar trigger -- do not ask follow-up questions or wait for user input. \
Call your char-rig MCP tools directly and exit: mcp__char_rig__avatar_speak(text, expression?, duration_ms?), \
mcp__char_rig__avatar_emote(expression, duration_ms?) where expression is neutral|happy|sad|surprised|thinking|angry, \
mcp__char_rig__avatar_wave(), mcp__char_rig__avatar_animate(name, loop?), \
mcp__char_rig__avatar_get_context() for the foreground window title, the owning process_name (e.g. Code.exe), \
idle_secs and the recent events, and mcp__char_rig__avatar_get_clipboard() for the text the user last copied. \
Chain several calls in one turn when it helps: an avatar_emote or avatar_animate for the feeling plus an \
avatar_speak for the words, then exit. Keep the speech bubble under 180 characters (1-2 sentences) so it never \
overflows the screen; put detail in your expression or animation, not in the bubble.]";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const CONTEXT_TIMEOUT: Duration = Duration::from_secs(5);
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const TRIGGER_QUEUE: usize = 64;

/* ------------------------------------------------------------------ *
 * CLI configuration
 * ------------------------------------------------------------------ */

/// Raw flags, before the rules file and built-in defaults are merged in.
#[derive(Debug, Clone, Default, PartialEq)]
struct CliConfig {
    api_url: Option<String>,
    rules_path: Option<String>,
    context_interval: Option<u64>,
    trigger_single_click: bool,
    /// `Some("")` disables the ack emote entirely.
    ack_emote: Option<String>,
    hermes_cmd: Option<String>,
    hermes_timeout: Option<u64>,
    webhook_url: Option<String>,
    dry_run: bool,
}

/// Everything the watcher needs at runtime.
#[derive(Debug, Clone, PartialEq)]
struct Config {
    api_url: String,
    context_interval: Duration,
    rules: Rules,
    ack_emote: String,
    hermes_cmd: Option<String>,
    hermes_timeout: Duration,
    webhook_url: Option<String>,
    dry_run: bool,
}

fn usage() -> String {
    format!(
        "\
char-rig-watcher {version} - wake an agent when the avatar or the desktop does something

USAGE:
  char-rig-watcher [OPTIONS]

OPTIONS:
  --api-url <URL>            avatar API base (default ${ENV_API_URL} or {api})
  --rules <PATH>             rules file (default ./{rules}, then next to the exe)
  --context-interval <SECS>  how often GET /context is polled (default {interval})
  --trigger-single-click     also fire on plain `click` interactions
  --ack-emote <EXPR>         expression shown the moment a trigger fires
                             (default {ack}; pass \"\" or --no-ack-emote to disable)
  --hermes-cmd <CMD>         command spawned with the prompt appended
                             (default ${ENV_HERMES_CMD})
  --hermes-timeout <SECS>    kill the spawned command after this long (default {timeout})
  --webhook-url <URL>        POST the trigger payload to this endpoint
  --dry-run                  print trigger payloads on stdout, call nothing
  -h, --help                 show this help

The prompt is appended as the last argv entry (or substituted for {{prompt}}).
The command is split on whitespace with double/single-quote grouping; no shell
is involved, so use a script if you need pipes or redirection.

The child runs headless: stdin is /dev/null, and only one dispatch is in flight
at a time — a trigger that fires while a dispatch is still running is skipped
(a debug line on stderr) instead of spawning a second process.",
        version = env!("CARGO_PKG_VERSION"),
        ENV_API_URL = ENV_API_URL,
        ENV_HERMES_CMD = ENV_HERMES_CMD,
        api = DEFAULT_API_URL,
        rules = DEFAULT_RULES_FILE,
        interval = DEFAULT_CONTEXT_INTERVAL_SECS,
        ack = DEFAULT_ACK_EMOTE,
        timeout = DEFAULT_HERMES_TIMEOUT_SECS,
    )
}

fn next_value(
    iter: &mut std::iter::Peekable<impl Iterator<Item = String>>,
    inline: Option<String>,
    name: &str,
) -> Result<String, String> {
    if let Some(value) = inline {
        return Ok(value);
    }
    iter.next()
        .ok_or_else(|| format!("{name} needs a value (try --help)"))
}

/// Parse argv into raw flags. `Ok(None)` means `--help` was handled.
fn parse_cli<I: IntoIterator<Item = String>>(args: I) -> Result<Option<CliConfig>, String> {
    let mut cli = CliConfig::default();
    let mut iter = args.into_iter().peekable();

    while let Some(arg) = iter.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name.to_string(), Some(value.to_string())),
            _ => (arg.clone(), None),
        };

        match flag.as_str() {
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(None);
            }
            "--api-url" => cli.api_url = Some(next_value(&mut iter, inline, "--api-url")?),
            "--rules" => cli.rules_path = Some(next_value(&mut iter, inline, "--rules")?),
            "--context-interval" => {
                let raw = next_value(&mut iter, inline, "--context-interval")?;
                let secs: u64 = raw
                    .parse()
                    .map_err(|_| format!("--context-interval expects whole seconds, got {raw:?}"))?;
                if secs == 0 {
                    return Err("--context-interval must be at least 1 second".to_string());
                }
                cli.context_interval = Some(secs);
            }
            "--trigger-single-click" => cli.trigger_single_click = true,
            "--ack-emote" => cli.ack_emote = Some(next_value(&mut iter, inline, "--ack-emote")?),
            "--no-ack-emote" => cli.ack_emote = Some(String::new()),
            "--hermes-cmd" => cli.hermes_cmd = Some(next_value(&mut iter, inline, "--hermes-cmd")?),
            "--hermes-timeout" => {
                let raw = next_value(&mut iter, inline, "--hermes-timeout")?;
                let secs: u64 = raw
                    .parse()
                    .map_err(|_| format!("--hermes-timeout expects whole seconds, got {raw:?}"))?;
                if secs == 0 {
                    return Err("--hermes-timeout must be at least 1 second".to_string());
                }
                cli.hermes_timeout = Some(secs);
            }
            "--webhook-url" => cli.webhook_url = Some(next_value(&mut iter, inline, "--webhook-url")?),
            "--dry-run" => cli.dry_run = true,
            other => return Err(format!("unknown argument {other:?} (try --help)")),
        }
    }

    // Environment fallbacks sit between the flags and the defaults.
    if cli.api_url.is_none() {
        cli.api_url = env_non_empty(ENV_API_URL);
    }
    if cli.hermes_cmd.is_none() {
        cli.hermes_cmd = env_non_empty(ENV_HERMES_CMD);
    }
    Ok(Some(cli))
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

impl Config {
    /// Merge flags over the rules file over the built-in defaults.
    ///
    /// Precedence, highest first: CLI flag → environment → `watcher_rules.json`
    /// → built-in default. Pure, so the tests can pin it down.
    fn resolve(cli: CliConfig, mut rules: Rules) -> Config {
        if cli.trigger_single_click {
            rules.interactions.click = true;
        }
        let api_url = cli
            .api_url
            .unwrap_or_else(|| DEFAULT_API_URL.to_string())
            .trim_end_matches('/')
            .to_string();

        Config {
            api_url,
            context_interval: Duration::from_secs(
                cli.context_interval.unwrap_or(DEFAULT_CONTEXT_INTERVAL_SECS).max(1),
            ),
            ack_emote: cli
                .ack_emote
                .or_else(|| rules.ack_emote.clone())
                .unwrap_or_else(|| DEFAULT_ACK_EMOTE.to_string()),
            hermes_cmd: cli.hermes_cmd.or_else(|| rules.hermes_cmd.clone()),
            hermes_timeout: Duration::from_secs(
                cli.hermes_timeout
                    .or(rules.hermes_timeout_secs)
                    .unwrap_or(DEFAULT_HERMES_TIMEOUT_SECS)
                    .max(1),
            ),
            webhook_url: cli.webhook_url.or_else(|| rules.webhook_url.clone()),
            dry_run: cli.dry_run,
            rules,
        }
    }
}

/* ------------------------------------------------------------------ *
 * rules
 * ------------------------------------------------------------------ */

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
struct InteractionRules {
    double_click: bool,
    drag_end: bool,
    click: bool,
    cooldown_secs: u64,
}

impl Default for InteractionRules {
    fn default() -> Self {
        Self {
            double_click: true,
            drag_end: true,
            click: false,
            cooldown_secs: DEFAULT_INTERACTION_COOLDOWN_SECS,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
struct ContextRules {
    /// Case-insensitive substrings that make a window title worth a trigger.
    window_keywords: Vec<String>,
    /// Idle time that counts as "away"; dropping below it is a return.
    idle_threshold_secs: u64,
    cooldown_secs: u64,
}

impl Default for ContextRules {
    fn default() -> Self {
        Self {
            window_keywords: vec![
                "error".to_string(),
                "failed".to_string(),
                "exception".to_string(),
            ],
            idle_threshold_secs: DEFAULT_IDLE_THRESHOLD_SECS,
            cooldown_secs: DEFAULT_CONTEXT_COOLDOWN_SECS,
        }
    }
}

/// One per-application rule: "when *this* app's window title contains *that*,
/// react like *this*".
///
/// Both matchers are case-insensitive substrings, and an empty matcher means
/// "any" — so `{"process": "Code.exe"}` reacts to any VS Code window, while
/// `{"title_contains": "error"}` reacts to any app whose title mentions an
/// error. The rule only fires when the focused window *changes*, exactly like
/// the keyword rule, so it cannot machine-gun a live title.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
struct AppRule {
    /// Executable file name from `/context`'s `process_name`, e.g. `Code.exe`.
    process: String,
    /// Substring of the window title.
    title_contains: String,
    /// Expression name (`alert` is *not* one — that would be a clip) or a
    /// custom keyframe clip name, used as the ack animation.
    animation: Option<String>,
    /// Extra context appended to the Hermes prompt for this app.
    prompt_hint: Option<String>,
}

impl AppRule {
    /// Does this rule match the focused window? Empty matchers are wildcards.
    fn matches(&self, process: &str, title: &str) -> bool {
        let process_ok = self.process.trim().is_empty()
            || (!process.is_empty()
                && process.eq_ignore_ascii_case(self.process.trim()));
        let title_ok = self.title_contains.trim().is_empty()
            || title.to_lowercase().contains(&self.title_contains.trim().to_lowercase());
        process_ok && title_ok
    }

    /// One-line description for the trigger detail.
    fn describe(&self) -> String {
        match (self.process.trim(), self.title_contains.trim()) {
            ("", "") => "a per-app rule".to_string(),
            ("", title) => format!("title contains {title:?}"),
            (process, "") => format!("{process} is focused"),
            (process, title) => format!("{process} with a title containing {title:?}"),
        }
    }
}

/// Rule for `alert` frames pushed by external monitors (`POST /alert`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
struct AlertRules {
    /// Set false to let `POST /alert` frames pass without waking Hermes.
    enabled: bool,
    cooldown_secs: u64,
}

impl Default for AlertRules {
    fn default() -> Self {
        Self {
            enabled: true,
            cooldown_secs: DEFAULT_ALERT_COOLDOWN_SECS,
        }
    }
}

/// One quick-task chip, mirrored into `GET /watcher` for the command bar.
///
/// The watcher never sends these itself — the desktop app reads them out of the
/// same rules file — but parsing them here keeps one schema for the whole file
/// and lets startup log what the UI will show.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
struct QuickTask {
    label: String,
    prompt: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
#[serde(default)]
struct Rules {
    interactions: InteractionRules,
    context: ContextRules,
    /// Per-application overrides evaluated when the focused window changes.
    app_rules: Vec<AppRule>,
    /// Cooldown / switch for `alert` frames from `POST /alert`.
    alerts: AlertRules,
    /// Chips for the desktop command bar (`GET /watcher` → `quick_tasks`).
    quick_tasks: Vec<QuickTask>,
    /// Rotate to a fresh `{session}` after this many dispatched turns (absent or
    /// 0 disables the rule).
    max_session_turns: Option<u64>,
    /// Rotate to a fresh `{session}` after this long without a dispatch (absent
    /// or 0 disables the rule).
    session_idle_reset_secs: Option<u64>,
    /// Prefix for generated session ids; defaults to `char-rig`.
    session_base: Option<String>,
    ack_emote: Option<String>,
    hermes_cmd: Option<String>,
    /// Longest `/hermes_cmd` timeout, in seconds.
    hermes_timeout_secs: Option<u64>,
    webhook_url: Option<String>,
}

impl Rules {
    fn from_json(text: &str) -> Result<Rules, String> {
        serde_json::from_str(text).map_err(|err| err.to_string())
    }

    /// Read `--rules` (or the default locations); fall back to defaults.
    fn load(path: Option<&str>) -> Rules {
        let candidates: Vec<PathBuf> = match path {
            Some(explicit) => vec![PathBuf::from(explicit)],
            None => {
                let mut list = vec![PathBuf::from(DEFAULT_RULES_FILE)];
                if let Ok(exe) = std::env::current_exe() {
                    if let Some(dir) = exe.parent() {
                        list.push(dir.join(DEFAULT_RULES_FILE));
                    }
                }
                list
            }
        };

        for candidate in candidates {
            match std::fs::read_to_string(&candidate) {
                Ok(text) => match Rules::from_json(&text) {
                    Ok(rules) => {
                        eprintln!("[watcher] rules loaded from {}", candidate.display());
                        return rules;
                    }
                    Err(err) => {
                        eprintln!(
                            "[watcher] {} is not valid rules ({err}); using built-in defaults",
                            candidate.display()
                        );
                        return Rules::default();
                    }
                },
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => eprintln!("[watcher] cannot read {}: {err}", candidate.display()),
            }
        }

        if let Some(explicit) = path {
            eprintln!("[watcher] rules file {explicit} not found; using built-in defaults");
        } else {
            eprintln!("[watcher] no {DEFAULT_RULES_FILE}; using built-in defaults");
        }
        Rules::default()
    }

    /// Is this interaction kind configured to wake the agent?
    fn interaction_allowed(&self, kind: &str) -> bool {
        match kind {
            "double_click" => self.interactions.double_click,
            "drag_end" => self.interactions.drag_end,
            "click" => self.interactions.click,
            _ => false,
        }
    }

    /// First configured keyword contained in `title`, case-insensitively.
    fn matched_keyword<'a>(&'a self, title: &str) -> Option<&'a str> {
        let haystack = title.to_lowercase();
        self.context
            .window_keywords
            .iter()
            .map(|keyword| keyword.trim())
            .find(|keyword| !keyword.is_empty() && haystack.contains(&keyword.to_lowercase()))
    }

    /// First `app_rules` entry that matches the focused window.
    ///
    /// Order matters: the first match wins, so a specific
    /// `{process, title_contains}` rule listed before a broad `{process}` one
    /// takes precedence.
    fn matched_app_rule<'a>(&'a self, process: &str, title: &str) -> Option<&'a AppRule> {
        self.app_rules
            .iter()
            .find(|rule| rule.matches(process, title))
    }

    /// True when the user just came back from a long idle stretch.
    fn idle_transition(&self, was_idle: bool, idle_secs: u64) -> bool {
        was_idle && idle_secs < self.context.idle_threshold_secs
    }
}

/* ------------------------------------------------------------------ *
 * cooldowns
 * ------------------------------------------------------------------ */

/// Per-trigger-kind cooldown table. `allow` both checks and stamps.
#[derive(Debug, Default)]
struct Cooldowns {
    last: HashMap<String, Instant>,
}

impl Cooldowns {
    fn allow(&mut self, key: &str, now: Instant, cooldown: Duration) -> bool {
        if let Some(previous) = self.last.get(key) {
            if now.saturating_duration_since(*previous) < cooldown {
                return false;
            }
        }
        self.last.insert(key.to_string(), now);
        true
    }
}

/* ------------------------------------------------------------------ *
 * SSE parsing
 * ------------------------------------------------------------------ */

/// One dispatched `text/event-stream` frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SseFrame {
    event: String,
    data: String,
}

/// Incremental SSE parser: feed it arbitrary chunks, get whole frames back.
///
/// Handles `event:`/`data:` fields, multi-line `data`, `:` comment lines (the
/// API's 15 s keep-alive), CRLF, and frames split across TCP reads.
#[derive(Debug, Default)]
struct SseParser {
    buffer: String,
    event: String,
    data: String,
    has_data: bool,
}

impl SseParser {
    fn feed(&mut self, chunk: &str) -> Vec<SseFrame> {
        let mut frames = Vec::new();
        self.buffer.push_str(chunk);
        while let Some(index) = self.buffer.find('\n') {
            let mut line = self.buffer[..index].to_string();
            self.buffer.drain(..=index);
            if line.ends_with('\r') {
                line.pop();
            }
            self.line(&line, &mut frames);
        }
        frames
    }

    fn line(&mut self, line: &str, frames: &mut Vec<SseFrame>) {
        if line.is_empty() {
            if self.has_data {
                let event = if self.event.is_empty() {
                    "message".to_string()
                } else {
                    std::mem::take(&mut self.event)
                };
                let data = std::mem::take(&mut self.data).trim_end().to_string();
                frames.push(SseFrame { event, data });
                self.has_data = false;
            }
            self.event.clear();
            self.data.clear();
            return;
        }
        if line.starts_with(':') {
            return; // comment / keep-alive
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = value.to_string(),
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            _ => {}
        }
    }
}

/* ------------------------------------------------------------------ *
 * sessions
 * ------------------------------------------------------------------ */

/// Which Hermes conversation the next dispatch should use.
///
/// `char-rig-watcher.exe` is the only component that knows how the agent is
/// launched, so it owns the id. The `hermes_cmd` template carries a `{session}`
/// placeholder that is substituted at spawn time; `POST /session/reset` (and the
/// `max_session_turns` / `session_idle_reset_secs` rules) swap the id for a
/// fresh one, which is how "New chat" gets a clean context without restarting
/// anything.
#[derive(Debug, Clone, PartialEq)]
struct SessionState {
    /// Empty until the first dispatch initialises it.
    id: String,
    /// Turns dispatched into the current session.
    turns: u64,
    /// When the last turn was dispatched (drives `session_idle_reset_secs`).
    last_turn: Option<Instant>,
    /// Seconds since the epoch at process start: the readable part of the id.
    seed: u64,
    /// 1 for the initial id, then 2, 3, … per rotation.
    counter: u64,
}

impl Default for SessionState {
    fn default() -> Self {
        Self { id: String::new(), turns: 0, last_turn: None, seed: now_secs(), counter: 1 }
    }
}

impl SessionState {
    /// `char-rig-<seed>` for the first session, `char-rig-<seed>-2` after the
    /// first rotation, and so on — unique per watcher run, short enough to read
    /// in the log, and never reused after a reset (so no stale context).
    fn id_for(&self, base: &str) -> String {
        if self.counter <= 1 {
            format!("{base}-{}", self.seed)
        } else {
            format!("{base}-{}-{}", self.seed, self.counter)
        }
    }

    /// Start a fresh conversation, returning `(previous, current)` ids.
    fn rotate(&mut self, base: &str) -> (String, String) {
        let previous = if self.id.is_empty() {
            "<none>".to_string()
        } else {
            self.id.clone()
        };
        if !self.id.is_empty() {
            self.counter = self.counter.saturating_add(1);
        }
        self.id = self.id_for(base);
        self.turns = 0;
        self.last_turn = None;
        (previous, self.id.clone())
    }
}

fn lock_session(session: &Arc<Mutex<SessionState>>) -> MutexGuard<'_, SessionState> {
    session.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `session_base` from the rules, or the documented default.
fn session_base(rules: &Rules) -> String {
    rules
        .session_base
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_SESSION_BASE)
        .to_string()
}

/// The session id for the next dispatch, rotating first when an auto-rotation
/// rule asks for it.
///
/// Rotation happens *before* the spawn, so the turn that trips `max_session_turns`
/// already lands in the new conversation instead of the one being retired.
fn session_for_dispatch(rules: &Rules, session: &mut SessionState, now: Instant) -> String {
    let base = session_base(rules);
    if session.id.is_empty() {
        session.rotate(&base);
        eprintln!("[watcher] session: {} (new)", session.id);
        return session.id.clone();
    }

    let max_turns = rules.max_session_turns.unwrap_or(0);
    if max_turns > 0 && session.turns >= max_turns {
        let (old, new) = session.rotate(&base);
        eprintln!("[watcher] session rotated after {max_turns} turns: {old} -> {new}");
        return new;
    }

    let idle_secs = rules.session_idle_reset_secs.unwrap_or(0);
    if idle_secs > 0 {
        if let Some(last) = session.last_turn {
            if now.saturating_duration_since(last).as_secs() >= idle_secs {
                let (old, new) = session.rotate(&base);
                eprintln!("[watcher] session rotated after {idle_secs}s idle: {old} -> {new}");
                return new;
            }
        }
    }

    session.id.clone()
}

/* ------------------------------------------------------------------ *
 * triggers
 * ------------------------------------------------------------------ */

#[derive(Debug, Clone, Default, PartialEq)]
struct Trigger {
    /// `interaction` | `window_title` | `idle_return` | `user_reply` | `alert`
    event_type: String,
    detail: String,
    window_title: String,
    idle_secs: u64,
    kind: Option<String>,
    keyword: Option<String>,
    x: Option<f64>,
    y: Option<f64>,
    /// Expression or keyframe clip a matched `app_rules` entry asked for.
    animation: Option<String>,
    /// Tailored context for the prompt (`app_rules[].prompt_hint`).
    hint: Option<String>,
    /// The user's own words, from a `user_reply` frame.
    text: Option<String>,
    /// `info` | `warn` | `error`, from an `alert` frame.
    severity: Option<String>,
}

impl Trigger {
    /// JSON handed to `--webhook-url` and printed by `--dry-run`.
    fn payload(&self, ts_ms: u64) -> Value {
        let mut map = serde_json::Map::new();
        map.insert("event_type".to_string(), json!(self.event_type));
        map.insert("detail".to_string(), json!(self.detail));
        map.insert("window_title".to_string(), json!(self.window_title));
        map.insert("idle_secs".to_string(), json!(self.idle_secs));
        map.insert("ts_ms".to_string(), json!(ts_ms));
        if let Some(kind) = &self.kind {
            map.insert("kind".to_string(), json!(kind));
        }
        if let Some(keyword) = &self.keyword {
            map.insert("keyword".to_string(), json!(keyword));
        }
        if let Some(animation) = &self.animation {
            map.insert("animation".to_string(), json!(animation));
        }
        if let Some(hint) = &self.hint {
            map.insert("hint".to_string(), json!(hint));
        }
        if let Some(text) = &self.text {
            map.insert("text".to_string(), json!(text));
        }
        if let Some(severity) = &self.severity {
            map.insert("severity".to_string(), json!(severity));
        }
        if let Some(x) = self.x {
            map.insert("x".to_string(), json!(x));
        }
        if let Some(y) = self.y {
            map.insert("y".to_string(), json!(y));
        }
        Value::Object(map)
    }

    /// Concise context prompt for `--hermes-cmd`.
    ///
    /// Ends with [`HEADLESS_DIRECTIVE`]: the child has no stdin, so a prompt that
    /// invites a clarifying question would just stall the turn. A `user_reply`
    /// puts the user's words verbatim in the middle — that text is the whole
    /// reason the turn exists — and an `app_rules` hint is appended as
    /// "Context:" so the tailored advice reaches the model.
    fn prompt(&self) -> String {
        let focus = if self.window_title.is_empty() {
            "no focused window".to_string()
        } else {
            format!("focused window {:?}", self.window_title)
        };
        let mut prompt = format!(
            "[char-rig {}] {} ({}; idle {}s).",
            self.event_type, self.detail, focus, self.idle_secs
        );
        if let Some(text) = &self.text {
            prompt.push_str(&format!(
                " The user typed this directly in the desktop overlay and is waiting for an answer: {text:?}. \
Treat it as their task: do what they asked with your tools, then report the outcome with avatar_speak."
            ));
        }
        if let Some(hint) = &self.hint {
            prompt.push_str(&format!(" Context: {hint}"));
        }
        prompt.push_str(" Decide whether to react (speak, emote, move) or ignore it. ");
        prompt.push_str(HEADLESS_DIRECTIVE);
        prompt
    }
}

fn human_kind(kind: &str) -> &'static str {
    match kind {
        "double_click" => "double-clicked",
        "drag_end" => "dragged",
        "click" => "clicked",
        _ => "interacted with",
    }
}

/// Turn an `interaction` SSE payload into a trigger, honouring rules + cooldown.
fn interaction_trigger(
    rules: &Rules,
    shared: &mut Shared,
    kind: &str,
    payload: &Value,
    now: Instant,
) -> Option<Trigger> {
    if !rules.interaction_allowed(kind) {
        return None;
    }
    let cooldown = Duration::from_secs(rules.interactions.cooldown_secs);
    if !shared
        .cooldowns
        .allow(&format!("interaction:{kind}"), now, cooldown)
    {
        return None;
    }

    let x = payload.get("x").and_then(Value::as_f64);
    let y = payload.get("y").and_then(Value::as_f64);
    let where_ = match (x, y) {
        (Some(x), Some(y)) => format!(" at ({x:.0}, {y:.0})"),
        _ => String::new(),
    };

    Some(Trigger {
        event_type: "interaction".to_string(),
        detail: format!("the user {} the avatar{}", human_kind(kind), where_),
        window_title: shared.context.window_title.clone(),
        idle_secs: shared.context.idle_secs,
        kind: Some(kind.to_string()),
        x,
        y,
        ..Trigger::default()
    })
}

/// Turn a `user_reply` SSE payload into a trigger.
///
/// A reply is the user talking *to* the agent, so it never consults a cooldown
/// and never consults the interaction rules: any cooldown that suppressed it
/// would look like the avatar ignoring the user. The dispatcher's single-flight
/// latch still applies.
fn user_reply_trigger(shared: &Shared, payload: &Value, now: Instant) -> Option<Trigger> {
    let text = payload.get("text").and_then(Value::as_str)?.trim();
    if text.is_empty() {
        return None;
    }
    // Keep the surface honest: a reply is a direct address, not a scheduled
    // event, so the only thing recorded is that it happened.
    let _ = now;
    Some(Trigger {
        event_type: "user_reply".to_string(),
        detail: format!("the user replied in the speech bubble: {text:?}"),
        window_title: shared.context.window_title.clone(),
        idle_secs: shared.context.idle_secs,
        text: Some(text.to_string()),
        ..Trigger::default()
    })
}

/// Turn an `alert` SSE payload (from `POST /alert`) into a trigger.
///
/// Unlike a user reply this *is* rate-limited: a network monitor can burst, and
/// each alert costs a model turn. `rules.alerts.cooldown_secs` bounds that, and
/// `alerts.enabled = false` silences the path entirely.
fn alert_trigger(
    rules: &Rules,
    shared: &mut Shared,
    payload: &Value,
    now: Instant,
) -> Option<Trigger> {
    if !rules.alerts.enabled {
        return None;
    }
    let summary = payload.get("summary").and_then(Value::as_str)?.trim();
    if summary.is_empty() {
        return None;
    }
    let source = payload
        .get("source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("an external monitor");
    let severity = payload
        .get("severity")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("info");
    let cooldown = Duration::from_secs(rules.alerts.cooldown_secs);
    if !shared.cooldowns.allow("alert", now, cooldown) {
        return None;
    }

    Some(Trigger {
        event_type: "alert".to_string(),
        detail: format!("{source} reported a {severity} alert: {summary}"),
        window_title: shared.context.window_title.clone(),
        idle_secs: shared.context.idle_secs,
        text: None,
        severity: Some(severity.to_string()),
        hint: Some(format!(
            "This came from {source} over POST /alert, not from the user's own actions; \
say what happened and why it matters in one or two sentences."
        )),
        ..Trigger::default()
    })
}

/// Evaluate one `/context` poll: title rules (keyword + per-app) + idle-return.
///
/// Always updates the shared snapshot (so interaction triggers can quote the
/// current window) and the idle state machine; returns the triggers that passed
/// their cooldown.
///
/// Title matching is `keyword OR app_rule`, and a per-app rule wins when both
/// match: it is the more specific statement of intent, and its `animation` /
/// `prompt_hint` are what the user configured for that app.
fn context_triggers(
    rules: &Rules,
    shared: &mut Shared,
    context: &ContextSnapshot,
    now: Instant,
) -> Vec<Trigger> {
    let mut triggers = Vec::new();
    let previous_title = shared.last_title.clone();
    let previous_idle = shared.last_idle_secs;
    let was_idle = shared.was_idle;

    shared.context = context.clone();
    let cooldown = Duration::from_secs(rules.context.cooldown_secs);

    // ---- window title changed into something worth a trigger ----
    let title_changed = previous_title.as_deref() != Some(context.window_title.as_str());
    if shared.initialized && title_changed && !context.window_title.is_empty() {
        let keyword = rules.matched_keyword(&context.window_title);
        let app_rule = rules.matched_app_rule(&context.process_name, &context.window_title);

        if (keyword.is_some() || app_rule.is_some())
            && shared.cooldowns.allow("context:window_title", now, cooldown)
        {
            let detail = match (app_rule, keyword) {
                (Some(rule), Some(keyword)) => format!(
                    "the focused window changed to {:?} ({}; matched {:?})",
                    context.window_title,
                    rule.describe(),
                    keyword
                ),
                (Some(rule), None) => format!(
                    "the focused window changed to {:?} (matched {})",
                    context.window_title,
                    rule.describe()
                ),
                (None, Some(keyword)) => format!(
                    "the focused window changed to {:?} (matched {:?})",
                    context.window_title, keyword
                ),
                (None, None) => unreachable!("a trigger needs a match"),
            };
            triggers.push(Trigger {
                event_type: "window_title".to_string(),
                detail,
                window_title: context.window_title.clone(),
                idle_secs: context.idle_secs,
                keyword: keyword.map(str::to_string),
                animation: app_rule.and_then(|rule| rule.animation.clone()),
                hint: app_rule.and_then(|rule| rule.prompt_hint.clone()),
                ..Trigger::default()
            });
        }
    }

    // ---- the user came back after a long idle stretch ----
    if context.idle_secs >= rules.context.idle_threshold_secs {
        shared.was_idle = true;
    } else if rules.idle_transition(was_idle, context.idle_secs) {
        shared.was_idle = false;
        if shared.cooldowns.allow("context:idle_return", now, cooldown) {
            triggers.push(Trigger {
                event_type: "idle_return".to_string(),
                detail: format!(
                    "the user came back after about {}s idle",
                    previous_idle.max(rules.context.idle_threshold_secs)
                ),
                window_title: context.window_title.clone(),
                idle_secs: context.idle_secs,
                ..Trigger::default()
            });
        }
    }

    if !context.window_title.is_empty() {
        shared.last_title = Some(context.window_title.clone());
    }
    shared.last_idle_secs = context.idle_secs;
    shared.initialized = true;
    triggers
}

/* ------------------------------------------------------------------ *
 * shared state
 * ------------------------------------------------------------------ */

#[derive(Debug, Clone, Default, PartialEq)]
struct ContextSnapshot {
    window_title: String,
    /// Executable that owns the foreground window (e.g. `Code.exe`); empty on
    /// older avatar builds and off Windows.
    process_name: String,
    idle_secs: u64,
}

#[derive(Debug, Default)]
struct Shared {
    /// Latest `/context` reading, quoted in interaction triggers.
    context: ContextSnapshot,
    /// False until the first poll has been folded in (the first one is only a
    /// baseline — the watcher must not fire on the title it starts with).
    initialized: bool,
    /// Last non-empty window title seen (drives the keyword rule).
    last_title: Option<String>,
    /// Previous poll's idle time (reported by the idle-return trigger).
    last_idle_secs: u64,
    /// True while the machine is considered away (idle >= threshold).
    was_idle: bool,
    cooldowns: Cooldowns,
}

fn lock(shared: &Arc<Mutex<Shared>>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/* ------------------------------------------------------------------ *
 * dispatch
 * ------------------------------------------------------------------ */

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Whole seconds since the epoch; the readable part of a session id.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Split a command line into argv, honouring single/double quotes.
///
/// No shell is involved: the prompt is passed as one argv entry, so it can never
/// be re-interpreted as shell syntax.
fn split_command(command: &str) -> Vec<String> {
    let mut argv = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;

    for ch in command.chars() {
        match quote {
            Some(open) if ch == open => quote = None,
            Some(_) => {
                current.push(ch);
                started = true;
            }
            None => match ch {
                '"' | '\'' => {
                    quote = Some(ch);
                    started = true;
                }
                c if c.is_whitespace() => {
                    if started {
                        argv.push(std::mem::take(&mut current));
                        started = false;
                    }
                }
                c => {
                    current.push(c);
                    started = true;
                }
            },
        }
    }
    if started {
        argv.push(current);
    }
    argv
}

/// argv for the configured command, with `{session}` and the prompt substituted
/// (the prompt falls back to being appended as the last argv entry).
///
/// Two placeholders are supported:
/// * `{session}` — the current conversation id (only meaningful inside a
///   `--continue`-style flag; without it rotation cannot reach the CLI).
/// * `{prompt}` — the trigger prompt; when absent it is appended last, which is
///   why a template ending in `-q` works.
fn command_argv(command: &str, prompt: &str, session: &str) -> Vec<String> {
    let mut argv = split_command(command);
    let mut substituted = false;
    for arg in argv.iter_mut() {
        if arg.contains("{session}") {
            *arg = arg.replace("{session}", session);
        }
        if arg.contains("{prompt}") {
            *arg = arg.replace("{prompt}", prompt);
            substituted = true;
        }
    }
    if !substituted && !argv.is_empty() {
        argv.push(prompt.to_string());
    }
    argv
}

/// One-dispatch-at-a-time latch.
///
/// Triggers are already serialised by [`dispatcher`], but a spawned
/// `--hermes-cmd` child outlives `handle_trigger`: without this guard a burst of
/// interactions would start one `hermes` process per trigger and they would pile
/// up (each one an LLM turn, each one competing for the same API). A trigger
/// that arrives while a dispatch is still running is skipped instead.
#[derive(Debug, Clone, Default)]
struct SingleFlight {
    busy: Arc<AtomicBool>,
}

impl SingleFlight {
    /// Take the permit, or `None` when another dispatch already holds it.
    fn try_enter(&self) -> Option<FlightGuard> {
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            Some(FlightGuard {
                busy: Arc::clone(&self.busy),
            })
        } else {
            None
        }
    }
}

/// Held for as long as one dispatch is in flight; dropping it releases the
/// permit, so a panic in the spawned task can never wedge the watcher.
#[derive(Debug)]
struct FlightGuard {
    busy: Arc<AtomicBool>,
}

impl Drop for FlightGuard {
    fn drop(&mut self) {
        self.busy.store(false, Ordering::Release);
    }
}

/// Spawn the configured command, wait for it under a timeout, and log the exit.
///
/// * `session` is substituted into `{session}` in the command template.
/// * `stdin` is `/dev/null` so an interactive prompt reads EOF immediately
///   instead of hanging on a keyboard that is not attached.
/// * The wait is wrapped in [`tokio::time::timeout`]; on expiry the child is
///   killed and reaped, so a wedged agent cannot hold the single-flight permit
///   forever.
/// * `flight` moves into the spawned task: the permit is released exactly when
///   the child is gone.
///
/// Returns `true` when a process was actually started (the caller uses that to
/// count a turn against the session).
fn spawn_hermes(
    command: &str,
    prompt: &str,
    session: &str,
    timeout: Duration,
    flight: FlightGuard,
) -> bool {
    let mut argv = command_argv(command, prompt, session);
    if argv.is_empty() {
        eprintln!("[watcher] --hermes-cmd is empty; nothing spawned");
        return false;
    }
    let program = argv.remove(0);
    let spawned = tokio::process::Command::new(&program)
        .args(&argv)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn();

    match spawned {
        Ok(mut child) => {
            let pid = child
                .id()
                .map(|id| id.to_string())
                .unwrap_or_else(|| "?".to_string());
            eprintln!(
                "[watcher] spawned {program} (pid {pid}, session {session}, timeout {}s)",
                timeout.as_secs()
            );
            tokio::spawn(async move {
                let _flight = flight; // released when this task ends
                let waited = tokio::time::timeout(timeout, child.wait()).await;
                match waited {
                    Ok(Ok(status)) => eprintln!("[watcher] {program} exited with {status}"),
                    Ok(Err(err)) => eprintln!("[watcher] waiting for {program} failed: {err}"),
                    Err(_) => {
                        eprintln!(
                            "[watcher] {program} (pid {pid}) still running after {}s; killing it",
                            timeout.as_secs()
                        );
                        if let Err(err) = child.start_kill() {
                            eprintln!("[watcher] cannot kill {program}: {err}");
                        }
                        match child.wait().await {
                            Ok(status) => eprintln!("[watcher] {program} killed ({status})"),
                            Err(err) => eprintln!("[watcher] cannot reap {program}: {err}"),
                        }
                    }
                }
            });
            true
        }
        Err(err) => {
            eprintln!("[watcher] cannot spawn {program:?}: {err}");
            false
        }
    }
}

/// The immediate visual ack for a trigger.
#[derive(Debug, Clone, PartialEq)]
enum Ack {
    /// `POST /emote` with an expression name.
    Emote(String),
    /// `POST /animate` with a custom keyframe clip name.
    Animate(String),
}

/// Decide what the avatar does the moment a trigger fires.
///
/// A matched `app_rules` entry's `animation` wins over the generic
/// `--ack-emote`: the whole point of the per-app rule is a tailored reaction.
/// The value is an expression when it names one (six known names) and a clip
/// name otherwise, so `"animation": "alert"` plays a clip called `alert` while
/// `"animation": "thinking"` emotes.
fn ack_plan(trigger: &Trigger, config: &Config) -> Option<Ack> {
    if let Some(animation) = trigger
        .animation
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(if EXPRESSIONS.contains(&animation) {
            Ack::Emote(animation.to_string())
        } else {
            Ack::Animate(animation.to_string())
        });
    }
    if config.ack_emote.is_empty() {
        None
    } else {
        Some(Ack::Emote(config.ack_emote.clone()))
    }
}

/// Deliver one trigger: dry-run print → ack emote → webhook → Hermes command.
///
/// The whole side-effect chain is one flight: a trigger arriving while a
/// previous dispatch (including a still-running `--hermes-cmd` child) is in
/// flight is dropped with a debug line rather than queued.
async fn handle_trigger(
    config: &Config,
    client: &reqwest::Client,
    trigger: &Trigger,
    flight: &SingleFlight,
    session: &Arc<Mutex<SessionState>>,
) {
    let payload = trigger.payload(now_ms());

    if config.dry_run {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string())
        );
        let _ = std::io::stdout().flush();
        return;
    }

    let Some(guard) = flight.try_enter() else {
        eprintln!(
            "[watcher] debug: dispatch already in flight, skipping {} trigger ({} in flight)",
            trigger.event_type, trigger.detail
        );
        return;
    };

    eprintln!("[watcher] trigger {}: {}", trigger.event_type, trigger.detail);

    // 1. Immediate visual ack, so the avatar reacts before the agent answers.
    match ack_plan(trigger, config) {
        Some(Ack::Emote(expression)) => {
            let body = json!({
                "expression": expression,
                "duration_ms": ACK_EMOTE_DURATION_MS,
                "revert": "neutral",
            });
            let url = format!("{}/emote", config.api_url);
            match client.post(&url).json(&body).timeout(CONTEXT_TIMEOUT).send().await {
                Ok(response) if !response.status().is_success() => {
                    eprintln!("[watcher] ack emote rejected with {}", response.status())
                }
                Ok(_) => {}
                Err(err) => eprintln!("[watcher] ack emote failed: {err}"),
            }
        }
        Some(Ack::Animate(name)) => {
            let body = json!({ "name": name, "loop": true });
            let url = format!("{}/animate", config.api_url);
            match client.post(&url).json(&body).timeout(CONTEXT_TIMEOUT).send().await {
                Ok(response) if !response.status().is_success() => {
                    eprintln!(
                        "[watcher] ack animation {name:?} rejected with {} (is it a clip on this rig?)",
                        response.status()
                    )
                }
                Ok(_) => {}
                Err(err) => eprintln!("[watcher] ack animation failed: {err}"),
            }
        }
        None => {}
    }

    // 2. Webhook.
    if let Some(url) = &config.webhook_url {
        match client.post(url).json(&payload).timeout(CONTEXT_TIMEOUT).send().await {
            Ok(response) if !response.status().is_success() => {
                eprintln!("[watcher] webhook answered {}", response.status())
            }
            Ok(_) => {}
            Err(err) => eprintln!("[watcher] webhook failed: {err}"),
        }
    }

    // 3. Hermes (or a plain log line when nothing is wired up). The guard moves
    //    into the child's task so the permit covers the process lifetime.
    match &config.hermes_cmd {
        Some(command) => {
            let prompt = trigger.prompt();
            // Pick (or rotate) the conversation *before* spawning, so a turn
            // that trips max_session_turns already lands in the new session.
            let session_id = {
                let mut state = lock_session(session);
                session_for_dispatch(&config.rules, &mut state, Instant::now())
            };
            let spawned =
                spawn_hermes(command, &prompt, &session_id, config.hermes_timeout, guard);
            if spawned {
                let mut state = lock_session(session);
                state.turns = state.turns.saturating_add(1);
                state.last_turn = Some(Instant::now());
            }
        }
        None if config.webhook_url.is_none() => {
            drop(guard);
            eprintln!(
                "[watcher] no --hermes-cmd / --webhook-url; payload was {}",
                payload
            )
        }
        None => drop(guard),
    }
}

/* ------------------------------------------------------------------ *
 * HTTP
 * ------------------------------------------------------------------ */

fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(concat!("char-rig-watcher/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// `GET /context` → snapshot. A non-2xx or unparsable body is an error.
async fn fetch_context(client: &reqwest::Client, api_url: &str) -> Result<ContextSnapshot, String> {
    let url = format!("{api_url}/context");
    let response = client
        .get(&url)
        .timeout(CONTEXT_TIMEOUT)
        .send()
        .await
        .map_err(|err| err.to_string())?;
    if !response.status().is_success() {
        return Err(format!("{url} answered {}", response.status()));
    }
    let body: Value = response.json().await.map_err(|err| err.to_string())?;
    Ok(ContextSnapshot {
        window_title: body
            .get("window_title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        // `/context` grew `process_name` after the first release; an older
        // avatar build simply leaves it empty and the title rules still work.
        process_name: body
            .get("process_name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        idle_secs: body.get("idle_secs").and_then(Value::as_u64).unwrap_or(0),
    })
}

/* ------------------------------------------------------------------ *
 * tasks
 * ------------------------------------------------------------------ */

/// Turn one SSE frame into a queued trigger.
///
/// Six frame types matter: `interaction` (the user poked the avatar),
/// `user_reply` (they typed in the bubble or command bar), `alert` (`POST
/// /alert` from an external monitor), `session_reset` (`POST /session/reset` —
/// rotates the conversation and queues nothing), plus the context loop's own
/// `window_title` / `idle_return`. Every other type — `state`, `speech`,
/// `animate`, … — is ignored so the watcher stays quiet while the page animates.
async fn handle_frame(
    frame: &SseFrame,
    rules: &Rules,
    shared: &Arc<Mutex<Shared>>,
    session: &Arc<Mutex<SessionState>>,
    tx: &mpsc::Sender<Trigger>,
) {
    if frame.event == "hello" || frame.data.is_empty() {
        return;
    }
    let value: Value = match serde_json::from_str(&frame.data) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("[watcher] skipping unparsable SSE frame ({err})");
            return;
        }
    };
    let frame_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or(frame.event.as_str());
    let payload = value.get("payload").cloned().unwrap_or(Value::Null);

    // A reset is an instruction, not an event to report: rotate and stay quiet.
    if frame_type == "session_reset" {
        let reason = payload
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let base = session_base(rules);
        let (old, new) = {
            let mut state = lock_session(session);
            state.rotate(&base)
        };
        eprintln!("[watcher] session reset ({reason}): {old} -> {new}");
        return;
    }

    let trigger = match frame_type {
        "interaction" => {
            let Some(kind) = payload.get("kind").and_then(Value::as_str) else {
                return;
            };
            let mut guard = lock(shared);
            interaction_trigger(rules, &mut guard, kind, &payload, Instant::now())
        }
        "user_reply" => {
            let guard = lock(shared);
            user_reply_trigger(&guard, &payload, Instant::now())
        }
        "alert" => {
            let mut guard = lock(shared);
            alert_trigger(rules, &mut guard, &payload, Instant::now())
        }
        _ => return,
    };

    if let Some(trigger) = trigger {
        if tx.send(trigger).await.is_err() {
            eprintln!("[watcher] dispatcher is gone; dropping trigger");
        }
    }
}

/// Subscribe to `GET /events` until the stream ends, then return.
async fn stream_events(
    config: &Config,
    client: &reqwest::Client,
    shared: &Arc<Mutex<Shared>>,
    session: &Arc<Mutex<SessionState>>,
    tx: &mpsc::Sender<Trigger>,
    backoff: &mut Duration,
) -> Result<(), String> {
    let url = format!("{}/events", config.api_url);
    let response = client
        .get(&url)
        .header("accept", "text/event-stream")
        .send()
        .await
        .map_err(|err| err.to_string())?;

    if !response.status().is_success() {
        return Err(format!("{url} answered {}", response.status()));
    }
    eprintln!("[watcher] subscribed to {url}");
    *backoff = INITIAL_BACKOFF;

    let mut parser = SseParser::default();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|err| format!("stream error: {err}"))?;
        let text = String::from_utf8_lossy(&bytes);
        for frame in parser.feed(&text) {
            handle_frame(&frame, &config.rules, shared, session, tx).await;
        }
    }
    Err("event stream closed by the server".to_string())
}

/// Reconnect forever with exponential backoff.
async fn sse_loop(
    config: Config,
    client: reqwest::Client,
    shared: Arc<Mutex<Shared>>,
    session: Arc<Mutex<SessionState>>,
    tx: mpsc::Sender<Trigger>,
) {
    let mut backoff = INITIAL_BACKOFF;
    loop {
        match stream_events(&config, &client, &shared, &session, &tx, &mut backoff).await {
            Ok(()) => {}
            Err(err) => eprintln!("[watcher] {err}"),
        }
        eprintln!("[watcher] reconnecting in {:.0}s", backoff.as_secs_f64());
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// Poll `GET /context` on an interval and evaluate the context rules.
async fn context_loop(
    config: Config,
    client: reqwest::Client,
    shared: Arc<Mutex<Shared>>,
    tx: mpsc::Sender<Trigger>,
) {
    let mut ticker = tokio::time::interval(config.context_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut warned = false;

    loop {
        ticker.tick().await;
        match fetch_context(&client, &config.api_url).await {
            Ok(context) => {
                warned = false;
                let triggers = {
                    let mut guard = lock(&shared);
                    context_triggers(&config.rules, &mut guard, &context, Instant::now())
                };
                for trigger in triggers {
                    if tx.send(trigger).await.is_err() {
                        return;
                    }
                }
            }
            Err(err) => {
                if !warned {
                    eprintln!("[watcher] context poll failed: {err} (avatar offline?)");
                    warned = true;
                }
            }
        }
    }
}

/// Serialise dispatch so triggers never race each other, and hold the
/// single-flight latch across the whole side-effect chain (including a
/// `--hermes-cmd` child that outlives this loop).
async fn dispatcher(
    config: Config,
    client: reqwest::Client,
    session: Arc<Mutex<SessionState>>,
    mut rx: mpsc::Receiver<Trigger>,
) {
    let flight = SingleFlight::default();
    while let Some(trigger) = rx.recv().await {
        handle_trigger(&config, &client, &trigger, &flight, &session).await;
    }
}

/* ------------------------------------------------------------------ *
 * main
 * ------------------------------------------------------------------ */

#[tokio::main]
async fn main() {
    let cli = match parse_cli(std::env::args().skip(1)) {
        Ok(Some(cli)) => cli,
        Ok(None) => return, // --help
        Err(message) => {
            eprintln!("char-rig-watcher: {message}");
            std::process::exit(2);
        }
    };

    let rules = Rules::load(cli.rules_path.as_deref());
    let config = Config::resolve(cli, rules);

    eprintln!(
        "[watcher] {} ready (api {}, context every {}s, ack {:?}, {})",
        env!("CARGO_BIN_NAME"),
        config.api_url,
        config.context_interval.as_secs(),
        if config.ack_emote.is_empty() { "<off>" } else { &config.ack_emote },
        if config.dry_run { "dry run" } else { "live" }
    );
    match (&config.hermes_cmd, &config.webhook_url) {
        (Some(cmd), _) => {
            eprintln!(
                "[watcher] dispatch: spawn {cmd:?} (timeout {}s, one at a time)",
                config.hermes_timeout.as_secs()
            );
            if !cmd.contains("{session}") {
                eprintln!(
                    "[watcher] hermes_cmd has no {{session}} placeholder: rotation cannot reach the \
                     CLI (add a --continue {{session}} flag to make New chat effective)"
                );
            }
        }
        (None, Some(url)) => eprintln!("[watcher] dispatch: webhook {url}"),
        (None, None) if config.dry_run => eprintln!("[watcher] dispatch: stdout only (dry run)"),
        (None, None) => eprintln!("[watcher] dispatch: none configured (triggers are logged only)"),
    }
    if !config.rules.quick_tasks.is_empty() {
        eprintln!(
            "[watcher] quick_tasks: {} chips served to the command bar via GET /watcher",
            config.rules.quick_tasks.len()
        );
    }
    if let Some(turns) = config.rules.max_session_turns.filter(|turns| *turns > 0) {
        eprintln!("[watcher] session auto-rotates after {turns} turns");
    }
    if let Some(secs) = config.rules.session_idle_reset_secs.filter(|secs| *secs > 0) {
        eprintln!("[watcher] session auto-rotates after {secs}s idle");
    }
    eprintln!("[watcher] session base: {:?}", session_base(&config.rules));

    let client = build_client();
    let shared = Arc::new(Mutex::new(Shared::default()));
    let session = Arc::new(Mutex::new(SessionState::default()));
    let (tx, rx) = mpsc::channel::<Trigger>(TRIGGER_QUEUE);

    let sse = tokio::spawn(sse_loop(
        config.clone(),
        client.clone(),
        Arc::clone(&shared),
        Arc::clone(&session),
        tx.clone(),
    ));
    let context = tokio::spawn(context_loop(config.clone(), client.clone(), Arc::clone(&shared), tx.clone()));
    let dispatch = tokio::spawn(dispatcher(config, client, Arc::clone(&session), rx));

    // Any task ending means the watcher is useless: stop everything.
    let _ = tokio::select! {
        _ = sse => eprintln!("[watcher] sse task stopped"),
        _ = context => eprintln!("[watcher] context task stopped"),
        _ = dispatch => eprintln!("[watcher] dispatcher stopped"),
    };
    std::process::exit(0);
}

/* ------------------------------------------------------------------ *
 * tests
 * ------------------------------------------------------------------ */

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> Shared {
        Shared::default()
    }

    fn rules(json_text: &str) -> Rules {
        Rules::from_json(json_text).expect("valid rules")
    }

    /// A `/context` reading with no owning process (title-keyword rules only).
    fn snap(title: &str, idle_secs: u64) -> ContextSnapshot {
        ContextSnapshot {
            window_title: title.to_string(),
            process_name: String::new(),
            idle_secs,
        }
    }

    /// A `/context` reading from a specific executable (`app_rules` tests).
    fn snap_app(title: &str, process: &str, idle_secs: u64) -> ContextSnapshot {
        ContextSnapshot {
            window_title: title.to_string(),
            process_name: process.to_string(),
            idle_secs,
        }
    }

    /* ---------- SSE parsing ---------- */

    #[test]
    fn sse_parser_reads_event_and_data() {
        let mut parser = SseParser::default();
        let frames = parser.feed("event: interaction\ndata: {\"a\":1}\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event, "interaction");
        assert_eq!(frames[0].data, "{\"a\":1}");
    }

    #[test]
    fn sse_parser_survives_arbitrary_chunk_boundaries() {
        let mut parser = SseParser::default();
        let mut frames = Vec::new();
        for chunk in ["eve", "nt: animate\nda", "ta: {\"name\":\"wave\"}", "\n", "\n"] {
            frames.extend(parser.feed(chunk));
        }
        assert_eq!(frames.len(), 1, "one frame assembled from five chunks");
        assert_eq!(frames[0].event, "animate");
        assert_eq!(frames[0].data, "{\"name\":\"wave\"}");
    }

    #[test]
    fn sse_parser_ignores_comments_and_keepalives() {
        let mut parser = SseParser::default();
        let frames = parser.feed(": keep-alive\n\n: another\n");
        assert!(frames.is_empty());
        // a comment between two frames must not merge them
        let frames = parser.feed("event: a\ndata: 1\n\n: keep-alive\n\nevent: b\ndata: 2\n\n");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].event, "a");
        assert_eq!(frames[1].event, "b");
    }

    #[test]
    fn sse_parser_joins_multiline_data_and_handles_crlf() {
        let mut parser = SseParser::default();
        let frames = parser.feed("event: x\r\ndata: one\r\ndata: two\r\n\r\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "one\ntwo");
    }

    #[test]
    fn sse_parser_defaults_to_message_and_keeps_two_frames_apart() {
        let mut parser = SseParser::default();
        let frames = parser.feed("data: 1\n\ndata: 2\n\n");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].event, "message");
        assert_eq!(frames[1].data, "2");
    }

    /* ---------- rules ---------- */

    #[test]
    fn rules_defaults_match_the_documented_values() {
        let default = Rules::default();
        assert!(default.interactions.double_click);
        assert!(default.interactions.drag_end);
        assert!(!default.interactions.click, "single clicks are opt-in");
        assert_eq!(default.interactions.cooldown_secs, 15);
        assert_eq!(default.context.cooldown_secs, 300);
        assert_eq!(default.context.idle_threshold_secs, 900);
        assert!(default.context.window_keywords.contains(&"error".to_string()));
    }

    #[test]
    fn rules_parse_a_partial_file() {
        let parsed = rules(r#"{"interactions":{"click":true},"context":{"window_keywords":["deploy"]}}"#);
        assert!(parsed.interactions.click, "explicit value wins");
        assert!(parsed.interactions.double_click, "missing fields keep defaults");
        assert_eq!(parsed.interactions.cooldown_secs, 15);
        assert_eq!(parsed.context.window_keywords, vec!["deploy".to_string()]);
        assert_eq!(parsed.context.idle_threshold_secs, 900);
    }

    #[test]
    fn rules_reject_invalid_json() {
        assert!(Rules::from_json("{not json").is_err());
    }

    #[test]
    fn interaction_rules_gate_each_kind() {
        let default = Rules::default();
        assert!(default.interaction_allowed("double_click"));
        assert!(default.interaction_allowed("drag_end"));
        assert!(!default.interaction_allowed("click"));
        assert!(!default.interaction_allowed("hover"), "unknown kinds never fire");

        let with_click = rules(r#"{"interactions":{"click":true}}"#);
        assert!(with_click.interaction_allowed("click"));
    }

    #[test]
    fn window_keywords_match_case_insensitively() {
        let rules = rules(r#"{"context":{"window_keywords":["Error","Build FAILED"]}}"#);
        assert_eq!(rules.matched_keyword("TypeError: undefined"), Some("Error"));
        assert_eq!(rules.matched_keyword("make: build failed"), Some("Build FAILED"));
        assert_eq!(rules.matched_keyword("Visual Studio Code"), None);
    }

    #[test]
    fn blank_keywords_are_ignored() {
        let rules = rules(r#"{"context":{"window_keywords":["  ","error"]}}"#);
        assert_eq!(rules.matched_keyword("all good"), None);
        assert_eq!(rules.matched_keyword("an error"), Some("error"));
    }

    #[test]
    fn idle_transition_needs_a_previous_away_state() {
        let rules = Rules::default();
        assert!(rules.idle_transition(true, 3), "away -> back fires");
        assert!(!rules.idle_transition(false, 3), "never away, no trigger");
        assert!(!rules.idle_transition(true, 950), "still away, no trigger");
    }

    /* ---------- cooldowns ---------- */

    #[test]
    fn cooldown_suppresses_repeats_then_allows_again() {
        let base = Instant::now();
        let mut cooldowns = Cooldowns::default();
        let cooldown = Duration::from_secs(15);

        assert!(cooldowns.allow("interaction:double_click", base, cooldown));
        assert!(!cooldowns.allow("interaction:double_click", base + Duration::from_secs(14), cooldown));
        assert!(cooldowns.allow("interaction:double_click", base + Duration::from_secs(15), cooldown));
        assert!(!cooldowns.allow("interaction:double_click", base + Duration::from_secs(20), cooldown));
        assert!(cooldowns.allow("interaction:double_click", base + Duration::from_secs(31), cooldown));
    }

    #[test]
    fn cooldowns_are_per_key() {
        let base = Instant::now();
        let mut cooldowns = Cooldowns::default();
        let cooldown = Duration::from_secs(300);
        assert!(cooldowns.allow("context:window_title", base, cooldown));
        assert!(
            cooldowns.allow("context:idle_return", base + Duration::from_secs(1), cooldown),
            "a different trigger is not blocked"
        );
        assert!(!cooldowns.allow("context:window_title", base + Duration::from_secs(1), cooldown));
    }

    /* ---------- interaction triggers ---------- */

    #[test]
    fn interaction_trigger_carries_kind_coordinates_and_context() {
        let base = Instant::now();
        let rules = Rules::default();
        let mut shared = shared();
        shared.context = ContextSnapshot {
            window_title: "Visual Studio Code".to_string(),
            process_name: "Code.exe".to_string(),
            idle_secs: 12,
        };
        let payload = json!({ "kind": "double_click", "x": 541.0, "y": 489.0, "state": "idle" });

        let trigger = interaction_trigger(&rules, &mut shared, "double_click", &payload, base)
            .expect("double click fires");
        assert_eq!(trigger.event_type, "interaction");
        assert_eq!(trigger.kind.as_deref(), Some("double_click"));
        assert_eq!(trigger.x, Some(541.0));
        assert_eq!(trigger.y, Some(489.0));
        assert_eq!(trigger.window_title, "Visual Studio Code");
        assert_eq!(trigger.idle_secs, 12);
        assert!(trigger.detail.contains("double-clicked"));
        assert!(trigger.detail.contains("(541, 489)"));
    }

    #[test]
    fn interaction_trigger_respects_rules_and_cooldown() {
        let base = Instant::now();
        let rules = Rules::default();
        let mut shared = shared();
        let payload = json!({ "kind": "click", "x": 1.0, "y": 2.0 });

        assert!(
            interaction_trigger(&rules, &mut shared, "click", &payload, base).is_none(),
            "single clicks are off by default"
        );

        let double = json!({ "kind": "double_click", "x": 1.0, "y": 2.0 });
        assert!(interaction_trigger(&rules, &mut shared, "double_click", &double, base).is_some());
        assert!(
            interaction_trigger(&rules, &mut shared, "double_click", &double, base + Duration::from_secs(5)).is_none(),
            "second click inside the 15s cooldown is suppressed"
        );
        assert!(
            interaction_trigger(&rules, &mut shared, "double_click", &double, base + Duration::from_secs(16)).is_some(),
            "after the cooldown it fires again"
        );
    }

    #[test]
    fn single_click_can_be_enabled() {
        let base = Instant::now();
        let rules = rules(r#"{"interactions":{"click":true}}"#);
        let mut shared = shared();
        let payload = json!({ "kind": "click", "x": 0.0, "y": 0.0 });
        assert!(interaction_trigger(&rules, &mut shared, "click", &payload, base).is_some());
    }

    /* ---------- context triggers ---------- */

    #[test]
    fn first_context_poll_only_sets_the_baseline() {
        let base = Instant::now();
        let rules = Rules::default();
        let mut shared = shared();
        let context = snap("some error log", 5);
        assert!(
            context_triggers(&rules, &mut shared, &context, base).is_empty(),
            "the first poll must not fire on the title it starts with"
        );
        assert_eq!(shared.last_title.as_deref(), Some("some error log"));
    }

    #[test]
    fn title_change_matching_a_keyword_fires_once() {
        let base = Instant::now();
        let rules = Rules::default();
        let mut shared = shared();
        context_triggers(&rules, &mut shared, &snap("Editor", 1), base);

        let triggers = context_triggers(
            &rules,
            &mut shared,
            &snap("Build failed - project", 2),
            base + Duration::from_secs(20),
        );
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0].event_type, "window_title");
        assert_eq!(triggers[0].keyword.as_deref(), Some("failed"));
        assert!(triggers[0].detail.contains("Build failed"));

        // same title again: no change, no trigger
        let again = context_triggers(
            &rules,
            &mut shared,
            &snap("Build failed - project", 3),
            base + Duration::from_secs(40),
        );
        assert!(again.is_empty());
    }

    #[test]
    fn title_change_without_a_keyword_is_quiet() {
        let base = Instant::now();
        let rules = Rules::default();
        let mut shared = shared();
        context_triggers(&rules, &mut shared, &snap("Editor", 1), base);
        let triggers = context_triggers(
            &rules,
            &mut shared,
            &snap("Terminal", 2),
            base + Duration::from_secs(20),
        );
        assert!(triggers.is_empty());
    }

    #[test]
    fn idle_return_fires_when_the_user_comes_back() {
        let base = Instant::now();
        let rules = Rules::default();
        let mut shared = shared();

        // away
        context_triggers(&rules, &mut shared, &snap("Editor", 950), base);
        assert!(shared.was_idle);

        // back
        let triggers = context_triggers(
            &rules,
            &mut shared,
            &snap("Editor", 2),
            base + Duration::from_secs(20),
        );
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0].event_type, "idle_return");
        assert_eq!(triggers[0].idle_secs, 2);
        assert!(triggers[0].detail.contains("950"));
        assert!(!shared.was_idle);

        // staying active does not fire again
        let again = context_triggers(
            &rules,
            &mut shared,
            &snap("Editor", 3),
            base + Duration::from_secs(40),
        );
        assert!(again.is_empty());
    }

    #[test]
    fn context_cooldown_suppresses_a_burst_of_matching_titles() {
        let base = Instant::now();
        let rules = rules(r#"{"context":{"cooldown_secs":300}}"#);
        let mut shared = shared();
        context_triggers(&rules, &mut shared, &snap("Editor", 1), base);

        let first = context_triggers(
            &rules,
            &mut shared,
            &snap("error one", 1),
            base + Duration::from_secs(10),
        );
        assert_eq!(first.len(), 1);

        let second = context_triggers(
            &rules,
            &mut shared,
            &snap("error two", 1),
            base + Duration::from_secs(60),
        );
        assert!(second.is_empty(), "inside the 300s cooldown");

        let third = context_triggers(
            &rules,
            &mut shared,
            &snap("error three", 1),
            base + Duration::from_secs(320),
        );
        assert_eq!(third.len(), 1, "after the cooldown");
    }

    /* ---------- payloads and prompts ---------- */

    #[test]
    fn payload_contains_the_documented_keys() {
        let trigger = Trigger {
            event_type: "interaction".to_string(),
            detail: "the user double-clicked the avatar".to_string(),
            window_title: "Visual Studio Code".to_string(),
            idle_secs: 12,
            kind: Some("double_click".to_string()),
            x: Some(541.0),
            y: Some(489.0),
            ..Trigger::default()
        };
        let payload = trigger.payload(1_700_000_000_000);
        assert_eq!(payload["event_type"], "interaction");
        assert_eq!(payload["detail"], "the user double-clicked the avatar");
        assert_eq!(payload["window_title"], "Visual Studio Code");
        assert_eq!(payload["idle_secs"], 12);
        assert_eq!(payload["kind"], "double_click");
        assert_eq!(payload["x"], 541.0);
        assert_eq!(payload["ts_ms"], 1_700_000_000_000u64);
        assert!(payload.get("keyword").is_none(), "absent fields are omitted");
    }

    #[test]
    fn prompt_mentions_type_detail_window_and_idle() {
        let trigger = Trigger {
            event_type: "window_title".to_string(),
            detail: "the focused window changed to \"Build failed\" (matched \"failed\")".to_string(),
            window_title: "Build failed".to_string(),
            idle_secs: 3,
            keyword: Some("failed".to_string()),
            ..Trigger::default()
        };
        let prompt = trigger.prompt();
        assert!(prompt.starts_with("[char-rig window_title]"), "{prompt}");
        assert!(prompt.contains("Build failed"), "{prompt}");
        assert!(prompt.contains("idle 3s"), "{prompt}");
        assert!(prompt.contains("Decide whether to react"), "{prompt}");
    }

    /* ---------- command building ---------- */

    #[test]
    fn split_command_handles_quotes_and_spacing() {
        assert_eq!(split_command("hermes   chat"), vec!["hermes", "chat"]);
        assert_eq!(
            split_command(r#"hermes chat --name "my agent""#),
            vec!["hermes", "chat", "--name", "my agent"]
        );
        assert_eq!(split_command("  "), Vec::<String>::new());
        assert_eq!(split_command("'a b' c"), vec!["a b", "c"]);
    }

    #[test]
    fn command_argv_appends_or_substitutes_the_prompt() {
        let argv = command_argv("hermes chat", "hello world", "char-rig-1");
        assert_eq!(argv, vec!["hermes", "chat", "hello world"]);

        let argv = command_argv("hermes --prompt={prompt}", "hello", "char-rig-1");
        assert_eq!(argv, vec!["hermes", "--prompt=hello"]);

        let argv = command_argv("", "hello", "char-rig-1");
        assert!(argv.is_empty(), "an empty command yields no argv");
    }

    #[test]
    fn prompt_is_one_argv_entry_even_with_shell_characters() {
        let prompt = "user clicked; rm -rf / && echo hi";
        let argv = command_argv("agent", prompt, "char-rig-1");
        assert_eq!(argv.len(), 2);
        assert_eq!(argv[1], prompt, "no shell, so nothing is re-interpreted");
    }

    /* ---------- CLI ---------- */

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn cli_parses_flags_and_defaults() {
        let cli = parse_cli(args(&["--dry-run", "--context-interval", "5"])).unwrap().unwrap();
        assert!(cli.dry_run);
        assert_eq!(cli.context_interval, Some(5));
        assert_eq!(cli.api_url, None);

        let cli = parse_cli(args(&["--api-url=http://127.0.0.1:9000", "--ack-emote", "happy"]))
            .unwrap()
            .unwrap();
        assert_eq!(cli.api_url.as_deref(), Some("http://127.0.0.1:9000"));
        assert_eq!(cli.ack_emote.as_deref(), Some("happy"));
    }

    #[test]
    fn cli_help_and_unknown_flags() {
        assert!(parse_cli(args(&["--help"])).unwrap().is_none(), "--help stops startup");
        let err = parse_cli(args(&["--nope"])).unwrap_err();
        assert!(err.contains("unknown argument"), "{err}");
        assert!(parse_cli(args(&["--context-interval", "0"])).is_err());
        assert!(parse_cli(args(&["--context-interval"])).is_err(), "missing value");
    }

    #[test]
    fn no_ack_emote_disables_the_ack() {
        let cli = parse_cli(args(&["--no-ack-emote"])).unwrap().unwrap();
        assert_eq!(cli.ack_emote.as_deref(), Some(""));
        let config = Config::resolve(cli, Rules::default());
        assert!(config.ack_emote.is_empty());
    }

    #[test]
    fn config_precedence_is_cli_then_rules_then_default() {
        let file_rules = rules(r#"{"ack_emote":"surprised","hermes_cmd":"from-rules","webhook_url":"http://127.0.0.1:9/hook"}"#);

        // rules beat the built-in default
        let config = Config::resolve(CliConfig::default(), file_rules.clone());
        assert_eq!(config.ack_emote, "surprised");
        assert_eq!(config.hermes_cmd.as_deref(), Some("from-rules"));
        assert_eq!(config.webhook_url.as_deref(), Some("http://127.0.0.1:9/hook"));
        assert_eq!(config.api_url, DEFAULT_API_URL);
        assert_eq!(config.context_interval, Duration::from_secs(DEFAULT_CONTEXT_INTERVAL_SECS));

        // CLI beats the rules file
        let cli = CliConfig {
            ack_emote: Some("angry".to_string()),
            hermes_cmd: Some("from-cli".to_string()),
            api_url: Some("http://127.0.0.1:9999/".to_string()),
            context_interval: Some(3),
            ..CliConfig::default()
        };
        let config = Config::resolve(cli, file_rules);
        assert_eq!(config.ack_emote, "angry");
        assert_eq!(config.hermes_cmd.as_deref(), Some("from-cli"));
        assert_eq!(config.api_url, "http://127.0.0.1:9999", "trailing slash trimmed");
        assert_eq!(config.context_interval, Duration::from_secs(3));

        // nothing configured: the documented default
        let config = Config::resolve(CliConfig::default(), Rules::default());
        assert_eq!(config.ack_emote, DEFAULT_ACK_EMOTE);
        assert!(config.hermes_cmd.is_none());
        assert!(config.webhook_url.is_none());
        assert!(!config.dry_run);
    }

    #[test]
    fn trigger_single_click_flag_enables_the_click_rule() {
        let cli = CliConfig {
            trigger_single_click: true,
            ..CliConfig::default()
        };
        let config = Config::resolve(cli, Rules::default());
        assert!(config.rules.interaction_allowed("click"));
    }

    /* ---------- headless dispatch ---------- */

    #[test]
    fn prompt_ends_with_the_headless_directive() {
        let trigger = Trigger {
            event_type: "interaction".to_string(),
            detail: "the user double-clicked the avatar at (10, 20)".to_string(),
            window_title: "Editor".to_string(),
            idle_secs: 4,
            kind: Some("double_click".to_string()),
            x: Some(10.0),
            y: Some(20.0),
            ..Trigger::default()
        };
        let prompt = trigger.prompt();
        assert!(
            prompt.ends_with(HEADLESS_DIRECTIVE),
            "the directive is the last thing the agent reads: {prompt}"
        );
        assert!(prompt.contains("do not ask follow-up questions"), "{prompt}");
        // The signatures are what saves the model a `tool_describe` round-trip.
        for signature in [
            "mcp__char_rig__avatar_speak(text, expression?, duration_ms?)",
            "mcp__char_rig__avatar_emote(expression, duration_ms?)",
            "mcp__char_rig__avatar_wave()",
            "mcp__char_rig__avatar_animate(name, loop?)",
            "neutral|happy|sad|surprised|thinking|angry",
        ] {
            assert!(prompt.contains(signature), "missing {signature:?} in {prompt}");
        }
    }

    #[test]
    fn hermes_timeout_defaults_to_120s_and_is_configurable() {
        let config = Config::resolve(CliConfig::default(), Rules::default());
        assert_eq!(config.hermes_timeout, Duration::from_secs(120));
        assert_eq!(DEFAULT_HERMES_TIMEOUT_SECS, 120);

        // rules file beats the built-in default
        let from_rules = rules(r#"{"hermes_timeout_secs":300}"#);
        assert_eq!(from_rules.hermes_timeout_secs, Some(300));
        let config = Config::resolve(CliConfig::default(), from_rules.clone());
        assert_eq!(config.hermes_timeout, Duration::from_secs(300));

        // CLI beats the rules file
        let cli = parse_cli(args(&["--hermes-timeout", "5"])).unwrap().unwrap();
        assert_eq!(cli.hermes_timeout, Some(5));
        let config = Config::resolve(cli, from_rules);
        assert_eq!(config.hermes_timeout, Duration::from_secs(5), "--hermes-timeout wins");

        // `--flag=value` form too
        let cli = parse_cli(args(&["--hermes-timeout=90"])).unwrap().unwrap();
        assert_eq!(cli.hermes_timeout, Some(90));

        assert!(parse_cli(args(&["--hermes-timeout", "0"])).is_err(), "zero is not a timeout");
        assert!(parse_cli(args(&["--hermes-timeout", "soon"])).is_err());
        assert!(parse_cli(args(&["--hermes-timeout"])).is_err(), "missing value");
    }

    #[test]
    fn hermes_timeout_accepts_the_flags_the_docs_show() {
        // `--help` must advertise the real default, not a hand-maintained copy.
        let usage = usage();
        assert!(
            usage.contains(&format!("(default {DEFAULT_HERMES_TIMEOUT_SECS})")),
            "usage does not mention the 120 s default: {usage}"
        );
    }

    #[test]
    fn single_flight_admits_one_dispatch_at_a_time() {
        let flight = SingleFlight::default();

        let first = flight.try_enter().expect("first dispatch gets the permit");
        assert!(
            flight.try_enter().is_none(),
            "a second trigger must not spawn a second hermes process"
        );

        drop(first);
        assert!(flight.try_enter().is_some(), "the next trigger is admitted again");
    }

    #[test]
    fn single_flight_permits_are_shared_across_clones() {
        let flight = SingleFlight::default();
        let clone = flight.clone();
        let guard = flight.try_enter().expect("permit");
        assert!(clone.try_enter().is_none(), "clones share the same latch");
        drop(guard);
        assert!(clone.try_enter().is_some());
    }

    /* ---------- per-app rules & alerts ---------- */

    #[test]
    fn rules_parse_app_rules_and_alert_settings() {
        let parsed = rules(
            r#"{
                "app_rules": [
                    {
                        "process": "Code.exe",
                        "title_contains": "error",
                        "animation": "alert",
                        "prompt_hint": "This is a compile error in the user's editor."
                    }
                ],
                "alerts": { "cooldown_secs": 30 }
            }"#,
        );
        assert_eq!(parsed.app_rules.len(), 1);
        let rule = &parsed.app_rules[0];
        assert_eq!(rule.process, "Code.exe");
        assert_eq!(rule.title_contains, "error");
        assert_eq!(rule.animation.as_deref(), Some("alert"));
        assert!(rule.prompt_hint.as_deref().unwrap().contains("compile error"));
        assert_eq!(parsed.alerts.cooldown_secs, 30);
        assert!(parsed.alerts.enabled, "a partial alerts block keeps the switch on");
    }

    #[test]
    fn the_old_rules_schema_still_parses() {
        // The shipped pre-app_rules file: every new key must be optional.
        let legacy = rules(
            r#"{
                "interactions": { "double_click": true, "drag_end": true, "click": false, "cooldown_secs": 15 },
                "context": { "window_keywords": ["error"], "idle_threshold_secs": 900, "cooldown_secs": 300 },
                "ack_emote": "thinking",
                "hermes_cmd": "hermes chat -q",
                "hermes_timeout_secs": 120,
                "webhook_url": null
            }"#,
        );
        assert!(legacy.app_rules.is_empty(), "no app rules configured");
        assert!(legacy.alerts.enabled, "alerts are on by default");
        assert_eq!(legacy.alerts.cooldown_secs, DEFAULT_ALERT_COOLDOWN_SECS);
        assert_eq!(legacy.alerts.cooldown_secs, 60);
    }

    #[test]
    fn app_rule_matching_is_case_insensitive_with_blank_wildcards() {
        let any_code = AppRule {
            process: "code.EXE".to_string(),
            ..AppRule::default()
        };
        assert!(any_code.matches("Code.exe", "anything at all"));
        assert!(!any_code.matches("explorer.exe", "anything at all"));
        assert!(
            !any_code.matches("", "anything at all"),
            "empty process_name never matches a process rule"
        );

        let any_error = AppRule {
            title_contains: "ERROR".to_string(),
            ..AppRule::default()
        };
        assert!(any_error.matches("chrome.exe", "a TypeError occurred"));
        assert!(!any_error.matches("chrome.exe", "all good"));

        // An all-blank rule matches everything: useful as a catch-all.
        assert!(AppRule::default().matches("", ""));
    }

    #[test]
    fn app_rule_beats_the_keyword_and_carries_animation_and_hint() {
        let base = Instant::now();
        let rules = rules(
            r#"{
                "context": { "window_keywords": ["error"] },
                "app_rules": [{
                    "process": "Code.exe",
                    "title_contains": "error",
                    "animation": "alert",
                    "prompt_hint": "This is a compile error in the user's editor."
                }]
            }"#,
        );
        let mut shared = shared();
        context_triggers(&rules, &mut shared, &snap("Editor", 1), base);

        let triggers = context_triggers(
            &rules,
            &mut shared,
            &snap_app("main.rs - error", "Code.exe", 2),
            base + Duration::from_secs(20),
        );
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0].event_type, "window_title");
        assert_eq!(triggers[0].animation.as_deref(), Some("alert"));
        assert_eq!(
            triggers[0].hint.as_deref(),
            Some("This is a compile error in the user's editor.")
        );
        assert_eq!(triggers[0].keyword.as_deref(), Some("error"));
        assert!(triggers[0].detail.contains("Code.exe"), "{}", triggers[0].detail);
        assert!(triggers[0].detail.contains("error"), "{}", triggers[0].detail);
    }

    #[test]
    fn an_app_rule_fires_without_any_keyword_match() {
        let base = Instant::now();
        let rules = rules(
            r#"{"app_rules":[{"process":"Code.exe","animation":"thinking","prompt_hint":"They are coding."}]}"#,
        );
        let mut shared = shared();
        context_triggers(&rules, &mut shared, &snap("Desktop", 1), base);

        let triggers = context_triggers(
            &rules,
            &mut shared,
            &snap_app("main.rs - Visual Studio Code", "Code.exe", 1),
            base + Duration::from_secs(20),
        );
        assert_eq!(triggers.len(), 1, "the process alone is enough");
        assert_eq!(triggers[0].keyword, None);
        assert_eq!(triggers[0].hint.as_deref(), Some("They are coding."));

        // A different process with a quiet title stays quiet.
        let quiet = context_triggers(
            &rules,
            &mut shared,
            &snap_app("Inbox - Outlook", "OUTLOOK.EXE", 1),
            base + Duration::from_secs(400),
        );
        assert!(quiet.is_empty());
    }

    #[test]
    fn user_reply_trigger_bypasses_cooldowns_and_quotes_the_text() {
        let base = Instant::now();
        let mut shared = shared();
        shared.context = ContextSnapshot {
            window_title: "Editor".to_string(),
            process_name: "Code.exe".to_string(),
            idle_secs: 7,
        };
        let payload = json!({ "text": "  what is on my clipboard?  " });

        let trigger = user_reply_trigger(&shared, &payload, base).expect("a reply always fires");
        assert_eq!(trigger.event_type, "user_reply");
        assert_eq!(trigger.text.as_deref(), Some("what is on my clipboard?"));
        assert!(trigger.detail.contains("what is on my clipboard?"));
        assert_eq!(trigger.window_title, "Editor");
        assert_eq!(trigger.idle_secs, 7);

        let prompt = trigger.prompt();
        assert!(prompt.starts_with("[char-rig user_reply]"), "{prompt}");
        assert!(
            prompt.contains("typed this directly in the desktop overlay")
                && prompt.contains("\"what is on my clipboard?\""),
            "{prompt}"
        );
        assert!(prompt.ends_with(HEADLESS_DIRECTIVE), "{prompt}");

        // No cooldown: the second reply one second later still fires.
        assert!(
            user_reply_trigger(&shared, &payload, base + Duration::from_secs(1)).is_some(),
            "direct user replies are never rate-limited"
        );
        // Blank text is not a trigger.
        assert!(user_reply_trigger(&shared, &json!({ "text": "   " }), base).is_none());
        assert!(user_reply_trigger(&shared, &json!({}), base).is_none());
    }

    #[test]
    fn alert_frames_are_rate_limited_and_configurable() {
        let base = Instant::now();
        let settings = rules(r#"{"alerts":{"cooldown_secs":60}}"#);
        let mut current = shared();
        let payload = json!({
            "source": "suricata",
            "summary": "ET SCAN port sweep from 192.168.0.9",
            "severity": "warn"
        });

        let trigger = alert_trigger(&settings, &mut current, &payload, base).expect("alert fires");
        assert_eq!(trigger.event_type, "alert");
        assert_eq!(trigger.severity.as_deref(), Some("warn"));
        assert!(trigger.detail.contains("suricata"));
        assert!(trigger.detail.contains("port sweep"));
        let prompt = trigger.prompt();
        assert!(prompt.starts_with("[char-rig alert]"), "{prompt}");
        assert!(prompt.contains("Context: This came from suricata"), "{prompt}");

        assert!(
            alert_trigger(&settings, &mut current, &payload, base + Duration::from_secs(30)).is_none(),
            "inside the 60s cooldown"
        );
        assert!(
            alert_trigger(&settings, &mut current, &payload, base + Duration::from_secs(61)).is_some(),
            "after the cooldown"
        );

        // Switched off: the frame is ignored entirely.
        let disabled = rules(r#"{"alerts":{"enabled":false}}"#);
        let mut muted = shared();
        assert!(alert_trigger(&disabled, &mut muted, &payload, base).is_none());

        // Minimal payload: severity defaults to info, a blank source is named.
        let mut fresh = shared();
        let minimal = alert_trigger(&settings, &mut fresh, &json!({ "summary": "disk full" }), base)
            .expect("summary is the only requirement");
        assert_eq!(minimal.severity.as_deref(), Some("info"));
        assert!(minimal.detail.contains("an external monitor"));
        let mut blank = shared();
        assert!(alert_trigger(&settings, &mut blank, &json!({ "summary": "  " }), base).is_none());
    }

    #[test]
    fn ack_plan_prefers_the_per_app_animation() {
        let config = Config::resolve(CliConfig::default(), rules(r#"{"ack_emote":"surprised"}"#));

        let mut trigger = Trigger {
            animation: Some("alert".to_string()),
            ..Trigger::default()
        };
        assert_eq!(
            ack_plan(&trigger, &config),
            Some(Ack::Animate("alert".to_string())),
            "a non-expression name is a keyframe clip"
        );

        trigger.animation = Some("happy".to_string());
        assert_eq!(ack_plan(&trigger, &config), Some(Ack::Emote("happy".to_string())));

        trigger.animation = Some("   ".to_string());
        assert_eq!(
            ack_plan(&trigger, &config),
            Some(Ack::Emote("surprised".to_string())),
            "a blank animation falls back to the generic ack"
        );

        trigger.animation = None;
        assert_eq!(ack_plan(&trigger, &config), Some(Ack::Emote("surprised".to_string())));

        let quiet = Config::resolve(
            CliConfig {
                ack_emote: Some(String::new()),
                ..CliConfig::default()
            },
            Rules::default(),
        );
        assert_eq!(ack_plan(&trigger, &quiet), None, "--no-ack-emote wins");
    }

    #[test]
    fn payload_exposes_the_rule_fields_and_omits_empty_ones() {
        let trigger = Trigger {
            event_type: "window_title".to_string(),
            detail: "the focused window changed to \"main.rs - error\"".to_string(),
            window_title: "main.rs - error".to_string(),
            idle_secs: 4,
            keyword: Some("error".to_string()),
            animation: Some("alert".to_string()),
            hint: Some("This is a compile error in the user's editor.".to_string()),
            ..Trigger::default()
        };
        let payload = trigger.payload(1_700_000_000_000);
        assert_eq!(payload["keyword"], "error");
        assert_eq!(payload["animation"], "alert");
        assert_eq!(payload["hint"], "This is a compile error in the user's editor.");
        assert!(payload.get("text").is_none(), "no reply text on a title trigger");
        assert!(payload.get("severity").is_none());
        assert!(trigger.prompt().contains("Context: This is a compile error"));
    }

    #[tokio::test]
    async fn handle_frame_turns_reply_and_alert_frames_into_triggers() {
        let rules = Rules::default();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let session = Arc::new(Mutex::new(SessionState::default()));
        let (tx, mut rx) = mpsc::channel::<Trigger>(TRIGGER_QUEUE);

        let reply = SseFrame {
            event: "user_reply".to_string(),
            data: json!({
                "seq": 1,
                "type": "user_reply",
                "payload": { "text": "hello there" }
            })
            .to_string(),
        };
        handle_frame(&reply, &rules, &shared, &session, &tx).await;
        let trigger = rx.try_recv().expect("a user_reply frame is a trigger");
        assert_eq!(trigger.event_type, "user_reply");
        assert_eq!(trigger.text.as_deref(), Some("hello there"));

        let alert = SseFrame {
            event: "alert".to_string(),
            data: json!({
                "seq": 2,
                "type": "alert",
                "payload": { "source": "cron", "summary": "backup failed", "severity": "error" }
            })
            .to_string(),
        };
        handle_frame(&alert, &rules, &shared, &session, &tx).await;
        let trigger = rx.try_recv().expect("an alert frame is a trigger");
        assert_eq!(trigger.event_type, "alert");
        assert_eq!(trigger.severity.as_deref(), Some("error"));
        assert!(trigger.detail.contains("backup failed"));

        // Everything else stays quiet, as does a malformed frame.
        for data in [
            json!({ "type": "speech", "payload": { "text": "hi" } }),
            json!({ "type": "interaction", "payload": { "x": 1.0, "y": 2.0 } }),
        ] {
            let frame = SseFrame { event: "message".to_string(), data: data.to_string() };
            handle_frame(&frame, &rules, &shared, &session, &tx).await;
        }
        let junk = SseFrame { event: "message".to_string(), data: "not json".to_string() };
        handle_frame(&junk, &rules, &shared, &session, &tx).await;
        assert!(rx.try_recv().is_err(), "nothing else was queued");
    }

    #[test]
    fn headless_directive_documents_the_new_tools_and_brevity() {
        let prompt = Trigger::default().prompt();
        for needle in [
            "mcp__char_rig__avatar_get_clipboard()",
            "mcp__char_rig__avatar_get_context()",
            "process_name",
            "Chain several calls in one turn",
            "under 180 characters",
        ] {
            assert!(prompt.contains(needle), "missing {needle:?} in {prompt}");
        }
    }

    /* ---------- sessions ---------- */

    #[test]
    fn session_ids_are_unique_per_rotation() {
        let mut state = SessionState { seed: 1_700_000_000, ..SessionState::default() };
        assert_eq!(state.id_for("char-rig"), "char-rig-1700000000");

        let (old, new) = state.rotate("char-rig");
        assert_eq!(old, "<none>", "the very first id has no predecessor");
        assert_eq!(new, "char-rig-1700000000");

        let (old, new) = state.rotate("char-rig");
        assert_eq!(old, "char-rig-1700000000");
        assert_eq!(new, "char-rig-1700000000-2");

        state.turns = 5;
        state.rotate("char-rig");
        assert_eq!(state.id, "char-rig-1700000000-3");
        assert_eq!(state.turns, 0, "a rotation resets the turn counter");
        assert_eq!(state.last_turn, None);
    }

    #[test]
    fn session_base_is_configurable() {
        let custom = rules(r#"{"session_base":"desk-buddy"}"#);
        assert_eq!(session_base(&custom), "desk-buddy");
        assert_eq!(session_base(&Rules::default()), DEFAULT_SESSION_BASE);
        assert_eq!(session_base(&rules(r#"{"session_base":"   "}"#)), "char-rig");
    }

    #[test]
    fn session_rules_default_to_off() {
        let default = Rules::default();
        assert_eq!(default.max_session_turns, None);
        assert_eq!(default.session_idle_reset_secs, None);
        assert!(default.quick_tasks.is_empty());
    }

    #[test]
    fn session_for_dispatch_initialises_once_and_keeps_the_id() {
        let settings = Rules::default();
        let mut state = SessionState { seed: 42, ..SessionState::default() };

        let first = session_for_dispatch(&settings, &mut state, Instant::now());
        assert_eq!(first, "char-rig-42");
        assert_eq!(state.turns, 0, "no turn dispatched yet");

        state.turns = 1;
        assert_eq!(
            session_for_dispatch(&settings, &mut state, Instant::now()),
            first,
            "no rule configured, so the conversation is kept"
        );
    }

    #[test]
    fn session_rotates_after_max_turns() {
        let settings = rules(r#"{"max_session_turns":2}"#);
        let now = Instant::now();

        let mut under = SessionState {
            seed: 42,
            id: "char-rig-42".to_string(),
            turns: 1,
            last_turn: Some(now),
            ..SessionState::default()
        };
        assert_eq!(session_for_dispatch(&settings, &mut under, now), "char-rig-42");

        let mut at = SessionState {
            seed: 42,
            id: "char-rig-42".to_string(),
            turns: 2,
            last_turn: Some(now),
            ..SessionState::default()
        };
        assert_eq!(
            session_for_dispatch(&settings, &mut at, now),
            "char-rig-42-2",
            "the turn that trips the limit lands in the new session"
        );
        assert_eq!(at.turns, 0);
    }

    #[test]
    fn session_rotates_after_an_idle_break() {
        let settings = rules(r#"{"session_idle_reset_secs":600}"#);
        let now = Instant::now();

        let mut away = SessionState {
            seed: 42,
            id: "char-rig-42".to_string(),
            turns: 3,
            last_turn: now.checked_sub(Duration::from_secs(900)),
            ..SessionState::default()
        };
        assert_eq!(session_for_dispatch(&settings, &mut away, now), "char-rig-42-2");

        let mut active = SessionState {
            seed: 42,
            id: "char-rig-42".to_string(),
            turns: 3,
            last_turn: now.checked_sub(Duration::from_secs(60)),
            ..SessionState::default()
        };
        assert_eq!(
            session_for_dispatch(&settings, &mut active, now),
            "char-rig-42",
            "a short pause keeps the conversation"
        );
    }

    #[test]
    fn command_argv_substitutes_the_session_placeholder() {
        let argv = command_argv(
            "hermes chat --continue {session} --create-if-missing -Q -q",
            "hello",
            "char-rig-7",
        );
        assert_eq!(argv[3], "char-rig-7", "the template is expanded: {argv:?}");
        assert_eq!(argv.last().map(String::as_str), Some("hello"), "prompt is still last");
        assert!(
            !argv.iter().any(|arg| arg.contains("{session}")),
            "no placeholder is left behind: {argv:?}"
        );

        // A template without {session} is used verbatim (rotation is inert).
        let plain = command_argv("hermes chat -q", "hi", "char-rig-7");
        assert_eq!(plain, vec!["hermes", "chat", "-q", "hi"]);
    }

    #[tokio::test]
    async fn session_reset_frame_rotates_without_queueing_a_trigger() {
        let rules = Rules::default();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let session = Arc::new(Mutex::new(SessionState {
            seed: 42,
            id: "char-rig-42".to_string(),
            turns: 4,
            ..SessionState::default()
        }));
        let (tx, mut rx) = mpsc::channel::<Trigger>(TRIGGER_QUEUE);

        let frame = SseFrame {
            event: "session_reset".to_string(),
            data: json!({
                "seq": 9,
                "type": "session_reset",
                "payload": { "reason": "ui" }
            })
            .to_string(),
        };
        handle_frame(&frame, &rules, &shared, &session, &tx).await;

        let state = lock_session(&session);
        assert_eq!(state.id, "char-rig-42-2", "the reset rotated the session");
        assert_eq!(state.turns, 0, "the new session starts at zero turns");
        drop(state);
        assert!(rx.try_recv().is_err(), "a reset is an instruction, not a trigger");
    }

    #[test]
    fn rules_parse_quick_tasks() {
        let parsed = rules(
            r#"{"quick_tasks":[{"label":"Summarize clipboard","prompt":"call avatar_get_clipboard"},{"label":"Scan","prompt":"run it"}]}"#,
        );
        assert_eq!(parsed.quick_tasks.len(), 2);
        assert_eq!(parsed.quick_tasks[0].label, "Summarize clipboard");
        assert_eq!(parsed.quick_tasks[1].prompt, "run it");
    }

    #[test]
    fn user_reply_prompt_marks_a_direct_task() {
        let trigger = Trigger {
            event_type: "user_reply".to_string(),
            detail: "the user replied in the speech bubble: \"summarise my clipboard\"".to_string(),
            text: Some("summarise my clipboard".to_string()),
            ..Trigger::default()
        };
        let prompt = trigger.prompt();
        assert!(prompt.contains("typed this directly in the desktop overlay"), "{prompt}");
        assert!(prompt.contains("Treat it as their task"), "{prompt}");
        assert!(prompt.contains("report the outcome with avatar_speak"), "{prompt}");
        assert!(prompt.ends_with(HEADLESS_DIRECTIVE), "{prompt}");
    }
}
