//! Browser and transport options shared by every command that opens one.

use clap::{Args, ValueEnum};
use ferridriver::backend::BackendKind;
use ferridriver::state::ConnectMode;

/// Browser backend and connection options.
#[derive(Args, Clone)]
pub struct BrowserArgs {
  /// Browser backend to use. Unset means "whatever the config says",
  /// falling back to `cdp-pipe`; there is deliberately no clap default,
  /// because a default is indistinguishable from an explicit choice and
  /// the config could then never be overridden on the command line.
  #[arg(long, help_heading = "Browser")]
  pub backend: Option<Backend>,

  /// Run the browser without a visible window. Off by default because
  /// MCP's canonical use case is an interactive debugging / agent
  /// session where the user wants to watch the browser.
  #[arg(long, overrides_with = "headed", help_heading = "Browser")]
  pub headless: bool,

  /// Force a visible window, overriding `headless = true` in the config.
  #[arg(long, overrides_with = "headless", help_heading = "Browser")]
  pub headed: bool,

  /// Path to Chrome/Chromium binary.
  #[arg(long, help_heading = "Browser")]
  pub executable_path: Option<String>,

  /// Connect to a running browser at the given WebSocket URL.
  #[arg(long, help_heading = "Browser")]
  pub connect: Option<String>,

  /// Auto-connect to a running Chrome by channel name.
  #[arg(long, help_heading = "Browser")]
  pub auto_connect: Option<String>,

  /// User data directory used by `--auto-connect`.
  #[arg(long, help_heading = "Browser")]
  pub user_data_dir: Option<String>,
}

/// The browser settings a run will actually use, after CLI flags are
/// applied on top of the config file.
///
/// Lives beside [`BrowserArgs`] because it IS the precedence rule for
/// those flags: `ferridriver mcp` and `ferridriver config` both read it,
/// so a report can never describe a resolution the server does not
/// perform.
pub struct EffectiveBrowser {
  pub backend: ferridriver::backend::BackendKind,
  pub headless: bool,
  /// Whether the value came from the command line rather than the file,
  /// so `ferridriver config` can say which one is in force.
  pub backend_from_cli: bool,
  pub headless_from_cli: bool,
}

/// Apply CLI-over-config precedence for the browser flags.
pub fn effective_browser(args: &BrowserArgs, mcp: &ferridriver_config::mcp::McpConfig) -> EffectiveBrowser {
  let cli_backend = args.backend_kind();
  let cli_headless = args.headless_override();
  EffectiveBrowser {
    backend: cli_backend.unwrap_or_else(|| mcp.backend_kind()),
    headless: cli_headless.unwrap_or_else(|| mcp.headless()),
    backend_from_cli: cli_backend.is_some(),
    headless_from_cli: cli_headless.is_some(),
  }
}

impl BrowserArgs {
  /// The backend the user asked for on the command line, if any.
  /// `None` means "defer to the config file".
  pub fn backend_kind(&self) -> Option<BackendKind> {
    self.backend.as_ref().map(backend_to_kind)
  }

  /// The backend wire name the user asked for, for the string-typed
  /// `[test]` override path.
  pub fn backend_name(&self) -> Option<&'static str> {
    self.backend.as_ref().map(|b| match b {
      Backend::CdpPipe => "cdp-pipe",
      Backend::CdpRaw => "cdp-raw",
      Backend::WebKit => "webkit",
      Backend::Bidi => "bidi",
    })
  }

  /// Explicit headed/headless choice, or `None` when neither flag was
  /// passed and the config decides.
  pub fn headless_override(&self) -> Option<bool> {
    match (self.headless, self.headed) {
      (true, _) => Some(true),
      (_, true) => Some(false),
      _ => None,
    }
  }

  pub fn connect_mode(&self) -> ConnectMode {
    resolve_connect_mode(self)
  }
}

#[derive(Args, Clone)]
pub struct TransportArgs {
  /// Transport protocol: stdio (default) or http.
  #[arg(long, default_value = "stdio", help_heading = "Transport")]
  pub transport: Transport,

  /// Port for HTTP transport.
  #[arg(long, default_value = "8080", help_heading = "Transport")]
  pub port: u16,
}

#[derive(Clone, ValueEnum)]
pub enum Backend {
  CdpPipe,
  CdpRaw,
  #[value(name = "webkit")]
  WebKit,
  Bidi,
}

#[derive(Clone, ValueEnum)]
pub enum Transport {
  Stdio,
  Http,
}

pub fn backend_to_kind(b: &Backend) -> BackendKind {
  match b {
    Backend::CdpPipe => BackendKind::CdpPipe,
    Backend::CdpRaw => BackendKind::CdpRaw,
    Backend::WebKit => BackendKind::WebKit,
    Backend::Bidi => BackendKind::Bidi,
  }
}

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
