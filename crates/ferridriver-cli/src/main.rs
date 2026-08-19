#![allow(clippy::doc_markdown)]
//! ferridriver -- single-binary CLI for browser automation.
//!
//! Subcommands: `mcp`, `bdd`, `test`, `run`, `install`, `codegen`, `session`.
//!
//! The unified `FerridriverConfig` is loaded exactly once per invocation and
//! its sections are passed to the selected subcommand.

// mimalloc as the global allocator. ~10–20% faster than system malloc
// on small thread-local allocs (the dominant per-RTT pattern in CDP dispatch).
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod cli;
mod config_cmd;
mod ext_cmd;
mod ext_typecheck;
mod ext_types;
mod merge_cmd;
mod run_console;
mod script_setup;
mod session_cmd;
mod test_ui;
mod trace_cmd;

use std::sync::Arc;

use clap::Parser;
use ferridriver_config::FerridriverConfig;
use ferridriver_config::layer;
use ferridriver_mcp::McpServer;
use ferridriver_script::ConsoleSink;

/// Re-resolve the layer stack with what the configured extension
/// packages contribute through `defineDefaults` underneath it.
///
/// A no-op when nothing is configured, which is every run that names no
/// extensions. The contributions are read under the host the subcommand
/// will run as: a package branches on `ferridriver.host`, so what it
/// asks for under `mcp` need not be what it asks for under `test`.
type ContributedDefaults = Vec<(String, serde_json::Value)>;

async fn apply_extension_defaults(
  config: FerridriverConfig,
  args: &cli::Cli,
  startup: &mut ferridriver_config::Startup,
) -> anyhow::Result<(FerridriverConfig, ContributedDefaults)> {
  let specs = config.extension_specs();
  let Some(host) = extension_host_of(&args.command) else {
    return Ok((config, Vec::new()));
  };
  if specs.is_empty() {
    return Ok((config, Vec::new()));
  }
  let caps = ferridriver_script::ScriptCaps::resolve_with_commands(
    &config.scripting.allow_env,
    config.scripting.allow.commands.clone(),
  )
  .with_extension_policy(config.extensions.policy());
  let sidecars: Vec<String> = config.sidecars.iter().map(|s| s.name.clone()).collect();
  let env = ferridriver_script::RequirementEnv::from_caps(&caps, &sidecars);
  // A refusal by `[extensions.policy]` is never skippable, so it fails
  // the command here rather than being logged while the run carries on
  // as if the package had simply been absent.
  let defaults = ferridriver_script::extension_defaults(&specs, &env, &caps.extension_policy, host)
    .await
    .map_err(|e| anyhow::anyhow!("{}", e.message))?;
  if defaults.is_empty() {
    return Ok((config, Vec::new()));
  }
  for (package, _) in &defaults {
    tracing::debug!(target: "ferridriver::extensions", package, host = host.as_str(), "extension.defaults.applied");
  }
  startup.set_extension_defaults(defaults.clone());
  Ok((startup.resolve()?, defaults))
}

/// Evaluate a `--config <file.ts|.js>` and re-resolve the layer stack
/// with it on top.
///
/// The third and last pass of startup, and the reason it is last: the
/// module is bundled, so the bundler environment, the alias table and
/// whatever the extension packages provide all have to be installed
/// before it can be compiled. That ordering is also why a config module
/// may not set any of them — [`ferridriver_config::layer`] refuses each
/// by name.
///
/// A run whose `--config` is a document (or absent) does none of this.
async fn apply_script_config(
  config: FerridriverConfig,
  args: &cli::Cli,
  startup: &mut ferridriver_config::Startup,
) -> anyhow::Result<(FerridriverConfig, Option<layer::ScriptConfig>)> {
  let Some(path) = args.config.as_deref().filter(|p| layer::is_script_config(p)) else {
    return Ok((config, None));
  };
  let caps = ferridriver_script::ScriptCaps::resolve_with_commands(
    &config.scripting.allow_env,
    config.scripting.allow.commands.clone(),
  )
  .with_extension_policy(config.extensions.policy());
  let cwd = std::env::current_dir()?;
  let document = ferridriver_script::config_module::evaluate(path, &cwd, caps)
    .await
    .map_err(|e| anyhow::anyhow!("{}", e.message))?;
  let script_config = layer::ScriptConfig {
    path: path.to_path_buf(),
    test: document,
  };
  startup.set_script_config(script_config.clone());
  Ok((startup.resolve()?, Some(script_config)))
}

/// The extension host a subcommand runs as, or `None` for one that
/// loads no extensions at all.
fn extension_host_of(command: &cli::Command) -> Option<ferridriver_script::ExtensionHost> {
  use ferridriver_script::ExtensionHost;
  match command {
    cli::Command::Mcp(_) => Some(ExtensionHost::Mcp),
    cli::Command::Bdd(_) => Some(ExtensionHost::Bdd),
    cli::Command::Test(_) | cli::Command::RustTest(_) => Some(ExtensionHost::Test),
    // `config` and `doctor` run nothing, but they exist to explain what
    // a run would see — so they read the contributions too, under the
    // neutral script host.
    cli::Command::Run(_) | cli::Command::Session(_) | cli::Command::Config(_) | cli::Command::Doctor(_) => {
      Some(ExtensionHost::Script)
    },
    cli::Command::Install(_)
    | cli::Command::Codegen(_)
    | cli::Command::Ext(_)
    | cli::Command::Trace(_)
    | cli::Command::MergeReports(_) => None,
  }
}

