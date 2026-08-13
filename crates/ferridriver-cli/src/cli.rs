//! CLI argument definitions.
//!
//! ferridriver is a single binary with subcommands:
//! - `mcp`     -- MCP server (stdio or HTTP) for browser automation agents
//! - `bdd`     -- run Gherkin/Cucumber feature files via the Rust test runner
//! - `test`    -- wrap `cargo nextest` (or `cargo test`) for unit/integration tests
//! - `run`     -- execute a JS/TS script with Playwright-style bindings
//! - `install` -- download browser binaries into the local cache
//! - `codegen` -- generate test scaffolding

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

  /// Config file path, applied on top of the discovered config layers
  /// (machine, user, repository, cwd). Format inferred from the
  /// extension.
  #[arg(short, long, global = true)]
  pub config: Option<PathBuf>,

  /// Ignore every config layer except `--config` (or, without it, the
  /// config in the current directory). For reproducible runs that must
  /// not pick up machine-, user- or repository-level settings. Also
  /// settable as `FERRIDRIVER_NO_INHERIT=1` (read by the loader, so it
  /// applies to child processes too).
  #[arg(long, global = true)]
  pub no_inherit: bool,

  #[command(subcommand)]
  pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
  /// Run the MCP server.
  Mcp(McpArgs),

  /// Run BDD/Cucumber feature files via the Rust test runner.
  Bdd(BddArgs),

  /// Run TypeScript/JavaScript test files (`*.test.ts`, `*.spec.ts`)
  /// through the native test runner — Playwright-shaped `test`/
  /// `describe`/`expect` from '@ferridriver/test', executed in the
  /// embedded QuickJS engine (no Node required).
  Test(TestRunArgs),

  /// Run cargo unit/integration tests via nextest (or cargo test).
  RustTest(RustTestArgs),

  /// Execute a JS script with Playwright-style bindings (script
  /// launches its own browser via `chromium()` / `firefox()` /
  /// `webkit()`).
  Run(RunArgs),

  /// Download browser binaries (Chrome for Testing) into the local cache.
  Install(InstallArgs),

  /// Generate test scaffolding from recorded interactions.
  Codegen(CodegenArgs),

  /// Manage and drive named browser sessions (bind / attach / list / close).
  Session(SessionArgs),
}

// ── session subcommand ──────────────────────────────────────────────────

#[derive(Args)]
pub struct SessionArgs {
  #[command(subcommand)]
  pub command: SessionCommand,
}

#[derive(Subcommand)]
pub enum SessionCommand {
  /// Launch a browser, bind it under `id`, and serve it in the background.
  /// Spawns a detached host process and returns once the session is live.
  Open(SessionOpenArgs),

  /// Internal: run the long-lived session host in the foreground (launch +
  /// bind + serve until killed). `open` spawns this detached; not meant to be
  /// invoked directly.
  #[command(hide = true)]
  Host(SessionHostArgs),

  /// Attach to a live session: connect and print its current snapshot.
  Attach(SessionTargetArgs),

  /// List all live sessions discoverable in the registry.
  List(SessionListArgs),

  /// Run a single verb against a live session and print the result.
  Exec(SessionExecArgs),

  /// Close a session: prune its registry entry (and stop its server if this
  /// process owns it).
  Close(SessionTargetArgs),

  /// Close every live session.
  CloseAll,
}

#[derive(Args)]
pub struct SessionOpenArgs {
  /// Session id to publish the browser under.
  pub id: String,

  /// URL to open in the session's first page (defaults to `about:blank`).
  pub url: Option<String>,

  #[command(flatten)]
  pub browser: BrowserArgs,
}

#[derive(Args)]
pub struct SessionHostArgs {
  /// Session id to publish the browser under.
  pub id: String,

  /// URL to open in the session's first page.
  pub url: Option<String>,

  #[command(flatten)]
  pub browser: BrowserArgs,
}

#[derive(Args)]
pub struct SessionTargetArgs {
  /// Session id.
  pub id: String,
}

#[derive(Args)]
pub struct SessionListArgs {
  /// Emit JSON instead of a human-readable table.
  #[arg(long)]
  pub json: bool,
}

#[derive(Args)]
pub struct SessionExecArgs {
  /// Session id.
  pub id: String,

  /// Verb to run (snapshot, goto, click, fill, press, hover, eval,
  /// screenshot, title, url, run-script, ...).
  pub verb: String,

