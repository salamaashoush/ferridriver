//! CLI argument definitions.
//!
//! The ferridriver binary is dedicated to running the MCP server.
//! Test running (E2E, BDD, CT) is handled by the TS CLI (`ferridriver-test`)
//! or by Rust macros (`main!()`, `bdd_main!()`) via `cargo test`.

use clap::{Args, Parser, ValueEnum};
use ferridriver::backend::BackendKind;
use ferridriver::state::ConnectMode;

#[derive(Parser)]
#[command(
  name = "ferridriver",
  about = "High-performance browser automation -- MCP server",
  version,
  propagate_version = true
)]
pub struct Cli {
  /// Verbose output (-v = debug, -vv = trace including CDP protocol)
  #[arg(short, long, action = clap::ArgAction::Count, global = true)]
  pub verbose: u8,

  #[command(flatten)]
  pub browser: BrowserArgs,

  #[command(flatten)]
  pub transport: TransportArgs,
}

// ── Browser / transport args ────────────────────────────────────────────

/// Browser backend and connection options.
#[derive(Args)]
pub struct BrowserArgs {
  /// Browser backend to use
  #[arg(long, default_value = "cdp-pipe")]
  pub backend: Backend,

  /// Run headless (default: true)
  #[arg(long)]
  pub headless: bool,

  /// Path to Chrome/Chromium binary
  #[arg(long)]
  pub executable_path: Option<String>,

  /// Connect to running browser at WebSocket URL
  #[arg(long)]
  pub connect: Option<String>,

  /// Auto-connect to running Chrome (by channel name)
  #[arg(long)]
  pub auto_connect: Option<String>,

  /// User data directory for auto-connect
  #[arg(long)]
  pub user_data_dir: Option<String>,
}

impl BrowserArgs {
  pub fn backend_kind(&self) -> BackendKind {
    backend_to_kind(&self.backend)
  }

  pub fn connect_mode(&self) -> ConnectMode {
    resolve_connect_mode(self)
  }
}

#[derive(Args)]
pub struct TransportArgs {
  /// Transport protocol: stdio (default) or http
  #[arg(long, default_value = "stdio")]
  pub transport: Transport,

  /// Port for HTTP transport
  #[arg(long, default_value = "8080")]
  pub port: u16,
}

#[derive(Clone, ValueEnum)]
pub enum Backend {
  CdpPipe,
  CdpRaw,
  #[cfg(target_os = "macos")]
  Webkit,
  Bidi,
}

#[derive(Clone, ValueEnum)]
pub enum Transport {
  Stdio,
  Http,
}

/// Convert Backend enum to BackendKind.
pub fn backend_to_kind(b: &Backend) -> BackendKind {
  match b {
    Backend::CdpPipe => BackendKind::CdpPipe,
    Backend::CdpRaw => BackendKind::CdpRaw,
    #[cfg(target_os = "macos")]
    Backend::Webkit => BackendKind::WebKit,
    Backend::Bidi => BackendKind::Bidi,
  }
}

/// Resolve the connect mode from CLI arguments.
pub fn resolve_connect_mode(args: &BrowserArgs) -> ConnectMode {
  if let Some(ref url) = args.connect {
    ConnectMode::ConnectUrl(url.clone())
  } else if let Some(ref channel) = args.auto_connect {
    ConnectMode::AutoConnect {
      channel: channel.clone(),
      user_data_dir: args.user_data_dir.clone(),
    }
  } else {
    ConnectMode::Launch
  }
}
