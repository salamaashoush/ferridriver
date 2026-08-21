//! Command-line surface.
//!
//! One module per command group, mirroring `commands/`: the arguments a
//! command takes live next to nothing else, so adding a flag means opening one
//! small file rather than scrolling a thousand-line enum.
//!
//! Two conventions hold across every command here, because a CLI that answers
//! `--help` differently per command is a CLI nobody learns:
//!
//! * options are grouped with `help_heading`, and the process-wide ones sit
//!   under `Global` at the bottom rather than interleaved with the command's
//!   own;
//! * every command carries `after_help` examples, which is what people
//!   actually read.

mod bdd;
mod browser;
mod config;
mod ext;
mod run;
mod runner;
mod session;
mod test;
mod tools;
mod trace;

pub use bdd::BddArgs;
pub use browser::{BrowserArgs, EffectiveBrowser, Transport, effective_browser};
pub use config::{ConfigArgs, DoctorArgs};
pub use ext::{ExtArgs, ExtCheckArgs, ExtCommand, ExtTypesArgs};
pub use run::RunArgs;
pub use runner::RunnerArgs;
pub use session::{SessionArgs, SessionCommand, SessionHostArgs, SessionListArgs, SessionOpenArgs, SessionTargetArgs};
pub use test::{RustTestArgs, TestRunArgs, TestRunner};
pub use tools::{CodegenArgs, CompletionsArgs, InitArgs, InstallArgs, McpArgs, MergeReportsArgs, UpgradeArgs};
pub use trace::{TraceArgs, TraceCommand, TraceLsArgs, TraceSection, TraceShowArgs, TraceViewArgs};

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

use crate::ui;

#[derive(Parser)]
#[command(
  name = "ferridriver",
  about = "Rust-based browser automation: MCP server, test runner, BDD runner, script runner",
  long_about = "ferridriver drives Chromium, Firefox and WebKit behind one Playwright-shaped API.\n\
    The same binary serves an MCP server for coding agents, runs TypeScript and Gherkin suites\n\
    natively without Node, executes one-off scripts, and reads the traces any of them recorded.",
  // `-V` is the short answer — the version a canary carries already names
  // its commit — while `--version` adds the channel and the target, so a bug
  // report names an artifact rather than a number anyone could be running.
  version = crate::build_info::VERSION,
  long_version = crate::build_info::LONG_VERSION,
  propagate_version = true,
  arg_required_else_help = true,
  // Wrap long help against a readable measure rather than the full width of
  // a maximised terminal, where a 200-column paragraph is unreadable.
  max_term_width = 100,
  after_help = "Examples:\n  \
    ferridriver init                       scaffold a project here\n  \
    ferridriver install chromium           download a browser\n  \
    ferridriver test tests/login.spec.ts   run one spec\n  \
    ferridriver run -e \"await page.goto('https://example.com')\" --instance dev\n  \
    ferridriver doctor                     check the setup end to end\n\n\
    Run `ferridriver <command> --help` for that command's options and examples."
)]
pub struct Cli {
  /// Verbose output (-v = debug, -vv = trace including CDP protocol)
  #[arg(short, long, action = clap::ArgAction::Count, global = true, help_heading = "Global")]
  pub verbose: u8,

  /// Warnings, errors and results only — no progress narration.
  #[arg(
    long,
    short = 'q',
    global = true,
    conflicts_with = "verbose",
    help_heading = "Global"
  )]
  pub quiet: bool,

  /// Output format for commands that produce a document. `json` also
  /// silences narration and renders failures as JSON, so a program reads
  /// one shape whether the run succeeded or not.
  ///
  /// A suite run's document is its report, not this flag: use `--reporter
  /// json` (or `junit`, `html`, `blob`) for `test` and `bdd`.
  #[arg(long, global = true, value_enum, default_value = "human", help_heading = "Global")]
  pub format: FormatArg,

  /// Shorthand for `--format json`.
  #[arg(long, global = true, conflicts_with = "format", help_heading = "Global")]
  pub json: bool,

  /// When to colour output. `auto` (the default) drops colour when the
  /// output is redirected, when `NO_COLOR` is set, and when a coding agent
  /// rather than a person is reading it.
  #[arg(
    long,
    global = true,
    value_enum,
    default_value = "auto",
    value_name = "WHEN",
    help_heading = "Global"
  )]
  pub color: ColorArg,

  /// Config file path, applied on top of the discovered config layers
  /// (machine, user, repository, cwd). Format inferred from the
  /// extension.
  #[arg(short, long, global = true, value_name = "PATH", help_heading = "Global")]
  pub config: Option<PathBuf>,

  /// Ignore every config layer except `--config` (or, without it, the
  /// config in the current directory). For reproducible runs that must
  /// not pick up machine-, user- or repository-level settings. Also
  /// settable as `FERRIDRIVER_NO_INHERIT=1` (read by the loader, so it
  /// applies to child processes too).
  #[arg(long, global = true, help_heading = "Global")]
  pub no_inherit: bool,

  #[command(subcommand)]
  pub command: Command,
}

