//! Local text-to-speech: Piper synthesis plus sequential playback.
//!
//! Every spoken line in the app funnels through [`crate::do_speak`] (the HTTP
//! `POST /speak`, the `speak` Tauri command, the MCP `avatar_speak` tool and
//! every agent reply all end up there), so this module hangs off that one hook:
//! the bubble appears *and* the avatar says the words out loud, no matter which
//! model produced them (`lfm3-fast`, `hermes_cli`, or anything else).
//!
//! Shape of the thing:
//!
//! * [`TtsConfig`] is deserialized from the `tts` block of `watcher_rules.json`.
//!   Every field has a default, so a rules file without a `tts` block keeps
//!   working exactly as before (silent, because the missing block means
//!   `enabled: true` with the shipped Piper paths — see [`TtsConfig::default`]).
//! * [`init`] validates the paths **once** and starts one dedicated worker
//!   thread. A missing `piper.exe` / `.onnx` is a single `warn` line, never a
//!   panic: the avatar simply stays silent.
//! * [`enqueue_speech`] sanitizes the text (markdown, emoji and control tags are
//!   not spoken), drops it if nothing readable is left, and hands it to the
//!   worker's FIFO queue. It never blocks: the HTTP server and the render loop
//!   are never waiting on Piper or on the speakers.
//! * The worker synthesizes one line into a temp `.wav`, plays it **synchronously
//!   on that thread** through [`rodio`], then deletes the file. Because only one
//!   thread ever owns the output stream, lines queue up instead of cutting each
//!   other off.
//!
//! Playback is deliberately *not* a native audio API: `rodio` sits on `cpal`,
//! which picks WASAPI on Windows, ALSA/PulseAudio/PipeWire on Linux and
//! CoreAudio on macOS. The same code path therefore works on all three targets
//! with no `#[cfg]` and no OS bindings in this file.
//!
//! `piper.exe` is spawned with `CREATE_NO_WINDOW` so no console flashes over the
//! overlay. On Windows it also needs `--espeak_data <dir>`: without it Piper
//! looks for `espeak-ng-data` relative to the current directory and fails to
//! find its phoneme tables.

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};

use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Shipped Piper binary (the verified default on this machine).
pub const DEFAULT_PIPER_EXE_PATH: &str = r"C:\hermes\tools\piper\piper.exe";
/// Shipped voice model: British southern-English female, low quality (fast).
pub const DEFAULT_PIPER_MODEL_PATH: &str =
    r"C:\hermes\tools\piper\voices\en_GB-southern_english_female-low.onnx";
/// Piper's phoneme tables. Required on Windows via `--espeak_data`.
pub const DEFAULT_ESPEAK_DATA_PATH: &str = r"C:\hermes\tools\piper\espeak-ng-data";
/// Neutral speaking speed. `length_scale` is Piper's "bigger = slower".
pub const DEFAULT_LENGTH_SCALE: f64 = 1.0;
/// Slowest/fastest speaking speed accepted (`length_scale` is clamped into it).
pub const MIN_LENGTH_SCALE: f64 = 0.25;
/// Slowest speaking speed accepted.
pub const MAX_LENGTH_SCALE: f64 = 4.0;
/// Longest line handed to Piper. A bubble is capped at 180 characters anyway;
/// this only bounds a `details`-heavy caller that posts something huge.
pub const MAX_TTS_TEXT_CHARS: usize = 1200;
/// How many synthesized lines may wait while one is playing.
pub const TTS_QUEUE_DEPTH: usize = 8;
/// Window of one envelope bucket, in milliseconds.
///
/// 25 ms is one video frame at 40 Hz: fine enough that the mouth follows a
/// syllable, coarse enough that a 10 s line is ~400 numbers instead of a
/// per-sample flood over the Tauri bridge.
pub const ENVELOPE_BUCKET_MS: u64 = 25;

/// `CreateProcess` flag: no console window for the child.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The `tts` block of `watcher_rules.json`.
///
/// `#[serde(default)]` on the container means every field is optional and a
/// rules file written before voice output existed still deserializes (that is
/// the backward-compatibility guarantee — see the unit tests).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TtsConfig {
    /// Master switch. `false` keeps the whole pipeline silent.
    pub enabled: bool,
    /// `piper.exe` to spawn.
    pub piper_exe_path: String,
    /// `.onnx` voice model passed as `--model`.
    pub model_path: String,
    /// Directory passed as `--espeak_data` (Windows needs it to find the
    /// phoneme tables). `null` omits the flag.
    pub espeak_data_path: Option<String>,
    /// Multi-speaker models only: `--speaker <id>`.
    pub speaker_id: Option<u32>,
    /// Speaking speed (`--length_scale`), 1.0 = the model's natural pace.
    pub length_scale: Option<f64>,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            piper_exe_path: DEFAULT_PIPER_EXE_PATH.to_string(),
            model_path: DEFAULT_PIPER_MODEL_PATH.to_string(),
            espeak_data_path: Some(DEFAULT_ESPEAK_DATA_PATH.to_string()),
            speaker_id: None,
            length_scale: Some(DEFAULT_LENGTH_SCALE),
        }
    }
}

