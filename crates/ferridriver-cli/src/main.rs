#![allow(clippy::doc_markdown)]
//! ferridriver -- single-binary CLI for browser automation.
//!
//! Subcommands: `mcp`, `bdd`, `test`, `install`, `codegen`, `config`.
//!
//! The unified `FerridriverConfig` is loaded exactly once per invocation and
//! its sections are passed to the selected subcommand.

// mimalloc as the global allocator. ~10–20% faster than system malloc
// on small thread-local allocs (the dominant per-RTT pattern in CDP dispatch).
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod cli;

use std::sync::Arc;

use clap::Parser;
use ferridriver_config::FerridriverConfig;
use ferridriver_mcp::McpServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  let args = cli::Cli::parse();

  ferridriver_test::logging::init(args.verbose);

  // Load the unified config exactly once. Each subcommand reads the
  // section it cares about from this single document.
  let config = FerridriverConfig::load(args.config.as_deref())?;

  match args.command {
    cli::Command::Mcp(mcp_args) => Box::pin(run_mcp(config, mcp_args)).await,
    cli::Command::Bdd(bdd_args) => Box::pin(run_bdd(config, bdd_args)).await,
    cli::Command::Test(test_args) => run_test(&test_args),
    cli::Command::Run(run_args) => Box::pin(run_script_cli(run_args)).await,
    cli::Command::Install(install_args) => Box::pin(run_install(install_args)).await,
    cli::Command::Codegen(_) => anyhow::bail!("`codegen` subcommand not yet implemented"),
    cli::Command::Config(_) => anyhow::bail!("`config` subcommand not yet implemented"),
  }
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

