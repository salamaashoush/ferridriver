//! `ferridriver ext` — the extension authoring commands.
//!
//! Writing an extension used to mean: edit, restart the MCP client (which
//! drops every browser session), read a log line, guess. Everything the
//! host knows about an extension — the entry files a package manifest
//! resolves to, the tools it registers, the capabilities each declared,
//! the requirements the host does not satisfy, the bundle error, and now
//! the TypeScript errors inside handlers that never ran — is reported in
//! one pass.
//!
//! `check` runs that pass once and exits non-zero on failure; `dev` runs
//! it in a watch loop. Both use the same resolution, requirement gate and
//! load the MCP server runs, so a green report means the server will load
//! the same set.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use ferridriver_config::FerridriverConfig;

use crate::cli;
use crate::ext_typecheck;
use crate::ext_types;

pub async fn run(config: FerridriverConfig, args: cli::ExtArgs) -> anyhow::Result<()> {
  match args.command {
    cli::ExtCommand::Check(check) => Box::pin(check_once(config, check)).await,
    cli::ExtCommand::Dev(mut dev) => {
      dev.watch = true;
      Box::pin(check_loop(config, dev)).await
    },
    cli::ExtCommand::Types(types) => write_types(types),
  }
}

/// `ferridriver ext types`: drop the declarations where an editor's
/// TypeScript will resolve them without an install step.
fn write_types(args: cli::ExtTypesArgs) -> anyhow::Result<()> {
  let root = match args.out {
    Some(dir) => dir,
    None => std::env::current_dir()?.join("node_modules"),
  };
  let written = ext_types::materialize(&root)?;
  let mut out = std::io::stdout().lock();
  for (name, path) in &written {
    writeln!(out, "{name} -> {}", path.display())?;
  }
  writeln!(
    out,
    "\nImport types with `import type {{ ToolContext }} from '@ferridriver/extension'`; \
     `defineTool` is a global."
  )?;
  Ok(())
}

/// One report cycle's outcome.
struct Report {
  payload: serde_json::Value,
  /// Roots to watch: an entry file's directory, or a package's own dir.
  roots: Vec<PathBuf>,
  ok: bool,
}

async fn check_once(config: FerridriverConfig, args: cli::ExtCheckArgs) -> anyhow::Result<()> {
  if args.watch {
    return Box::pin(check_loop(config, args)).await;
  }
  let specs = specs_from(&config, &args.paths)?;
  let report = Box::pin(build_report(&config, &specs, !args.no_typecheck)).await;
  print_report(&report, args.json)?;
  if report.ok {
    return Ok(());
  }
  std::process::exit(1);
}

async fn check_loop(config: FerridriverConfig, args: cli::ExtCheckArgs) -> anyhow::Result<()> {
  let specs = specs_from(&config, &args.paths)?;
  let mut report = Box::pin(build_report(&config, &specs, !args.no_typecheck)).await;
  print_report(&report, args.json)?;

  let mut watched: Vec<PathBuf> = Vec::new();
  let mut watchers = watch_roots(&report.roots, &mut watched)?;

  let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
  loop {
    println!("\n[ext dev] watching {} root(s) (Ctrl-C to quit)\n", watchers.len());
    let changed = {
      let recvs: Vec<_> = watchers.iter().map(|w| Box::pin(w.recv())).collect();
      tokio::select! {
        _ = tokio::signal::ctrl_c() => return Ok(()),
        _ = sigterm.recv() => return Ok(()),
        (change, _, _) = futures::future::select_all(recvs) => change,
      }
    };
    if changed.is_none() {
      return Ok(());
    }
    for w in &watchers {
      let _ = w.drain_deduped();
    }

    // Re-resolve from scratch: a manifest edit can change the entry set,
    // so reusing the previous file list would keep loading stale entries.
    let specs = specs_from(&config, &args.paths)?;
    report = Box::pin(build_report(&config, &specs, !args.no_typecheck)).await;
    print_report(&report, args.json)?;

    // ...and the root set with it: adding an `entries` item under a new
    // directory, or fixing the spec that made a package resolve at all,
    // introduces a root the original watcher list does not cover, so
    // edits there would never trigger another run.
    if report.roots != watched {
      watchers = watch_roots(&report.roots, &mut watched)?;
    }
  }
}

