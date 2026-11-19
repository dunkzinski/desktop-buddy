//! Local model discovery for the Settings modal's **Discover models** button.
//!
//! The app never starts a model server, so the only honest way to fill the AI
//! brain tab is to ask: probe the configured hosts and ports for an
//! OpenAI-shaped `/v1/models`, llama.cpp's root-mounted `/models`, or Ollama's
//! `/api/tags`, and list the `.gguf` weights sitting in the local model folder
//! (unloaded weights are still worth showing).
//!
//! `discovery_hosts` is what keeps this from being loopback-only: a LAN box, a
//! Tailscale peer or a second local interface is just another `(host, port)`
//! pair. The cross product of hosts and ports is probed **in parallel**, so ten
//! extra targets cost one budget rather than ten.
//!
//! Everything here is **blocking** with a short per-request timeout and runs on
//! `spawn_blocking` from the route, so an offline port costs at most
//! [`DISCOVERY_TIMEOUT_MS`] and never touches the async workers — the modal
//! shows a spinner, not a frozen window.
//!
//! Nothing is written: discovery only *reports*. Importing a model is the UI's
//! job (it fills a draft profile card), and persisting that is `POST /config`.

use std::net::{Ipv6Addr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Per-request budget. A closed port fails instantly (connection refused); a
/// port that is open but not an HTTP server has to time out, and 1.5 s is the
/// point where six ports still feel like a button press rather than a hang.
pub const DISCOVERY_TIMEOUT_MS: u64 = 1500;

/// How deep the `.gguf` scan walks (`models/<vendor>/<repo>/*.gguf` is 2).
pub const MAX_GGUF_DEPTH: usize = 3;

/// The port a URL names explicitly, if it names one (`…:8001/v1` → 8001).
///
/// Pure, so the "a profile's endpoint already implies a discovery port" rule is
/// testable without a socket.
pub fn port_in_url(url: &str) -> Option<u16> {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let (_, port) = authority.rsplit_once(':')?;
    port.parse::<u16>().ok().filter(|port| *port > 0)
}

/// One `(host, port)` pair to probe. A `discovery_hosts` entry that names its
/// own port (`192.168.0.36:11434`) becomes exactly one target; a bare host is
/// crossed with every entry in `discovery_ports`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Target {
    /// Host as typed, without brackets for IPv6 (`::1`, not `[::1]`).
    pub host: String,
    pub port: u16,
}

impl Target {
    /// The `host:port` the chips show; loopback keeps the short `:port` form
    /// the modal always used, everything else names its host.
    pub fn label(&self) -> String {
        if self.is_loopback() {
            format!(":{}", self.port)
        } else {
            format!("{}:{}", self.authority(), self.port)
        }
    }

    /// Is this the machine the app runs on? (`0.0.0.0` is how some servers
    /// print their bind address, so it counts too.)
    pub fn is_loopback(&self) -> bool {
        let host = self.host.trim().to_ascii_lowercase();
        let host = host.trim_start_matches('[').trim_end_matches(']');
        matches!(host, "127.0.0.1" | "localhost" | "::1" | "0.0.0.0" | "::")
            || host.starts_with("127.")
    }

    /// The host as it belongs inside a URL: IPv6 literals need brackets.
    pub fn authority(&self) -> String {
        let host = self.host.trim();
        if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_string()
        }
    }

    fn socket_host(&self) -> String {
        self.host
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string()
    }
}

/// Split `host:port` / `[::1]:8001` into a bare host and an optional port.
///
/// Pure, and deliberately conservative: a bare IPv6 address (`::1`) has many
/// colons and no port, so only a single trailing `:digits` (or the bracketed
/// form) is read as a port.
pub fn split_host_port(entry: &str) -> (String, Option<u16>) {
    let text = entry.trim();
    if let Some(rest) = text.strip_prefix('[') {
        if let Some((host, tail)) = rest.split_once(']') {
            if host.is_empty() {
                return (text.to_string(), None);
            }
            let port = tail
                .strip_prefix(':')
                .and_then(|port| port.parse::<u16>().ok())
                .filter(|port| *port > 0);
            return (host.to_string(), port);
        }
        return (text.to_string(), None);
    }
    if text.matches(':').count() == 1 {
        if let Some((host, port)) = text.rsplit_once(':') {
            if let Ok(port) = port.parse::<u16>() {
                if port > 0 && !host.is_empty() {
                    return (host.to_string(), Some(port));
                }
            }
        }
    }
    (text.to_string(), None)
}

