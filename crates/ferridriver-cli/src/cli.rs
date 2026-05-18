//! CLI argument definitions.
//!
//! ferridriver is a single binary with subcommands:
//! - `mcp`     -- MCP server (stdio or HTTP) for browser automation agents
//! - `bdd`     -- run Gherkin/Cucumber feature files via the Rust test runner
//! - `test`    -- wrap `cargo nextest` (or `cargo test`) for unit/integration tests
//! - `codegen` -- generate test scaffolding
//! - `config`  -- inspect resolved configuration

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use ferridriver::backend::BackendKind;
use ferridriver::state::ConnectMode;

#[derive(Parser)]
#[command(
  name = "ferridriver",
  about = "Rust-based browser automation: MCP server, BDD runner, test wrapper",
  version,
  propagate_version = true
)]
pub struct Cli {
  /// Verbose output (-v = debug, -vv = trace including CDP protocol)
  #[arg(short, long, action = clap::ArgAction::Count, global = true)]
  pub verbose: u8,

  /// Config file path. Auto-searches `ferridriver.toml` (TOML/YAML/JSON
  /// inferred from extension) if not specified.
  #[arg(short, long, global = true)]
  pub config: Option<PathBuf>,

  #[command(subcommand)]
  pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
  /// Run the MCP server.
  Mcp(McpArgs),

  /// Run BDD/Cucumber feature files via the Rust test runner.
  Bdd(BddArgs),

  /// Run cargo unit/integration tests via nextest (or cargo test).
  Test(TestArgs),

  /// Execute a JS script with Playwright-style bindings (script
  /// launches its own browser via `chromium()` / `firefox()` /
  /// `webkit()`).
  Run(RunArgs),

  /// Generate test scaffolding from recorded interactions.
  Codegen(CodegenArgs),

  /// Print the resolved configuration and exit.
  Config(ConfigArgs),
}

// ── mcp subcommand ──────────────────────────────────────────────────────

#[derive(Args)]
pub struct McpArgs {
  #[command(flatten)]
  pub browser: BrowserArgs,

  #[command(flatten)]
  pub transport: TransportArgs,
}

// ── bdd subcommand ──────────────────────────────────────────────────────

#[derive(Args)]
pub struct BddArgs {
  /// Feature file globs. Overrides `[bdd].features` from config.
  pub features: Vec<String>,

  /// Tag filter expression, e.g. `@smoke and not @wip`.
  #[arg(long)]
  pub tags: Option<String>,

  /// Parse and report scenarios without executing steps.
  #[arg(long)]
  pub dry_run: bool,

  /// Stop after the first failing scenario.
  #[arg(long)]
  pub fail_fast: bool,

  /// Treat undefined or pending steps as failures.
  #[arg(long)]
  pub strict: bool,

  /// Per-step timeout in milliseconds.
  #[arg(long)]
  pub step_timeout: Option<u64>,

  /// Scenario execution order: `defined`, `random`, or `random:<seed>`.
  #[arg(long)]
  pub order: Option<String>,

  /// Gherkin keyword language (e.g. `en`, `de`, `fr`).
  #[arg(long)]
  pub language: Option<String>,

  /// Number of parallel workers.
  #[arg(long)]
  pub workers: Option<usize>,

  /// Reporter spec list, e.g. `terminal,junit:target/junit.xml`.
  #[arg(long)]
  pub reporter: Vec<String>,

  /// JavaScript step-definition file globs, e.g.
  /// `--steps 'steps/**/*.js'`. May be repeated. Overrides
  /// `[test].steps` from config. Defaults to `steps/**/*.js` and
  /// `step_definitions/**/*.js` when omitted.
  #[arg(long)]
  pub steps: Vec<String>,

  #[command(flatten)]
  pub browser: BrowserArgs,
}

// ── test subcommand ─────────────────────────────────────────────────────

#[derive(Args)]
pub struct TestArgs {
  /// Test name filter passed through to the underlying runner.
  pub filter: Option<String>,

  /// Cargo package filter (`-p <name>`). May be repeated.
  #[arg(short = 'p', long = "package")]
  pub packages: Vec<String>,

  /// Force a specific runner backend regardless of config.
  #[arg(long, value_enum)]
  pub runner: Option<TestRunner>,

  /// nextest profile name.
  #[arg(long)]
  pub profile: Option<String>,

  /// Pass remaining arguments through to the underlying runner.
  #[arg(last = true)]
  pub passthrough: Vec<String>,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum TestRunner {
  Nextest,
  Cargo,
}

// ── run subcommand ──────────────────────────────────────────────────────

#[derive(Args)]
pub struct RunArgs {
  /// Script file (`.js`/`.mjs`), or `-` to read source from stdin.
  /// Omit when using `--eval`.
  pub script: Option<String>,

  /// Inline script source (alternative to a file / stdin).
  #[arg(short = 'e', long = "eval", conflicts_with = "script")]
  pub eval: Option<String>,

  /// Per-script wall-clock timeout in milliseconds.
  #[arg(long)]
  pub timeout_ms: Option<u64>,

  /// Positional args exposed to the script as the `args` global
  /// (strings). Pass after `--`.
  #[arg(last = true)]
  pub script_args: Vec<String>,
}

// ── codegen subcommand ──────────────────────────────────────────────────

#[derive(Args)]
pub struct CodegenArgs {
  /// URL to open in the codegen browser.
  pub url: Option<String>,

  /// Output file for generated test code.
  #[arg(short, long)]
  pub output: Option<PathBuf>,

  /// Output language: `ts`, `js`, `rust`.
  #[arg(long, default_value = "ts")]
  pub language: String,

  #[command(flatten)]
  pub browser: BrowserArgs,
}

// ── config subcommand ───────────────────────────────────────────────────

#[derive(Args)]
pub struct ConfigArgs {
  /// Output format: `toml`, `json`, `yaml`.
  #[arg(long, default_value = "toml")]
  pub format: ConfigFormat,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum ConfigFormat {
  Toml,
  Json,
  Yaml,
}

// ── Shared browser / transport args ─────────────────────────────────────

/// Browser backend and connection options.
#[derive(Args, Clone)]
pub struct BrowserArgs {
  /// Browser backend to use.
  #[arg(long, default_value = "cdp-pipe")]
  pub backend: Backend,

  /// Run the browser without a visible window. Off by default because
  /// MCP's canonical use case is an interactive debugging / agent
  /// session where the user wants to watch the browser.
  #[arg(long)]
  pub headless: bool,

  /// Path to Chrome/Chromium binary.
  #[arg(long)]
  pub executable_path: Option<String>,

  /// Connect to a running browser at the given WebSocket URL.
  #[arg(long)]
  pub connect: Option<String>,

  /// Auto-connect to a running Chrome by channel name.
  #[arg(long)]
  pub auto_connect: Option<String>,

  /// User data directory used by `--auto-connect`.
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

#[derive(Args, Clone)]
pub struct TransportArgs {
  /// Transport protocol: stdio (default) or http.
  #[arg(long, default_value = "stdio")]
  pub transport: Transport,

  /// Port for HTTP transport.
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

pub fn backend_to_kind(b: &Backend) -> BackendKind {
  match b {
    Backend::CdpPipe => BackendKind::CdpPipe,
    Backend::CdpRaw => BackendKind::CdpRaw,
    #[cfg(target_os = "macos")]
    Backend::Webkit => BackendKind::WebKit,
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