/// `--format`, mapped onto [`ui::Format`] so the presentation layer owns the
/// meaning and clap only owns the spelling.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FormatArg {
  Human,
  Json,
}

/// `--color`, mapped onto [`ui::ColorChoice`].
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorArg {
  Auto,
  Always,
  Never,
}

impl Cli {
  /// The presentation policy these flags describe.
  #[must_use]
  pub fn presentation(&self) -> (ui::ColorChoice, ui::Format, bool) {
    let color = match self.color {
      ColorArg::Auto => ui::ColorChoice::Auto,
      ColorArg::Always => ui::ColorChoice::Always,
      ColorArg::Never => ui::ColorChoice::Never,
    };
    let format = match (self.json, self.format) {
      (true, _) | (_, FormatArg::Json) => ui::Format::Json,
      (false, FormatArg::Human) => ui::Format::Human,
    };
    (color, format, self.quiet)
  }
}

#[derive(Subcommand)]
pub enum Command {
  /// Scaffold a project here: a config, a first spec, and the type
  /// declarations an editor needs.
  #[command(after_help = "Examples:\n  \
    ferridriver init\n  \
    ferridriver init --bdd                  also scaffold features/ and steps/\n  \
    ferridriver init --config-format yaml\n  \
    ferridriver init --force                overwrite what is already there")]
  Init(InitArgs),

  /// Run the MCP server.
  #[command(after_help = "Examples:\n  \
    ferridriver mcp                          stdio, for a coding agent\n  \
    ferridriver mcp --transport http --port 8080\n  \
    ferridriver mcp --backend webkit --headed")]
  Mcp(McpArgs),