/// Compile every JS reporter the config names and install the factory
/// `create_reporters` consults for a name outside its built-in table.
/// Runs before the runner is built, because a reporter that cannot load
/// has to fail the command rather than the run.
async fn install_js_reporters(
  test: &ferridriver_test::config::TestConfig,
  caps: &ferridriver_script::ScriptCaps,
) -> anyhow::Result<()> {
  let cwd = std::env::current_dir()?;
  ferridriver_script::reporter::install(test, &cwd, caps)
    .await
    .map_err(|e| anyhow::anyhow!("{}", e.message))
}

/// Install the config's `[bundler]` section (import aliases, virtual
/// modules, resolution controls) plus `[test].tsconfig` into the
/// process-global slot every bundle path reads. Relative paths resolve
/// against the config file's directory, falling back to the cwd for a
/// default/in-memory config.
///
/// Runs before the subcommand dispatch because a session or compile
/// runtime built before the install would bundle against an empty
/// environment for its whole life.
fn install_bundler_env(config: &FerridriverConfig) {
  let base = config
    .source_dir
    .clone()
    .or_else(|| std::env::current_dir().ok())
    .unwrap_or_else(|| std::path::PathBuf::from("."));
  let env = ferridriver_script::bundle::BundlerEnv::from_config(&config.bundler, &base)
    .with_tsconfig(config.test.tsconfig.as_deref(), &base);
  ferridriver_script::bundle::set_bundler_env(env);
}

/// Install `[test].moduleAliases` plus any `--module-alias` flags into
/// the process-global slot the native module resolver, the throwaway
/// compile runtimes and the rolldown externals all read.
///
/// Runs at startup for EVERY subcommand, before anything bundles: the
/// table is read by the first resolver built, and a session created
/// before an alias arrives would keep resolving without it. The
/// resolved-config path re-states the same table later, which the
/// merge treats as the no-op it is.
fn install_module_aliases(test: &ferridriver_test::config::TestConfig, flags: &[String]) -> anyhow::Result<()> {
  let mut table: Vec<(String, String)> = test
    .module_aliases
    .iter()
    .map(|(k, v)| (k.clone(), v.clone()))
    .collect();
  for spec in flags {
    let (from, to) = spec
      .split_once('=')
      .ok_or_else(|| anyhow::anyhow!("invalid --module-alias {spec:?} (expected <specifier>=<native module>)"))?;
    table.push((from.trim().to_string(), to.trim().to_string()));
  }
  ferridriver_script::set_module_aliases(table).map_err(|e| anyhow::anyhow!("{e}"))
}

/// `--module-alias` flags of whichever subcommand carries them, so the
/// merge can happen at startup rather than inside one subcommand's
/// config resolution.
fn module_alias_flags(command: &cli::Command) -> &[String] {
  match command {
    cli::Command::Test(args) => &args.module_alias,
    _ => &[],
  }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  let args = cli::Cli::parse();

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

  // Two passes over the layer stack. The first learns which extension
  // packages to load — that list is itself config, so it cannot come
  // from a package — then the packages are read and whatever they
  // contributed through `defineDefaults` is applied BENEATH every file
  // for the second. The operator tables go in between: extraction IS a
  // bundle, so the bundler environment and the alias table have to be
  // installed before it runs.
  // One startup, resolved in passes that SHARE the files they read: the
  // stack is folded again when a package contributes defaults or a
  // config module has to be layered, but each file is read once between
  // them.
  let mut startup = ferridriver_config::Startup::new(args.config.as_deref(), !args.no_inherit);
  let config = startup.resolve()?;
  install_bundler_env(&config);
  install_module_aliases(&config.test, module_alias_flags(&args.command))?;
  let (config, contributed) = Box::pin(apply_extension_defaults(config, &args, &mut startup)).await?;
  let (config, script_config) = Box::pin(apply_script_config(config, &args, &mut startup)).await?;

  match args.command {
    cli::Command::Mcp(mcp_args) => Box::pin(run_mcp(config, mcp_args)).await,
    cli::Command::Bdd(bdd_args) => Box::pin(run_bdd(config, bdd_args)).await,
    cli::Command::Test(test_args) => Box::pin(run_test_native(config, test_args)).await,
    cli::Command::RustTest(test_args) => {
      if test_args.ui {
        Box::pin(test_ui::run(config, test_args)).await
      } else if test_args.watch {
        Box::pin(run_test_watch(config, test_args)).await
      } else {
        run_test(&test_args)
      }
    },
    cli::Command::Run(run_args) => Box::pin(run_script_cli(config, run_args)).await,
    cli::Command::Install(install_args) => Box::pin(run_install(install_args)).await,
    cli::Command::Codegen(codegen_args) => Box::pin(run_codegen(codegen_args)).await,
    cli::Command::Session(session_args) => {
      let origin = session_cmd::ConfigOrigin {
        explicit: args.config.as_deref(),
        inherit: !args.no_inherit,
      };
      Box::pin(session_cmd::run(config, origin, session_args)).await
    },
    cli::Command::Config(config_args) => config_cmd::run_config(
      args.config.as_deref(),
      !args.no_inherit,
      contributed,
      script_config,
      &config_args,
    ),
    cli::Command::Doctor(doctor_args) => {
      Box::pin(config_cmd::run_doctor(
        args.config.as_deref(),
        !args.no_inherit,
        contributed,
        script_config,
        doctor_args,
      ))
      .await
    },
    cli::Command::Ext(ext_args) => Box::pin(ext_cmd::run(config, ext_args)).await,
    cli::Command::Trace(trace_args) => Box::pin(trace_cmd::run(&config, trace_args)).await,
    cli::Command::MergeReports(merge_args) => Box::pin(merge_cmd::run(config, merge_args)).await,
  }
}

