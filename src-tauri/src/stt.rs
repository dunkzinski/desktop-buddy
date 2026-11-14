//! Push-to-talk voice input: microphone → 16 kHz mono PCM → Silero VAD + Whisper.
//!
//! Hold `Alt+V` (or the **Push to talk** button in the control panel), speak,
//! let go. The clip is captured at the device's rate, downmixed to mono and
//! resampled to 16 kHz, written to a temp `.wav`, handed to `whisper-cli.exe`
//! (with its built-in Silero VAD), and the cleaned transcript is posted to the
//! app's own `POST /reply` — the exact path a typed message takes, so the
//! watcher wakes and the avatar answers (and speaks, through `tts.rs`).
//!
//! Capture is `cpal`, the same cross-platform audio layer `rodio` plays through:
//! WASAPI on Windows, ALSA/PulseAudio/PipeWire on Linux, CoreAudio on macOS. The
//! device's own format is accepted as-is and downmixed to mono in the callback;
//! [`resample_pcm16`] then brings it to 16 kHz for Whisper. No `waveIn*`, no
//! `#[cfg]` in the capture path.
//!
//! Shape of the thing:
//!
//! * [`SttConfig`] is the `stt` block of `watcher_rules.json`, `#[serde(default)]`
//!   so a rules file from before voice input existed still loads.
//! * [`init`] validates the whisper binary, both models and the microphone
//!   **once**, then starts the single `avatar-ptt` thread. Anything missing is
//!   one `warn` line and `ready: false` — never a panic.
//! * That one thread owns the whole state machine (`idle` → `listening` →
//!   `transcribing`): it polls the hold shortcut through
//!   [`crate::platform::key_is_down`], drains the cpal stream while the key is
//!   held, and runs Whisper. The UI button talks to the same state machine
//!   through [`start`] / [`stop`].
//! * A press shorter than `min_speech_ms` is discarded silently (an accidental
//!   tap must not spawn a Whisper run), and so is a transcript that cleans down
//!   to nothing.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter};

/// Shipped whisper.cpp CLI (built with VAD support).
pub const DEFAULT_WHISPER_EXE_PATH: &str =
    r"C:\hermes\tools\whisper.cpp\build\bin\Release\whisper-cli.exe";
/// Shipped Whisper model: English base, quantised.
pub const DEFAULT_WHISPER_MODEL_PATH: &str = r"C:\hermes\tools\whisper.cpp\ggml-base.en.bin";
/// Shipped Silero VAD model, used through `whisper-cli --vad`.
pub const DEFAULT_SILERO_VAD_MODEL_PATH: &str =
    r"C:\hermes\tools\whisper.cpp\ggml-silero-v6.2.0.bin";
/// Default push-to-talk shortcut (hold to talk).
pub const DEFAULT_STT_HOTKEY: &str = "Alt+V";
/// Silero speech probability above which a frame counts as speech.
pub const DEFAULT_VAD_THRESHOLD: f64 = 0.5;
/// Whisper language (`auto` lets the model decide).
pub const DEFAULT_STT_LANGUAGE: &str = "en";
/// Presses shorter than this are treated as an accidental tap.
pub const DEFAULT_MIN_SPEECH_MS: u64 = 250;

/// Everything downstream (Whisper, VAD, the WAV writer) is 16 kHz.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;
/// `CreateProcess` flag: no console window for whisper-cli.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// How often the PTT thread checks the hotkey / its command channel.
const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Hard cap on one hold, so a stuck key cannot grow the clip forever.
const MAX_CAPTURE_SECS: u64 = 60;
/// `POST /reply` accepts at most this many characters (mirrors `MAX_REPLY_CHARS`).
const MAX_TRANSCRIPT_CHARS: usize = 500;

/// The `stt` block of `watcher_rules.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SttConfig {
    /// Master switch for push-to-talk.
    pub enabled: bool,
    /// Hold-to-talk shortcut, same grammar as `hotkey` (`Alt+V` by default).
    pub hotkey: String,
    /// `whisper-cli.exe` to spawn.
    pub whisper_exe_path: String,
    /// Whisper `.bin` model (`-m`).
    pub whisper_model_path: String,
    /// Silero VAD model for `--vad --vad-model`. `null` disables VAD.
    pub silero_vad_model_path: Option<String>,
    /// `--vad-threshold` (0..1).
    pub vad_threshold: f64,
    /// `-l` language code.
    pub language: String,
    /// Clips shorter than this are dropped without transcribing.
    pub min_speech_ms: u64,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            hotkey: DEFAULT_STT_HOTKEY.to_string(),
            whisper_exe_path: DEFAULT_WHISPER_EXE_PATH.to_string(),
            whisper_model_path: DEFAULT_WHISPER_MODEL_PATH.to_string(),
            silero_vad_model_path: Some(DEFAULT_SILERO_VAD_MODEL_PATH.to_string()),
            vad_threshold: DEFAULT_VAD_THRESHOLD,
            language: DEFAULT_STT_LANGUAGE.to_string(),
            min_speech_ms: DEFAULT_MIN_SPEECH_MS,
        }
    }
}

