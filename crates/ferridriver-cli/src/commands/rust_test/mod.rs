//! `ferridriver rust-test` — the cargo test wrapper.
//!
//! Three ways to run the same command: once, on every `.rs` change, or under
//! the web app in `ui`. All three build it through [`base_test_command`], so
//! the `FERRITEST_*` environment a harness binary reads cannot differ between
//! a plain run and a watched one.

pub mod ui;

use ferridriver_config::FerridriverConfig;

use crate::cli;
use crate::ui as term;

/// Build the underlying cargo command shared by the plain `test` path
/// and the `--ui` cycle spawner: runner selection, `FERRITEST_*` env
/// exports, and package filters. Callers append positionals /
/// passthrough / UI-cycle env on top.
pub fn base_test_command(args: &cli::RustTestArgs, runner: cli::TestRunner) -> std::process::Command {
  let (program, base_args): (&str, Vec<String>) = match runner {
    cli::TestRunner::Nextest => {
      let mut a = vec!["nextest".into(), "run".into()];
      if let Some(profile) = args.profile.as_deref() {
        a.push("--profile".into());
        a.push(profile.to_string());
      }
      ("cargo", a)
    },
    cli::TestRunner::Cargo => ("cargo", vec!["test".into()]),
  };

  let mut cmd = std::process::Command::new(program);
  cmd.args(&base_args);
  if args.headless {
    cmd.env("FERRITEST_HEADLESS", "1");
  }
  if let Some(backend) = args.backend.as_deref() {
    cmd.env("FERRITEST_BACKEND", backend);
  }
  if let Some(workers) = args.workers {
    cmd.env("FERRITEST_WORKERS", workers.to_string());
  }
  if let Some(grep) = args.grep.as_deref() {
    cmd.env("FERRITEST_GREP", grep);
  }
  if let Some(tag) = args.tag.as_deref() {
    cmd.env("FERRITEST_TAG", tag);
  }
  if let Some(retries) = args.retries {
    cmd.env("FERRITEST_RETRIES", retries.to_string());
  }
  for pkg in &args.packages {
    cmd.arg("-p").arg(pkg);
  }
  cmd
}

/// The exact command `ferridriver test` runs once — shared with watch
/// mode, which re-runs it per file change.
fn full_test_command(args: &cli::RustTestArgs, chosen_runner: cli::TestRunner) -> std::process::Command {
  use std::process::Stdio;
  let mut cmd = base_test_command(args, chosen_runner);
  if let Some(filter) = args.filter.as_deref() {
    // For nextest, filter is a positional. For cargo test, filter is also positional.
    cmd.arg(filter);
  }
  if !args.passthrough.is_empty() {
    cmd.arg("--");
    for arg in &args.passthrough {
      cmd.arg(arg);
    }
  }
  cmd
    .stdout(Stdio::inherit())
    .stderr(Stdio::inherit())
    .stdin(Stdio::inherit());
  cmd
}

pub fn run(args: &cli::RustTestArgs) -> anyhow::Result<()> {
  let chosen_runner = args.runner.unwrap_or(detect_test_runner());
  let mut cmd = full_test_command(args, chosen_runner);

  tracing::info!(
    runner = chosen_runner.name(),
    args = ?cmd.get_args().collect::<Vec<_>>(),
    "running cargo tests"
  );

  let status = cmd
    .status()
    .map_err(|e| anyhow::anyhow!("failed to spawn `cargo`: {e}"))?;
  if status.success() {
    Ok(())
  } else {
    std::process::exit(status.code().unwrap_or(1));
  }
}

/// `ferridriver test --watch`: run the test command, then re-run it
/// whenever a `.rs` file under the working directory changes
/// (`testIgnore` patterns from the resolved `[test]` config excluded).
/// A change arriving while a cycle runs queues exactly one re-run for
/// when it finishes; Ctrl-C / SIGTERM kill the cycle's whole process
/// group (cargo, harness binaries, browsers) and exit.
pub async fn run_watch(config: FerridriverConfig, args: cli::RustTestArgs) -> anyhow::Result<()> {
  let overrides = ferridriver_test::config::CliOverrides {
    headless_override: args.headless.then_some(true),
    backend: args.backend.clone(),
    workers: args.workers.map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
    tag: args.tag.clone(),
    retries: args.retries,
    ..Default::default()
  };
  let test_config = ferridriver_test::config::resolve_config_from(config.test, &overrides)
    .map_err(|e| anyhow::anyhow!("config error: {e}"))?;
  let cwd = std::env::current_dir()?;
  let watcher = ferridriver_test::watch::FileWatcher::new(&cwd, &["**/*.rs".to_string()], &test_config.test_ignore)
    .map_err(|e| anyhow::anyhow!("start file watcher: {e}"))?;
  let chosen_runner = args.runner.unwrap_or(detect_test_runner());
  let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

  loop {
    let mut cmd = full_test_command(&args, chosen_runner);
    // Fresh process group so an interrupt kills cargo + harness
    // binaries + their browsers without touching this process.
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = tokio::process::Command::from(cmd)
      .spawn()
      .map_err(|e| anyhow::anyhow!("failed to spawn `cargo`: {e}"))?;
    let pid = child.id().and_then(|p| i32::try_from(p).ok());

    let mut rerun_pending = false;
    let status = loop {
      tokio::select! {
        status = child.wait() => break status?,
        _ = tokio::signal::ctrl_c() => {
          ui::kill_process_group(pid);
          return Ok(());
        },
        _ = sigterm.recv() => {
          ui::kill_process_group(pid);
          return Ok(());
        },
        change = watcher.recv() => {
          if change.is_some() {
            let _ = watcher.drain_deduped();
            rerun_pending = true;
          }
        },
      }
    };

    let outcome = if status.success() {
      term::success("passed")
    } else {
      term::failure("failed")
    };
    if rerun_pending {
      term::say(&format!(
        "\n{}  {outcome}; files changed during the run — re-running\n",
        term::badge("watch", &console::Style::new().on_cyan().black())
      ));
      continue;
    }
    term::say(&format!(
      "\n{}  {outcome}; waiting for changes {}\n",
      term::badge("watch", &console::Style::new().on_cyan().black()),
      term::dim("(Ctrl-C to quit)")
    ));
    tokio::select! {
      _ = tokio::signal::ctrl_c() => return Ok(()),
      _ = sigterm.recv() => return Ok(()),
      change = watcher.recv() => {
        if change.is_none() {
          return Ok(());
        }
        let _ = watcher.drain_deduped();
      },
    }
  }
}

fn detect_test_runner() -> cli::TestRunner {
  // Probe for nextest availability with `cargo nextest --version`. Cheap (~5ms).
  let probe = std::process::Command::new("cargo")
    .args(["nextest", "--version"])
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status();
  match probe {
    Ok(s) if s.success() => cli::TestRunner::Nextest,
    _ => cli::TestRunner::Cargo,
  }
}