/// Clean one `discovery_hosts` entry: trim, drop an accidental `http(s)://`
/// prefix, drop any path/query, and drop the entry entirely when what is left
/// cannot be a host.
///
/// `Some("192.168.0.36:11434")` for `" https://192.168.0.36:11434/v1/ "`.
pub fn normalize_host(raw: &str) -> Option<String> {
    let mut text = raw.trim();
    let lower = text.to_ascii_lowercase();
    for scheme in ["http://", "https://"] {
        if lower.starts_with(scheme) {
            text = &text[scheme.len()..];
            break;
        }
    }
    // A path, query or fragment after the authority is never part of a host.
    if let Some(cut) = text.find(['/', '?', '#']) {
        text = &text[..cut];
    }
    let text = text.trim();
    if text.is_empty() || text.contains(char::is_whitespace) || text.contains('@') || text.contains('\\') {
        return None;
    }

    let (host, port) = split_host_port(text);
    if host.is_empty() {
        return None;
    }
    if host.contains(':') {
        // Anything left with a colon must be a real IPv6 literal, otherwise the
        // single-colon port split above would have caught it.
        if host.parse::<Ipv6Addr>().is_err() {
            return None;
        }
    } else {
        let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_');
        if !host.chars().all(allowed) || host.starts_with(['.', '-']) || host.ends_with(['.', '-']) {
            return None;
        }
    }

    Some(match port {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

/// Cross the configured hosts with the configured ports (a `host:port` entry is
/// probed at its own port only), preserving order and dropping duplicates.
pub fn targets(hosts: &[String], ports: &[u16]) -> Vec<Target> {
    let mut out: Vec<Target> = Vec::new();
    let mut push = |target: Target| {
        if !out.contains(&target) {
            out.push(target);
        }
    };
    for raw in hosts {
        let Some(host) = normalize_host(raw) else { continue };
        let (host, explicit) = split_host_port(&host);
        match explicit {
            Some(port) => push(Target { host, port }),
            None => {
                for port in ports {
                    push(Target {
                        host: host.clone(),
                        port: *port,
                    });
                }
            }
        }
    }
    out
}

/// `[{id, owned_by, multimodal}]` out of an OpenAI-shaped model list.
///
/// Accepts `{"data": […]}` (OpenAI, llama.cpp, vLLM) and `{"models": […]}`
/// (older llama.cpp), and reads llama.cpp's `architecture.input_modalities`
/// when it is there so a vision model can be labelled as one.
fn openai_rows(body: &Value) -> Vec<(String, String, bool)> {
    let list = body
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| body.get("models").and_then(Value::as_array));
    let Some(list) = list else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|entry| {
            let id = entry
                .get("id")
                .and_then(Value::as_str)
                .or_else(|| entry.get("name").and_then(Value::as_str))
                .or_else(|| entry.get("model").and_then(Value::as_str))?
                .trim()
                .to_string();
            if id.is_empty() {
                return None;
            }
            let owned_by = entry
                .get("owned_by")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let multimodal = entry
                .pointer("/architecture/input_modalities")
                .and_then(Value::as_array)
                .map(|mods| mods.iter().any(|modality| modality.as_str() == Some("image")))
                .unwrap_or(false);
            Some((id, owned_by, multimodal))
        })
        .collect()
}

/// `[{name}]` out of Ollama's `/api/tags`, plus a vision hint from `families`.
fn ollama_rows(body: &Value) -> Vec<(String, String, bool)> {
    let Some(list) = body.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|entry| {
            let id = entry
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| entry.get("model").and_then(Value::as_str))?
                .trim()
                .to_string();
            if id.is_empty() {
                return None;
            }
            let size = entry
                .pointer("/details/parameter_size")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let multimodal = entry
                .pointer("/details/families")
                .and_then(Value::as_array)
                .map(|families| families.iter().any(|family| family.as_str() == Some("clip")))
                .unwrap_or(false);
            Some((id, size, multimodal))
        })
        .collect()
}

/// What is left of a port's budget (`DISCOVERY_TIMEOUT_MS` in total, connect
/// included), so three doomed requests cannot add up to 4.5 s in front of the
/// user.
fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

fn get_json(timeout: Duration, url: &str) -> Option<Value> {
    if timeout.is_zero() {
        return None;
    }
    // A client per request: the timeout is the *remaining* budget, which a
    // shared client cannot express.
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .no_proxy()
        .build()
        .ok()?;
    let response = client.get(url).send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json::<Value>().ok()
}