/// Launch the interactive recorder: open a headed browser, capture the user's
/// interactions, and emit a runnable script (TypeScript by default) to stdout
/// or `--output`. The emitted script runs standalone via `ferridriver run`
/// and replays on a live session via the MCP `run_script` tool.
async fn run_codegen(args: cli::CodegenArgs) -> anyhow::Result<()> {
  use ferridriver::codegen::OutputLanguage;
  use ferridriver::codegen::recorder::{Recorder, RecorderOptions};

  let url = args.url.unwrap_or_else(|| "about:blank".to_string());
  let options = RecorderOptions {
    url,
    language: OutputLanguage::parse_cli(&args.language),
    output_file: args.output.as_deref().map(|p| p.to_string_lossy().into_owned()),
    viewport: None,
  };
  Recorder::new(options)
    .start()
    .await
    .map_err(|e| anyhow::anyhow!("codegen: {e}"))
}

async fn run_install(args: cli::InstallArgs) -> anyhow::Result<()> {
  use ferridriver::install::{BrowserInstaller, InstallProgress};

  let installer = BrowserInstaller::new();
  let progress = |p: InstallProgress| match p {
    InstallProgress::Resolving => eprintln!("Resolving latest version..."),
    InstallProgress::Downloading {
      bytes_downloaded,
      total_bytes,
    } => match total_bytes {
      Some(total) => eprintln!("Downloading {bytes_downloaded}/{total} bytes"),
      None => eprintln!("Downloading {bytes_downloaded} bytes"),
    },
    InstallProgress::Extracting => eprintln!("Extracting..."),
    InstallProgress::Complete { version, path } => eprintln!("Installed {version} -> {path}"),
    InstallProgress::AlreadyInstalled { version, path } => eprintln!("Already installed {version} -> {path}"),
    InstallProgress::InstallingDeps { distro } => eprintln!("Installing system dependencies ({distro})..."),
    InstallProgress::DepsInstalled => eprintln!("System dependencies installed"),
  };

  let mut browsers = args.browsers;
  if browsers.is_empty() {
    browsers.push("chromium".to_string());
  }

  if args.with_deps {
    installer.install_system_deps(progress).await?;
  }

  for browser in &browsers {
    match browser.as_str() {
      "chromium" => {
        installer.install_chromium(progress).await?;
      },
      "chromium-headless-shell" => {
        installer.install_chromium_headless_shell(progress).await?;
      },
      "firefox" => {
        installer.install_firefox(progress).await?;
      },
      "webkit" => {
        installer.install_webkit(progress).await?;
      },
      other => {
        anyhow::bail!("unknown browser {other:?} (expected chromium, chromium-headless-shell, firefox, or webkit)")
      },
    }
  }

  Ok(())
}

