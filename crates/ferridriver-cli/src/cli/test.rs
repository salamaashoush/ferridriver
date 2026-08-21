//! `ferridriver test` and `ferridriver rust-test` arguments.

use clap::{Args, ValueEnum};

use super::browser::BrowserArgs;
use super::runner::RunnerArgs;

#[allow(clippy::struct_excessive_bools)]
#[derive(Args)]
pub struct TestRunArgs {
  /// Test file paths or globs. Overrides `[test].testMatch` from config.
  pub files: Vec<String>,

  /// Only run tests whose full title matches this substring/regex.
  #[arg(long, short = 'g', help_heading = "Selection")]
  pub grep: Option<String>,

  /// Skip tests whose full title matches this substring/regex.
  #[arg(long, help_heading = "Selection")]
  pub grep_invert: Option<String>,

  /// Only run tests carrying this tag (from `test(title, { tag })`).
  #[arg(long, help_heading = "Selection")]
  pub tag: Option<String>,

  /// Only re-run the tests that failed in the previous run.
  #[arg(long, help_heading = "Selection")]
  pub last_failed: bool,

  /// Only run test files changed since the given git ref (default HEAD).
  #[arg(long, num_args = 0..=1, default_missing_value = "HEAD", help_heading = "Selection")]
  pub only_changed: Option<String>,

  /// List discovered tests without running them.
  #[arg(long, help_heading = "Selection")]
  pub list: bool,

  /// Retry count for failing tests.
  #[arg(long, help_heading = "Run")]
  pub retries: Option<u32>,

  /// Per-test timeout in milliseconds.
  #[arg(long, help_heading = "Run")]
  pub timeout: Option<u64>,

  /// Stop after this many failures.
  #[arg(long, help_heading = "Run")]
  pub max_failures: Option<u32>,

  /// Run each test N times.
  #[arg(long, help_heading = "Run")]
  pub repeat_each: Option<u32>,

  /// Fail the run when `test.only` is present (CI guard).
  #[arg(long, help_heading = "Run")]
  pub forbid_only: bool,

  /// Serve an extra import specifier from a native module, repeatable:
  /// `--module-alias @playwright/test=@ferridriver/test`. Merged on top
  /// of `[test].moduleAliases`; lets a suite written against another
  /// runner run byte-for-byte unmodified.
  #[arg(long = "module-alias", value_name = "SPECIFIER=NATIVE_MODULE", help_heading = "Run")]
  pub module_alias: Vec<String>,

  #[command(flatten)]
  pub runner: RunnerArgs,

  #[command(flatten)]
  pub browser: BrowserArgs,
}

#[derive(Args)]
pub struct RustTestArgs {
  /// Test name filter passed through to the underlying runner.
  pub filter: Option<String>,

  /// Cargo package filter (`-p <name>`). May be repeated.
  #[arg(short = 'p', long = "package", help_heading = "Selection")]
  pub packages: Vec<String>,

  /// Force a specific runner backend regardless of config.
  #[arg(long, value_enum, help_heading = "Run")]
  pub runner: Option<TestRunner>,

  /// nextest profile name.
  #[arg(long, help_heading = "Run")]
  pub profile: Option<String>,

  /// Run ferritest harness binaries headless (exported as
  /// `FERRITEST_HEADLESS`; non-harness test binaries ignore it).
  #[arg(long, help_heading = "Harness")]
  pub headless: bool,

  /// Browser backend for ferritest harness binaries (`cdp-pipe`,
  /// `cdp-raw`, `bidi`, `webkit`; exported as `FERRITEST_BACKEND`).
  #[arg(long, help_heading = "Harness")]
  pub backend: Option<String>,

  /// Worker count for ferritest harness binaries (exported as
  /// `FERRITEST_WORKERS`).
  #[arg(long, help_heading = "Harness")]
  pub workers: Option<usize>,

  /// Test-title filter for ferritest harness binaries (exported as
  /// `FERRITEST_GREP`).
  #[arg(long, short = 'g', help_heading = "Harness")]
  pub grep: Option<String>,

  /// Tag filter for ferritest harness binaries (exported as
  /// `FERRITEST_TAG`).
  #[arg(long, help_heading = "Harness")]
  pub tag: Option<String>,

  /// Retry count for ferritest harness binaries (exported as
  /// `FERRITEST_RETRIES`).
  #[arg(long, help_heading = "Harness")]
  pub retries: Option<u32>,

  /// Watch mode: run the tests, then re-run the same command whenever a
  /// `.rs` file changes (`testIgnore` patterns from the config are
  /// excluded). A change arriving mid-run queues one re-run after the
  /// current cycle finishes.
  #[arg(long, conflicts_with = "ui", help_heading = "Run")]
  pub watch: bool,

  /// UI mode: serve a localhost web app that lists ferritest harness
  /// tests, streams live results, and refreshes on file changes or
  /// in-app commands. Each cycle respawns `cargo test` (nextest cannot
  /// enumerate ferritest harness binaries); harness binaries stream
  /// events back over a unix socket. Filtering happens in-app, so
  /// --grep and positional filters conflict with this flag.
  #[arg(long, conflicts_with_all = ["filter", "grep", "passthrough"], help_heading = "Run")]
  pub ui: bool,

  /// Port for the --ui server (defaults to an ephemeral free port;
  /// the chosen URL is printed on startup).
  #[arg(long, requires = "ui", help_heading = "Run")]
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

impl TestRunner {
  #[must_use]
  pub fn name(self) -> &'static str {
    match self {
      Self::Nextest => "nextest",
      Self::Cargo => "cargo",
    }
  }
}
