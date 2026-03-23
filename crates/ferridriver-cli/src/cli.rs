//! CLI argument definitions.

use clap::{Args, Parser, Subcommand, ValueEnum};
use ferridriver::backend::BackendKind;
use ferridriver::state::ConnectMode;

#[derive(Parser)]
#[command(
    name = "ferridriver",
    about = "High-performance browser automation",
    version,
    propagate_version = true,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run as an MCP server
    Mcp {
        #[command(flatten)]
        browser: BrowserArgs,

        #[command(flatten)]
        transport: TransportArgs,
    },
}

/// Browser backend and connection options.
#[derive(Args)]
pub struct BrowserArgs {
    /// Browser backend to use
    #[arg(long, value_enum, default_value_t = Backend::CdpWs)]
    backend: Backend,

    /// Connect to a running Chrome instance via WebSocket or HTTP URL
    #[arg(long, conflicts_with = "auto_connect")]
    connect: Option<String>,

    /// Auto-detect and connect to a running Chrome (reads DevToolsActivePort)
    #[arg(long, conflicts_with = "connect")]
    auto_connect: bool,

    /// Chrome release channel
    #[arg(long, default_value = "stable", requires = "auto_connect")]
    channel: String,

    /// Chrome user data directory
    #[arg(long)]
    user_data_dir: Option<String>,
}

/// MCP transport options.
#[derive(Args)]
pub struct TransportArgs {
    /// MCP transport protocol
    #[arg(long, value_enum, default_value_t = Transport::Stdio)]
    pub transport: Transport,

    /// HTTP listen port (requires --transport http)
    #[arg(long, default_value_t = 8080, requires_if("http", "transport"))]
    pub port: u16,
}

#[derive(Clone, ValueEnum)]
enum Backend {
    /// Chrome DevTools Protocol over WebSocket (via chromiumoxide)
    #[value(name = "cdp-ws")]
    CdpWs,
    /// Chrome DevTools Protocol over pipes (fd 3/4)
    #[value(name = "cdp-pipe")]
    CdpPipe,
    /// Raw CDP over WebSocket (our own, fully parallel)
    #[value(name = "cdp-raw")]
    CdpRaw,
    /// Native WKWebView (macOS only)
    #[cfg(target_os = "macos")]
    #[value(name = "webkit")]
    WebKit,
}

#[derive(Clone, ValueEnum)]
pub enum Transport {
    /// Standard IO (for Claude Code, CLI clients)
    Stdio,
    /// Streamable HTTP + SSE (for remote clients, web UIs)
    Http,
}

impl BrowserArgs {
    pub fn backend_kind(&self) -> BackendKind {
        match self.backend {
            Backend::CdpWs => BackendKind::CdpWs,
            Backend::CdpPipe => BackendKind::CdpPipe,
            Backend::CdpRaw => BackendKind::CdpRaw,
            #[cfg(target_os = "macos")]
            Backend::WebKit => BackendKind::WebKit,
        }
    }

    pub fn connect_mode(&self) -> ConnectMode {
        if self.auto_connect {
            ConnectMode::AutoConnect {
                channel: self.channel.clone(),
                user_data_dir: self.user_data_dir.clone(),
            }
        } else if let Some(url) = &self.connect {
            ConnectMode::ConnectUrl(url.clone())
        } else {
            ConnectMode::Launch
        }
    }
}