/// Build the underlying cargo command shared by the plain `test` path
/// and the `--ui` cycle spawner: runner selection, `FERRITEST_*` env
/// exports, and package filters. Callers append positionals /
/// passthrough / UI-cycle env on top.
pub(crate) fn base_test_command(args: &cli::RustTestArgs, runner: cli::TestRunner) -> std::process::Command {
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

fn run_test(args: &cli::RustTestArgs) -> anyhow::Result<()> {
  let chosen_runner = args.runner.unwrap_or(detect_test_runner());
  let mut cmd = full_test_command(args, chosen_runner);

  tracing::info!(
    runner = ?chosen_runner_name(chosen_runner),
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
async fn run_test_watch(config: FerridriverConfig, args: cli::RustTestArgs) -> anyhow::Result<()> {
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
          test_ui::kill_process_group(pid);
          return Ok(());
        },
        _ = sigterm.recv() => {
          test_ui::kill_process_group(pid);
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

    let outcome = if status.success() { "passed" } else { "failed" };
    if rerun_pending {
      println!("\n[watch] tests {outcome}; files changed during the run — re-running\n");
      continue;
    }
    println!("\n[watch] tests {outcome}; waiting for file changes (Ctrl-C to quit)\n");
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

fn chosen_runner_name(r: cli::TestRunner) -> &'static str {
  match r {
    cli::TestRunner::Nextest => "nextest",
    cli::TestRunner::Cargo => "cargo",
  }
}

async fn run_test_native(config: FerridriverConfig, args: cli::TestRunArgs) -> anyhow::Result<()> {
  // Thread the `[scripting]` env allow-list + sidecars into the test VM
  // — same resolution the MCP server, `ferridriver run` and BDD use.
  let caps = ferridriver_script::ScriptCaps::resolve_with_commands(
    &config.scripting.allow_env,
    config.scripting.allow.commands.clone(),
  )
  .with_extension_policy(config.extensions.policy())
  .with_extension_settings(config.extensions.settings());
  ferridriver_testjs::set_test_script_caps(caps.clone());
  ferridriver_testjs::set_test_sidecars(script_setup::sidecar_specs(&config));

  let mut overrides = ferridriver_test::config::CliOverrides {
    test_files: args.files,
    grep: args.grep,
    grep_invert: args.grep_invert,
    tag: args.tag,
    workers: args.workers.map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
    retries: args.retries,
    timeout: args.timeout,
    reporter: args.reporter,
    project_filter: args.project,
    watch: args.watch,
    ui: args.ui,
    ui_port: args.ui_port,
    last_failed: args.last_failed,
    only_changed: args.only_changed,
    fail_fast: args.fail_fast,
    max_failures: args.max_failures,
    repeat_each: args.repeat_each,
    forbid_only: args.forbid_only,
    list_only: args.list,
    extensions: config.extension_specs(),
    module_aliases: args.module_alias,
    ..Default::default()
  };
  overrides.headless_override = args.browser.headless_override();
  overrides.backend = args.browser.backend_name().map(str::to_string);
  overrides.executable_path = args.browser.executable_path;
  if let Some(ref spec) = args.shard {
    overrides.shard =
      Some(ferridriver_test::config::ShardArg::parse(spec).map_err(|e| anyhow::anyhow!("invalid --shard: {e}"))?);
  }

  // Resolved before `config.test` is moved below.
  if let Some(mode) = args.debug {
    let setup = script_setup::resolve(&config, &std::env::current_dir()?, &[]).await?;
    ferridriver_script::debug_session::install(mode, setup.into_session_script(), &mut overrides);
  }

  let test_config = ferridriver_test::config::resolve_config_from(config.test, &overrides)
    .map_err(|e| anyhow::anyhow!("config error: {e}"))?;
  install_module_aliases(&test_config, &[])?;
  install_js_reporters(&test_config, &caps).await?;

  let exit_code = Box::pin(ferridriver_testjs::run_ts_tests_with(test_config, overrides)).await;
  if exit_code == 0 {
    Ok(())
  } else {
    std::process::exit(exit_code);
  }
}

async fn run_bdd(config: FerridriverConfig, args: cli::BddArgs) -> anyhow::Result<()> {
  // Thread the `[scripting]` env allow-list into the BDD step VM — the
  // same resolution the MCP server and `ferridriver run` use. Must be
  // set before the run starts.
  let caps = ferridriver_script::ScriptCaps::resolve_with_commands(
    &config.scripting.allow_env,
    config.scripting.allow.commands.clone(),
  )
  .with_extension_policy(config.extensions.policy())
  .with_extension_settings(config.extensions.settings());
  ferridriver_bdd::js::set_bdd_script_caps(caps.clone());
  ferridriver_bdd::js::set_bdd_sidecars(script_setup::sidecar_specs(&config));
  let mut overrides = ferridriver_test::config::CliOverrides {
    bdd_tags: args.tags,
    project_filter: args.project,
    bdd_dry_run: args.dry_run,
    watch: args.watch,
    ui: args.ui,
    ui_port: args.ui_port,
    bdd_fail_fast: args.fail_fast,
    bdd_strict: match (args.strict, args.no_strict) {
      (true, _) => Some(true),
      (_, true) => Some(false),
      _ => None,
    },
    bdd_step_timeout: args.step_timeout,
    bdd_order: args.order,
    bdd_language: args.language,
    bdd_steps: args.steps,
    world_parameters: args.world_parameters,
    extensions: config.extension_specs(),
    workers: args.workers.map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
    reporter: args.reporter,
    ..Default::default()
  };
  // `--headless` opts into headless. Default config is headed, so leaving
  // the flag unset means visible windows -- matching the new CLI
  // convention where the user watches tests run by default.
  overrides.headless_override = args.browser.headless_override();
  overrides.backend = args.browser.backend_name().map(str::to_string);
  overrides.executable_path = args.browser.executable_path;

  if let Some(ref spec) = args.shard {
    overrides.shard =
      Some(ferridriver_test::config::ShardArg::parse(spec).map_err(|e| anyhow::anyhow!("invalid --shard: {e}"))?);
  }

  // Resolved before `config.test` is moved below.
  if let Some(mode) = args.debug {
    let setup = script_setup::resolve(&config, &std::env::current_dir()?, &[]).await?;
    ferridriver_script::debug_session::install(mode, setup.into_session_script(), &mut overrides);
  }

  let mut test_config = ferridriver_test::config::resolve_config_from(config.test, &overrides)
    .map_err(|e| anyhow::anyhow!("config error: {e}"))?;

  // CLI-supplied feature globs override the [test].features list when provided.
  if !args.features.is_empty() {
    test_config.features = args.features;
  }
  install_module_aliases(&test_config, &[])?;
  install_js_reporters(&test_config, &caps).await?;

  let exit_code = Box::pin(ferridriver_bdd::run_bdd_with(test_config, overrides)).await;
  if exit_code == 0 {
    Ok(())
  } else {
    std::process::exit(exit_code);
  }
}

/// Execute a JS script through the ferridriver-script engine with the
/// full Playwright-style binding surface. The script launches its own
/// browser via `chromium()` / `firefox()` / `webkit()`; `--backend`
/// chooses what a plain `chromium()` resolves to. No page is pre-bound.
/// Where a `run` script came from: a real file on disk, or inline source
/// (`--eval` / stdin). Determines how an ES-module entry is materialized
/// for bundling and which directory imports resolve against.
enum ScriptOrigin {
  File(std::path::PathBuf),
  Inline,
}

async fn run_script_cli(file_config: FerridriverConfig, args: cli::RunArgs) -> anyhow::Result<()> {
  use std::io::Read as _;

  let (source, origin) = match (args.eval.clone(), args.script.as_deref()) {
    (Some(code), _) => (code, ScriptOrigin::Inline),
    (None, Some("-")) => {
      let mut s = String::new();
      std::io::stdin().read_to_string(&mut s)?;
      (s, ScriptOrigin::Inline)
    },
    (None, Some(path)) => (
      std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("read {path}: {e}"))?,
      ScriptOrigin::File(std::path::PathBuf::from(path)),
    ),
    (None, None) => anyhow::bail!("provide a script path, `-` for stdin, or --eval <code>"),
  };

  let cwd = std::env::current_dir()?;
  let script_args: Vec<serde_json::Value> = args
    .script_args
    .iter()
    .cloned()
    .map(serde_json::Value::String)
    .collect();

  // `--code-out` implies `--code`: a file to write is a language to render.
  let code_language = args
    .code
    .as_deref()
    .or(args.code_out.as_ref().map(|_| "ts"))
    .map(ferridriver::codegen::OutputLanguage::parse_cli);
  let collected_code = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));

  // Against a live session the browser, the extensions and the sandboxes all
  // belong to the host process; this process only bundles (so relative
  // imports resolve against the directory the user typed the command in) and
  // renders. Its actions happen in the host, which streams them back as
  // events, so no local observer is installed for that path at all.
  if let Some(id) = args.session.as_deref() {
    return run_against_session(
      id,
      &args,
      &origin,
      &source,
      &cwd,
      script_args,
      code_language,
      &collected_code,
    )
    .await;
  }

  // Config comes from the global `-c/--config` (already loaded and
  // shimmed in `main`), falling back to a discovered ferridriver.toml —
  // the same document the MCP server reads. Threading it here fixes
  // `run -c` dropping the config's `extensions:` / scripting settings.
  let setup = script_setup::resolve(&file_config, &cwd, &args.extensions).await?;
  // Read off before the struct is spread into the run context below.
  let setup_secrets = setup.secrets.clone();
  let artifacts_budget = setup.artifacts_budget;
  let artifacts_sandbox = setup.artifacts.clone();

  // Installed AFTER the config resolves, because the echoed source has to
  // know the declared secrets: an observer registered earlier would render
  // the credential it was configured to hide.
  if args.trace || code_language.is_some() {
    ferridriver::trace::set_action_observer(Arc::new(run_console::RunObserver {
      trace: args.trace,
      code: code_language,
      echo_code: args.code_out.is_none(),
      collected: Arc::clone(&collected_code),
      secrets: setup_secrets.clone(),
    }));
  }

  let ctx = ferridriver_script::RunContext {
    vars: Arc::new(ferridriver_script::InMemoryVars::new()),
    sandbox: setup.sandbox,
    artifacts: setup.artifacts,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: setup.extensions,
    host: ferridriver_script::ExtensionHost::Script,
    caps: setup.caps,
    // A local `ferridriver run` has no session key; extensions see
    // `session: undefined` and must not assume one.
    session: None,
  };

  let opts = ferridriver_script::RunOptions {
    timeout: args.timeout_ms.map(std::time::Duration::from_millis),
    memory_limit: None,
    stack_size: None,
    gc_threshold: None,
  };

  // Default is Node-shaped streaming; `--json` keeps the buffered document
  // machine consumers parse. The choice is the flag alone, not stdout's
  // TTY-ness, so a pipeline gets the same bytes a terminal does.
  let engine_config = ferridriver_script::ScriptEngineConfig {
    console_sink: (!args.json).then(|| Arc::new(run_console::StreamingConsole) as Arc<dyn ConsoleSink>),
    ..setup.engine
  };
  let session = ferridriver_script::Session::create(engine_config, &ctx)
    .await
    .map_err(|e| anyhow::anyhow!("session create: {}", e.message))?;

  // ES-module sources (TypeScript, or static `import`/`export`) are
  // rolldown-bundled + transpiled + compiled to bytecode (disk-cached for
  // file inputs), then run as a module; the run result is its `default`
  // export. Plain scripts keep the wrap-and-eval path where top-level
  // `return` yields the result.
  let result = if needs_bundle(&origin, &source) {
    let (entry, bundle_cwd, _tmp) = bundle_entry(&origin, &source, &cwd)?;
    let bundle = ferridriver_script::bundle_and_compile(std::slice::from_ref(&entry), &bundle_cwd)
      .await
      .map_err(|e| anyhow::anyhow!("bundle {}: {}", entry.display(), e.message))?;
    session.execute_module(&bundle, &script_args, opts, &ctx).await.result
  } else {
    session.execute(&source, &script_args, opts, &ctx).await.result
  };

  finish_code(&collected_code, code_language, args.code_out.as_deref())?;
  sweep_artifacts(artifacts_budget, artifacts_sandbox.as_deref()).await;
  // A local run's script launches and owns its own browser, so this process
  // never holds a page to read state from.
  let report = args
    .report
    .then(|| RunReport::collect(code_language, &collected_code, None, setup_secrets));
  report_code_result(&result, args.json, &collected_code, report.as_ref())
}