impl TtsConfig {
    /// Read the `tts` block out of a rules file.
    ///
    /// A missing file, a missing block or a malformed block all fall back to
    /// [`TtsConfig::default`] — a bad `tts` value must never stop the app.
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
        // `"tts": null` is how a hand-edited rules file says "no voice block";
        // treat it exactly like an absent key instead of warning about it.
        let Some(block) = document.get("tts").filter(|value| !value.is_null()) else {
            return (Self::default(), None);
        };
        match serde_json::from_value::<TtsConfig>(block.clone()) {
            Ok(config) => (config, None),
            Err(err) => (
                Self::default(),
                Some(format!("the `tts` block in {} is invalid: {err}", path.display())),
            ),
        }
    }

    /// The effective `--length_scale`, clamped to something a voice can do.
    pub fn speed(&self) -> f64 {
        self.length_scale
            .unwrap_or(DEFAULT_LENGTH_SCALE)
            .clamp(MIN_LENGTH_SCALE, MAX_LENGTH_SCALE)
    }

    /// The `--espeak_data` value, when one is configured and non-empty.
    pub fn espeak_data(&self) -> Option<&str> {
        self.espeak_data_path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    /// The first problem with the configured files, if any.
    ///
    /// Checked once at [`init`]; the answer is logged once and kept in the
    /// status so the panel can explain a silent avatar. `enabled: false` is not
    /// a *problem*: it is the shipping mute, and the panel can unmute it.
    pub fn path_problem(&self) -> Option<String> {
        let exe = Path::new(self.piper_exe_path.trim());
        if !exe.is_file() {
            return Some(format!("piper.exe was not found at {}", exe.display()));
        }
        let model = Path::new(self.model_path.trim());
        if !model.is_file() {
            return Some(format!("the voice model was not found at {}", model.display()));
        }
        if let Some(dir) = self.espeak_data() {
            if !Path::new(dir).is_dir() {
                return Some(format!("the espeak-ng data directory was not found at {dir}"));
            }
        }
        None
    }

    /// The JSON the panel and `GET /tts` see.
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "piper_exe_path": self.piper_exe_path,
            "model_path": self.model_path,
            "espeak_data_path": self.espeak_data(),
            "speaker_id": self.speaker_id,
            "length_scale": self.speed(),
        })
    }
}

/// One-shot runtime state: the config, the mute switch and the queue.
struct TtsRuntime {
    config: TtsConfig,
    /// Live mute switch (`set_enabled`), independent of the file's `enabled`.
    enabled: AtomicBool,
    /// False when the config could not produce audio (paths missing).
    ready: bool,
    /// Why it is not ready (or "disabled"), for the status payload.
    reason: Option<String>,
    /// `(epoch, text)`: the worker drops a line whose epoch is stale, which is
    /// how [`interrupt`] discards the queue without touching the speaker thread.
    queue: SyncSender<(u64, String)>,
    /// Bumped by [`interrupt`] so everything already queued is dropped.
    epoch: Arc<AtomicU64>,
}

impl TtsRuntime {
    fn start(config: TtsConfig) -> Self {
        let problem = config.path_problem();
        let ready = problem.is_none();
        if let Some(problem) = &problem {
            // One clear warning, never a panic: the avatar keeps working,
            // it just has no voice.
            log::warn!("tts: voice output is unavailable — {problem}");
        } else if !config.enabled {
            log::info!("tts: voice output is muted (`tts.enabled` is false in watcher_rules.json)");
        }
        let config_enabled = config.enabled;

        let (queue, receiver) = mpsc::sync_channel::<(u64, String)>(TTS_QUEUE_DEPTH);
        let worker_config = config.clone();
        let epoch = Arc::new(AtomicU64::new(0));
        let worker_epoch = Arc::clone(&epoch);
        let spawned = std::thread::Builder::new()
            .name("avatar-tts".to_string())
            .spawn(move || run_worker(receiver, worker_config, worker_epoch));
        let ready = match spawned {
            Ok(_) => ready,
            Err(err) => {
                log::warn!("tts: could not start the audio worker: {err}");
                false
            }
        };

        if ready {
            log::info!(
                "tts: pid voice ready ({} · length_scale {})",
                Path::new(&config.model_path)
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| config.model_path.clone()),
                config.speed()
            );
        }

        Self {
            config,
            enabled: AtomicBool::new(config_enabled),
            ready,
            reason: problem,
            queue,
            epoch,
        }
    }

    fn live(&self) -> bool {
        self.ready && self.enabled.load(Ordering::Relaxed)
    }

    fn enqueue(&self, text: &str) -> bool {
        if !self.live() {
            return false;
        }
        let clean = sanitize_for_speech(text);
        if clean.is_empty() {
            return false;
        }
        let spoken = truncate_chars(&clean, MAX_TTS_TEXT_CHARS);
        let epoch = self.epoch.load(Ordering::Relaxed);
        match self.queue.try_send((epoch, spoken)) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                log::debug!("tts: queue is full, dropping a line");
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                log::warn!("tts: the audio worker has stopped");
                false
            }
        }
    }

    fn status(&self) -> Value {
        let enabled = self.enabled.load(Ordering::Relaxed);
        let mut payload = self.config.to_json();
        payload["ok"] = Value::Bool(true);
        payload["enabled"] = Value::Bool(enabled);
        payload["ready"] = Value::Bool(self.ready);
        payload["reason"] = match (&self.reason, enabled) {
            (Some(reason), _) => Value::String(reason.clone()),
            (None, false) => Value::String("muted from the control panel".to_string()),
            (None, true) => Value::Null,
        };
        payload
    }
}

