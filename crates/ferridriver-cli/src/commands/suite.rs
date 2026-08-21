//! The half of `test` and `bdd` that is the same command.
//!
//! Both lower their flags into one `CliOverrides` and drive one `TestRunner`.
//! Written out twice, the two copies drifted — the `--shard` error read
//! differently in each, and the reporter list was maintained in two places —
//! so the shared steps live here and each command contributes only what is
//! actually its own.

use ferridriver_config::FerridriverConfig;
use ferridriver_script::ScriptCaps;
use ferridriver_test::config::{CliOverrides, TestConfig};

use crate::cli;
use crate::commands::{bootstrap, script_setup};

/// The capabilities a suite's script VM runs under: the `[scripting]` env
/// allow-list, the declared commands, and the extension policy. Resolved the
/// same way for the MCP server and `ferridriver run`, because a step body and
/// a script are the same engine.
#[must_use]
pub fn caps(config: &FerridriverConfig) -> ScriptCaps {
  ScriptCaps::resolve_with_commands(&config.scripting.allow_env, config.scripting.allow.commands.clone())
    .with_extension_policy(config.extensions.policy())
    .with_extension_settings(config.extensions.settings())
}

/// Fold the flags every suite shares — selection, parallelism, reporting, and
/// the browser — into the overrides the runner reads.
///
/// # Errors
/// When `--shard` is not a valid `X/N`.
pub fn apply_shared(
  overrides: &mut CliOverrides,
  runner: &cli::RunnerArgs,
  browser: &cli::BrowserArgs,
) -> anyhow::Result<()> {
  overrides.project_filter.clone_from(&runner.project);
  overrides.workers = runner.workers.map(|n| u32::try_from(n).unwrap_or(u32::MAX));
  overrides.reporter.clone_from(&runner.reporter);
  overrides.watch = runner.watch;
  overrides.ui = runner.ui;
  overrides.ui_port = runner.ui_port;
  overrides.shard = runner.shard()?;
  overrides.headless_override = browser.headless_override();
  overrides.backend = browser.backend_name().map(str::to_string);
  overrides.executable_path.clone_from(&browser.executable_path);
  Ok(())
}

/// Everything between "the overrides are filled in" and "the runner may
/// start": the debug session, the merged config, the module aliases and the
/// JavaScript reporters.
///
/// `--debug` is resolved here rather than by the caller because it has to see
/// `config.test` before [`ferridriver_test::config::resolve_config_from`]
/// consumes it.
///
/// # Errors
/// When the debug session cannot be built, the merged config is invalid, a
/// module alias is malformed, or a configured JS reporter fails to compile.
pub async fn resolve(
  config: FerridriverConfig,
  caps: &ScriptCaps,
  debug: Option<ferridriver_test::debug::DebugMode>,
  overrides: &mut CliOverrides,
) -> anyhow::Result<TestConfig> {
  overrides.extensions = config.extension_specs();
  if let Some(mode) = debug {
    let setup = script_setup::resolve(&config, &std::env::current_dir()?, &[]).await?;
    ferridriver_script::debug_session::install(mode, setup.into_session_script(), overrides);
  }
  let test_config = ferridriver_test::config::resolve_config_from(config.test, overrides)
    .map_err(|e| anyhow::anyhow!("config error: {e}"))?;
  bootstrap::install_module_aliases(&test_config, &[])?;
  bootstrap::install_js_reporters(&test_config, caps).await?;
  Ok(test_config)
}

/// Hand a runner's exit status to the process.
///
/// Exiting rather than returning keeps the runner's own code — a suite that
/// failed exits 1, a suite that could not start exits with whatever the
/// runner chose — but `exit` runs no destructors, so a piped stdout has to be
/// flushed here or the last of the report is lost on exactly the failing runs
/// someone wanted the log of.
pub fn finish(exit_code: i32) {
  use std::io::Write as _;
  if exit_code == 0 {
    return;
  }
  let _ = std::io::stdout().flush();
  let _ = std::io::stderr().flush();
  std::process::exit(exit_code);
}
