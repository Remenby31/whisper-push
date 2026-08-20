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

/// Synthesize and play. Runs on the connection thread, so the caller's timeout
/// covers the whole thing.
fn speak(text: &str, voice: Option<&str>) -> Result<()> {
    // Entitlement is enforced here rather than in the proxy: the capability
    // lives in the daemon, and the daemon is the only thing that can be sure.
    if !crate::license::is_entitled() {
        bail!("Whisper Push is not licensed on this device — speech is unavailable.");
    }
    let cfg = crate::config::Config::load()?;
    let voice = voice.unwrap_or(&cfg.tts_voice);

    let audio = crate::tts::synth(text, voice, &cfg.tts_model)?;
    crate::audio::playback::play_pcm(&audio, crate::tts::SAMPLE_RATE)
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