/// The process-wide runtime. `OnceLock` because the app has exactly one voice
/// and [`init`] is called once from `run()`; every other entry point only reads.
static RUNTIME: OnceLock<TtsRuntime> = OnceLock::new();

/// Validate the config, start the audio worker and log one line about it.
///
/// Safe to call once; later calls return the existing status untouched.
pub fn init(config: TtsConfig) -> Value {
    RUNTIME.get_or_init(|| TtsRuntime::start(config)).status()
}

/// True once [`init`] has run and the voice is both ready and unmuted.
pub fn is_speaking_enabled() -> bool {
    RUNTIME.get().map(TtsRuntime::live).unwrap_or(false)
}

/// The JSON for `GET /tts` and `get_tts_status`.
pub fn status_json() -> Value {
    match RUNTIME.get() {
        Some(runtime) => runtime.status(),
        None => {
            let config = TtsConfig::default();
            let mut payload = config.to_json();
            payload["ok"] = Value::Bool(true);
            payload["enabled"] = Value::Bool(false);
            payload["ready"] = Value::Bool(false);
            payload["reason"] = Value::String("voice output has not been initialised".to_string());
            payload
        }
    }
}

/// Mute or unmute the voice at runtime (not persisted to the rules file).
///
/// Muting also purges whatever is playing, so a toggle is immediate.
pub fn set_enabled(enabled: bool) -> Value {
    if let Some(runtime) = RUNTIME.get() {
        runtime.enabled.store(enabled, Ordering::Relaxed);
    }
    if !enabled {
        stop_playback();
        emit_envelope_stop();
    }
    log::info!("tts: voice output {}", if enabled { "unmuted" } else { "muted" });
    status_json()
}

/// Cut the voice off *now*: stop the line that is playing and drop every line
/// still queued. Called when push-to-talk starts, so the microphone never picks
/// up the avatar's own voice.
pub fn interrupt() {
    let Some(runtime) = RUNTIME.get() else { return };
    // The bump invalidates every queued `(epoch, text)` pair; the worker skips
    // them as it drains the channel, so only lines spoken after this point play.
    runtime.epoch.fetch_add(1, Ordering::Relaxed);
    stop_playback();
    // The worker also emits this when its `play_file` returns, but an
    // interrupted line may have nothing playing yet; the page must drop the
    // envelope immediately either way.
    emit_envelope_stop();
    log::debug!("tts: interrupted for push-to-talk");
}

/// Queue one spoken line. Returns `false` when it was skipped (muted, not
/// ready, or nothing readable after sanitizing).
pub fn enqueue_speech(text: &str) -> bool {
    RUNTIME
        .get()
        .map(|runtime| runtime.enqueue(text))
        .unwrap_or(false)
}

/* ------------------------------------------------------------------ *
 * envelope analysis (audio-driven lip sync)
 * ------------------------------------------------------------------ */

/// Where `tts-envelope` / `tts-envelope-stop` go.
///
/// The TTS worker is a plain thread with no Tauri handle, so the app installs
/// one emitter at startup ([`set_emitter`]) and the module only ever calls it.
/// A missing emitter (unit tests, headless runs) makes emission a no-op.
type Emitter = Arc<dyn Fn(&str, Value) + Send + Sync>;

static EMITTER: OnceLock<Emitter> = OnceLock::new();

/// Install the event sink used for the lip-sync envelope. First call wins.
pub fn set_emitter(emitter: Emitter) {
    let _ = EMITTER.set(emitter);
}

fn emit(event: &str, payload: Value) {
    if let Some(emitter) = EMITTER.get() {
        emitter(event, payload);
    }
}

/// Emit the "no envelope is playing" signal (playback finished, muted, cut off).
fn emit_envelope_stop() {
    emit("tts-envelope-stop", json!({}));
}

/// One 16-bit PCM section of a WAV file.
struct PcmSection<'a> {
    channels: u16,
    sample_rate: u32,
    bits: u16,
    data: &'a [u8],
}

/// Walk the RIFF chunks and pull out `fmt ` + `data`.
///
/// Deliberately strict about what it can read: PCM (format 1) and
/// WAVE_FORMAT_EXTENSIBLE (0xFFFE, which is what a 16-bit Piper wav reports on
/// some builds) at 8 or 16 bits. Anything else (float, ADPCM, a truncated
/// download) yields `None` and the caller simply speaks without a mouth
/// envelope — never an error.
fn parse_wav(bytes: &[u8]) -> Option<PcmSection<'_>> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut format: Option<(u16, u16, u32, u16)> = None;
    let mut data: Option<&[u8]> = None;
    let mut offset = 12usize;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]) as usize;
        let start = offset + 8;
        // A chunk may overrun the file (a truncated temp wav); clamp instead of
        // indexing past the end.
        let end = start.saturating_add(size).min(bytes.len());
        if id == b"fmt " && end.saturating_sub(start) >= 16 {
            let chunk = &bytes[start..end];
            let audio_format = u16::from_le_bytes([chunk[0], chunk[1]]);
            let channels = u16::from_le_bytes([chunk[2], chunk[3]]);
            let sample_rate = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
            let bits = u16::from_le_bytes([chunk[14], chunk[15]]);
            format = Some((audio_format, channels, sample_rate, bits));
        } else if id == b"data" {
            data = Some(&bytes[start..end]);
        }
        // Chunks are word-aligned: an odd size is padded with one byte.
        offset = start.saturating_add(size).saturating_add(size % 2);
    }
    let (audio_format, channels, sample_rate, bits) = format?;
    if !matches!(audio_format, 1 | 0xFFFE) || channels == 0 || sample_rate == 0 {
        return None;
    }
    if !matches!(bits, 8 | 16) {
        return None;
    }
    Some(PcmSection { channels, sample_rate, bits, data: data? })
}

