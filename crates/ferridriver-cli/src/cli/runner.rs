//! The flags `test` and `bdd` share.
//!
//! Both drive the same `TestRunner` through the same `CliOverrides`, so the
//! selection, parallelism, sharding and reporting flags are one set of flags
//! with one meaning. They were declared twice, and drifted: the reporter list
//! was copied verbatim into both, `--shard` documented `X/N` in one place and
//! `X/N (e.g. 2/4 …)` in the other. Declared once, they cannot drift, and
//! `--help` groups them under one heading instead of scattering them through
//! thirty flat options.

use clap::Args;

/// Selection, parallelism and reporting for a suite run.
#[allow(clippy::struct_excessive_bools)]
#[derive(Args)]
pub struct RunnerArgs {
  /// Run only these `[test.projects]` entries (repeatable). Without this
  /// flag the suite runs on every configured project, or on the single
  /// `[test.browser]` when no projects are configured.
  #[arg(long, help_heading = "Run")]
  pub project: Vec<String>,

  /// Number of parallel workers.
  #[arg(long, help_heading = "Run")]
  pub workers: Option<usize>,

  /// Shard across CI machines, `X/N` (e.g. `2/4` runs the second of four
  /// shards).
  #[arg(long, help_heading = "Run")]
  pub shard: Option<String>,

  /// Stop after the first failure.
  #[arg(long, help_heading = "Run")]
  pub fail_fast: bool,

  /// Reporter name, repeatable (e.g. `--reporter line --reporter junit`).
  ///
  /// Terminal: `list` (default), `line`, `dot`, `progress`, `github`,
  /// `tap`, `tap-flat`, `teamcity`, `usage`, `null`.
  /// Files: `json`, `junit`, `html`, `blob`, `markdown`, `ctrf`,
  /// `allure`, `rerun`, `cucumber-json`, `messages`.
  ///
  /// File reporters write into the run's output directory; set paths and
  /// per-reporter options with `[[test.reporter]]` in the config file, or
  /// point one at a path with `FERRIDRIVER_<NAME>_OUTPUT_FILE`.
  #[arg(long, help_heading = "Reporting")]
  pub reporter: Vec<String>,

  /// Watch mode: re-run on file changes, with an interactive terminal TUI
  /// where there is a TTY and plain re-runs where there is not.
  #[arg(long, help_heading = "Run")]
  pub watch: bool,

  /// UI mode: serve a localhost web app that lists what will run, streams
  /// live results, and re-runs on file changes or in-app commands. Traces
  /// are recorded for every test so the app can link into the trace
  /// viewer. Wins over --watch when both are passed.
  #[arg(long, help_heading = "Run")]
  pub ui: bool,

  /// Port for the --ui server (defaults to an ephemeral free port; the
  /// chosen URL is printed on startup).
  #[arg(long, requires = "ui", help_heading = "Run")]
  pub ui_port: Option<u16>,

  /// Stop the run and publish its live browser as a session, so a script
  /// can drive exactly the state it is in:
  ///
  ///   ferridriver run --session <id> --context <ctx> --eval "…"
  ///
  /// `--debug` stops in front of the first API call and steps from there
  /// (`testDebug.stepOver()`, `testDebug.pauseAt('spec.ts:42')`,
  /// `testDebug.resume()`). `--debug=fail` instead stops at the first
  /// failure, before teardown, with the page still on it.
  ///
  /// Forces one worker and stops after one failure: a parked worker beside
  /// running ones makes the output unreadable and the browser contended.
  #[arg(long, value_name = "WHERE", num_args = 0..=1, default_missing_value = "start", help_heading = "Run")]
  pub debug: Option<ferridriver_test::debug::DebugMode>,
}

impl RunnerArgs {
  /// Parse `--shard` into the runner's own type.
  ///
  /// # Errors
  /// When the spec is not `X/N` with `1 <= X <= N`.
  pub fn shard(&self) -> anyhow::Result<Option<ferridriver_test::config::ShardArg>> {
    self
      .shard
      .as_deref()
      .map(|spec| {
        ferridriver_test::config::ShardArg::parse(spec).map_err(|e| anyhow::anyhow!("invalid --shard `{spec}`: {e}"))
      })
      .transpose()
  }
}