  /// Browser context within the session (the `:context` half of a session
  /// key). Defaults to the session's default context.
  #[arg(long)]
  pub context: Option<String>,

  /// CSS selector for element verbs (click / fill / hover / press).
  #[arg(long)]
  pub selector: Option<String>,

  /// Ref from the last snapshot for element verbs (alternative to selector).
  #[arg(long = "ref")]
  pub r#ref: Option<String>,

  /// Value for `fill`.
  #[arg(long)]
  pub value: Option<String>,

  /// Key for `press` (e.g. `Enter`, `Control+a`).
  #[arg(long)]
  pub key: Option<String>,

  /// URL for `goto`.
  #[arg(long)]
  pub url: Option<String>,

  /// Expression for `eval`.
  #[arg(long)]
  pub expression: Option<String>,

  /// Script source for `run-script` (or `-` to read from stdin).
  #[arg(long)]
  pub source: Option<String>,

  /// Write binary results (screenshot) to this file instead of printing a
  /// base64 blob.
  #[arg(long)]
  pub output: Option<PathBuf>,
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

// Independent bool flags from `clap` parse — grouping into enums adds
// no value; each flag has its own --foo.
#[allow(clippy::struct_excessive_bools)]
#[derive(Args)]
pub struct BddArgs {
  /// Feature file globs. Overrides `[bdd].features` from config.
  pub features: Vec<String>,

  /// Tag filter expression, e.g. `@smoke and not @wip`.
  #[arg(long)]
  pub tags: Option<String>,

  /// Run only on these `[test.projects]` entries (repeatable). Without
  /// this flag the suite runs on every configured project, or on the
  /// single `[test.browser]` when no projects are configured.
  #[arg(long)]
  pub project: Vec<String>,

  /// Parse and report scenarios without executing steps.
  #[arg(long)]
  pub dry_run: bool,

  /// Watch mode: re-run scenarios on file changes with an interactive
  /// terminal TUI (falls back to plain re-runs without a TTY). Feature
  /// edits re-run just the changed files; step/source edits re-run
  /// everything with a freshly bundled step graph.
  #[arg(long)]
  pub watch: bool,

  /// UI mode: serve a localhost web app that lists scenarios, streams
  /// live results, and re-runs on file changes or in-app commands.
  /// Traces are recorded for every test so the app can link into the
  /// Playwright trace viewer. Wins over --watch when both are passed.
  #[arg(long)]
  pub ui: bool,

  /// Port for the --ui server (defaults to an ephemeral free port;
  /// the chosen URL is printed on startup).
  #[arg(long, requires = "ui")]
  pub ui_port: Option<u16>,

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

  /// Shard the scenarios across CI machines, `X/N` (e.g. `2/4` runs the
  /// second of four shards).
  #[arg(long)]
  pub shard: Option<String>,

  /// Reporter name, repeatable (e.g. `--reporter terminal --reporter junit`).
  /// Each name is matched exactly; file reporters write into the run's output
  /// directory. Set paths/options with `[[test.reporter]]` in the config file.
  #[arg(long)]
  pub reporter: Vec<String>,

  /// JavaScript step-definition file globs, e.g.
  /// `--steps 'steps/**/*.js'`. May be repeated. Overrides
  /// `[test].steps` from config. Defaults to `steps/**/*.js` and
  /// `step_definitions/**/*.js` when omitted.
  #[arg(long)]
  pub steps: Vec<String>,

  /// Cucumber world parameters as a JSON object, exposed to every
  /// scenario as `this.parameters`. Overrides `[test].worldParameters`.
  #[arg(long)]
  pub world_parameters: Option<String>,

  #[command(flatten)]
  pub browser: BrowserArgs,
}

// ── test subcommand ─────────────────────────────────────────────────────

#[allow(clippy::struct_excessive_bools)]
#[derive(Args)]
pub struct TestRunArgs {
  /// Test file paths or globs. Overrides `[test].testMatch` from config.
  pub files: Vec<String>,

  /// Only run tests whose full title matches this substring/regex.
  #[arg(long, short = 'g')]
  pub grep: Option<String>,

  /// Skip tests whose full title matches this substring/regex.
  #[arg(long)]
  pub grep_invert: Option<String>,

  /// Only run tests carrying this tag (from `test(title, { tag })`).
  #[arg(long)]
  pub tag: Option<String>,