/// Read one sample as `-1.0..=1.0`.
fn sample_at(data: &[u8], index: usize, bits: u16) -> f32 {
    match bits {
        8 => {
            // 8-bit PCM is unsigned, centred on 128.
            let byte = data.get(index).copied().unwrap_or(128) as f32;
            (byte - 128.0) / 128.0
        }
        _ => {
            let offset = index * 2;
            let low = data.get(offset).copied().unwrap_or(0);
            let high = data.get(offset + 1).copied().unwrap_or(0);
            i16::from_le_bytes([low, high]) as f32 / 32768.0
        }
    }
}

/// Normalized RMS envelope of a WAV file, one value per [`ENVELOPE_BUCKET_MS`].
///
/// `0.0..=1.0` with the loudest bucket pinned to `1.0`, so the page can index
/// it directly as a mouth opening without knowing how loud Piper's voice is.
/// An unreadable file, a non-PCM wav or pure silence returns an empty vector —
/// the page then keeps the typewriter lip sync.
pub fn wav_envelope(bytes: &[u8], bucket_ms: u64) -> Vec<f32> {
    let Some(section) = parse_wav(bytes) else {
        return Vec::new();
    };
    let bytes_per_sample = (section.bits as usize + 7) / 8;
    let channels = section.channels.max(1) as usize;
    let frame_bytes = bytes_per_sample * channels;
    if frame_bytes == 0 || section.data.len() < frame_bytes {
        return Vec::new();
    }
    let frames = section.data.len() / frame_bytes;
    let bucket_ms = bucket_ms.max(1);
    let frames_per_bucket = ((section.sample_rate as u64 * bucket_ms) / 1000).max(1) as usize;

    let mut envelope = Vec::with_capacity(frames / frames_per_bucket + 1);
    let mut index = 0usize;
    while index < frames {
        let end = (index + frames_per_bucket).min(frames);
        let mut sum = 0.0f64;
        let mut count = 0usize;
        for frame in index..end {
            for channel in 0..channels {
                let sample =
                    sample_at(section.data, frame * channels + channel, section.bits) as f64;
                sum += sample * sample;
                count += 1;
            }
        }
        let rms = if count == 0 { 0.0 } else { (sum / count as f64).sqrt() };
        envelope.push(rms as f32);
        index = end;
    }

    let peak = envelope.iter().copied().fold(0.0f32, f32::max);
    if peak > 1e-6 {
        for value in &mut envelope {
            *value = (*value / peak).clamp(0.0, 1.0);
        }
    } else {
        // Pure digital silence: no mouth movement to show.
        return Vec::new();
    }
    envelope
}

/// The `tts-envelope` payload for one synthesized line.
pub fn envelope_payload(envelope: &[f32]) -> Value {
    json!({ "bucket_ms": ENVELOPE_BUCKET_MS, "samples": envelope })
}

/* ------------------------------------------------------------------ *
 * audio worker
 * ------------------------------------------------------------------ */

fn run_worker(receiver: Receiver<(u64, String)>, config: TtsConfig, epoch: Arc<AtomicU64>) {
    let mut sequence: u64 = 0;
    let mut last_error = String::new();
    // Opened once and kept alive for the worker's lifetime: dropping the
    // `OutputStream` closes the device. A machine with no output device (a
    // headless CI box, a container) still synthesizes and still emits the
    // envelope — it just cannot be heard, and says so once.
    let mut output = open_output();
    while let Ok((line_epoch, text)) = receiver.recv() {
        if line_epoch != epoch.load(Ordering::Relaxed) {
            // `interrupt()` discarded this line (the user started talking).
            continue;
        }
        sequence = sequence.wrapping_add(1);
        let wav = temp_wav_path(sequence);
        match synthesize(&config, &text, &wav) {
            Ok(()) => {
                // Read the wav back before playing it: the RMS envelope is what
                // the page animates the mouth with, and the payload must reach
                // the page as playback *starts* (the worker then blocks inside
                // PlaySoundW until the line is over).
                let envelope = std::fs::read(&wav)
                    .map(|bytes| wav_envelope(&bytes, ENVELOPE_BUCKET_MS))
                    .unwrap_or_default();
                if !envelope.is_empty() {
                    emit("tts-envelope", envelope_payload(&envelope));
                    log::info!(
                        "tts: speaking {} chars ({} envelope buckets)",
                        text.chars().count(),
                        envelope.len()
                    );
                }
                if output.is_none() {
                    output = open_output();
                }
                match output.as_ref() {
                    Some(audio) => play_file(audio, &wav),
                    None => log::debug!("tts: no audio output device; skipping playback"),
                }
                if !envelope.is_empty() {
                    emit_envelope_stop();
                }
                if let Err(err) = std::fs::remove_file(&wav) {
                    log::debug!("tts: could not delete {}: {err}", wav.display());
                }
            }
            Err(err) => {
                let _ = std::fs::remove_file(&wav);
                // One line per distinct failure: a permanently broken Piper
                // must not flood the log once per speech event.
                if err != last_error {
                    log::warn!("tts: {err}");
                    last_error = err;
                }
            }
        }
    }
    log::info!("tts: audio worker stopped");
}