/// Watch each root, recording which ones were requested so the next cycle
/// can tell whether the set moved.
fn watch_roots(
  roots: &[PathBuf],
  watched: &mut Vec<PathBuf>,
) -> anyhow::Result<Vec<ferridriver_test::watch::FileWatcher>> {
  let mut watchers = Vec::new();
  for root in roots {
    match ferridriver_test::watch::FileWatcher::new(root, &[], &[]) {
      Ok(w) => watchers.push(w),
      Err(e) => eprintln!("[ext dev] cannot watch {}: {e}", root.display()),
    }
  }
  if watchers.is_empty() {
    anyhow::bail!("no watchable extension roots");
  }
  watched.clear();
  watched.extend_from_slice(roots);
  Ok(watchers)
}

/// The specs to load: CLI paths (anchored to the cwd) when given,
/// otherwise the resolved config's `extensions`.
fn specs_from(config: &FerridriverConfig, paths: &[String]) -> anyhow::Result<Vec<ferridriver_config::ExtensionSpec>> {
  if paths.is_empty() {
    let specs = config.extension_specs();
    if specs.is_empty() {
      anyhow::bail!(
        "no extensions to check: pass a path/package, or set `extensions` in ferridriver.toml \
         (see `ferridriver config`)"
      );
    }
    return Ok(specs);
  }
  let cwd = std::env::current_dir()?;
  Ok(
    paths
      .iter()
      .map(|p| ferridriver_config::ExtensionSpec {
        // A bare `./x` from the shell means "relative to where I am",
        // never to whichever config layer happened to declare things.
        spec: p.clone(),
        base_dir: cwd.clone(),
      })
      .collect(),
  )
}

/// What resolution + the requirement gate decided, before anything is
/// loaded or type-checked.
struct Resolution {
  resolved: Vec<ferridriver_script::ResolvedExtension>,
  resolve_errors: Vec<(String, ferridriver_script::error::ScriptError)>,
  issues: Vec<ferridriver_mcp::extension::RequirementIssue>,
  blocked: Vec<String>,
  /// Directories to watch: a package's own dir, or an entry's parent.
  roots: Vec<PathBuf>,
  /// Package directories, for `tsconfig` inheritance and checker lookup.
  package_dirs: Vec<PathBuf>,
  /// Entry files of the packages that survived the gate — what gets loaded.
  files: Vec<PathBuf>,
  /// Every resolved entry file, gate or no gate. Type checking is static:
  /// an author fixing a type error should not first have to satisfy the
  /// package's runtime requirements (a sidecar, a binary on PATH).
  type_files: Vec<PathBuf>,
}

fn resolve_and_gate(config: &FerridriverConfig, specs: &[ferridriver_config::ExtensionSpec]) -> Resolution {
  let (resolved, resolve_errors) = ferridriver_script::discover::resolve_extensions(specs);

  let policy = config.extensions.policy();
  let settings = config.extensions.settings();
  let sidecars: Vec<String> = config.sidecars.iter().map(|s| s.name.clone()).collect();
  let issues = ferridriver_mcp::extension::requirements::check(
    &resolved,
    &ferridriver_mcp::extension::RequirementEnv {
      policy: &policy,
      allow_env: &config.scripting.allow_env,
      sidecars: &sidecars,
      settings: &settings,
    },
  );
  let blocked = ferridriver_mcp::extension::requirements::blocked_specs(&resolved, &issues);

  let mut roots: Vec<PathBuf> = Vec::new();
  let mut package_dirs: Vec<PathBuf> = Vec::new();
  let mut files: Vec<PathBuf> = Vec::new();
  let mut type_files: Vec<PathBuf> = Vec::new();
  for r in &resolved {
    let root = r
      .package_dir
      .clone()
      .or_else(|| r.files.first().and_then(|f| f.parent().map(Path::to_path_buf)));
    if let Some(root) = root
      && !roots.contains(&root)
    {
      roots.push(root);
    }
    if let Some(dir) = &r.package_dir
      && !package_dirs.contains(dir)
    {
      package_dirs.push(dir.clone());
    }
    for f in &r.files {
      if !type_files.contains(f) {
        type_files.push(f.clone());
      }
    }
    if blocked.contains(&r.spec) {
      continue;
    }
    for f in &r.files {
      if !files.contains(f) {
        files.push(f.clone());
      }
    }
  }

  Resolution {
    resolved,
    resolve_errors,
    issues,
    blocked,
    roots,
    package_dirs,
    files,
    type_files,
  }
}

