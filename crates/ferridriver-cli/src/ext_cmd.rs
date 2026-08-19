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
  /// What each host's gate decided: the entries it loads, and the specs
  /// it holds back. An entry narrowed with `hosts`, or one whose own
  /// `requires` are unmet, differs per host — reporting one host's
  /// answer as THE answer is what made a narrow declaration look like a
  /// broken package.
  per_host: Vec<(ferridriver_script::ExtensionHost, Vec<PathBuf>, Vec<String>)>,
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
  // The gate every host runs (`ferridriver_script::extension_load`), so
  // what `ext check` reports is what a run would actually load.
  let policy = config.extensions.policy();
  let settings = config.extensions.settings();
  let sidecars: Vec<String> = config.sidecars.iter().map(|s| s.name.clone()).collect();
  let env = ferridriver_script::RequirementEnv {
    policy: &policy,
    allow_env: &config.scripting.allow_env,
    sidecars: &sidecars,
    settings: &settings,
  };
  let per_host: Vec<(ferridriver_script::ExtensionHost, Vec<PathBuf>, Vec<String>)> =
    ferridriver_script::ExtensionHost::ALL
      .iter()
      .map(|host| {
        let g = ferridriver_script::gate(specs, &env, *host);
        (*host, g.files, g.blocked)
      })
      .collect();
  // The report's own shape comes from the script host; what differs per
  // host is carried in `per_host`.
  let gated = ferridriver_script::gate(specs, &env, ferridriver_script::ExtensionHost::Script);
  let ferridriver_script::GatedExtensions {
    resolved,
    resolve_errors,
    issues,
    blocked,
    files,
    all_files: type_files,
    // The claim table's own diagnostics are already folded into
    // `issues`; `ext check` reports them there.
    provided: _,
  } = gated;

  let mut roots: Vec<PathBuf> = Vec::new();
  let mut package_dirs: Vec<PathBuf> = Vec::new();
  for r in &resolved {
    let root = r
      .package_dir
      .clone()
      .or_else(|| r.paths().next().and_then(|f| f.parent().map(Path::to_path_buf)));
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
  }

  Resolution {
    resolved,
    resolve_errors,
    issues,
    blocked,
    per_host,
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
    per_host,
    roots,
    package_dirs,
    files,
    type_files,
  } = resolve_and_gate(config, specs);

  let (registrations, load_errors) = extract_per_host(config, specs, &per_host).await;
  let loaded: Vec<PathBuf> = files.clone();

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
    "hosts": hosts_json(&per_host, &registrations),
    "errors": errors,
    "typecheck": {
      "checker": types.checker,
      "passed": types.passed,
      "skipped": types.skipped,
      "diagnostics": types.diagnostics,
    },
    "ok": ok,
  });
  Report { payload, roots, ok }
}

/// One bundle group's registrations under one host.
///
/// A group, not a file: a package's entries share one rolldown graph and
/// one set of registries, so what they registered cannot be split back
/// apart per file. Reporting it per file would have to invent an
/// attribution, and did — a package's whole contribution showed up under
/// whichever entry happened to be first.
struct CompiledGroup {
  files: Vec<PathBuf>,
  snapshot: ferridriver_script::ExtensionSnapshot,
}

/// What each host compiled.
type Registrations = Vec<(ferridriver_script::ExtensionHost, Vec<CompiledGroup>)>;

/// Compile and extract whatever any host would load.
///
/// One pass per host, because the set of entries differs per host and a
/// package narrowed away on one is still worth reporting on the others.
/// Passes after the first are disk-cache hits for files already seen, so
/// the cost is one compile per distinct file, not four.
async fn extract_per_host(
  config: &FerridriverConfig,
  specs: &[ferridriver_config::ExtensionSpec],
  per_host: &[(ferridriver_script::ExtensionHost, Vec<PathBuf>, Vec<String>)],
) -> (Registrations, Vec<String>) {
  let policy = config.extensions.policy();
  let settings = config.extensions.settings();
  let sidecars: Vec<String> = config.sidecars.iter().map(|s| s.name.clone()).collect();
  let env = ferridriver_script::RequirementEnv {
    policy: &policy,
    allow_env: &config.scripting.allow_env,
    sidecars: &sidecars,
    settings: &settings,
  };

  let mut registrations = Registrations::new();
  let mut errors: Vec<String> = Vec::new();
  for (host, files, _) in per_host {
    if files.is_empty() {
      registrations.push((*host, Vec::new()));
      continue;
    }
    let (_, compiled, failures) = ferridriver_script::extension_load::load(specs, &env, &policy, *host).await;
    registrations.push((
      *host,
      compiled
        .into_iter()
        .map(|cp| CompiledGroup {
          files: cp.files,
          snapshot: cp.snapshot,
        })
        .collect(),
    ));
    for (path, e) in failures {
      let message = format!("{}: {}", path.display(), e.message);
      if !errors.contains(&message) {
        errors.push(message);
      }
    }
  }
  (registrations, errors)
}