/// `%TEMP%\desktop-buddy-tts-<pid>-<seq>.wav`
fn temp_wav_path(sequence: u64) -> PathBuf {
    std::env::temp_dir().join(format!(
        "desktop-buddy-tts-{}-{sequence}.wav",
        std::process::id()
    ))
}

/// Run Piper once: write the UTF-8 text to its stdin, wait, check the wav.
fn synthesize(config: &TtsConfig, text: &str, wav: &Path) -> Result<(), String> {
    let mut command = Command::new(config.piper_exe_path.trim());
    command
        .arg("--model")
        .arg(config.model_path.trim())
        .arg("--output_file")
        .arg(wav)
        .arg("--length_scale")
        .arg(format!("{:.3}", config.speed()));
    if let Some(dir) = config.espeak_data() {
        command.arg("--espeak_data").arg(dir);
    }
    if let Some(speaker) = config.speaker_id {
        command.arg("--speaker").arg(speaker.to_string());
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command
        .spawn()
        .map_err(|err| format!("could not start piper.exe: {err}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|err| format!("could not write to piper's stdin: {err}"))?;
        // Closing stdin is what tells Piper the sentence is complete.
        drop(stdin);
    }
    let output = child
        .wait_with_output()
        .map_err(|err| format!("piper.exe did not finish: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        return Err(if stderr.is_empty() {
            format!("piper.exe exited with {}", output.status)
        } else {
            format!("piper.exe exited with {}: {stderr}", output.status)
        });
    }
    match std::fs::metadata(wav) {
        Ok(meta) if meta.len() > 44 => Ok(()),
        Ok(meta) => Err(format!("piper.exe wrote a {} byte wav", meta.len())),
        Err(err) => Err(format!("piper.exe wrote no wav: {err}")),
    }
}

/// The sink of the line that is currently playing.
///
/// The worker and [`stop_playback`] share the sink through an `Arc`, so muting
/// or push-to-talk can call `stop()` from any thread while the worker is parked
/// inside `sleep_until_end()` — `Sink::stop` takes `&self` and unblocks it.
static PLAYER: Mutex<Option<Arc<Sink>>> = Mutex::new(None);

/// The process's audio output: the device stream plus its handle.
///
/// `_stream` must outlive every `Sink` created from `handle`; dropping it closes
/// the device and silences playback mid-line.
struct AudioOutput {
    _stream: OutputStream,
    handle: OutputStreamHandle,
}

/// Open the default output device, or explain once why there is none.
fn open_output() -> Option<AudioOutput> {
    match OutputStream::try_default() {
        Ok((stream, handle)) => Some(AudioOutput { _stream: stream, handle }),
        Err(err) => {
            log::warn!("tts: no audio output device is available ({err}); the avatar stays silent");
            None
        }
    }
}

/// Play one wav to the end, blocking the worker thread.
///
/// Blocking is the point: the queue is sequential, so the next line must not
/// start until this one has finished. `rodio` decodes the wav itself (the
/// `wav` feature), so nothing here knows or cares which OS mixer is underneath.
fn play_file(output: &AudioOutput, path: &Path) {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) => {
            log::warn!("tts: could not open {}: {err}", path.display());
            return;
        }
    };
    let source = match Decoder::new(BufReader::new(file)) {
        Ok(source) => source,
        Err(err) => {
            log::warn!("tts: could not decode {}: {err}", path.display());
            return;
        }
    };
    let sink = match Sink::try_new(&output.handle) {
        Ok(sink) => Arc::new(sink),
        Err(err) => {
            log::warn!("tts: could not open a playback sink: {err}");
            return;
        }
    };
    sink.append(source);
    if let Ok(mut player) = PLAYER.lock() {
        *player = Some(Arc::clone(&sink));
    }
    sink.sleep_until_end();
    // Only clear the slot if it is still ours: a line queued behind this one
    // may already have replaced it.
    if let Ok(mut player) = PLAYER.lock() {
        if player.as_ref().map(|held| Arc::ptr_eq(held, &sink)).unwrap_or(false) {
            *player = None;
        }
    }
}

/// Stop whatever is playing (mute, push-to-talk, shutdown).
///
/// `Sink::stop()` is safe from any thread and also unblocks the worker's
/// `sleep_until_end()`, so the queue drains immediately instead of waiting for
/// the line to finish.
fn stop_playback() {
    if let Ok(mut player) = PLAYER.lock() {
        if let Some(sink) = player.take() {
            sink.stop();
        }
    }
}

/* ------------------------------------------------------------------ *
 * text sanitizing
 * ------------------------------------------------------------------ */

