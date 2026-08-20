//! MCP server — lets an agent speak to the user out loud.
//!
//! This process is a *proxy*, not an engine. It holds no model and opens no
//! audio device; it forwards to the running Whisper Push daemon over a Unix
//! socket (see [`crate::ipc`]). That keeps startup instant, keeps exactly one
//! copy of Kokoro in memory no matter how many agents are connected, and lets
//! the daemon's licence check be the single authority.

pub mod install;

use crate::ipc;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};
use rmcp::{ServiceExt, tool, tool_router};
use serde::Deserialize;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SpeakArgs {
    /// What to say. One or two sentences — this is speech, not a document.
    pub text: String,
    /// Optional Kokoro voice, e.g. `af_heart` (US female), `bm_george` (UK
    /// male), `ff_siwis` (French). Defaults to the user's configured voice.
    #[serde(default)]
    pub voice: Option<String>,
}

#[derive(Clone)]
pub struct Speaker {
    /// Read by the code `#[tool_handler]` generates, which the dead-code pass
    /// doesn't follow — the field is genuinely used at runtime (tools/list and
    /// tools/call both go through it).
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl Default for Speaker {
    fn default() -> Self {
        Self::new()
    }
}

impl Speaker {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[rmcp::tool_handler]
impl rmcp::ServerHandler for Speaker {
    /// Identify as Whisper Push, not as the SDK. Clients surface this name to
    /// the user, and `rmcp` (the default) tells them nothing.
    fn get_info(&self) -> rmcp::model::ServerInfo {
        // Both structs are #[non_exhaustive], so mutate a default rather than
        // building one literally.
        let mut info = rmcp::model::ServerInfo::default();
        // Overriding get_info drops the default capability set, and a server
        // that doesn't advertise `tools` won't get a tools/list from clients.
        info.capabilities.tools = Some(rmcp::model::ToolsCapability::default());
        info.server_info.name = "whisper-push".into();
        info.server_info.version = env!("CARGO_PKG_VERSION").into();
        info.instructions = Some(
            "Whisper Push can speak to the user out loud. Use `speak` for \
             short spoken updates when reading the screen is inconvenient."
                .into(),
        );
        info
    }
}

#[tool_router]
impl Speaker {
    /// Speak to the user out loud, Jarvis-style, through their speakers.
    ///
    /// Use this when a spoken word beats another wall of text: a short reply
    /// while they are reading something else, a heads-up that a long task
    /// finished, a warning they should hear even if they have looked away from
    /// the screen. The user cannot scroll back through speech, so keep it to
    /// one or two sentences and put the important part first. Do not read out
    /// code, file paths, or long lists — say what happened and leave the detail
    /// on screen. This does not capture a reply; it is one-way.
    ///
    /// Returns as soon as the utterance is accepted, not when it finishes
    /// playing, so you can carry on working while it speaks. Consecutive calls
    /// are queued and played in order rather than talked over each other.
    #[tool(name = "speak")]
    async fn speak(
        &self,
        Parameters(args): Parameters<SpeakArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let req = ipc::Request::Speak {
            text: args.text.clone(),
            voice: args.voice,
        };
        // The socket call blocks (synthesis + playback), so keep it off the
        // async reactor.
        let result = tokio::task::spawn_blocking(move || ipc::request(&req))
            .await
            .map_err(|e| ErrorData::internal_error(format!("speak task failed: {e}"), None))?;

        match result {
            // "Queued", not "Spoke": the audio is still playing when we return,
            // and telling the model otherwise would invite it to assume the
            // user has already heard it.
            Ok(r) if r.ok => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Speaking now: {}",
                args.text
            ))])),
            Ok(r) => Ok(CallToolResult::error(vec![ContentBlock::text(
                r.error.unwrap_or_else(|| "Speech failed".into()),
            )])),
            Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(
                e.to_string(),
            )])),
        }
    }
}

/// Run the stdio MCP server until the client disconnects.
pub fn run() -> anyhow::Result<()> {
    // stdout is the MCP channel — anything else written there corrupts the
    // protocol, which is why the `mcp` subcommand never initialises file/stdout
    // logging.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(async {
        let service = Speaker::new().serve(rmcp::transport::io::stdio()).await?;
        service.waiting().await?;
        Ok::<_, anyhow::Error>(())
    })
}