  /// Number of parallel workers.
  #[arg(long)]
  pub workers: Option<usize>,

  /// Retry count for failing tests.
  #[arg(long)]
  pub retries: Option<u32>,

  /// Per-test timeout in milliseconds.
  #[arg(long)]
  pub timeout: Option<u64>,

  /// Reporter name, repeatable (e.g. `--reporter list --reporter junit`).
  #[arg(long)]
  pub reporter: Vec<String>,

  /// Shard the tests across CI machines, `X/N`.
  #[arg(long)]
  pub shard: Option<String>,

  /// Run only the tests of these projects (repeatable).
  #[arg(long)]
  pub project: Vec<String>,

  /// Watch mode: re-run on file changes.
  #[arg(long)]
  pub watch: bool,

  /// UI mode: serve a localhost web app that lists tests, streams live
  /// results, and re-runs on file changes or in-app commands.
  #[arg(long)]
  pub ui: bool,

  /// Port for the --ui server (defaults to an ephemeral free port).
  #[arg(long, requires = "ui")]
  pub ui_port: Option<u16>,

  /// Only re-run the tests that failed in the previous run.
  #[arg(long)]
  pub last_failed: bool,

  /// Only run test files changed since the given git ref (default HEAD).
  #[arg(long, num_args = 0..=1, default_missing_value = "HEAD")]
  pub only_changed: Option<String>,

  /// Stop after the first failing test.
  #[arg(long)]
  pub fail_fast: bool,

  /// Stop after this many failures.
  #[arg(long)]
  pub max_failures: Option<u32>,

  /// Run each test N times.
  #[arg(long)]
  pub repeat_each: Option<u32>,

  /// Fail the run when `test.only` is present (CI guard).
  #[arg(long)]
  pub forbid_only: bool,

  /// List discovered tests without running them.
  #[arg(long)]
  pub list: bool,

  /// Serve an extra import specifier from a native module, repeatable:
  /// `--module-alias @playwright/test=@ferridriver/test`. Merged on top
  /// of `[test].moduleAliases`; lets a suite written against another
  /// runner run byte-for-byte unmodified.
  #[arg(long = "module-alias", value_name = "SPECIFIER=NATIVE_MODULE")]
  pub module_alias: Vec<String>,

  #[command(flatten)]
  pub browser: BrowserArgs,
}

#[derive(Args)]
pub struct RustTestArgs {
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

  /// Run ferritest harness binaries headless (exported as
  /// `FERRITEST_HEADLESS`; non-harness test binaries ignore it).
  #[arg(long)]
  pub headless: bool,

  /// Browser backend for ferritest harness binaries (`cdp-pipe`,
  /// `cdp-raw`, `bidi`, `webkit`; exported as `FERRITEST_BACKEND`).
  #[arg(long)]
  pub backend: Option<String>,

  /// Worker count for ferritest harness binaries (exported as
  /// `FERRITEST_WORKERS`).
  #[arg(long)]
  pub workers: Option<usize>,

  /// Test-title filter for ferritest harness binaries (exported as
  /// `FERRITEST_GREP`).
  #[arg(long, short = 'g')]
  pub grep: Option<String>,

  /// Tag filter for ferritest harness binaries (exported as
  /// `FERRITEST_TAG`).
  #[arg(long)]
  pub tag: Option<String>,

  /// Retry count for ferritest harness binaries (exported as
  /// `FERRITEST_RETRIES`).
  #[arg(long)]
  pub retries: Option<u32>,

  /// Watch mode: run the tests, then re-run the same command whenever a
  /// `.rs` file changes (`testIgnore` patterns from the config are
  /// excluded). A change arriving mid-run queues one re-run after the
  /// current cycle finishes.
  #[arg(long, conflicts_with = "ui")]
  pub watch: bool,

  /// UI mode: serve a localhost web app that lists ferritest harness
  /// tests, streams live results, and refreshes on file changes or
  /// in-app commands. Each cycle respawns `cargo test` (nextest cannot
  /// enumerate ferritest harness binaries); harness binaries stream
  /// events back over a unix socket. Filtering happens in-app, so
  /// --grep and positional filters conflict with this flag.
  #[arg(long, conflicts_with_all = ["filter", "grep", "passthrough"])]
  pub ui: bool,