fn run_test(args: &cli::TestArgs) -> anyhow::Result<()> {
  use std::process::{Command, Stdio};

  let chosen_runner = args.runner.unwrap_or(detect_test_runner());

  let (program, base_args): (&str, Vec<String>) = match chosen_runner {
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

  let mut cmd = Command::new(program);
  cmd.args(&base_args);
  for pkg in &args.packages {
    cmd.arg("-p").arg(pkg);
  }
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

  tracing::info!(
    runner = ?chosen_runner_name(chosen_runner),
    args = ?cmd.get_args().collect::<Vec<_>>(),
    "running cargo tests"
  );

  let status = cmd
    .status()
    .map_err(|e| anyhow::anyhow!("failed to spawn `{program}`: {e}"))?;
  if status.success() {
    Ok(())
  } else {
    std::process::exit(status.code().unwrap_or(1));
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

async fn run_bdd(config: FerridriverConfig, args: cli::BddArgs) -> anyhow::Result<()> {
  // Thread the `[scripting]` env allow-list into the BDD step VM — the
  // same resolution the MCP server and `ferridriver run` use. Must be
  // set before the run starts.
  ferridriver_bdd::js::set_bdd_script_caps(ferridriver_script::ScriptCaps::resolve(&config.scripting.allow_env));
  let mut overrides = ferridriver_test::config::CliOverrides {
    bdd_tags: args.tags,
    bdd_dry_run: args.dry_run,
    bdd_fail_fast: args.fail_fast,
    bdd_strict: args.strict,
    bdd_step_timeout: args.step_timeout,
    bdd_order: args.order,
    bdd_language: args.language,
    bdd_steps: args.steps,
    world_parameters: args.world_parameters,
    extensions: config.extensions.clone(),
    workers: args.workers.map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
    reporter: args.reporter,
    ..Default::default()
  };
  // `--headless` opts into headless. Default config is headed, so leaving
  // the flag unset means visible windows -- matching the new CLI
  // convention where the user watches tests run by default.
  if args.browser.headless {
    overrides.headless = true;
  }
  // Likewise, only override backend / executable_path when the user supplied
  // a non-default value. clap fills in defaults for `--backend`, so use the
  // raw arg presence by checking the user-relevant flags.
  if !matches!(args.browser.backend, cli::Backend::CdpPipe) {
    overrides.backend = match args.browser.backend {
      cli::Backend::CdpPipe => Some("cdp-pipe".into()),
      cli::Backend::CdpRaw => Some("cdp-raw".into()),
      cli::Backend::WebKit => Some("webkit".into()),
      cli::Backend::Bidi => Some("bidi".into()),
    };
  }
  overrides.executable_path = args.browser.executable_path;

  if let Some(ref spec) = args.shard {
    overrides.shard =
      Some(ferridriver_test::config::ShardArg::parse(spec).map_err(|e| anyhow::anyhow!("invalid --shard: {e}"))?);
  }

  let mut test_config = ferridriver_test::config::resolve_config_from(config.test, &overrides)
    .map_err(|e| anyhow::anyhow!("config error: {e}"))?;

  // CLI-supplied feature globs override the [test].features list when provided.
  if !args.features.is_empty() {
    test_config.features = args.features;
  }

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

async fn run_script_cli(args: cli::RunArgs) -> anyhow::Result<()> {
  use std::io::Read as _;

  let (source, origin) = match (args.eval, args.script.as_deref()) {
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
  let sandbox = Arc::new(
    ferridriver_script::PathSandbox::new(&cwd)
      .map_err(|e| anyhow::anyhow!("sandbox init ({}): {}", cwd.display(), e.message))?,
  );
  // `ferridriver run` honours a ferridriver.toml in scope for the
  // scripting sandbox env allow-list.
  let scripting = FerridriverConfig::load(None).unwrap_or_default().scripting;
  let caps = ferridriver_script::ScriptCaps::resolve(&scripting.allow_env);

  let ctx = ferridriver_script::RunContext {
    vars: Arc::new(ferridriver_script::InMemoryVars::new()),
    sandbox,
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    plugins: Vec::new(),
    trusted_modules: false,
    host: ferridriver_script::ExtensionHost::Script,
    caps,
  };

  let opts = ferridriver_script::RunOptions {
    timeout: args.timeout_ms.map(std::time::Duration::from_millis),
    memory_limit: None,
    stack_size: None,
    gc_threshold: None,
  };
  let script_args: Vec<serde_json::Value> = args.script_args.into_iter().map(serde_json::Value::String).collect();

  let session = ferridriver_script::Session::create(ferridriver_script::ScriptEngineConfig::default(), &ctx)
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

  println!("{}", serde_json::to_string_pretty(&result)?);
  if let ferridriver_script::Outcome::Error { ref error } = result.outcome {
    eprintln!("[{}] {} ({}ms)", error.kind, error.message, result.duration_ms);
    std::process::exit(1);
  }
  Ok(())
}

/// True when the source must run as a bundled ES module (TypeScript file
/// extension, or top-level `import`/`export`). Plain scripts stay on the
/// wrap-and-eval path where top-level `return` yields the result.
fn needs_bundle(origin: &ScriptOrigin, source: &str) -> bool {
  if let ScriptOrigin::File(p) = origin {
    if ferridriver_script::is_typescript_path(p) {
      return true;
    }
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

async fn run_mcp(config: FerridriverConfig, args: cli::McpArgs) -> anyhow::Result<()> {
  // The mcp section drives chrome args, instances, and server metadata.
  // CLI flags fall back when the [mcp] section is empty so the user can
  // launch the server with no config file at all.
  let extension_paths: Vec<std::path::PathBuf> = config.extensions.iter().map(std::path::PathBuf::from).collect();
  let scripting = config.scripting;
  let mcp = config.mcp;
  let backend = if mcp.browser.backend.is_some() {
    mcp.backend_kind()
  } else {
    args.browser.backend_kind()
  };
  let headless = if mcp.browser.headless.is_some() {
    mcp.headless()
  } else {
    args.browser.headless
  };
  let connect_mode = args.browser.connect_mode();

  let caps = ferridriver_script::ScriptCaps::resolve(&scripting.allow_env);
  let mut server = McpServer::with_options(connect_mode, backend, headless, Arc::new(mcp)).with_script_caps(caps);
  server.load_extensions(&extension_paths).await;
  match args.transport.transport {
    cli::Transport::Stdio => ferridriver_mcp::mcp::serve_stdio_with(server).await,
    cli::Transport::Http => ferridriver_mcp::mcp::serve_http_with(server, args.transport.port).await,
  }
}