async fn build_report(
  config: &FerridriverConfig,
  specs: &[ferridriver_config::ExtensionSpec],
  typecheck: bool,
) -> Report {
  let Resolution {
    resolved,
    resolve_errors,
    issues,
    blocked,
    roots,
    package_dirs,
    files,
    type_files,
  } = resolve_and_gate(config, specs);

  let (loaded, load_errors) = if files.is_empty() {
    (Vec::new(), Vec::new())
  } else {
    ferridriver_mcp::extension::load_all(&files, &config.extensions.policy()).await
  };

  // The scratch dir holds the generated tsconfig + the embedded
  // declarations; it must outlive the compiler run.
  let scratch = if typecheck && !type_files.is_empty() {
    tempfile::tempdir().ok()
  } else {
    None
  };
  let types = match &scratch {
    Some(dir) => ext_typecheck::run(&type_files, &package_dirs, dir.path()),
    None => ext_typecheck::TypecheckOutcome {
      checker: None,
      diagnostics: Vec::new(),
      passed: true,
      skipped: Some(if typecheck {
        "nothing to check".to_string()
      } else {
        "--no-typecheck".to_string()
      }),
    },
  };

  let mut errors: Vec<String> = resolve_errors
    .iter()
    .map(|(spec, e)| format!("{spec}: {}", e.message))
    .collect();
  errors.extend(load_errors.iter().map(ToString::to_string));
  errors.extend(
    issues
      .iter()
      .filter(|i| i.blocking)
      .map(|i| format!("{}: {}", i.source, i.message)),
  );
  let ok = errors.is_empty() && !loaded.is_empty() && types.passed;

  let payload = serde_json::json!({
    "specs": specs_json(&resolved, &issues, &blocked),
    "loaded": loaded_json(&loaded),
    "errors": errors,
    "typecheck": {
      "checker": types.checker,
      "passed": types.passed,
      "skipped": types.skipped,
      "diagnostics": types.diagnostics,
    },
    "toolCount": loaded.iter().map(|f| f.tools.len()).sum::<usize>(),
    "ok": ok,
  });
  Report { payload, roots, ok }
}

/// Per-spec resolution + gate result, for the report.
fn specs_json(
  resolved: &[ferridriver_script::ResolvedExtension],
  issues: &[ferridriver_mcp::extension::RequirementIssue],
  blocked: &[String],
) -> Vec<serde_json::Value> {
  resolved
    .iter()
    .map(|r| {
      let source = ferridriver_mcp::extension::requirements::source_label(r);
      serde_json::json!({
        "spec": r.spec,
        "packageDir": r.package_dir,
        "files": r.files,
        "manifest": r.manifest,
        "blocked": blocked.contains(&r.spec),
        "requirements": issues
          .iter()
          .filter(|i| i.source == source)
          .map(|i| serde_json::json!({ "message": i.message, "blocking": i.blocking }))
          .collect::<Vec<_>>(),
      })
    })
    .collect()
}

/// Per-file tool manifests, for the report.
fn loaded_json(loaded: &[ferridriver_mcp::extension::LoadedExtension]) -> Vec<serde_json::Value> {
  loaded
    .iter()
    .map(|f| {
      serde_json::json!({
        "path": f.path,
        "tools": f.tools.iter().map(|t| {
          let mut commands: Vec<&String> = t.allow.commands.keys().collect();
          commands.sort();
          serde_json::json!({
            "name": t.name,
            "title": t.title,
            "description": t.description,
            "exposeAsMcpTool": t.expose_as_mcp_tool,
            "timeoutMs": t.timeout_ms,
            "allow": { "commands": commands, "net": t.allow.net },
            "inputSchema": t.input_schema,
            "outputSchema": t.output_schema,
          })
        }).collect::<Vec<_>>(),
      })
    })
    .collect()
}

