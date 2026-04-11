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
  /// Verbose output (-v = debug, -vv = trace including CDP protocol)
  #[arg(short, long, action = clap::ArgAction::Count, global = true)]
  pub verbose: u8,

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
    common: CommonRunArgs,
  },

  /// Run BDD/Gherkin feature tests
  Bdd {
    #[command(flatten)]
    common: CommonRunArgs,

    #[command(flatten)]
    bdd: BddOnlyArgs,

    /// Feature file patterns or specific .feature files
    #[arg(last = true)]
    features: Vec<String>,
  },

  /// Install browsers for automation
  Install {
    /// Browser to install (default: chromium)
    #[arg(default_value = "chromium")]
    browser: String,

    /// Also install system dependencies (Linux: apt packages for fonts, libs)
    #[arg(long)]
    with_deps: bool,
  },

  /// Record user interactions and generate test code
  Codegen {
    /// URL to open in the browser
    url: String,

    /// Output language: rust, typescript (ts), gherkin (bdd)
    #[arg(long, short, default_value = "rust")]
    language: String,

    /// Write generated code to file instead of stdout
    #[arg(long, short)]
    output: Option<String>,

    /// Viewport size (WxH, e.g. "1280x720")
    #[arg(long)]
    viewport: Option<String>,
  },
}

// ── Shared args for test and BDD subcommands ────────────────────────────

/// Arguments shared between `test` and `bdd` subcommands.
#[derive(Args)]
pub struct CommonRunArgs {
  /// Backend protocol: cdp-pipe (default), cdp-raw, webkit, bidi
  #[arg(long)]
  pub backend: Option<Backend>,

  /// Browser to launch: chromium (default), firefox, webkit
  #[arg(long)]
  pub browser: Option<String>,

  /// Number of parallel workers (0 = auto)
  #[arg(long, short = 'j')]
  pub workers: Option<u32>,

  /// Number of retries for failed tests/scenarios
  #[arg(long)]
  pub retries: Option<u32>,

  /// Reporter: terminal, junit, json, cucumber-json
  #[arg(long)]
  pub reporter: Vec<String>,

  /// Grep pattern to filter by name
  #[arg(long, short)]
  pub grep: Option<String>,

  /// Invert grep pattern (exclude matching)
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

  /// List tests/scenarios without running them
  #[arg(long)]
  pub list: bool,

  /// Tag filter
  #[arg(long)]
  pub tag: Option<String>,

  /// Output directory for reports and artifacts
  #[arg(long)]
  pub output: Option<String>,

  /// Configuration profile to apply
  #[arg(long)]
  pub profile: Option<String>,

  /// Fail if .only() / @only is found (CI safety net)
  #[arg(long)]
  pub forbid_only: bool,

  /// Re-run only previously failed tests (from @rerun.txt)
  #[arg(long)]
  pub last_failed: bool,

  /// Record video: off, on, retain-on-failure
  #[arg(long)]
  pub video: Option<String>,

  /// Record trace: off, on, retain-on-failure, on-first-retry
  #[arg(long)]
  pub trace: Option<String>,

  /// Path to storage state JSON (pre-authenticated session)
  #[arg(long)]
  pub storage_state: Option<String>,

  /// Serve a static directory as the test server (sets base_url automatically)
  #[arg(long)]
  pub web_server_dir: Option<String>,

  /// Start a dev server command before tests (requires --web-server-url)
  #[arg(long)]
  pub web_server_cmd: Option<String>,

  /// URL to wait for when using --web-server-cmd
  #[arg(long)]
  pub web_server_url: Option<String>,

  /// Watch mode: re-run on file changes
  #[arg(long, short = 'w')]
  pub watch: bool,
}

// ── BDD-only args ───────────────────────────────────────────────────────

/// Arguments specific to the `bdd` subcommand.
#[derive(Args)]
pub struct BddOnlyArgs {
  /// Tag filter expression (e.g., "@smoke and not @wip")
  #[arg(long, short = 't')]
  pub tags: Option<String>,

  /// Dry run: validate step definitions without executing
  #[arg(long)]
  pub dry_run: bool,

  /// Stop on first scenario failure
  #[arg(long)]
  pub fail_fast: bool,

  /// Per-step timeout in milliseconds
  #[arg(long)]
  pub step_timeout: Option<u64>,

  /// Strict mode: treat undefined/pending steps as errors
  #[arg(long)]
  pub strict: bool,

  /// Scenario execution order: "defined" (default) or "random" / "random:SEED"
  #[arg(long)]
  pub order: Option<String>,

  /// Default language for Gherkin keyword i18n (e.g., "fr", "de")
  #[arg(long)]
  pub language: Option<String>,
}

// ── Helper: convert CommonRunArgs to CliOverrides ───────────────────────

impl CommonRunArgs {
  /// Convert to the config system's `CliOverrides`.
  pub fn to_overrides(&self) -> Result<ferridriver_test::config::CliOverrides, String> {
    Ok(ferridriver_test::config::CliOverrides {
      workers: self.workers,
      retries: self.retries,
      reporter: self.reporter.clone(),
      grep: self.grep.clone(),
      grep_invert: self.grep_invert.clone(),
      tag: self.tag.clone(),
      headed: self.headed,
      shard: self
        .shard
        .as_deref()
        .map(ferridriver_test::config::ShardArg::parse)
        .transpose()?,
      config_path: self.config.clone(),
      output_dir: self.output.clone(),
      test_files: Vec::new(),
      list_only: self.list,
      update_snapshots: None,
      profile: self.profile.clone(),
      forbid_only: self.forbid_only,
      last_failed: self.last_failed,
      video: self.video.clone(),
      trace: self.trace.clone(),
      storage_state: self.storage_state.clone(),
      browser: self.browser.clone(),
      backend: self.backend.as_ref().map(|b| backend_to_string(b)),
      ..Default::default()
    })
  }

  /// Build `WebServerConfig` entries from CLI flags.
  pub fn web_server_configs(&self) -> Vec<ferridriver_test::config::WebServerConfig> {
    let mut configs = Vec::new();
    if let Some(ref dir) = self.web_server_dir {
      configs.push(ferridriver_test::config::WebServerConfig {
        static_dir: Some(dir.clone()),
        ..Default::default()
      });
    }
    if let Some(ref cmd) = self.web_server_cmd {
      configs.push(ferridriver_test::config::WebServerConfig {
        command: Some(cmd.clone()),
        url: self.web_server_url.clone(),
        ..Default::default()
      });
    }
    configs
  }
}

// ── Browser / transport args (MCP-specific) ─────────────────────────────

/// Browser backend and connection options (MCP subcommand).
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

/// Convert Backend enum to the string format expected by config.
pub fn backend_to_string(b: &Backend) -> String {
  match b {
    Backend::CdpPipe => "cdp-pipe".into(),
    Backend::CdpRaw => "cdp-raw".into(),
    #[cfg(target_os = "macos")]
    Backend::Webkit => "webkit".into(),
    Backend::Bidi => "bidi".into(),
  }
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