/// Write the generated source to `out`, wrapped in the language's test
/// scaffolding so the file runs as-is. Without `out` the lines have already
/// been streamed as they happened and there is nothing left to do.
fn finish_code(
  collected: &Arc<std::sync::Mutex<Vec<String>>>,
  language: Option<ferridriver::codegen::OutputLanguage>,
  out: Option<&std::path::Path>,
) -> anyhow::Result<()> {
  let (Some(language), Some(path)) = (language, out) else {
    return Ok(());
  };
  let lines = collected
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone();
  let emitter = language.emitter();
  // No opening navigation in the scaffolding: unlike the interactive recorder
  // — which navigates before recording starts — an echoed run already has its
  // `goto` among the lines. The file is exactly the actions that happened, in
  // the order they happened, and a run that started on the session's current
  // page correctly begins there.
  let mut file = emitter.header("");
  for line in &lines {
    file.push_str(line);
    file.push('\n');
  }
  file.push_str(&emitter.footer());
  std::fs::write(path, file).map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
  eprintln!("wrote {} ({} action(s))", path.display(), lines.len());
  Ok(())
}

/// Run a script against a live session: this process bundles and renders, the
/// host owns the browser, the extensions and the sandboxes.
#[allow(clippy::too_many_arguments)] // every one is a distinct piece of the run
async fn run_against_session(
  id: &str,
  args: &cli::RunArgs,
  origin: &ScriptOrigin,
  source: &str,
  cwd: &std::path::Path,
  script_args: Vec<serde_json::Value>,
  code_language: Option<ferridriver::codegen::OutputLanguage>,
  collected_code: &Arc<std::sync::Mutex<Vec<String>>>,
) -> anyhow::Result<()> {
  if !args.extensions.is_empty() {
    anyhow::bail!(
      "--extension cannot be combined with --session: a session's extensions are loaded by its host. \
       Pass --extension to `ferridriver session open` instead."
    );
  }
  let mut request = build_script_request(origin, source, cwd, script_args, args.timeout_ms).await?;
  request.trace = args.trace;
  request.code_language = args.code.clone().or(args.code_out.as_ref().map(|_| "ts".to_string()));
  request.page_state = args.report;
  let sinks = session_cmd::RunSinks {
    code: Arc::clone(collected_code),
    // Streaming code to stderr would interleave with a file's contents to
    // no one's benefit; when a file is the destination, that is the only
    // destination.
    echo_code: args.code_out.is_none(),
    ..Default::default()
  };
  let result = session_cmd::run_on_session(id, args.context.as_deref(), request, args.json, &sinks).await?;
  finish_code(collected_code, code_language, args.code_out.as_deref())?;
  // The host redacted everything it sent, so the client renders it as-is.
  let report = args.report.then(|| {
    let page = sinks
      .page
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone();
    RunReport::collect(
      code_language,
      collected_code,
      page,
      ferridriver::response::Secrets::default(),
    )
  });
  report_code_result(&result, args.json, collected_code, report.as_ref())
}