impl SttConfig {
    /// Read the `stt` block out of a rules file.
    ///
    /// A missing file, a missing block or a malformed block all fall back to
    /// [`SttConfig::default`]; the caller logs the warning once.
    pub fn load(rules_path: Option<&Path>) -> (Self, Option<String>) {
        let Some(path) = rules_path else {
            return (Self::default(), Some("no watcher_rules.json was found".to_string()));
        };
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) => {
                return (Self::default(), Some(format!("could not read {}: {err}", path.display())))
            }
        };
        let document: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(err) => {
                return (Self::default(), Some(format!("{} is not valid JSON: {err}", path.display())))
            }
        };
        let Some(block) = document.get("stt") else {
            return (Self::default(), None);
        };
        match serde_json::from_value::<SttConfig>(block.clone()) {
            Ok(config) => (config, None),
            Err(err) => (
                Self::default(),
                Some(format!("the `stt` block in {} is invalid: {err}", path.display())),
            ),
        }
    }

    /// The VAD model path, when one is configured and non-empty.
    pub fn vad_model(&self) -> Option<&str> {
        self.silero_vad_model_path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    /// `--vad-threshold`, clamped to Silero's meaningful range.
    pub fn threshold(&self) -> f64 {
        if self.vad_threshold.is_finite() {
            self.vad_threshold.clamp(0.01, 0.99)
        } else {
            DEFAULT_VAD_THRESHOLD
        }
    }

    /// `-l`, falling back to `en` when blank.
    pub fn language(&self) -> &str {
        let trimmed = self.language.trim();
        if trimmed.is_empty() {
            DEFAULT_STT_LANGUAGE
        } else {
            trimmed
        }
    }

    /// The first problem with the configured files (or the microphone), if any.
    ///
    /// `enabled: false` is not a problem: it is the shipped mute and the panel
    /// can switch it back on at runtime.
    pub fn path_problem(&self) -> Option<String> {
        let exe = Path::new(self.whisper_exe_path.trim());
        if !exe.is_file() {
            return Some(format!("whisper-cli.exe was not found at {}", exe.display()));
        }
        let model = Path::new(self.whisper_model_path.trim());
        if !model.is_file() {
            return Some(format!("the Whisper model was not found at {}", model.display()));
        }
        if let Some(vad) = self.vad_model() {
            if !Path::new(vad).is_file() {
                return Some(format!("the Silero VAD model was not found at {vad}"));
            }
        }
        None
    }

    /// The JSON `GET /stt` / the panel see (paths and tuning, not live state).
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "hotkey": self.hotkey,
            "whisper_exe_path": self.whisper_exe_path,
            "whisper_model_path": self.whisper_model_path,
            "silero_vad_model_path": self.vad_model(),
            "vad_threshold": self.threshold(),
            "language": self.language(),
            "min_speech_ms": self.min_speech_ms,
        })
    }
}

/// What the PTT worker is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PttState {
    Idle,
    Listening,
    Transcribing,
}

impl PttState {
    pub fn as_str(self) -> &'static str {
        match self {
            PttState::Idle => "idle",
            PttState::Listening => "listening",
            PttState::Transcribing => "transcribing",
        }
    }
}

/// Commands the UI (and `stop()` on mute) send to the worker thread.
enum PttCommand {
    Start,
    Stop,
    Shutdown,
}

/// Mutable half of the status: what the badge shows.
#[derive(Debug, Clone)]
struct LiveStatus {
    state: PttState,
    /// The last cleaned transcript (kept for the panel's tooltip and tests).
    transcript: String,
    /// The last failure, cleared by a successful run.
    error: String,
    /// Rate the mic actually opened at (16 000 when the device accepted it).
    sample_rate: u32,
    /// Duration of the last captured clip in ms (0 = nothing captured yet).
    clip_ms: u64,
    /// Peak absolute sample of the last clip (0 = silence; 32767 = clipping).
    peak: i32,
}

impl Default for LiveStatus {
    fn default() -> Self {
        Self {
            state: PttState::Idle,
            transcript: String::new(),
            error: String::new(),
            sample_rate: 0,
            clip_ms: 0,
            peak: 0,
        }
    }
}

struct SttRuntime {
    config: SttConfig,
    /// Live on/off switch, independent of the file's `enabled`.
    enabled: AtomicBool,
    /// False when the paths or the microphone were unusable.
    ready: bool,
    /// Why it is not ready, for the status payload.
    reason: Option<String>,
    commands: Sender<PttCommand>,
    /// Handed to the worker by [`spawn_worker`]. It lives here so the worker can
    /// be started *after* the `OnceLock` is populated — spawning inside
    /// `get_or_init` would make the worker's `RUNTIME.get()` return `None`.
    receiver: Mutex<Option<Receiver<PttCommand>>>,
    status: Mutex<LiveStatus>,
}

impl SttRuntime {
    fn start(config: SttConfig) -> Self {
        let mut problem = config.path_problem();
        if problem.is_none() {
            match input_device_count() {
                Ok(0) => problem = Some("no microphone is available".to_string()),
                Ok(_) => {}
                Err(err) => problem = Some(err),
            }
        }
        let ready = problem.is_none();
        if let Some(problem) = &problem {
            // One clear warning, never a panic: everything else keeps working.
            log::warn!("stt: push-to-talk is unavailable — {problem}");
        } else if !config.enabled {
            log::info!("stt: push-to-talk is off (`stt.enabled` is false in watcher_rules.json)");
        }

        let (commands, receiver) = mpsc::channel::<PttCommand>();
        if ready {
            log::info!(
                "stt: push-to-talk ready (hold {} · {} · VAD {})",
                config.hotkey,
                Path::new(&config.whisper_model_path)
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| config.whisper_model_path.clone()),
                if config.vad_model().is_some() { "on" } else { "off" }
            );
        }