struct PortProbe {
    reachable: bool,
    detail: String,
    models: Vec<Value>,
}

impl PortProbe {
    fn silent() -> Self {
        Self {
            reachable: false,
            detail: "nothing answered (no /v1/models, /models or /api/tags)".to_string(),
            models: Vec::new(),
        }
    }
}

/// Ask one target what it serves, inside a single `DISCOVERY_TIMEOUT_MS` budget.
///
/// The TCP connect comes first and is the only thing a closed port pays for: on
/// this machine a connection to a free loopback port *times out* instead of
/// being refused, so without it every dead port would cost three request
/// timeouts. Then OpenAI-compatible (`v1/models`), llama.cpp's root mount
/// (`models`) and Ollama (`api/tags`).
fn probe_target(target: &Target) -> PortProbe {
    let deadline = Instant::now() + Duration::from_millis(DISCOVERY_TIMEOUT_MS);
    let Ok(mut addresses) = (target.socket_host().as_str(), target.port).to_socket_addrs() else {
        return PortProbe::silent();
    };
    let Some(address) = addresses.next() else {
        return PortProbe::silent();
    };
    if std::net::TcpStream::connect_timeout(&address, remaining(deadline)).is_err() {
        return PortProbe::silent();
    }

    let base = format!("http://{}:{}", target.authority(), target.port);
    let mut answered = false;
    let mut detail = String::new();

    // Both map to the `/v1` endpoint, because that is the prefix
    // `chat/completions` hangs off.
    for path in ["/v1/models", "/models"] {
        if let Some(body) = get_json(remaining(deadline), &format!("{base}{path}")) {
            answered = true;
            let rows = openai_rows(&body);
            if !rows.is_empty() {
                let endpoint = format!("{base}/v1");
                let models = rows
                    .into_iter()
                    .map(|(id, owned_by, multimodal)| {
                        json!({
                            "id": id,
                            "label": id,
                            "endpoint": endpoint,
                            "host": target.host,
                            "port": target.port,
                            "loopback": target.is_loopback(),
                            "source": "openai_compat",
                            "owned_by": owned_by,
                            "multimodal": multimodal,
                        })
                    })
                    .collect();
                return PortProbe {
                    reachable: true,
                    detail: format!("{path} listed model(s)"),
                    models,
                };
            }
            detail = format!("{path} answered but listed no models");
        }
    }

    // Ollama speaks its own dialect on /api/tags and OpenAI's on /v1.
    if let Some(body) = get_json(remaining(deadline), &format!("{base}/api/tags")) {
        answered = true;
        let rows = ollama_rows(&body);
        if !rows.is_empty() {
            let endpoint = format!("{base}/v1");
            let models = rows
                .into_iter()
                .map(|(id, parameter_size, multimodal)| {
                    json!({
                        "id": id,
                        "label": id,
                        "endpoint": endpoint,
                        "host": target.host,
                        "port": target.port,
                        "loopback": target.is_loopback(),
                        "source": "ollama",
                        "owned_by": parameter_size,
                        "multimodal": multimodal,
                    })
                })
                .collect();
            return PortProbe {
                reachable: true,
                detail: "/api/tags listed model(s)".to_string(),
                models,
            };
        }
        detail = "/api/tags answered but listed no models".to_string();
    }

    if answered {
        return PortProbe {
            reachable: true,
            detail: if detail.is_empty() {
                "answered, but served no model list".to_string()
            } else {
                detail
            },
            models: Vec::new(),
        };
    }
    PortProbe::silent()
}

/// Directories that may hold `.gguf` weights.
fn gguf_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for (var, suffix) in [
        ("LOCALAPPDATA", ["hermes", "models"].as_slice()),
        ("USERPROFILE", ["AppData", "Local", "hermes", "models"].as_slice()),
        ("HOME", [".cache", "hermes", "models"].as_slice()),
    ] {
        if let Ok(base) = std::env::var(var) {
            if base.trim().is_empty() {
                continue;
            }
            let mut path = PathBuf::from(base);
            for part in suffix {
                path.push(part);
            }
            if path.is_dir() && !dirs.contains(&path) {
                dirs.push(path);
            }
        }
    }
    dirs
}