/// Bring the artifacts root back under its configured ceiling, protecting
/// what this run just wrote — the script produced those outputs deliberately,
/// and deleting them on the way out would make the ceiling delete the very
/// thing the run was for.
async fn sweep_artifacts(
  budget: Option<ferridriver::response::OutputBudget>,
  artifacts: Option<&ferridriver_script::PathSandbox>,
) {
  let (Some(budget), Some(artifacts)) = (budget, artifacts) else {
    return;
  };
  let evicted = budget.enforce(artifacts.root(), &artifacts.written()).await;
  if evicted.files > 0 {
    tracing::info!(
      files = evicted.files,
      bytes = evicted.bytes,
      "artifacts budget: evicted least-recently-modified outputs"
    );
  }
}

/// What `--report` renders around a finished run.
struct RunReport {
  /// The language the echoed lines are written in; `None` when `--code` was
  /// not asked for, in which case there is no code section.
  language: Option<ferridriver::codegen::OutputLanguage>,
  code: Vec<String>,
  /// The page the run finished on. Reported by a session host; a local run's
  /// script owns its own browser, so this process has no handle to read.
  page: Option<ferridriver::response::PageState>,
  secrets: ferridriver::response::Secrets,
}

impl RunReport {
  fn collect(
    language: Option<ferridriver::codegen::OutputLanguage>,
    collected: &Arc<std::sync::Mutex<Vec<String>>>,
    page: Option<ferridriver::response::PageState>,
    secrets: ferridriver::response::Secrets,
  ) -> Self {
    Self {
      language,
      code: collected
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone(),
      page,
      secrets,
    }
  }
}