  /// Run TypeScript/JavaScript test files (`*.test.ts`, `*.spec.ts`).
  ///
  /// Playwright-shaped `test` / `describe` / `expect` from
  /// `@ferridriver/test`, executed in the embedded `QuickJS` engine. No
  /// Node required.
  #[command(
    visible_alias = "t",
    after_help = "Examples:\n  \
      ferridriver test\n  \
      ferridriver test tests/login.spec.ts -g \"signs in\"\n  \
      ferridriver test --project webkit --workers 4\n  \
      ferridriver test --ui                     browse and re-run in a web app\n  \
      ferridriver test --last-failed --debug    step through what broke\n  \
      ferridriver test --shard 2/4 --reporter blob   one CI shard"
  )]
  Test(TestRunArgs),

  /// Run BDD/Cucumber feature files through the same test runner.
  #[command(
    visible_alias = "b",
    after_help = "Examples:\n  \
      ferridriver bdd\n  \
      ferridriver bdd tests/features/ --tags \"@smoke and not @wip\"\n  \
      ferridriver bdd --steps 'tests/steps/**/*.ts' --dry-run\n  \
      ferridriver bdd --ui"
  )]
  Bdd(BddArgs),

  /// Run cargo unit/integration tests via nextest (or cargo test).
  #[command(
    visible_alias = "rt",
    after_help = "Examples:\n  \
      ferridriver rust-test\n  \
      ferridriver rust-test -p ferridriver --backend webkit\n  \
      ferridriver rust-test --watch\n  \
      ferridriver rust-test -- --nocapture       pass through to the runner"
  )]
  RustTest(RustTestArgs),

  /// Execute a JS/TS script with Playwright-style bindings.
  ///
  /// Without `--instance` or `--session` the script owns its own browser
  /// via `chromium()` / `firefox()` / `webkit()`, so a script that never
  /// opens one costs nothing.
  #[command(
    visible_alias = "r",
    after_help = "Examples:\n  \
      ferridriver run script.ts\n  \
      ferridriver run -e \"return (await page.title())\" --session dev\n  \
      ferridriver run script.ts --instance staging --headed --trace\n  \
      ferridriver run script.ts --code rust --code-out tests/e2e.rs\n  \
      echo \"…\" | ferridriver run - --format json"
  )]
  Run(RunArgs),

  /// Download browser binaries into the local cache.
  #[command(after_help = "Examples:\n  \
    ferridriver install                      chromium\n  \
    ferridriver install firefox webkit\n  \
    ferridriver install --with-deps chromium   also the system libraries (Linux)")]
  Install(InstallArgs),

  /// Record interactions in a browser and emit them as a test.
  #[command(after_help = "Examples:\n  \
    ferridriver codegen https://example.com\n  \
    ferridriver codegen https://example.com --language gherkin -o login.feature")]
  Codegen(CodegenArgs),

  /// Manage and drive named browser sessions (open / attach / list / close).
  #[command(
    visible_alias = "s",
    after_help = "Examples:\n  \
      ferridriver session open dev https://example.com\n  \
      ferridriver session list\n  \
      ferridriver run -e \"await page.click('#go')\" --session dev\n  \
      ferridriver session close dev"
  )]
  Session(SessionArgs),

  /// Show the resolved configuration: which files layered, what each key
  /// resolved to, and where that value came from.
  #[command(
    visible_alias = "cfg",
    after_help = "Examples:\n  \
      ferridriver config\n  \
      ferridriver config --resolved --format json\n  \
      ferridriver config --backend webkit    what a run with this flag would see"
  )]
  Config(ConfigArgs),

  /// Check that this setup will actually work: config found, extensions
  /// loadable, instance commands runnable, browsers installed.
  #[command(after_help = "Examples:\n  \
    ferridriver doctor\n  \
    ferridriver doctor --instances     also run each instance's commands\n  \
    ferridriver doctor --format json")]
  Doctor(DoctorArgs),

  /// Author extensions: load them and report what they register.
  #[command(after_help = "Examples:\n  \
    ferridriver ext check\n  \
    ferridriver ext dev ./my-extension     re-check on every save\n  \
    ferridriver ext types                  write the .d.ts an editor needs")]
  Ext(ExtArgs),

  /// Read recorded traces: open one in the viewer, print one in the
  /// terminal, or list what a run left behind.
  #[command(after_help = "Examples:\n  \
    ferridriver trace ls\n  \
    ferridriver trace view                 the newest trace\n  \
    ferridriver trace show --errors        just what failed, as text")]
  Trace(TraceArgs),

  /// Merge the `blob` reports of several shards into one report.
  ///
  /// Each shard runs with `--reporter blob`, writing a `report-N.zip`;
  /// this replays every event in those blobs through the reporters you
  /// name, producing the single HTML / `JUnit` / JSON report the run would
  /// have produced unsharded.
  #[command(
    name = "merge-reports",
    after_help = "Examples:\n  \
      ferridriver merge-reports blob-report/\n  \
      ferridriver merge-reports shard-*/report.zip --reporter html --reporter junit"
  )]
  MergeReports(MergeReportsArgs),

  /// Replace this binary with the newest release.
  #[command(after_help = "Examples:\n  \
    ferridriver upgrade                 the newest release on your channel\n  \
    ferridriver upgrade --check         report what is available, change nothing\n  \
    ferridriver upgrade --canary        follow the builds from every push to main\n  \
    ferridriver upgrade --stable        move a canary build back onto releases\n  \
    ferridriver upgrade --tag v0.4.0    install one exact release")]
  Upgrade(UpgradeArgs),

  /// Generate a shell completion script.
  #[command(after_help = "Examples:\n  \
    ferridriver completions zsh > ~/.zfunc/_ferridriver\n  \
    ferridriver completions bash >> ~/.bashrc\n  \
    ferridriver completions fish > ~/.config/fish/completions/ferridriver.fish")]
  Completions(CompletionsArgs),
}
