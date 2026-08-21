//! `ferridriver run` — execute a script against a browser.
//!
//! Three shapes of the same command: a script that owns its own browser, one
//! bound to a configured `[browser]` instance, and one driving a live session
//! in another process. What differs is where `page` comes from; the bundling,
//! the artifact sweep, the action observer and the result document are shared.

pub mod console;

use std::sync::Arc;

use ferridriver_config::FerridriverConfig;
use ferridriver_script::ConsoleSink;

use crate::cli;
use crate::commands::{instance, script_setup, session};
use crate::ui;

/// Where a `run` script came from: a real file on disk, or inline source
/// (`--eval` / stdin). Determines how an ES-module entry is materialized
/// for bundling and which directory imports resolve against.
enum ScriptOrigin {
  File(std::path::PathBuf),
  Inline,
}

/// The script a `run` invocation names, and where it came from.
///
/// The origin decides more than diagnostics: a file is bundled relative to
/// its own directory and its bytecode is disk-cached, while inline source is
/// neither.
fn read_script_source(args: &cli::RunArgs) -> anyhow::Result<(String, ScriptOrigin)> {
  use std::io::Read as _;

  match (args.eval.clone(), args.script.as_deref()) {
    (Some(code), _) => Ok((code, ScriptOrigin::Inline)),
    (None, Some("-")) => {
      let mut source = String::new();
      std::io::stdin().read_to_string(&mut source)?;
      Ok((source, ScriptOrigin::Inline))
    },
    (None, Some(path)) => Ok((
      std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("read {path}: {e}"))?,
      ScriptOrigin::File(std::path::PathBuf::from(path)),
    )),
    (None, None) => anyhow::bail!("provide a script path, `-` for stdin, or --eval <code>"),
  }
}

/// Execute a JS script through the ferridriver-script engine with the
/// full Playwright-style binding surface. The script launches its own
/// browser via `chromium()` / `firefox()` / `webkit()`; `--backend`
/// chooses what a plain `chromium()` resolves to. No page is pre-bound.
pub async fn run(file_config: FerridriverConfig, args: cli::RunArgs) -> anyhow::Result<()> {
  let (source, origin) = read_script_source(&args)?;

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
  let artifacts_dir = setup.artifacts.clone();

  // Installed AFTER the config resolves, because the echoed source has to
  // know the declared secrets: an observer registered earlier would render
  // the credential it was configured to hide.
  if args.trace || code_language.is_some() {
    ferridriver::trace::set_action_observer(Arc::new(console::RunObserver {
      trace: args.trace,
      code: code_language,
      echo_code: args.code_out.is_none(),
      collected: Arc::clone(&collected_code),
      secrets: setup_secrets.clone(),
    }));
  }

  // `--instance` provisions the browser the config describes for that name,
  // through the same state the MCP server builds -- so an instance's args and
  // discover commands mean here what they mean there. Absent the flag nothing
  // is launched: a script that never opens a browser pays for none.
  let provisioned = match args.instance.as_deref() {
    // Boxed: the future holds the whole `[mcp]` config plus the launch
    // state it builds, which is several kilobytes to carry inline on the
    // stack of every `run` -- including the ones that provision nothing.
    Some(name) => Some(Box::pin(instance::provision_instance(file_config.mcp, name, args.headed)).await?),
    None => None,
  };
  let (page, browser_context, browser) = match provisioned {
    Some((page, ctx_ref, browser)) => (Some(page), Some(ctx_ref), Some(browser)),
    None => (None, None, None),
  };

  let ctx = ferridriver_script::RunContext {
    vars: Arc::new(ferridriver_script::InMemoryVars::new()),
    script_root: setup.script_root.clone(),
    artifacts: setup.artifacts,
    page,
    browser_context,
    request: None,
    browser,
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
    console_sink: (!ui::json()).then(|| Arc::new(console::StreamingConsole) as Arc<dyn ConsoleSink>),
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
  sweep_artifacts(artifacts_budget, artifacts_dir.as_deref()).await;
  // A local run's script launches and owns its own browser, so this process
  // never holds a page to read state from.
  let report = args
    .report
    .then(|| RunReport::collect(code_language, &collected_code, None, setup_secrets));
  report_code_result(&result, &collected_code, report.as_ref())
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
  let sinks = session::RunSinks {
    code: Arc::clone(collected_code),
    // Streaming code to stderr would interleave with a file's contents to
    // no one's benefit; when a file is the destination, that is the only
    // destination.
    echo_code: args.code_out.is_none(),
    ..Default::default()
  };
  let result = session::run_on_session(id, args.context.as_deref(), request, ui::json(), &sinks).await?;
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
  report_code_result(&result, collected_code, report.as_ref())
}

/// Bring the artifacts root back under its configured ceiling, protecting
/// what this run just wrote — the script produced those outputs deliberately,
/// and deleting them on the way out would make the ceiling delete the very
/// thing the run was for.
async fn sweep_artifacts(
  budget: Option<ferridriver::response::OutputBudget>,
  artifacts: Option<&ferridriver_script::OutputDir>,
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
  collected: &Arc<std::sync::Mutex<Vec<String>>>,
  report: Option<&RunReport>,
) -> anyhow::Result<()> {
  let lines = collected
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone();

  if let Some(report) = report {
    let response = build_response(result, report);
    if ui::json() {
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
      eprintln!(
        "{}",
        ui::failure(&format!("{}: {} ({}ms)", error.kind, error.message, result.duration_ms))
      );
      std::process::exit(1);
    }
    return Ok(());
  }

  if !ui::json() || lines.is_empty() {
    return report_result(result);
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
fn report_result(result: &ferridriver_script::ScriptResult) -> anyhow::Result<()> {
  if ui::json() {
    println!("{}", serde_json::to_string_pretty(result)?);
    if let ferridriver_script::Outcome::Error { ref error } = result.outcome {
      eprintln!(
        "{}",
        ui::failure(&format!("{}: {} ({}ms)", error.kind, error.message, result.duration_ms))
      );
      std::process::exit(1);
    }
  } else {
    console::print_result(result);
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
