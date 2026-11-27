//! ferridriver — single-binary browser automation.
//!
//! This file does three things and nothing else: resolve the configuration
//! layer stack, install the presentation policy, and hand off to
//! [`commands::dispatch`]. Every command's implementation lives in
//! `commands/`, its arguments in `cli/`, and everything it prints goes
//! through `ui/`.

// A failed write to stdout or stderr must not end the process; ui's macros
// take every write instead (see ui/mod.rs).
#![deny(clippy::print_stdout, clippy::print_stderr)]

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
      let code = error::report(&err);
      if err.is::<error::ExitNow>() {
        std::process::exit(code);
      }
      u8::try_from(code).map_or(std::process::ExitCode::FAILURE, std::process::ExitCode::from)
    },
  }
}

async fn run() -> anyhow::Result<()> {
  let args = cli::Cli::parse();

  let (color, format, quiet) = args.presentation();
  ui::init(color, format, quiet);
  ferridriver_test::logging::init(args.verbose);

  // Reclaim browsers (and their profile dirs) left behind by an earlier
  // ferridriver process that died without teardown — only on commands that
  // launch browsers directly (skipping utility commands and `mcp`, which
  // sweeps on its own). Spawned ahead of config resolution so the process
  // sweep overlaps with config and extension loading rather than adding
  // serial startup latency, then awaited before dispatch so a short run
  // never exits before cleanup finishes.
  let sweep = args
    .sweeps_stale_browsers()
    .then(|| tokio::task::spawn_blocking(ferridriver::backend::process::sweep_stale_browsers));

  // Commands that never read configuration skip layer discovery, `.ts`
  // config bundling, and extension defaults completely.
  let (config, startup, contributed) = if args.reads_config() {
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
    //
    // Only the commands that read configuration accept the flag, so this can
    // no longer fail a command that would never have opened the file.
    let source = args.config_source();
    if let Some(path) = source.path.as_deref()
      && !path.exists()
    {
      return Err(error::usage(format!("--config {} does not exist", path.display())));
    }
    let mut startup = ferridriver_config::Startup::new(source.path.as_deref(), source.inherit());
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
    (config, startup, contributed)
  } else {
    (
      ferridriver_config::FerridriverConfig::default(),
      ferridriver_config::Startup::new(None, false),
      Vec::new(),
    )
  };

  if let Some(handle) = sweep {
    let reclaimed = handle.await.unwrap_or(0);
    if reclaimed > 0 {
      tracing::info!(count = reclaimed, "reclaimed browsers leaked by earlier runs");
    }
  }

  Box::pin(commands::dispatch(args, config, &startup, contributed)).await
}
