//! Local IPC: a Unix socket the MCP proxy talks to.
//!
//! The daemon is already running and already keeps models warm. Loading a
//! second copy of Kokoro inside every MCP client would be wasteful and would
//! fight the daemon for the output device, so `whisper-push mcp` holds no model
//! at all — it forwards here, and the daemon does the work.
//!
//! The protocol is one JSON object per line, request and response, which is
//! enough for a single verb and keeps the proxy trivial.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;
use tracing::{debug, warn};

/// Refuse anything larger; a `speak` request is a sentence, not a document.
const MAX_REQUEST_BYTES: u64 = 64 * 1024;

/// How long the proxy waits for the daemon. Synthesis of a long sentence plus a
/// cold model load has to fit inside this.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Synthesize and play through the user's speakers.
    Speak {
        text: String,
        #[serde(default)]
        voice: Option<String>,
    },
    /// Liveness probe, so the proxy can give a good error instead of hanging.
    Ping,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    fn ok() -> Self {
        Self {
            ok: true,
            error: None,
        }
    }
    fn err(e: impl std::fmt::Display) -> Self {
        Self {
            ok: false,
            error: Some(e.to_string()),
        }
    }
}

pub fn socket_path() -> PathBuf {
    crate::config::data_dir().join("whisper-push.sock")
}

/// Start the listener on a background thread. Never fatal: a daemon that can't
/// bind the socket must still dictate.
pub fn spawn() {
    std::thread::Builder::new()
        .name("ipc".into())
        .spawn(|| {
            if let Err(e) = serve() {
                warn!("IPC server stopped: {e}");
            }
        })
        .ok();
}

fn serve() -> Result<()> {
    let path = socket_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A socket file left behind by a crash would make bind() fail with
    // EADDRINUSE. The single-instance lock already guarantees no other daemon
    // is live, so removing it is safe.
    let _ = std::fs::remove_file(&path);

    let listener =
        UnixListener::bind(&path).with_context(|| format!("Failed to bind {}", path.display()))?;

    // Owner-only: this socket makes the machine speak.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    debug!("IPC listening on {}", path.display());

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                // One short-lived thread per connection: synthesis blocks, and
                // a slow client must not stall the next one.
                std::thread::spawn(move || {
                    if let Err(e) = handle(s) {
                        debug!("IPC connection ended: {e}");
                    }
                });
            }
            Err(e) => warn!("IPC accept failed: {e}"),
        }
    }
    Ok(())
}

fn handle(stream: UnixStream) -> Result<()> {
    let mut writer = stream.try_clone()?;
    let mut line = String::new();
    BufReader::new(stream.take(MAX_REQUEST_BYTES)).read_line(&mut line)?;

    let response = match serde_json::from_str::<Request>(line.trim()) {
        Ok(req) => dispatch(req),
        Err(e) => Response::err(format!("Malformed request: {e}")),
    };
    writeln!(writer, "{}", serde_json::to_string(&response)?)?;
    writer.flush()?;
    Ok(())
}

fn dispatch(req: Request) -> Response {
    match req {
        Request::Ping => Response::ok(),
        Request::Speak { text, voice } => match speak(&text, voice.as_deref()) {
            Ok(()) => Response::ok(),
            Err(e) => Response::err(e),
        },
    }
}

/// At most this many utterances may wait for rendering. An agent in a loop
/// should get an error, not build an unbounded backlog the user then has to sit
/// through.
const SPEECH_QUEUE_LIMIT: usize = 8;

/// Queued work for the speech thread.
struct Job {
    speech: crate::tts::Speech,
    variant: String,
}

static SPEECH_TX: std::sync::OnceLock<Result<crossbeam_channel::Sender<Job>, String>> =
    std::sync::OnceLock::new();

/// Build a two-stage ordered pipeline.
///
/// The renderer is serial because the ONNX session is shared. Playback has its
/// own serial worker so the renderer can synthesize utterance N+1 while N is
/// audible. The zero-capacity handoff intentionally allows exactly one rendered
/// utterance ahead: more would only retain large PCM buffers in memory.
fn spawn_ordered_pipeline<J, A>(
    queue_limit: usize,
    render: impl Fn(J) -> Result<A> + Send + 'static,
    play: impl Fn(A) -> Result<()> + Send + 'static,
) -> std::io::Result<crossbeam_channel::Sender<J>>
where
    J: Send + 'static,
    A: Send + 'static,
{
    let (job_tx, job_rx) = crossbeam_channel::bounded::<J>(queue_limit);
    let (audio_tx, audio_rx) = crossbeam_channel::bounded::<A>(0);

    std::thread::Builder::new()
        .name("tts-playback".into())
        .spawn(move || {
            for audio in audio_rx {
                if let Err(e) = play(audio) {
                    warn!("TTS playback failed: {e}");
                }
            }
        })?;

    std::thread::Builder::new()
        .name("tts-render".into())
        .spawn(move || {
            for job in job_rx {
                match render(job) {
                    Ok(audio) => {
                        // Rendezvous with the playback worker. While it plays
                        // this item, the loop immediately starts rendering the
                        // next one; if that finishes early, it waits here with
                        // just one completed buffer in memory.
                        if audio_tx.send(audio).is_err() {
                            warn!("TTS playback worker stopped");
                            break;
                        }
                    }
                    // These failures happen after the MCP call has returned, so
                    // log them and keep the queue moving.
                    Err(e) => warn!("TTS synthesis failed: {e}"),
                }
            }
        })?;

    Ok(job_tx)
}