        Self {
            enabled: AtomicBool::new(config.enabled),
            config,
            ready,
            reason: problem,
            commands,
            receiver: Mutex::new(Some(receiver)),
            status: Mutex::new(LiveStatus::default()),
        }
    }

    /// Start the `avatar-ptt` thread. Called by [`init`] *after* `RUNTIME` holds
    /// this runtime, so the worker's `RUNTIME.get()` is always `Some`.
    fn spawn_worker(&self, app: AppHandle, api_url: String) {
        if !self.ready {
            return;
        }
        let Some(receiver) = self.receiver.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return;
        };
        let config = self.config.clone();
        let spawned = std::thread::Builder::new()
            .name("avatar-ptt".to_string())
            .spawn(move || run_worker(receiver, config, app, api_url));
        if let Err(err) = spawned {
            log::warn!("stt: could not start the push-to-talk worker: {err}");
        }
    }

    fn live(&self) -> bool {
        self.ready && self.enabled.load(Ordering::Relaxed)
    }

    fn status(&self) -> Value {
        let enabled = self.enabled.load(Ordering::Relaxed);
        let live = self.status.lock().unwrap_or_else(|err| err.into_inner()).clone();
        let mut payload = self.config.to_json();
        payload["ok"] = Value::Bool(true);
        payload["enabled"] = Value::Bool(enabled);
        payload["ready"] = Value::Bool(self.ready);
        payload["state"] = Value::String(live.state.as_str().to_string());
        payload["last_transcript"] = Value::String(live.transcript);
        payload["last_error"] = if live.error.is_empty() {
            Value::Null
        } else {
            Value::String(live.error)
        };
        payload["sample_rate"] = Value::from(live.sample_rate);
        payload["last_clip_ms"] = Value::from(live.clip_ms);
        payload["last_peak"] = Value::from(live.peak);
        payload["reason"] = match (&self.reason, enabled) {
            (Some(reason), _) => Value::String(reason.clone()),
            (None, false) => Value::String("push-to-talk is switched off".to_string()),
            (None, true) => Value::Null,
        };
        payload
    }

    fn set_state(&self, state: PttState) {
        let mut live = self.status.lock().unwrap_or_else(|err| err.into_inner());
        live.state = state;
    }

    fn state(&self) -> PttState {
        self.status.lock().unwrap_or_else(|err| err.into_inner()).state
    }
}

/// The process-wide runtime. `OnceLock` because there is exactly one microphone
/// stream and one Whisper worker per app.
static RUNTIME: OnceLock<SttRuntime> = OnceLock::new();

/// Validate the config, start the PTT worker and return the initial status.
pub fn init(config: SttConfig, app: AppHandle, api_url: String) -> Value {
    let runtime = RUNTIME.get_or_init(|| SttRuntime::start(config));
    // After the `OnceLock` is populated, so the worker sees its own runtime.
    runtime.spawn_worker(app, api_url);
    runtime.status()
}

/// The JSON for `GET /stt` and `get_stt_status`.
pub fn status_json() -> Value {
    match RUNTIME.get() {
        Some(runtime) => runtime.status(),
        None => {
            let config = SttConfig::default();
            let mut payload = config.to_json();
            payload["ok"] = Value::Bool(true);
            payload["enabled"] = Value::Bool(false);
            payload["ready"] = Value::Bool(false);
            payload["state"] = Value::String(PttState::Idle.as_str().to_string());
            payload["last_transcript"] = Value::String(String::new());
            payload["last_error"] = Value::Null;
            payload["sample_rate"] = Value::from(0);
            payload["reason"] = Value::String("push-to-talk has not been initialised".to_string());
            payload
        }
    }
}

/// True when a hold would actually record.
pub fn is_live() -> bool {
    RUNTIME.get().map(SttRuntime::live).unwrap_or(false)
}

/// Mute or unmute push-to-talk at runtime (not persisted to the rules file).
pub fn set_enabled(enabled: bool) -> Value {
    if let Some(runtime) = RUNTIME.get() {
        runtime.enabled.store(enabled, Ordering::Relaxed);
        if !enabled && runtime.state() == PttState::Listening {
            let _ = runtime.commands.send(PttCommand::Stop);
        }
    }
    log::info!("stt: push-to-talk {}", if enabled { "enabled" } else { "disabled" });
    status_json()
}

/// Begin a hold (the panel button, or the hotkey's rising edge).
///
/// Also silences the avatar's own voice: a mic that can hear the Piper playback
/// would transcribe it, so the TTS queue is purged and dropped first.
pub fn start(app: &AppHandle) -> Value {
    let Some(runtime) = RUNTIME.get() else {
        return status_json();
    };
    if !runtime.live() {
        return runtime.status();
    }
    if runtime.state() != PttState::Idle {
        // Already listening (or transcribing): a second start is a no-op.
        return runtime.status();
    }
    crate::tts::interrupt();
    if runtime.commands.send(PttCommand::Start).is_ok() {
        runtime.set_state(PttState::Listening);
        emit(app, &runtime.status());
    }
    runtime.status()
}

/// End a hold: the worker stops the mic, runs Whisper and posts the transcript.
pub fn stop(app: &AppHandle) -> Value {
    let Some(runtime) = RUNTIME.get() else {
        return status_json();
    };
    let _ = runtime.commands.send(PttCommand::Stop);
    emit(app, &runtime.status());
    runtime.status()
}

/// Ask the worker to exit (used on shutdown; the process exit would do it too).
pub fn shutdown() {
    if let Some(runtime) = RUNTIME.get() {
        let _ = runtime.commands.send(PttCommand::Shutdown);
    }
}