/// What each host loads and what it found there.
///
/// The report used to be tool-shaped: it counted `defineTool` and said
/// nothing else, so a package contributing fixtures, steps or config
/// defaults read as an MCP server that had forgotten to register
/// anything. Every kind the extraction snapshot carries is reported, per
/// host, and a kind nobody registered is simply absent.
fn hosts_json(
  per_host: &[(ferridriver_script::ExtensionHost, Vec<PathBuf>, Vec<String>)],
  registrations: &Registrations,
) -> serde_json::Value {
  let mut out = serde_json::Map::new();
  for (host, _, blocked) in per_host {
    let name = host.as_str();
    let groups = registrations
      .iter()
      .find(|(h, _)| h == host)
      .map(|(_, groups)| groups.as_slice())
      .unwrap_or_default();
    let entries: Vec<serde_json::Value> = groups
      .iter()
      .map(|group| {
        let regs = group.snapshot.for_host(name);
        let mut kinds = serde_json::Map::new();
        if let Some(r) = regs {
          for (kind, count) in [
            ("tools", r.tools.len()),
            ("steps", r.steps.len()),
            ("hooks", r.hooks.len()),
            ("paramTypes", r.param_types.len()),
            ("tests", r.tests.len()),
            ("fixtures", r.fixtures.len()),
            ("configDefaults", r.defaults.len()),
          ] {
            if count > 0 {
              kinds.insert(kind.to_string(), count.into());
            }
          }
        }
        serde_json::json!({
          "files": group.files,
          "kinds": kinds,
          "tools": regs.map(|r| r.tools.clone()).unwrap_or_default(),
          "error": regs.and_then(|r| r.error.clone()),
        })
      })
      .collect();
    out.insert(
      name.to_string(),
      serde_json::json!({ "entries": entries, "blocked": blocked }),
    );
  }
  serde_json::Value::Object(out)
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
        "files": r.paths().collect::<Vec<_>>(),
        "entries": r.files,
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
/// Print one cycle's report.
///
/// Flushes explicitly: `ext check` exits through `std::process::exit`,
/// which runs no destructors, so a report piped to a file (the CI /
/// pre-commit shape) was block-buffered and lost on the failing runs.
/// What each host loads, and what it found there.
///
/// Per host, because what a package contributes is a function of the
/// host it loads under, and one host's answer is not the package.
fn print_hosts(out: &mut impl std::io::Write, payload: &serde_json::Value) -> anyhow::Result<()> {
  for (host, view) in payload["hosts"].as_object().into_iter().flatten() {
    let entries = view["entries"].as_array().cloned().unwrap_or_default();
    let blocked = view["blocked"].as_array().map_or(0, Vec::len);
    if entries.is_empty() {
      let why = if blocked > 0 {
        format!(" ({blocked} package(s) held back)")
      } else {
        String::new()
      };
      writeln!(out, "\n{host}: nothing loads{why}")?;
      continue;
    }
    writeln!(out, "\n{host}")?;
    for group in &entries {
      for path in group["files"].as_array().into_iter().flatten() {
        writeln!(out, "  {}", path.as_str().unwrap_or_default())?;
      }
      if let Some(error) = group["error"].as_str() {
        writeln!(out, "    failed: {error}")?;
      }
      let kinds = group["kinds"].as_object().cloned().unwrap_or_default();
      if kinds.is_empty() {
        if group["error"].is_null() {
          writeln!(out, "    registers nothing")?;
        }
      } else {
        let summary: Vec<String> = kinds.iter().map(|(kind, count)| format!("{count} {kind}")).collect();
        writeln!(out, "    {}", summary.join(", "))?;
      }
      for t in group["tools"].as_array().into_iter().flatten() {
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
  }
  Ok(())
}

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
    for entry in spec["entries"].as_array().into_iter().flatten() {
      let narrowed = entry["hosts"]
        .as_array()
        .map(|hosts| {
          let names: Vec<&str> = hosts.iter().filter_map(serde_json::Value::as_str).collect();
          format!("  [{}]", names.join(", "))
        })
        .unwrap_or_default();
      writeln!(out, "    {}{narrowed}", entry["path"].as_str().unwrap_or_default())?;
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

  print_hosts(&mut out, &report.payload)?;

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