/// Assemble the response contract for a finished run.
///
/// The order is the order an agent reads in: what went wrong, what came back,
/// what reproduces it, where the browser now is.
fn build_response(result: &ferridriver_script::ScriptResult, report: &RunReport) -> ferridriver::response::Response {
  let mut response = ferridriver::response::Response::new().with_secrets(report.secrets.clone());
  match &result.outcome {
    ferridriver_script::Outcome::Error { error } => {
      let name = error.name.clone().unwrap_or_else(|| error.kind.to_string());
      response.error(vec![format!("{name}: {}", error.message)]);
    },
    ferridriver_script::Outcome::Ok { success } => match &success.value {
      serde_json::Value::Null => {},
      serde_json::Value::String(s) => response.result(s.lines().map(str::to_string).collect()),
      value => response.result(
        serde_json::to_string_pretty(value)
          .unwrap_or_else(|_| value.to_string())
          .lines()
          .map(str::to_string)
          .collect(),
      ),
    },
  }
  if let Some(language) = report.language {
    response.code(report.code.clone(), language);
  }
  if let Some(page) = &report.page {
    response.page(page);
  }
  response
}

/// [`report_result`], with the generated source folded into the `--json`
/// document so a machine consumer still reads exactly one object, and the
/// `--report` sections rendered when the caller asked for them.
fn report_code_result(
  result: &ferridriver_script::ScriptResult,
  json: bool,
  collected: &Arc<std::sync::Mutex<Vec<String>>>,
  report: Option<&RunReport>,
) -> anyhow::Result<()> {
  let lines = collected
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone();

  if let Some(report) = report {
    let response = build_response(result, report);
    if json {
      let mut document = serde_json::to_value(result)?;
      if let Some(object) = document.as_object_mut() {
        if !lines.is_empty() {
          object.insert("code".to_string(), serde_json::json!(lines));
        }
        object.insert("report".to_string(), response.to_json());
      }
      println!("{}", serde_json::to_string_pretty(&document)?);
    } else {
      // Console already streamed while the script ran; the sections are what
      // is left to say about it.
      print!("{}", response.render());
    }
    if let ferridriver_script::Outcome::Error { ref error } = result.outcome {
      eprintln!("[{}] {} ({}ms)", error.kind, error.message, result.duration_ms);
      std::process::exit(1);
    }
    return Ok(());
  }

  if !json || lines.is_empty() {
    return report_result(result, json);
  }
  let mut document = serde_json::to_value(result)?;
  if let Some(object) = document.as_object_mut() {
    object.insert("code".to_string(), serde_json::json!(lines));
  }
  println!("{}", serde_json::to_string_pretty(&document)?);
  if let ferridriver_script::Outcome::Error { ref error } = result.outcome {
    eprintln!("[{}] {} ({}ms)", error.kind, error.message, result.duration_ms);
    std::process::exit(1);
  }
  Ok(())
}

/// Print a run's result and exit non-zero when the script failed.
fn report_result(result: &ferridriver_script::ScriptResult, json: bool) -> anyhow::Result<()> {
  if json {
    println!("{}", serde_json::to_string_pretty(result)?);
    if let ferridriver_script::Outcome::Error { ref error } = result.outcome {
      eprintln!("[{}] {} ({}ms)", error.kind, error.message, result.duration_ms);
      std::process::exit(1);
    }
  } else {
    run_console::print_result(result);
    if result.is_err() {
      std::process::exit(1);
    }
  }
  Ok(())
}