  /// Port for the --ui server (defaults to an ephemeral free port;
  /// the chosen URL is printed on startup).
  #[arg(long, requires = "ui")]
  pub ui_port: Option<u16>,

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
  /// Script file, or `-` to read source from stdin. Omit when using
  /// `--eval`. A `.ts`/`.tsx` file, or any source with top-level
  /// `import`/`export`, is rolldown-bundled + transpiled + run as an ES
  /// module (its `default` export is the result). Plain `.js` scripts run
  /// as before, where top-level `return <value>` is the result.
  pub script: Option<String>,

  /// Inline script source (alternative to a file / stdin). Treated as an
  /// ES module when it contains top-level `import`/`export`.
  #[arg(short = 'e', long = "eval", conflicts_with = "script")]
  pub eval: Option<String>,

  /// Per-script wall-clock timeout in milliseconds.
  #[arg(long)]
  pub timeout_ms: Option<u64>,

  /// Print one machine-readable JSON result document on stdout, with every
  /// `console.*` call buffered inside its `console` array, and stream
  /// nothing while the script runs. This is the mode to parse.
  ///
  /// Without this flag -- terminal or pipe alike -- console output streams
  /// as it happens on the stream Node uses (`log`/`info`/`debug` to stdout,
  /// `warn`/`error`/`trace` to stderr), followed by the script's return
  /// value on stdout.
  #[arg(long)]
  pub json: bool,

  /// Log every Playwright-level action (`page.*`, `locator.*`, `expect.*`)
  /// to stderr as it starts and finishes, with its parameters, call log and
  /// duration. Independent of `context.tracing.start()` — nothing is
  /// recorded to a trace zip.
  #[arg(long)]
  pub trace: bool,

  /// Extension file(s), directory(ies), or ESM package specifiers to
  /// load, exposing their `tool` registrations to scripts as `tools.*`.
  /// Repeatable; merged with the `extensions` list from `ferridriver.toml`.
  #[arg(long = "extension")]
  pub extensions: Vec<String>,

  /// Positional args exposed to the script as the `args` global
  /// (strings). Pass after `--`.
  #[arg(last = true)]
  pub script_args: Vec<String>,
}

// ── install subcommand ──────────────────────────────────────────────────

#[derive(Args)]
pub struct InstallArgs {
  /// Browsers to install: `chromium`, `chromium-headless-shell`,
  /// `firefox`, `webkit`. Defaults to `chromium` when omitted.
  pub browsers: Vec<String>,

  /// Also install required system libraries (Linux only; uses the
  /// platform package manager and may require sudo).
  #[arg(long)]
  pub with_deps: bool,
}

// ── codegen subcommand ──────────────────────────────────────────────────

#[derive(Args)]
pub struct CodegenArgs {
  /// URL to open in the codegen browser.
  pub url: Option<String>,

  /// Output file for generated test code.
  #[arg(short, long)]
  pub output: Option<PathBuf>,

  /// Output language: `ts` (runnable script, default), `rust`
  /// (`#[ferritest]`), or `gherkin` (`.feature`).
  #[arg(long, default_value = "ts")]
  pub language: String,

  #[command(flatten)]
  pub browser: BrowserArgs,
}

// ── Shared browser / transport args ─────────────────────────────────────

/// Browser backend and connection options.
#[derive(Args, Clone)]
pub struct BrowserArgs {
  /// Browser backend to use. Unset means "whatever the config says",
  /// falling back to `cdp-pipe`; there is deliberately no clap default,
  /// because a default is indistinguishable from an explicit choice and
  /// the config could then never be overridden on the command line.
  #[arg(long)]
  pub backend: Option<Backend>,

  /// Run the browser without a visible window. Off by default because
  /// MCP's canonical use case is an interactive debugging / agent
  /// session where the user wants to watch the browser.
  #[arg(long, overrides_with = "headed")]
  pub headless: bool,

  /// Force a visible window, overriding `headless = true` in the config.
  #[arg(long, overrides_with = "headless")]
  pub headed: bool,

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
}

/// Apply CLI-over-config precedence for the browser flags.
pub fn effective_browser(args: &BrowserArgs, mcp: &ferridriver_config::mcp::McpConfig) -> EffectiveBrowser {
  let cli_backend = args.backend_kind();
  let cli_headless = args.headless_override();
  EffectiveBrowser {
    backend: cli_backend.unwrap_or_else(|| mcp.backend_kind()),
    headless: cli_headless.unwrap_or_else(|| mcp.headless()),
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