/// Strip everything Piper should not read out loud.
///
/// Markdown emphasis/code markers, fenced code blocks, links (the label stays,
/// the URL goes), bare URLs, inline `<tags>`, emoji and control characters are
/// removed; whitespace is collapsed. An empty result means "nothing to say" and
/// the caller skips synthesis entirely.
pub fn sanitize_for_speech(text: &str) -> String {
    let text = strip_code_fences(text);
    let text = strip_links(&text);
    let text = strip_urls(&text);
    let text = strip_tags(&text);

    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '\n' || c == '\r' || c == '\t' {
            out.push(' ');
            continue;
        }
        if c.is_control() || is_emoji(c) || is_markup_marker(c) {
            continue;
        }
        out.push(c);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Drop ``` / ~~~ fenced blocks (and their contents) entirely.
fn strip_code_fences(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_fence = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// `[label](url)` → `label`, `![alt](url)` → nothing.
fn strip_links(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let image = chars[i] == '!' && chars.get(i + 1) == Some(&'[');
        if image || chars[i] == '[' {
            let open = if image { i + 1 } else { i };
            if let Some(close) = matching(&chars, open, '[', ']') {
                if chars.get(close + 1) == Some(&'(') {
                    if let Some(end) = matching(&chars, close + 1, '(', ')') {
                        if !image {
                            out.extend(chars[open + 1..close].iter());
                        }
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Where the bracket opened at `open` closes, honouring nesting.
fn matching(chars: &[char], open: usize, start: char, end: char) -> Option<usize> {
    if chars.get(open) != Some(&start) {
        return None;
    }
    let mut depth = 0usize;
    for (offset, c) in chars[open..].iter().enumerate() {
        if *c == start {
            depth += 1;
        } else if *c == end {
            depth -= 1;
            if depth == 0 {
                return Some(open + offset);
            }
        }
    }
    None
}

/// Drop whitespace-separated tokens that are just a URL.
fn strip_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for token in text.split_inclusive(char::is_whitespace) {
        let trimmed = token.trim();
        let lower = trimmed.to_ascii_lowercase();
        let is_url = lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("www.");
        if is_url && !trimmed.is_empty() {
            continue;
        }
        out.push_str(token);
    }
    out
}

/// Drop inline `<tag>` / `</tag>` spans (expression/control tags) but keep
/// ordinary comparisons like `a < b`.
fn strip_tags(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<' {
            if let Some(len) = tag_len(&chars, i) {
                i += len;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn tag_len(chars: &[char], start: usize) -> Option<usize> {
    let mut name_len = 0usize;
    for (offset, c) in chars[start + 1..].iter().enumerate() {
        if *c == '>' {
            return (name_len > 0 && name_len <= 32).then_some(offset + 2);
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '/')) {
            return None;
        }
        name_len += 1;
    }
    None
}

/// Characters that are markup, not prose.
fn is_markup_marker(c: char) -> bool {
    matches!(
        c,
        '*' | '_' | '`' | '#' | '~' | '|' | '^' | '[' | ']' | '{' | '}' | '\\'
    )
}

/// Emoji, dingbats, arrows and the invisible joiners around them.
fn is_emoji(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1FAFF      // emoji, pictographs, symbols
        | 0x2600..=0x27BF      // misc symbols + dingbats
        | 0x2B00..=0x2BFF      // extra arrows/stars
        | 0x2300..=0x23FF      // misc technical (⌚ ⏰ …)
        | 0x25A0..=0x25FF      // geometric shapes (bullets)
        | 0x2190..=0x21FF      // arrows
        | 0xFE00..=0xFE0F      // variation selectors
        | 0x200D              // zero-width joiner
        | 0x203C | 0x2049 | 0x2060
    )
}