fn emit(app: &AppHandle, payload: &Value) {
    if let Err(err) = app.emit("stt-status", payload.clone()) {
        log::warn!("failed to emit stt-status: {err}");
    }
}

/* ------------------------------------------------------------------ *
 * the worker thread
 * ------------------------------------------------------------------ */

fn run_worker(receiver: Receiver<PttCommand>, config: SttConfig, app: AppHandle, api_url: String) {
    let hotkey = match crate::parse_hotkey(&config.hotkey) {
        Ok(hotkey) => Some(hotkey),
        Err(err) => {
            log::warn!("stt: hotkey {:?} is unusable ({err}); only the panel button will talk", config.hotkey);
            None
        }
    };
    let runtime = RUNTIME.get();
    let mut hotkey_held = false;

    loop {
        // Drop anything queued while we were busy: a tap during transcription
        // must not silently start a recording afterwards.
        while matches!(receiver.try_recv(), Ok(PttCommand::Start) | Ok(PttCommand::Stop)) {}

        // ---- idle: wait for the hotkey's rising edge or the panel button ----
        let source = loop {
            if let Some(hotkey) = hotkey.as_ref() {
                let down = crate::platform::key_is_down(hotkey);
                if down && !hotkey_held {
                    hotkey_held = true;
                    break StartSource::Hotkey;
                }
                hotkey_held = down;
            }
            match receiver.recv_timeout(POLL_INTERVAL) {
                Ok(PttCommand::Start) => break StartSource::Panel,
                Ok(PttCommand::Stop) => {}
                Ok(PttCommand::Shutdown) => return,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        };

        if let Some(runtime) = runtime {
            if !runtime.enabled.load(Ordering::Relaxed) {
                continue;
            }
            runtime.set_state(PttState::Listening);
            emit(&app, &runtime.status());
        }

        // ---- listening: drain the mic until the hold ends ----
        let outcome = capture_until_stop(&receiver, hotkey.as_ref(), source, &mut hotkey_held);
        let (samples, sample_rate, peak) = match outcome {
            CaptureOutcome::Shutdown => return,
            CaptureOutcome::Failed(err) => {
                if let Some(runtime) = runtime {
                    runtime.status.lock().unwrap_or_else(|e| e.into_inner()).error = err.clone();
                    runtime.set_state(PttState::Idle);
                    emit(&app, &runtime.status());
                }
                log::warn!("stt: {err}");
                continue;
            }
            CaptureOutcome::Audio { samples, sample_rate, peak } => (samples, sample_rate, peak),
        };
        log::info!(
            "stt: captured {} ms at {} Hz (peak {peak})",
            samples.len() as u64 * 1000 / TARGET_SAMPLE_RATE as u64,
            sample_rate
        );

        // ---- transcribing ----
        let duration_ms = samples.len() as u64 * 1000 / TARGET_SAMPLE_RATE as u64;
        if duration_ms < config.min_speech_ms {
            // An accidental tap: no Whisper run, no log spam.
            log::debug!("stt: {duration_ms} ms is below min_speech_ms, discarded");
            if let Some(runtime) = runtime {
                runtime.set_state(PttState::Idle);
                emit(&app, &runtime.status());
            }
            continue;
        }

        if let Some(runtime) = runtime {
            runtime.set_state(PttState::Transcribing);
            let mut live = runtime.status.lock().unwrap_or_else(|e| e.into_inner());
            live.sample_rate = sample_rate;
            live.clip_ms = duration_ms;
            live.peak = peak;
            drop(live);
            emit(&app, &runtime.status());
        }

        let result = transcribe(&config, &samples);
        if let Some(runtime) = runtime {
            let mut live = runtime.status.lock().unwrap_or_else(|e| e.into_inner());
            match &result {
                Ok(transcript) => {
                    live.transcript = transcript.clone();
                    live.error.clear();
                }
                Err(err) => {
                    live.error = err.clone();
                }
            }
            live.state = PttState::Idle;
            drop(live);
            emit(&app, &runtime.status());
        }

        match result {
            Ok(transcript) => {
                if transcript.is_empty() {
                    log::info!("stt: the clip held no speech after sanitizing; nothing sent");
                    continue;
                }
                log::info!("stt: transcript {:?}", transcript);
                if let Err(err) = post_reply(&api_url, &transcript) {
                    log::warn!("stt: could not post the transcript to /reply: {err}");
                    if let Some(runtime) = runtime {
                        runtime.status.lock().unwrap_or_else(|e| e.into_inner()).error = err;
                        emit(&app, &runtime.status());
                    }
                }
            }
            Err(err) => log::warn!("stt: {err}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartSource {
    Hotkey,
    Panel,
}

enum CaptureOutcome {
    Audio { samples: Vec<i16>, sample_rate: u32, peak: i32 },
    Failed(String),
    Shutdown,
}

/// Record until the hold ends, then hand back 16 kHz mono samples.
fn capture_until_stop(
    receiver: &Receiver<PttCommand>,
    hotkey: Option<&crate::Hotkey>,
    source: StartSource,
    hotkey_held: &mut bool,
) -> CaptureOutcome {
    let mut stream = match MicStream::open() {
        Ok(stream) => stream,
        Err(err) => return CaptureOutcome::Failed(err),
    };
    if let Err(err) = stream.start() {
        return CaptureOutcome::Failed(err);
    }

    let mut samples: Vec<i16> = Vec::new();
    let started = Instant::now();
    loop {
        stream.drain_into(&mut samples);
        let mut done = false;
        if source == StartSource::Hotkey {
            if let Some(hotkey) = hotkey {
                let down = crate::platform::key_is_down(hotkey);
                if !down {
                    *hotkey_held = false;
                    done = true;
                }
            }
        }
        if !done {
            match receiver.try_recv() {
                Ok(PttCommand::Stop) => done = true,
                Ok(PttCommand::Start) => {}
                Ok(PttCommand::Shutdown) => {
                    stream.finish(&mut samples);
                    return CaptureOutcome::Shutdown;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    stream.finish(&mut samples);
                    return CaptureOutcome::Shutdown;
                }
            }
        }
        if !done && started.elapsed().as_secs() >= MAX_CAPTURE_SECS {
            log::info!("stt: hold reached {MAX_CAPTURE_SECS}s; stopping the clip");
            done = true;
        }
        if done {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }

    let (raw, sample_rate) = stream.finish(&mut samples);
    let mono16k = if sample_rate == TARGET_SAMPLE_RATE {
        raw
    } else {
        resample_pcm16(&raw, sample_rate, TARGET_SAMPLE_RATE)
    };
    let peak = mono16k.iter().map(|s| (*s as i32).abs()).max().unwrap_or(0);
    CaptureOutcome::Audio { samples: mono16k, sample_rate, peak }
}

/// Write the clip, run Whisper (with Silero VAD) and clean its stdout.
fn transcribe(config: &SttConfig, samples: &[i16]) -> Result<String, String> {
    let wav = temp_wav_path();
    std::fs::write(&wav, wav_bytes(samples, TARGET_SAMPLE_RATE))
        .map_err(|err| format!("could not write {}: {err}", wav.display()))?;

    let result = run_whisper(config, &wav);
    // Delete the clip immediately, whatever happened.
    if let Err(err) = std::fs::remove_file(&wav) {
        log::debug!("stt: could not delete {}: {err}", wav.display());
    }
    let output = result?;
    Ok(sanitize_transcript(&output))
}

fn run_whisper(config: &SttConfig, wav: &Path) -> Result<String, String> {
    let mut command = Command::new(config.whisper_exe_path.trim());
    command
        .arg("-m")
        .arg(config.whisper_model_path.trim())
        .arg("-f")
        .arg(wav)
        .arg("-l")
        .arg(config.language())
        .arg("-nt")
        // -np keeps whisper's banners off stdout, so what is left is the text.
        .arg("-np");
    if let Some(vad) = config.vad_model() {
        command
            .arg("--vad")
            .arg("--vad-model")
            .arg(vad)
            .arg("--vad-threshold")
            .arg(format!("{:.2}", config.threshold()));
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let output = command
        .output()
        .map_err(|err| format!("could not start whisper-cli.exe: {err}"))?;
    if !output.status.success() {
        return Err(format!("whisper-cli.exe exited with {}", output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// `%TEMP%\char-rig-stt-<pid>-<n>.wav`
fn temp_wav_path() -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("char-rig-stt-{}-{seq}.wav", std::process::id()))
}

/// A 44-byte-header PCM WAV: mono, 16-bit, `sample_rate`.
pub fn wav_bytes(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // WAVE_FORMAT_PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

/// Linear-interpolation resampler, in place of a whole audio crate.
///
/// The mic is opened at 16 kHz when the device allows it, so this is the
/// fallback path for a device that only does 44.1/48 kHz.
pub fn resample_pcm16(samples: &[i16], from_rate: u32, to_rate: u32) -> Vec<i16> {
    if samples.is_empty() || from_rate == 0 || to_rate == 0 || from_rate == to_rate {
        return samples.to_vec();
    }
    let ratio = from_rate as f64 / to_rate as f64;
    let out_len = ((samples.len() as f64) / ratio).round().max(1.0) as usize;
    let last = samples.len() - 1;
    let mut out = Vec::with_capacity(out_len);
    for index in 0..out_len {
        let position = index as f64 * ratio;
        let low = position.floor() as usize;
        let frac = position - low as f64;
        let a = samples[low.min(last)] as f64;
        let b = samples[(low + 1).min(last)] as f64;
        out.push((a + (b - a) * frac).round().clamp(-32768.0, 32767.0) as i16);
    }
    out
}

/// Interleaved 16-bit little-endian PCM → mono i16 (averaging the channels).
pub fn downmix_pcm16(data: &[u8], channels: u16) -> Vec<i16> {
    let channels = channels.max(1) as usize;
    let frame_bytes = channels * 2;
    let frames = data.len() / frame_bytes;
    let mut out = Vec::with_capacity(frames);
    for frame in 0..frames {
        let base = frame * frame_bytes;
        let mut sum: i32 = 0;
        for channel in 0..channels {
            let at = base + channel * 2;
            sum += i16::from_le_bytes([data[at], data[at + 1]]) as i32;
        }
        out.push((sum / channels as i32) as i16);
    }
    out
}

/// Clean Whisper's stdout into something worth posting.
///
/// Strips `[BLANK_AUDIO]` / `(music)` / `[inaudible]`-style non-speech markers,
/// stray timestamp brackets, control characters and whitespace runs. An empty
/// result means "nothing was said" and the caller sends nothing.
pub fn sanitize_transcript(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if c == '[' || c == '(' {
            let close = if c == '[' { ']' } else { ')' };
            if let Some(end) = chars[index + 1..].iter().position(|ch| *ch == close) {
                let inner: String = chars[index + 1..index + 1 + end].iter().collect();
                if is_non_speech(&inner) {
                    out.push(' ');
                    index += end + 2;
                    continue;
                }
            }
        }
        if c.is_control() {
            out.push(' ');
        } else {
            out.push(c);
        }
        index += 1;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Is this bracketed span a Whisper non-speech marker (or a timestamp)?
fn is_non_speech(inner: &str) -> bool {
    // `[00:00:00.000 --> 00:00:02.000]` — digits, colons, dots and the arrow.
    // Checked before normalising, because normalising eats the arrow's dashes.
    let raw = inner.trim();
    if raw.contains("-->")
        && raw
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ':' | '.' | ' ' | '-' | '>'))
    {
        return true;
    }
    let normalized: String = raw
        .chars()
        .map(|c| if c == '_' || c == '-' { ' ' } else { c })
        .collect();
    let word = normalized.trim().trim_end_matches('.').to_ascii_lowercase();
    if word.is_empty() {
        return true;
    }
    matches!(
        word.as_str(),
        "blank audio"
            | "blankaudio"
            | "music"
            | "inaudible"
            | "silence"
            | "sound"
            | "noise"
            | "laughter"
            | "applause"
            | "pause"
            | "breathing"
            | "static"
            | "beep"
            | "cough"
            | "sigh"
            | "unclear"
            | "indistinct"
            | "background noise"
    )
}

/// Post the transcript exactly like a typed message (`POST /reply`).
fn post_reply(api_url: &str, text: &str) -> Result<(), String> {
    let (text, _) = crate::truncate_chars(text, MAX_TRANSCRIPT_CHARS);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|err| err.to_string())?;
    let response = client
        .post(format!("{}/reply", api_url.trim_end_matches('/')))
        .json(&serde_json::json!({ "text": text }))
        .send()
        .map_err(|err| err.to_string())?;
    if !response.status().is_success() {
        return Err(format!("POST /reply answered {}", response.status()));
    }
    Ok(())
}

/* ------------------------------------------------------------------ *
 * microphone (cpal) + hotkey state
 * ------------------------------------------------------------------ */

/// How many input devices the host can see (`0` = no microphone at all).
fn input_device_count() -> Result<u32, String> {
    use cpal::traits::HostTrait;

    let host = cpal::default_host();
    let devices = host
        .input_devices()
        .map_err(|err| format!("could not enumerate microphones: {err}"))?;
    Ok(devices.count() as u32)
}

/// One open capture stream, downmixed to mono at the device's own rate.
///
/// `cpal` owns the device; the callback appends into `buffer` and the worker
/// drains it on its 15 ms poll. Keeping the conversion in the callback (rather
/// than buffering raw interleaved bytes) means the rest of the module only ever
/// sees mono `i16`, exactly what the old winmm path produced.
struct MicStream {
    stream: Option<cpal::Stream>,
    buffer: Arc<Mutex<Vec<i16>>>,
    sample_rate: u32,
}

impl MicStream {
    /// Open the default input device at its preferred format.
    fn open() -> Result<Self, String> {
        use cpal::traits::{DeviceTrait, HostTrait};

        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| "no default microphone is available".to_string())?;
        let supported = device
            .default_input_config()
            .map_err(|err| format!("the microphone has no usable input format: {err}"))?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let sample_rate = config.sample_rate.0;
        let channels = config.channels as usize;

        let buffer = Arc::new(Mutex::new(Vec::<i16>::new()));
        let sink = Arc::clone(&buffer);
        let error_callback = |err| log::warn!("stt: microphone stream error: {err}");

        // One arm per sample format the device may hand us; all of them end up
        // as mono i16 in `sink`.
        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _: &_| append_mono(&sink, data.iter().copied(), channels),
                error_callback,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _: &_| {
                    append_mono(&sink, data.iter().map(|s| *s as f32 / 32768.0), channels)
                },
                error_callback,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_input_stream(
                &config,
                move |data: &[u16], _: &_| {
                    append_mono(
                        &sink,
                        data.iter().map(|s| (*s as f32 - 32768.0) / 32768.0),
                        channels,
                    )
                },
                error_callback,
                None,
            ),
            other => {
                return Err(format!("unsupported microphone sample format {other:?}"));
            }
        }
        .map_err(|err| format!("could not open the microphone: {err}"))?;

        Ok(Self { stream: Some(stream), buffer, sample_rate })
    }

    fn start(&mut self) -> Result<(), String> {
        use cpal::traits::StreamTrait;

        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| "the microphone stream is closed".to_string())?;
        stream
            .play()
            .map_err(|err| format!("could not start the microphone: {err}"))
    }

    /// Move everything captured so far into `out`.
    fn drain_into(&mut self, out: &mut Vec<i16>) {
        if let Ok(mut buffer) = self.buffer.lock() {
            out.append(&mut buffer);
        }
    }

    /// Stop the device, collect the tail and report the device's sample rate.
    fn finish(mut self, out: &mut Vec<i16>) -> (Vec<i16>, u32) {
        // Dropping the stream stops the callback; drain afterwards so the last
        // few milliseconds are not lost.
        self.stream = None;
        self.drain_into(out);
        (std::mem::take(out), self.sample_rate)
    }
}

/// Average interleaved frames down to mono and append them as `i16`.
fn append_mono(buffer: &Mutex<Vec<i16>>, samples: impl Iterator<Item = f32>, channels: usize) {
    let channels = channels.max(1);
    let Ok(mut out) = buffer.lock() else { return };
    let mut sum = 0.0f32;
    let mut frame = 0usize;
    for sample in samples {
        sum += sample;
        frame += 1;
        if frame == channels {
            let mono = (sum / channels as f32).clamp(-1.0, 1.0);
            out.push((mono * 32767.0) as i16);
            sum = 0.0;
            frame = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_match_the_shipped_whisper_install() {
        let config = SttConfig::default();
        assert!(config.enabled);
        assert_eq!(config.hotkey, DEFAULT_STT_HOTKEY);
        assert_eq!(config.whisper_exe_path, DEFAULT_WHISPER_EXE_PATH);
        assert_eq!(config.whisper_model_path, DEFAULT_WHISPER_MODEL_PATH);
        assert_eq!(config.vad_model(), Some(DEFAULT_SILERO_VAD_MODEL_PATH));
        assert_eq!(config.threshold(), DEFAULT_VAD_THRESHOLD);
        assert_eq!(config.language(), "en");
        assert_eq!(config.min_speech_ms, DEFAULT_MIN_SPEECH_MS);
    }

    #[test]
    fn config_deserializes_a_full_stt_block() {
        let config: SttConfig = serde_json::from_str(
            r#"{
                "enabled": false,
                "hotkey": "Ctrl+Shift+M",
                "whisper_exe_path": "D:/whisper/whisper-cli.exe",
                "whisper_model_path": "D:/whisper/ggml-small.en.bin",
                "silero_vad_model_path": null,
                "vad_threshold": 0.7,
                "language": "de",
                "min_speech_ms": 400
            }"#,
        )
        .expect("full block");
        assert!(!config.enabled);
        assert_eq!(config.hotkey, "Ctrl+Shift+M");
        assert_eq!(config.vad_model(), None);
        assert_eq!(config.threshold(), 0.7);
        assert_eq!(config.language(), "de");
        assert_eq!(config.min_speech_ms, 400);
    }

    #[test]
    fn config_tolerates_a_partial_stt_block() {
        let config: SttConfig = serde_json::from_str(r#"{"enabled": false}"#).expect("partial");
        assert!(!config.enabled);
        assert_eq!(config.whisper_exe_path, DEFAULT_WHISPER_EXE_PATH);
        assert_eq!(config.hotkey, DEFAULT_STT_HOTKEY);
        assert_eq!(config.min_speech_ms, DEFAULT_MIN_SPEECH_MS);
    }

    /// Backward compatibility: a rules file written before voice input existed
    /// must still load, and a broken block must degrade to the defaults.
    #[test]
    fn legacy_rules_file_without_an_stt_block_still_loads() {
        let dir = std::env::temp_dir().join(format!("char-rig-stt-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("watcher_rules.json");
        std::fs::write(&path, r#"{"ack_emote": "thinking", "tts": {"enabled": true}}"#).unwrap();

        let (config, warning) = SttConfig::load(Some(&path));
        assert!(warning.is_none(), "an absent stt block is not an error");
        assert_eq!(config, SttConfig::default());

        std::fs::write(&path, r#"{"stt": {"vad_threshold": "loud"}}"#).unwrap();
        let (config, warning) = SttConfig::load(Some(&path));
        assert!(warning.is_some(), "a bad block is reported once");
        assert_eq!(config, SttConfig::default());

        let (config, warning) = SttConfig::load(Some(&dir.join("missing.json")));
        assert!(warning.is_some());
        assert_eq!(config, SttConfig::default());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn threshold_and_language_are_clamped_and_defaulted() {
        let config = SttConfig {
            vad_threshold: 9.0,
            language: "   ".to_string(),
            ..SttConfig::default()
        };
        assert_eq!(config.threshold(), 0.99);
        assert_eq!(config.language(), "en");
        let nan = SttConfig {
            vad_threshold: f64::NAN,
            ..SttConfig::default()
        };
        assert_eq!(nan.threshold(), DEFAULT_VAD_THRESHOLD);
    }

    #[test]
    fn path_problem_names_the_first_missing_piece() {
        let config = SttConfig {
            whisper_exe_path: "Z:/definitely/not/whisper-cli.exe".to_string(),
            ..SttConfig::default()
        };
        let problem = config.path_problem().expect("missing exe");
        assert!(problem.contains("whisper-cli.exe"), "{problem}");
    }

    #[test]
    fn resampling_hits_the_target_length_and_keeps_the_signal() {
        // 48 kHz → 16 kHz is exactly a third of the samples.
        let samples: Vec<i16> = vec![1000; 48_000];
        let resampled = resample_pcm16(&samples, 48_000, 16_000);
        assert_eq!(resampled.len(), 16_000);
        assert!(resampled.iter().all(|value| (*value - 1000).abs() <= 1));

        // 8 kHz → 16 kHz doubles it.
        let up = resample_pcm16(&vec![500i16; 800], 8_000, 16_000);
        assert_eq!(up.len(), 1600);

        // A no-op when the rates match, and an empty clip stays empty.
        assert_eq!(resample_pcm16(&[1, 2, 3], 16_000, 16_000), vec![1, 2, 3]);
        assert!(resample_pcm16(&[], 48_000, 16_000).is_empty());

        // A ramp stays monotonic: no reversed or duplicated edges.
        let ramp: Vec<i16> = (0..4800).map(|value| (value / 4) as i16).collect();
        let down = resample_pcm16(&ramp, 48_000, 16_000);
        assert!(down.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    /// The cpal callback's downmix: interleaved frames → mono i16, averaged
    /// (not summed) so a stereo mic does not clip at half volume.
    #[test]
    fn append_mono_averages_stereo_frames() {
        let buffer = Mutex::new(Vec::new());
        append_mono(&buffer, [0.5f32, -0.5, 0.25, 0.25].into_iter(), 2);
        let out = buffer.lock().unwrap().clone();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], 0);
        assert!((out[1] as i32 - 8191).abs() <= 1, "{out:?}");
    }

    #[test]
    fn append_mono_passes_a_mono_stream_through_and_clamps() {
        let buffer = Mutex::new(Vec::new());
        append_mono(&buffer, [0.0f32, 1.0, -1.0, 2.0].into_iter(), 1);
        assert_eq!(*buffer.lock().unwrap(), vec![0, 32767, -32767, 32767]);
    }

    #[test]
    fn append_mono_ignores_a_partial_trailing_frame() {
        let buffer = Mutex::new(Vec::new());
        append_mono(&buffer, [0.5f32, 0.5, 0.5].into_iter(), 2);
        assert_eq!(buffer.lock().unwrap().len(), 1);
    }

    #[test]
        fn downmix_averages_stereo_frames() {
        // L=100, R=300 → 200; then L=-100, R=100 → 0.
        let mut data = Vec::new();
        data.extend_from_slice(&100i16.to_le_bytes());
        data.extend_from_slice(&300i16.to_le_bytes());
        data.extend_from_slice(&(-100i16).to_le_bytes());
        data.extend_from_slice(&100i16.to_le_bytes());
        assert_eq!(downmix_pcm16(&data, 2), vec![200, 0]);
        // Mono passes through, and a trailing partial frame is ignored.
        assert_eq!(downmix_pcm16(&[1, 0, 2, 0, 3], 1), vec![1, 2]);
    }

    #[test]
    fn wav_header_is_a_valid_mono_pcm16_file() {
        let bytes = wav_bytes(&[1, -1, 100], 16_000);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(u16::from_le_bytes([bytes[20], bytes[21]]), 1, "PCM");
        assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), 1, "mono");
        assert_eq!(u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]), 16_000);
        assert_eq!(u16::from_le_bytes([bytes[34], bytes[35]]), 16, "16-bit");
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]), 6);
        assert_eq!(bytes.len(), 44 + 6);
        assert_eq!(&bytes[44..46], &1i16.to_le_bytes());
        assert_eq!(&bytes[46..48], &(-1i16).to_le_bytes());
    }

    #[test]
    fn sanitize_drops_whisper_non_speech_markers() {
        assert_eq!(sanitize_transcript("[BLANK_AUDIO]"), "");
        assert_eq!(sanitize_transcript("  [BLANK_AUDIO]  \n"), "");
        assert_eq!(sanitize_transcript("(music)"), "");
        assert_eq!(sanitize_transcript("[inaudible]"), "");
        assert_eq!(sanitize_transcript("[ Silence ]"), "");
        assert_eq!(sanitize_transcript("(applause)"), "");
        // A marker inside real speech leaves the words behind.
        assert_eq!(
            sanitize_transcript("open the pod bay doors [inaudible] please"),
            "open the pod bay doors please"
        );
        // Timestamp brackets go too.
        assert_eq!(
            sanitize_transcript("[00:00:00.000 --> 00:00:02.400]  hello there "),
            "hello there"
        );
    }

    /// End-to-end: real 16-bit PCM WAV → whisper-cli (with Silero VAD) → text.
    ///
    /// Ignored by default because it needs the shipped binaries *and* a clip;
    /// run it with `CHAR_RIG_STT_WAV=<path to a 16 kHz mono wav>` and
    /// `cargo test --lib transcribe_a_real_clip -- --ignored`.
    #[test]
    #[ignore]
    fn transcribe_a_real_clip() {
        let path = match std::env::var("CHAR_RIG_STT_WAV") {
            Ok(path) if !path.trim().is_empty() => path,
            _ => {
                eprintln!("CHAR_RIG_STT_WAV is not set; skipping");
                return;
            }
        };
        let bytes = std::fs::read(&path).expect("the clip reads");
        // Minimal 44-byte-header PCM WAV parser: mono 16-bit only.
        assert_eq!(&bytes[0..4], b"RIFF");
        let rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
        let channels = u16::from_le_bytes([bytes[22], bytes[23]]);
        let mut offset = 12;
        while offset + 8 <= bytes.len() {
            let id = &bytes[offset..offset + 4];
            let size = u32::from_le_bytes([
                bytes[offset + 4],
                bytes[offset + 5],
                bytes[offset + 6],
                bytes[offset + 7],
            ]) as usize;
            if id == b"data" {
                offset += 8;
                break;
            }
            offset += 8 + size + (size % 2);
        }
        let data = &bytes[offset..];
        let mono = downmix_pcm16(data, channels);
        let mono16k = resample_pcm16(&mono, rate, TARGET_SAMPLE_RATE);
        let config = SttConfig::default();
        let transcript = transcribe(&config, &mono16k).expect("whisper runs");
        eprintln!("transcript: {transcript:?}");
        assert!(!transcript.is_empty(), "the clip should hold speech");
    }

    #[test]
    fn sanitize_keeps_ordinary_punctuation_and_collapses_whitespace() {
        assert_eq!(
            sanitize_transcript("  Hello,\r\n\r\n   world.\tHow are you?  "),
            "Hello, world. How are you?"
        );
        // Parentheses that are part of the sentence survive.
        assert_eq!(sanitize_transcript("it costs five (5) dollars"), "it costs five (5) dollars");
        // Control characters never reach the reply pipeline.
        assert_eq!(sanitize_transcript("\u{7}hi\u{0}there"), "hi there");
        assert_eq!(sanitize_transcript(""), "");
        assert_eq!(sanitize_transcript("[]"), "");
    }
}