fn speech_queue() -> Result<&'static crossbeam_channel::Sender<Job>> {
    match SPEECH_TX.get_or_init(|| {
        spawn_ordered_pipeline(
            SPEECH_QUEUE_LIMIT,
            |job: Job| crate::tts::render(&job.speech, &job.variant),
            |audio| crate::audio::playback::play_pcm(&audio, crate::tts::SAMPLE_RATE),
        )
        .map_err(|e| format!("Failed to start TTS workers: {e}"))
    }) {
        Ok(tx) => Ok(tx),
        Err(e) => bail!(e.clone()),
    }
}

fn enqueue(job: Job) -> Result<()> {
    match speech_queue()?.try_send(job) {
        Ok(()) => Ok(()),
        Err(crossbeam_channel::TrySendError::Full(_)) => bail!(
            "Speech queue is full ({SPEECH_QUEUE_LIMIT} utterances are waiting for synthesis) — \
             wait for it to drain before speaking again."
        ),
        Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
            bail!("Speech workers are unavailable — restart Whisper Push and try again.")
        }
    }
}

#[cfg(test)]
mod pipeline_tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Event {
        Render(usize),
        PlayStart(usize),
        PlayEnd(usize),
    }

    #[test]
    fn renders_the_next_item_during_current_playback_and_keeps_order() {
        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        let (release_tx, release_rx) = crossbeam_channel::bounded(1);

        let render_events = events_tx.clone();
        let play_events = events_tx;
        let tx = spawn_ordered_pipeline(
            4,
            move |id| {
                render_events.send(Event::Render(id)).unwrap();
                Ok(id)
            },
            move |id| {
                play_events.send(Event::PlayStart(id)).unwrap();
                if id == 1 {
                    release_rx.recv().unwrap();
                }
                play_events.send(Event::PlayEnd(id)).unwrap();
                Ok(())
            },
        )
        .unwrap();

        tx.send(1).unwrap();
        tx.send(2).unwrap();

        // Playback 1 is deliberately blocked. Rendering 2 must still complete
        // before we release it, proving the two stages overlap.
        let mut events = Vec::new();
        while !(events.contains(&Event::PlayStart(1)) && events.contains(&Event::Render(2))) {
            events.push(
                events_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("pipeline stalled before rendering ahead"),
            );
        }
        assert!(!events.contains(&Event::PlayEnd(1)));

        release_tx.send(()).unwrap();
        while !events.contains(&Event::PlayEnd(2)) {
            events.push(
                events_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("pipeline stalled during ordered playback"),
            );
        }

        let starts: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::PlayStart(id) => Some(*id),
                _ => None,
            })
            .collect();
        assert_eq!(starts, vec![1, 2]);
    }
}

/// Validate, then hand off. Returns as soon as the utterance is accepted, so
/// the agent isn't blocked for the length of the speech.
///
/// Everything the caller could act on is checked *before* returning — licence,
/// unknown voice, missing espeak-ng, empty text — so making this async does not
/// turn real errors into silence.
fn speak(text: &str, voice: Option<&str>) -> Result<()> {
    // Entitlement is enforced here rather than in the proxy: the capability
    // lives in the daemon, and the daemon is the only thing that can be sure.
    if !crate::license::is_entitled() {
        bail!("Whisper Push is not licensed on this device — speech is unavailable.");
    }
    let cfg = crate::config::Config::load()?;
    let voice = voice.unwrap_or(&cfg.tts_voice);

    let speech = crate::tts::prepare(text, voice)?;
    enqueue(Job {
        speech,
        variant: cfg.tts_model.clone(),
    })
}

// ─── Client side (used by the MCP proxy) ────────────────────────────────────

/// Send one request to the running daemon and wait for its reply.
pub fn request(req: &Request) -> Result<Response> {
    let path = socket_path();
    let stream = UnixStream::connect(&path).map_err(|e| {
        anyhow::anyhow!(
            "Cannot reach Whisper Push ({e}). Is the app running? \
             Launch it from /Applications or the menu bar, then retry."
        )
    })?;
    stream.set_read_timeout(Some(CLIENT_TIMEOUT))?;
    stream.set_write_timeout(Some(CLIENT_TIMEOUT))?;

    let mut writer = stream.try_clone()?;
    writeln!(writer, "{}", serde_json::to_string(req)?)?;
    writer.flush()?;

    let mut line = String::new();
    BufReader::new(stream.take(MAX_REQUEST_BYTES)).read_line(&mut line)?;
    if line.trim().is_empty() {
        bail!("Whisper Push closed the connection without replying");
    }
    Ok(serde_json::from_str(line.trim())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speak_request_roundtrips() {
        let json = r#"{"cmd":"speak","text":"hello","voice":"af_heart"}"#;
        match serde_json::from_str::<Request>(json).unwrap() {
            Request::Speak { text, voice } => {
                assert_eq!(text, "hello");
                assert_eq!(voice.as_deref(), Some("af_heart"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn voice_is_optional() {
        let json = r#"{"cmd":"speak","text":"hello"}"#;
        match serde_json::from_str::<Request>(json).unwrap() {
            Request::Speak { voice, .. } => assert!(voice.is_none()),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn unknown_command_is_rejected() {
        assert!(serde_json::from_str::<Request>(r#"{"cmd":"rm_rf","text":"x"}"#).is_err());
        assert!(serde_json::from_str::<Request>("not json").is_err());
    }

    #[test]
    fn error_response_carries_the_message() {
        let r = Response::err("boom");
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"ok\":false"));
        assert!(s.contains("boom"));
        // A success response stays minimal.
        assert_eq!(
            serde_json::to_string(&Response::ok()).unwrap(),
            r#"{"ok":true}"#
        );
    }
}
