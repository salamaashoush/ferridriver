//! CLI argument definitions.

use clap::{Args, Parser, Subcommand, ValueEnum};
use ferridriver::backend::BackendKind;
use ferridriver::state::ConnectMode;

#[derive(Parser)]
#[command(
  name = "ferridriver",
  about = "High-performance browser automation",
  version,
  propagate_version = true
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

  /// Run E2E tests
  Test {
    /// Test file patterns or specific files
    #[arg(trailing_var_arg = true)]
    files: Vec<String>,

    #[command(flatten)]
    test_args: TestArgs,
  },

  /// Run BDD/Gherkin feature tests
  Bdd {
    #[command(flatten)]
    bdd_args: BddArgs,

    /// Feature file patterns or specific .feature files
    #[arg(last = true)]
    features: Vec<String>,
  },
}

/// Test runner options.
#[derive(Args)]
pub struct TestArgs {
  /// Number of parallel workers (0 = auto)
  #[arg(long, short = 'j')]
  pub workers: Option<u32>,

  /// Number of retries for failed tests
  #[arg(long)]
  pub retries: Option<u32>,

  /// Reporter: terminal, junit, json
  #[arg(long)]
  pub reporter: Vec<String>,

  /// Grep pattern to filter tests by name
  #[arg(long, short)]
  pub grep: Option<String>,

  /// Invert grep pattern (exclude matching tests)
  #[arg(long)]
  pub grep_invert: Option<String>,

  /// Shard: current/total (e.g., "1/3")
  #[arg(long)]
  pub shard: Option<String>,

  /// Config file path
  #[arg(long, short)]
  pub config: Option<String>,

  /// Run in headed mode (show browser window)
  #[arg(long)]
  pub headed: bool,

  /// List tests without running them
  #[arg(long)]
  pub list: bool,

  /// Tag filter
  #[arg(long)]
  pub tag: Option<String>,

  /// Output directory for reports
  #[arg(long)]
  pub output: Option<String>,

  /// Configuration profile to apply
  #[arg(long)]
  pub profile: Option<String>,

  /// Fail if test.only() is found (CI safety net)
  #[arg(long)]
  pub forbid_only: bool,
}

/// BDD runner options.
#[derive(Args)]
pub struct BddArgs {
  /// Tag filter expression (e.g., "@smoke and not @wip")
  #[arg(long, short = 't')]
  pub tags: Option<String>,

  /// Number of parallel workers (0 = auto)
  #[arg(long, short = 'j')]
  pub workers: Option<u32>,

  /// Number of retries for failed scenarios
  #[arg(long)]
  pub retries: Option<u32>,

  /// Reporter: terminal, junit, json, cucumber-json
  #[arg(long)]
  pub reporter: Vec<String>,

  /// Grep pattern to filter scenarios by name
  #[arg(long, short)]
  pub grep: Option<String>,

  /// Invert grep pattern (exclude matching scenarios)
  #[arg(long)]
  pub grep_invert: Option<String>,

  /// Shard: current/total (e.g., "1/3")
  #[arg(long)]
  pub shard: Option<String>,

  /// Config file path
  #[arg(long, short)]
  pub config: Option<String>,

  /// Run in headed mode (show browser window)
  #[arg(long)]
  pub headed: bool,

  /// List scenarios without running them
  #[arg(long)]
  pub list: bool,

  /// Dry run: validate step definitions without executing
  #[arg(long)]
  pub dry_run: bool,

  /// Stop on first scenario failure
  #[arg(long)]
  pub fail_fast: bool,

  /// Per-step timeout in milliseconds
  #[arg(long)]
  pub step_timeout: Option<u64>,

  /// Output directory for reports
  #[arg(long)]
  pub output: Option<String>,

  /// Strict mode: treat undefined/pending steps as errors
  #[arg(long)]
  pub strict: bool,

  /// Scenario execution order: "defined" (default) or "random" / "random:SEED"
  #[arg(long)]
  pub order: Option<String>,

  /// Default language for Gherkin keyword i18n (e.g., "fr", "de")
  #[arg(long)]
  pub language: Option<String>,

  /// Configuration profile to apply
  #[arg(long)]
  pub profile: Option<String>,

  /// Fail if @only tag is found (CI safety net)
  #[arg(long)]
  pub forbid_only: bool,
}

/// Browser backend and connection options.
#[derive(Args)]
pub struct BrowserArgs {
  /// Browser backend to use
  #[arg(long, value_enum, default_value_t = Backend::CdpPipe)]
  backend: Backend,

  /// Connect to a running Chrome instance via WebSocket or HTTP URL
  #[arg(long, conflicts_with = "auto_connect")]
  connect: Option<String>,

  /// Auto-detect and connect to a running Chrome (reads `DevToolsActivePort`)
  #[arg(long, conflicts_with = "connect")]
  auto_connect: bool,

  /// Chrome release channel
  #[arg(long, default_value = "stable", requires = "auto_connect")]
  channel: String,

  /// Chrome user data directory
  #[arg(long)]
  user_data_dir: Option<String>,

  /// Run in headless mode (hide browser window). Default: headed.
  #[arg(long)]
  pub headless: bool,
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
  /// Chrome `DevTools` Protocol over pipes (fd 3/4)
  #[value(name = "cdp-pipe")]
  CdpPipe,
  /// Raw CDP over WebSocket (our own, fully parallel)
  #[value(name = "cdp-raw")]
  CdpRaw,
  /// Native `WKWebView` (macOS only)
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