/// Turn the resolved script source into the request a session host runs.
///
/// Module sources are bundled HERE, not host-side: relative imports and
/// `node_modules` resolve against the directory the command was typed in, and
/// only this process knows it. The host compiles what comes back, so bytecode
/// built by one binary is never loaded by another.
async fn build_script_request(
  origin: &ScriptOrigin,
  source: &str,
  cwd: &std::path::Path,
  args: Vec<serde_json::Value>,
  timeout_ms: Option<u64>,
) -> anyhow::Result<ferridriver_session::ScriptRequest> {
  if !needs_bundle(origin, source) {
    return Ok(ferridriver_session::ScriptRequest {
      kind: ferridriver_session::ScriptKind::Source,
      code: source.to_string(),
      source_map: None,
      module_name: None,
      args,
      timeout_ms,
      trace: false,
      code_language: None,
      page_state: false,
    });
  }
  let (entry, bundle_cwd, _tmp) = bundle_entry(origin, source, cwd)?;
  let bundled = ferridriver_script::bundle_source(std::slice::from_ref(&entry), &bundle_cwd)
    .await
    .map_err(|e| anyhow::anyhow!("bundle {}: {}", entry.display(), e.message))?;
  Ok(ferridriver_session::ScriptRequest {
    kind: ferridriver_session::ScriptKind::Module,
    code: bundled.code,
    source_map: bundled.source_map_json,
    module_name: Some(module_label(origin)),
    args,
    timeout_ms,
    trace: false,
    code_language: None,
    page_state: false,
  })
}

/// Stack-frame label for a module run: the script's own file name, so a host
/// -side error reads like a local one.
fn module_label(origin: &ScriptOrigin) -> String {
  match origin {
    ScriptOrigin::File(path) => path.file_name().map_or_else(
      || "ferridriver-run.js".to_string(),
      |n| n.to_string_lossy().into_owned(),
    ),
    ScriptOrigin::Inline => "ferridriver-run.js".to_string(),
  }
}

/// True when the source must run as a bundled ES module (TypeScript file
/// extension, or top-level `import`/`export`). Plain scripts stay on the
/// wrap-and-eval path where top-level `return` yields the result.
fn needs_bundle(origin: &ScriptOrigin, source: &str) -> bool {
  if let ScriptOrigin::File(p) = origin
    && ferridriver_script::is_typescript_path(p)
  {
    return true;
  }
  ferridriver_script::source_is_es_module(source)
}

/// Removes a materialized temp entry file on drop.
struct TmpEntryGuard(std::path::PathBuf);
impl Drop for TmpEntryGuard {
  fn drop(&mut self) {
    let _ = std::fs::remove_file(&self.0);
  }
}

/// Resolve the rolldown entry path + bundler cwd for a module-mode run.
/// File inputs bundle in place (imports resolve against the file's dir);
/// inline sources are written to a temp `.ts` entry in `cwd` so relative
/// imports resolve against `cwd`, cleaned up via the returned guard.
fn bundle_entry(
  origin: &ScriptOrigin,
  source: &str,
  cwd: &std::path::Path,
) -> anyhow::Result<(std::path::PathBuf, std::path::PathBuf, Option<TmpEntryGuard>)> {
  match origin {
    ScriptOrigin::File(p) => {
      let dir = p
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map_or_else(|| cwd.to_path_buf(), std::path::Path::to_path_buf);
      Ok((p.clone(), dir, None))
    },
    ScriptOrigin::Inline => {
      let entry = cwd.join(format!(".ferridriver-run-{}.ts", std::process::id()));
      std::fs::write(&entry, source).map_err(|e| anyhow::anyhow!("write temp entry {}: {e}", entry.display()))?;
      Ok((entry.clone(), cwd.to_path_buf(), Some(TmpEntryGuard(entry))))
    },
  }
}

async fn run_mcp(mut config: FerridriverConfig, args: cli::McpArgs) -> anyhow::Result<()> {
  // The mcp section drives chrome args, instances, and server metadata.
  // CLI flags fall back when the [mcp] section is empty so the user can
  // launch the server with no config file at all.
  let sidecars = script_setup::sidecar_specs(&config);
  let extensions = config.extension_specs();
  let extension_policy = config.extensions.policy();
  let extension_settings = config.extensions.settings();
  let test_config = config.test.clone();
  let scripting = std::mem::take(&mut config.scripting);
  // CLI flags beat the config file. The old order was inverted, so
  // `--headless` / `--backend` were silently dropped whenever the
  // config set those keys at all.
  let effective = cli::effective_browser(&args.browser, &config.mcp);
  let (backend, headless) = (effective.backend, effective.headless);
  let connect_mode = args.browser.connect_mode();

  let caps =
    ferridriver_script::ScriptCaps::resolve_with_commands(&scripting.allow_env, scripting.allow.commands.clone())
      .with_extension_policy(extension_policy)
      .with_extension_settings(extension_settings);
  let mut server = McpServer::with_options(connect_mode, backend, headless, Arc::new(config))
    .with_script_caps(caps)
    .with_sidecars(sidecars)
    .with_test_config(test_config);
  server.load_extensions(&extensions).await;
  match args.transport.transport {
    cli::Transport::Stdio => Box::pin(ferridriver_mcp::mcp::serve_stdio_with(server)).await,
    cli::Transport::Http => Box::pin(ferridriver_mcp::mcp::serve_http_with(server, args.transport.port)).await,
  }
}