/// Clip to `max` characters on a char boundary (Piper input, not bytes).
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(max).collect();
    // Do not leave a half word dangling off the end.
    if let Some(index) = clipped.rfind(' ') {
        if index > max / 2 {
            clipped.truncate(index);
        }
    }
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_match_the_shipped_piper_install() {
        let config = TtsConfig::default();
        assert!(config.enabled);
        assert_eq!(config.piper_exe_path, DEFAULT_PIPER_EXE_PATH);
        assert_eq!(config.model_path, DEFAULT_PIPER_MODEL_PATH);
        assert_eq!(config.espeak_data(), Some(DEFAULT_ESPEAK_DATA_PATH));
        assert_eq!(config.speaker_id, None);
        assert_eq!(config.speed(), 1.0);
    }

    #[test]
    fn config_deserializes_a_full_tts_block() {
        let config: TtsConfig = serde_json::from_str(
            r#"{
                "enabled": false,
                "piper_exe_path": "D:/tools/piper.exe",
                "model_path": "D:/voices/amy.onnx",
                "espeak_data_path": "D:/tools/espeak-ng-data",
                "speaker_id": 3,
                "length_scale": 1.25
            }"#,
        )
        .expect("full block");
        assert!(!config.enabled);
        assert_eq!(config.piper_exe_path, "D:/tools/piper.exe");
        assert_eq!(config.model_path, "D:/voices/amy.onnx");
        assert_eq!(config.speaker_id, Some(3));
        assert_eq!(config.speed(), 1.25);
    }

    #[test]
    fn config_tolerates_a_partial_tts_block() {
        let config: TtsConfig =
            serde_json::from_str(r#"{"enabled": false}"#).expect("partial block");
        assert!(!config.enabled);
        // Everything else falls back to the default, not to an empty string.
        assert_eq!(config.piper_exe_path, DEFAULT_PIPER_EXE_PATH);
        assert_eq!(config.model_path, DEFAULT_PIPER_MODEL_PATH);
        assert_eq!(config.speed(), DEFAULT_LENGTH_SCALE);
    }

    #[test]
    fn explicit_nulls_keep_optional_fields_optional() {
        let config: TtsConfig = serde_json::from_str(
            r#"{"espeak_data_path": null, "speaker_id": null, "length_scale": null}"#,
        )
        .expect("nulls");
        assert_eq!(config.espeak_data(), None);
        assert_eq!(config.speaker_id, None);
        assert_eq!(config.speed(), DEFAULT_LENGTH_SCALE);
    }

    /// The backward-compatibility guarantee: a rules file from before voice
    /// output existed still loads and keeps the voice on the shipped paths.
    #[test]
    fn legacy_rules_file_without_a_tts_block_still_loads() {
        let dir = std::env::temp_dir().join(format!("desktop-buddy-tts-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("watcher_rules.json");
        std::fs::write(&path, r#"{"ack_emote": "thinking", "hotkey": "Alt+Space"}"#).unwrap();

        let (config, warning) = TtsConfig::load(Some(&path));
        assert!(warning.is_none(), "an absent tts block is not an error");
        assert_eq!(config, TtsConfig::default());

        // A malformed block degrades to the default with one warning.
        std::fs::write(&path, r#"{"tts": {"speaker_id": "loud"}}"#).unwrap();
        let (config, warning) = TtsConfig::load(Some(&path));
        assert!(warning.is_some(), "a bad block is reported once");
        assert_eq!(config, TtsConfig::default());

        // No file at all is also just a warning.
        let (config, warning) = TtsConfig::load(Some(&dir.join("missing.json")));
        assert!(warning.is_some());
        assert_eq!(config, TtsConfig::default());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_problem_names_the_first_missing_piece() {
        let config = TtsConfig {
            piper_exe_path: "Z:/definitely/not/piper.exe".to_string(),
            ..TtsConfig::default()
        };
        let problem = config.path_problem().expect("missing exe");
        assert!(problem.contains("piper.exe"), "{problem}");

        // A muted config with real paths is not a *problem* — the panel can
        // still unmute it at runtime.
        let muted = TtsConfig {
            enabled: false,
            ..TtsConfig::default()
        };
        if Path::new(DEFAULT_PIPER_EXE_PATH).is_file() {
            assert_eq!(muted.path_problem(), None);
        }
    }

    #[test]
    fn length_scale_is_clamped() {
        let slow = TtsConfig {
            length_scale: Some(99.0),
            ..TtsConfig::default()
        };
        assert_eq!(slow.speed(), MAX_LENGTH_SCALE);
        let fast = TtsConfig {
            length_scale: Some(0.0),
            ..TtsConfig::default()
        };
        assert_eq!(fast.speed(), MIN_LENGTH_SCALE);
    }

    #[test]
    fn sanitize_strips_markdown_and_keeps_the_prose() {
        let text = "**Build failed** in `main.rs`: the _type_ is #wrong\n\n\
                    See [the log](https://example.com/log.txt) 🎉 and https://example.com/x";
        let clean = sanitize_for_speech(text);
        assert_eq!(clean, "Build failed in main.rs: the type is wrong See the log and");
    }

    #[test]
    fn sanitize_drops_fenced_code_and_images() {
        let text = "Here you go:\n```rust\nfn main() { println!(\"hi\"); }\n```\nDone ![chart](chart.png) now";
        assert_eq!(sanitize_for_speech(text), "Here you go: Done now");
    }

    #[test]
    fn sanitize_drops_expression_tags_and_control_chars() {
        let text = "<happy>Sure thing</happy>\u{7} 5 < 7 ok";
        assert_eq!(sanitize_for_speech(text), "Sure thing 5 < 7 ok");
    }

    #[test]
    fn sanitize_collapses_whitespace_and_reports_empty() {
        assert_eq!(sanitize_for_speech("  hello   \n\n  world  "), "hello world");
        assert_eq!(sanitize_for_speech("```\ncode only\n```"), "");
        assert_eq!(sanitize_for_speech("*#`~|"), "");
        assert_eq!(sanitize_for_speech("🎉🎉"), "");
        assert_eq!(sanitize_for_speech("   "), "");
    }


    /* ---------- audio envelope (audio-driven lip sync) ---------- */

    /// Minimal 16-bit PCM WAV, so the parser is exercised on real bytes.
    fn wav_bytes(samples: &[i16], channels: u16, rate: u32) -> Vec<u8> {
        let data_len = (samples.len() * 2) as u32;
        let mut out = Vec::with_capacity(44 + data_len as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
        out.extend_from_slice(&(channels * 2).to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for sample in samples {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        out
    }

    #[test]
    fn wav_envelope_buckets_a_constant_tone_at_full_scale() {
        // 1 s of a constant tone at 8 kHz → 40 buckets of 25 ms (200 frames).
        let samples = vec![1000i16; 8000];
        let envelope = wav_envelope(&wav_bytes(&samples, 1, 8000), ENVELOPE_BUCKET_MS);
        assert_eq!(envelope.len(), 40, "one bucket per {} ms", ENVELOPE_BUCKET_MS);
        for value in &envelope {
            assert!(*value > 0.999 && *value <= 1.0, "{value}");
        }
    }

    #[test]
    fn wav_envelope_tracks_loud_then_quiet_and_normalises_to_the_peak() {
        // Half loud, half quiet: the shape must survive normalisation.
        let mut samples = vec![10000i16; 8000];
        samples.extend(std::iter::repeat(1000i16).take(8000));
        let envelope = wav_envelope(&wav_bytes(&samples, 1, 8000), ENVELOPE_BUCKET_MS);
        assert_eq!(envelope.len(), 80);
        assert!(envelope[0] > 0.99, "the loud half is the peak: {}", envelope[0]);
        assert!(
            (envelope[79] - 0.1).abs() < 0.01,
            "a tenth the amplitude is a tenth the envelope: {}",
            envelope[79]
        );
        // Monotonic through the step.
        assert!(envelope[39] > envelope[41]);
    }

    #[test]
    fn wav_envelope_downmixes_stereo_by_averaging_channels() {
        // One voice in the left channel only, then the same voice in both:
        // the both-channels half is sqrt(2) louder in RMS, so ~0.707 after
        // normalisation — proof the channels are averaged, not summed.
        let mut samples = Vec::new();
        for _ in 0..4000 {
            samples.push(1000i16);
            samples.push(0i16);
        }
        for _ in 0..4000 {
            samples.push(1000i16);
            samples.push(1000i16);
        }
        let envelope = wav_envelope(&wav_bytes(&samples, 2, 8000), ENVELOPE_BUCKET_MS);
        let first = envelope[0];
        let last = *envelope.last().expect("buckets");
        assert!((first - 1.0 / 2f32.sqrt()).abs() < 0.02, "{first}");
        assert!((last - 1.0).abs() < 0.02, "{last}");
    }

    #[test]
    fn wav_envelope_rejects_silence_and_unreadable_files() {
        // Digital silence has no shape to animate.
        assert!(wav_envelope(&wav_bytes(&vec![0i16; 4000], 1, 8000), 25).is_empty());
        // Not a RIFF file at all.
        assert!(wav_envelope(b"definitely not a wav", 25).is_empty());
        // A header that stops mid-chunk must not panic.
        let mut truncated = wav_bytes(&[500i16; 400], 1, 8000);
        truncated.truncate(30);
        assert!(wav_envelope(&truncated, 25).is_empty());
        // An empty buffer is empty, not a panic.
        assert!(wav_envelope(&[], 25).is_empty());
    }

    #[test]
    fn wav_envelope_reads_8_bit_pcm_centred_on_128() {
        // 8-bit PCM is unsigned: 128 is silence, 255 is full scale.
        let mut bytes = Vec::new();
        let data: Vec<u8> = std::iter::repeat(255u8).take(1600).collect();
        let data_len = data.len() as u32;
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
        bytes.extend_from_slice(&8000u32.to_le_bytes());
        bytes.extend_from_slice(&8000u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&8u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.extend_from_slice(&data);

        let envelope = wav_envelope(&bytes, ENVELOPE_BUCKET_MS);
        assert_eq!(envelope.len(), 8, "1600 samples at 8 kHz is 200 ms");
        assert!(envelope.iter().all(|value| *value > 0.99));
    }

    #[test]
    fn envelope_payload_carries_the_bucket_size() {
        let payload = envelope_payload(&[0.0, 0.5, 1.0]);
        assert_eq!(payload["bucket_ms"], ENVELOPE_BUCKET_MS);
        assert_eq!(payload["samples"].as_array().map(Vec::len), Some(3));
    }

    /// The pipeline order the worker relies on: sanitize **first**, then clip.
    /// Clipping first would cut a markdown span in half and leave a stray `*`
    /// for Piper to read out loud.
    #[test]
    fn speech_pipeline_sanitizes_before_truncating() {
        let raw = format!("**{}** 🎉 https://example.com/x", "word ".repeat(400));
        let clean = sanitize_for_speech(&raw);
        assert!(!clean.contains('*'), "markdown survived sanitizing");
        assert!(!clean.contains("http"), "a URL survived sanitizing");
        let spoken = truncate_chars(&clean, MAX_TTS_TEXT_CHARS);
        assert!(spoken.chars().count() <= MAX_TTS_TEXT_CHARS);
        assert!(!spoken.contains('*'));
        assert!(spoken.starts_with("word word"));
    }

    /// An explicit `"tts": null` is a hand-edited "no voice block", not an
    /// error: it must load the defaults silently.
    #[test]
    fn an_explicit_null_tts_block_is_treated_as_absent() {
        let dir = std::env::temp_dir().join(format!("desktop-buddy-tts-null-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("watcher_rules.json");
        std::fs::write(&path, r#"{"tts": null, "hotkey": "Alt+Space"}"#).unwrap();

        let (config, warning) = TtsConfig::load(Some(&path));
        assert!(warning.is_none(), "null is not a malformed block: {warning:?}");
        assert_eq!(config, TtsConfig::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Before `init` there is no runtime, so speaking must be a quiet no-op
    /// rather than a panic (the HTTP server can be hit before setup finishes).
    #[test]
    fn enqueue_is_a_no_op_before_init() {
        assert!(!enqueue_speech("hello"));
        assert!(!is_speaking_enabled());
        assert_eq!(status_json()["ready"], false);
    }

    #[test]
    fn truncate_never_splits_a_character() {
        let clipped = truncate_chars("alpha beta gamma", 12);
        assert!(clipped.chars().count() <= 12, "{clipped}");
        assert!(clipped.starts_with("alpha"));
        assert_eq!(truncate_chars("short", 40), "short");
    }
}
