//! ferridriver — single-binary browser automation.
//!
//! This file does three things and nothing else: resolve the configuration
//! layer stack, install the presentation policy, and hand off to
//! [`commands::dispatch`]. Every command's implementation lives in
//! `commands/`, its arguments in `cli/`, and everything it prints goes
//! through `ui/`.

// mimalloc as the global allocator. ~10–20% faster than system malloc
// on small thread-local allocs (the dominant per-RTT pattern in CDP dispatch).
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod build_info;
mod cli;
mod commands;
mod error;
mod ui;

use clap::Parser;

use commands::bootstrap;

#[tokio::main]
async fn main() -> std::process::ExitCode {
  match Box::pin(run()).await {
    Ok(()) => std::process::ExitCode::SUCCESS,
    Err(err) => {
      #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
      let code = error::report(&err) as u8;
      std::process::ExitCode::from(code)
    },
  }
}

async fn run() -> anyhow::Result<()> {
  let args = cli::Cli::parse();

  let (color, format, quiet) = args.presentation();
  ui::init(color, format, quiet);
  ferridriver_test::logging::init(args.verbose);

  // Reclaim browsers (and their profile dirs) left behind by an earlier
  // ferridriver process that died without teardown. Every subcommand
  // launches browsers, so every subcommand cleans up after the last one.
  // Awaited, not fire-and-forget: a short command would otherwise exit
  // before the sweep ran, and the leftovers would survive every run.
  let reclaimed = tokio::task::spawn_blocking(ferridriver::backend::process::sweep_stale_browsers)
    .await
    .unwrap_or(0);
  if reclaimed > 0 {
    tracing::info!(count = reclaimed, "reclaimed browsers leaked by earlier runs");
  }

  // One startup, folded as many times as it has to be and no more. The
  // documents are folded first because what a `.ts` layer needs in order
  // to be compiled — `extensions`, `[bundler]`, `[scripting]`,
  // `[test].moduleAliases` — is exactly what a `.ts` layer may not set;
  // then the operator tables install, the packages are read, and the
  // stack folds again with every layer in its own slot. Each file is
  // read once across all of it.
  // A named config that is not there is a typo, and continuing on the
  // discovered layers instead runs the command against a configuration the
  // user did not ask for. It used to warn and carry on.
  if let Some(path) = args.config.as_deref()
    && !path.exists()
  {
    anyhow::bail!("--config {} does not exist", path.display());
  }
  let mut startup = ferridriver_config::Startup::new(args.config.as_deref(), !args.no_inherit);
  let config = startup.resolve_documents()?;
  bootstrap::install_bundler_env(&config);
  bootstrap::install_module_aliases(&config.test, bootstrap::module_alias_flags(&args.command))?;
  bootstrap::install_module_loader(&config, &mut startup);
  let contributed = Box::pin(bootstrap::read_extension_defaults(&config, &args)).await?;
  // The second and last fold, and only when there is something new to
  // fold IN: a package contributed defaults, or the stack holds a module
  // the first fold deliberately skipped. A `.toml`-only run with no
  // extensions never gets here — its first fold was already the answer.
  let config = if contributed.is_empty() && !startup.has_module_layer() {
    config
  } else {
    startup.set_extension_defaults(contributed.clone());
    startup.resolve()?
  };

  Box::pin(commands::dispatch(args, config, &startup, contributed)).await
}