/// Print one cycle's report.
///
/// Flushes explicitly: `ext check` exits through `std::process::exit`,
/// which runs no destructors, so a report piped to a file (the CI /
/// pre-commit shape) was block-buffered and lost on the failing runs.
fn print_report(report: &Report, json: bool) -> anyhow::Result<()> {
  let mut out = std::io::stdout().lock();
  if json {
    writeln!(out, "{}", serde_json::to_string_pretty(&report.payload)?)?;
    out.flush()?;
    return Ok(());
  }

  for spec in report.payload["specs"].as_array().into_iter().flatten() {
    let name = spec["spec"].as_str().unwrap_or_default();
    let count = spec["files"].as_array().map_or(0, Vec::len);
    writeln!(out, "{name}")?;
    if let Some(dir) = spec["packageDir"].as_str() {
      let entries = spec["manifest"]["entries"].as_array().map_or(0, Vec::len);
      writeln!(out, "  package {dir} ({entries} declared entry/entries)")?;
    }
    writeln!(out, "  {count} entry file(s)")?;
    for f in spec["files"].as_array().into_iter().flatten() {
      writeln!(out, "    {}", f.as_str().unwrap_or_default())?;
    }
    for issue in spec["requirements"].as_array().into_iter().flatten() {
      let blocking = issue["blocking"] == serde_json::Value::Bool(true);
      writeln!(
        out,
        "  {} {}",
        if blocking { "UNMET:" } else { "note: " },
        issue["message"].as_str().unwrap_or_default()
      )?;
    }
    if spec["blocked"] == serde_json::Value::Bool(true) {
      writeln!(out, "  SKIPPED: requirements above are unmet")?;
    }
  }

  let types = &report.payload["typecheck"];
  writeln!(out, "\nTypes")?;
  match (types["checker"].as_str(), types["skipped"].as_str()) {
    (_, Some(reason)) => writeln!(out, "  skipped: {reason}")?,
    (Some(checker), None) => {
      if types["passed"] == serde_json::Value::Bool(true) {
        writeln!(out, "  {checker}: no errors")?;
      } else {
        writeln!(out, "  {checker}:")?;
        for d in types["diagnostics"].as_array().into_iter().flatten() {
          writeln!(out, "    {}", d.as_str().unwrap_or_default())?;
        }
      }
    },
    (None, None) => writeln!(out, "  no checker available")?,
  }

  writeln!(out, "\nTools ({})", report.payload["toolCount"])?;
  for file in report.payload["loaded"].as_array().into_iter().flatten() {
    writeln!(out, "  {}", file["path"].as_str().unwrap_or_default())?;
    for t in file["tools"].as_array().into_iter().flatten() {
      let promoted = if t["exposeAsMcpTool"] == serde_json::Value::Bool(true) {
        "mcp tool"
      } else {
        "binding only"
      };
      writeln!(out, "    {} [{promoted}]", t["name"].as_str().unwrap_or_default())?;
      let commands = t["allow"]["commands"].as_array().map_or(0, Vec::len);
      let net = t["allow"]["net"].as_array().map_or(0, Vec::len);
      if commands > 0 || net > 0 {
        writeln!(out, "      allow: {commands} command(s), {net} net host(s)")?;
      }
    }
  }

  let errors = report.payload["errors"].as_array().cloned().unwrap_or_default();
  if !errors.is_empty() {
    writeln!(out, "\nErrors")?;
    for e in &errors {
      writeln!(out, "  {}", e.as_str().unwrap_or_default())?;
    }
  }
  writeln!(out, "\n{}", if report.ok { "ok" } else { "FAILED" })?;
  out.flush()?;
  Ok(())
}