fn walk_gguf(dir: &Path, depth: usize, out: &mut Vec<Value>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            if depth < MAX_GGUF_DEPTH {
                walk_gguf(&path, depth + 1, out);
            }
            continue;
        }
        let is_gguf = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("gguf"))
            .unwrap_or(false);
        if !is_gguf {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("model")
            .to_string();
        out.push(json!({
            "id": stem,
            "label": stem,
            "endpoint": "",
            "port": Value::Null,
            "source": "gguf",
            "owned_by": "",
            "multimodal": false,
            "path": path.display().to_string(),
            "size_bytes": std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0),
        }));
    }
}

fn gguf_models() -> (Vec<PathBuf>, Vec<Value>) {
    let dirs = gguf_dirs();
    let mut models = Vec::new();
    for dir in &dirs {
        walk_gguf(dir, 0, &mut models);
    }
    models.sort_by(|a, b| {
        a["id"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .cmp(&b["id"].as_str().unwrap_or("").to_lowercase())
    });
    (dirs, models)
}

/// Probe every `(host, port)` target and scan the local model folder.
/// Blocking; call it from `spawn_blocking`.
///
/// Targets are probed **in parallel**: each has a hard `DISCOVERY_TIMEOUT_MS`
/// budget, so six dead ports (or six hosts) cost one budget rather than six,
/// and the button stays a button press instead of a freeze.
pub fn discover(targets: &[Target]) -> Value {
    let started = Instant::now();

    let probes: Vec<PortProbe> = std::thread::scope(|scope| {
        let handles: Vec<_> = targets
            .iter()
            .map(|target| scope.spawn(move || probe_target(target)))
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or_else(|_| PortProbe::silent()))
            .collect()
    });

    let mut probed = Vec::new();
    let mut models: Vec<Value> = Vec::new();
    let mut seen: std::collections::BTreeSet<(String, u16, String)> =
        std::collections::BTreeSet::new();

    for (target, probe) in targets.iter().zip(probes) {
        for model in probe.models {
            let id = model["id"].as_str().unwrap_or("").to_string();
            if seen.insert((target.host.clone(), target.port, id)) {
                models.push(model);
            }
        }
        probed.push(json!({
            "host": target.host,
            "port": target.port,
            "label": target.label(),
            "loopback": target.is_loopback(),
            "reachable": probe.reachable,
            "detail": probe.detail,
        }));
    }

    let (gguf_dirs, gguf) = gguf_models();
    let servers = probed.iter().filter(|entry| entry["reachable"] == json!(true)).count();
    let summary = format!(
        "{} server(s) answered on {} target(s) · {} served model(s) · {} local .gguf",
        servers,
        targets.len(),
        models.len(),
        gguf.len()
    );
    models.extend(gguf);

    json!({
        "ok": true,
        "summary": summary,
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "timeout_ms": DISCOVERY_TIMEOUT_MS,
        "targets": probed,
        // Kept under the historical name too: older callers read `ports`.
        "ports": probed,
        "models": models,
        "gguf_dirs": gguf_dirs.iter().map(|dir| dir.display().to_string()).collect::<Vec<String>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn port_in_url_reads_only_an_explicit_port() {
        assert_eq!(port_in_url("http://127.0.0.1:8001/v1"), Some(8001));
        assert_eq!(port_in_url("http://localhost:9931"), Some(9931));
        assert_eq!(port_in_url("https://user:pw@127.0.0.1:11434/v1"), Some(11434));
        assert_eq!(port_in_url("http://127.0.0.1/v1"), None);
        assert_eq!(port_in_url("http://127.0.0.1:0/v1"), None);
        assert_eq!(port_in_url("not a url"), None);
    }

    #[test]
    fn hosts_are_normalised_before_they_are_probed() {
        // Trimming, an accidental scheme and a trailing path/slash all go away.
        assert_eq!(normalize_host(" 127.0.0.1 "), Some("127.0.0.1".to_string()));
        assert_eq!(
            normalize_host("https://192.168.0.36:11434/v1/"),
            Some("192.168.0.36:11434".to_string())
        );
        assert_eq!(
            normalize_host("http://ollama.lan/"),
            Some("ollama.lan".to_string())
        );
        assert_eq!(normalize_host("host:99999"), None, "not a port");
        assert_eq!(normalize_host("two words"), None);
        assert_eq!(normalize_host(""), None);
        assert_eq!(normalize_host("http://"), None);
        // IPv6 keeps its brackets out of the stored value.
        assert_eq!(normalize_host("[::1]:8001"), Some("::1:8001".to_string()));
        assert_eq!(normalize_host("::1"), Some("::1".to_string()));
    }

    #[test]
    fn a_host_with_a_port_is_one_target_and_a_bare_host_gets_every_port() {
        let hosts = vec![
            "127.0.0.1".to_string(),
            "192.168.0.36:11434".to_string(),
            "http://192.168.0.36:11434/".to_string(), // duplicate after cleaning
            "tailscale-box".to_string(),
        ];
        let targets = targets(&hosts, &[8001, 11434]);
        assert_eq!(
            targets,
            vec![
                Target { host: "127.0.0.1".into(), port: 8001 },
                Target { host: "127.0.0.1".into(), port: 11434 },
                Target { host: "192.168.0.36".into(), port: 11434 },
                Target { host: "tailscale-box".into(), port: 8001 },
                Target { host: "tailscale-box".into(), port: 11434 },
            ]
        );
        assert_eq!(targets[0].label(), ":8001", "loopback keeps the short form");
        assert_eq!(targets[2].label(), "192.168.0.36:11434");
        assert!(!targets[2].is_loopback());
        assert!(targets[0].is_loopback());
    }

    #[test]
    fn openai_rows_read_data_models_and_multimodal() {
        let body = json!({
            "object": "list",
            "data": [
                {"id": "qwen3-coder-30b", "owned_by": "colibri"},
                {"id": "lfm3-vl-2b", "architecture": {"input_modalities": ["text", "image"]}},
                {"id": "   "}
            ]
        });
        let rows = openai_rows(&body);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "qwen3-coder-30b");
        assert_eq!(rows[0].1, "colibri");
        assert!(!rows[0].2);
        assert!(rows[1].2, "image in input_modalities means multimodal");

        // Older llama.cpp names the array `models`.
        let old = json!({ "models": [ { "name": "tiny" } ] });
        assert_eq!(openai_rows(&old), vec![("tiny".to_string(), String::new(), false)]);
    }

    #[test]
    fn ollama_rows_read_names_and_clip_families() {
        let body = json!({
            "models": [
                {"name": "llama3.2:3b", "details": {"parameter_size": "3.2B", "families": ["llama"]}},
                {"name": "llava:7b", "details": {"families": ["llama", "clip"]}}
            ]
        });
        let rows = ollama_rows(&body);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "llama3.2:3b");
        assert!(!rows[0].2);
        assert!(rows[1].2, "a clip family is a vision model");
    }

    /// One accept-per-request HTTP server: enough to prove the probe path and
    /// the endpoint it reports, without a real model server.
    fn serve(body: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buffer = [0u8; 2048];
                let _ = stream.read(&mut buffer);
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        port
    }

    #[test]
    fn a_probe_reports_the_v1_endpoint_and_every_model() {
        let port = serve(r#"{"object":"list","data":[{"id":"tiny"},{"id":"small"}]}"#);
        let probe = probe_target(&Target {
            host: "127.0.0.1".to_string(),
            port,
        });
        assert!(probe.reachable, "{}", probe.detail);
        assert_eq!(probe.models.len(), 2);
        assert_eq!(probe.models[0]["endpoint"], json!(format!("http://127.0.0.1:{port}/v1")));
        assert_eq!(probe.models[0]["port"], json!(port));
        assert_eq!(probe.models[0]["source"], json!("openai_compat"));
    }

    #[test]
    fn a_closed_port_is_reported_as_silent_not_as_an_error() {
        // Bind then drop, so the port is almost certainly free.
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("addr").port()
        };
        let started = Instant::now();
        let probe = probe_target(&Target {
            host: "127.0.0.1".to_string(),
            port,
        });
        assert!(!probe.reachable);
        assert!(probe.models.is_empty());
        // The whole port is bounded by one budget, connect included — three
        // doomed requests must not add up to 4.5 s.
        assert!(
            started.elapsed() < Duration::from_millis(DISCOVERY_TIMEOUT_MS + 800),
            "a silent port took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn discover_reports_ports_and_a_summary_even_with_nothing_listening() {
        let body = discover(&[]);
        assert_eq!(body["ok"], json!(true));
        assert!(body["summary"].as_str().unwrap_or("").contains("0 server(s)"));
        assert!(body["models"].is_array());
        assert_eq!(body["timeout_ms"], json!(DISCOVERY_TIMEOUT_MS));
    }
}
