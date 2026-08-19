//! `ferridriver config` and `ferridriver doctor`.
//!
//! Both exist because the config system had no observable surface: the
//! loader picked files, resolved paths and discovered extensions
//! entirely in silence, so a dangling extension path or a config file
//! that was never being read looked identical to a working setup. Every
//! check here answers a question that previously required reading
//! ferridriver's source.

use std::io::Write as _;
use std::path::Path;

use ferridriver_config::FerridriverConfig;
use ferridriver_config::layer::{self, LoadOptions};

use crate::cli::{self, EffectiveBrowser, effective_browser};

/// The load options a run with these global flags would use.
///
/// `--no-inherit` has to reach HERE too: `config` and `doctor` exist to
/// show what a run will actually use, and resolving the full stack while
/// the run resolves one file made them describe a setup nobody was going
/// to get.
/// The startup's own options, so this command explains the stack the run
/// would actually use — including a `.ts` layer, which needs the module
/// loader the startup already installed.
fn load_options(startup: &ferridriver_config::Startup) -> LoadOptions {
  startup.options().clone()
}

/// `ferridriver config`: show the layer stack and what each key
/// resolved to.
///
/// `defaults` is what the configured extension packages contributed
/// through `defineDefaults`, already read by startup's second pass.
/// Without them this command would answer "where did this value come
/// from" while omitting a whole layer.
pub fn run_config(
  startup: &ferridriver_config::Startup,
  defaults: Vec<(String, serde_json::Value)>,
  args: &cli::ConfigArgs,
) -> anyhow::Result<()> {
  let mut options = load_options(startup);
  options.extension_defaults = defaults;
  let resolved = layer::resolve(&options)?;
  let effective = effective_browser(&args.browser, &resolved.config.mcp);

  if args.resolved {
    // TOML is the canonical authoring format, but a merged document can
    // hold shapes TOML cannot express (a null from an explicit JSON
    // null); fall back rather than fail the command.
    let out = match (args.json, toml::to_string_pretty(&resolved.document)) {
      (false, Ok(text)) => text,
      _ => serde_json::to_string_pretty(&resolved.document)?,
    };
    println!("{out}");
    return Ok(());
  }

  let specs = extension_report(&resolved.config);

  if args.json {
    let payload = serde_json::json!({
      "layers": resolved.layers,
      "warnings": resolved.warnings,
      "provenance": resolved
        .provenance
        .iter()
        .map(|(k, v)| (k.clone(), v.describe()))
        .collect::<std::collections::BTreeMap<_, _>>(),
      // Per-key contributor lists for the additive arrays, whose value is
      // the concatenation of several layers rather than any one layer's.
      "contributors": resolved
        .contributors
        .iter()
        .map(|(k, v)| (k.clone(), v.iter().map(layer::Origin::describe).collect::<Vec<_>>()))
        .collect::<std::collections::BTreeMap<_, _>>(),
      "effective": {
        "mcp": {
          "browser": {
            "backend": format!("{:?}", effective.backend),
            "headless": effective.headless,
            "backendFromCli": effective.backend_from_cli,
            "headlessFromCli": effective.headless_from_cli,
          },
        },
      },
      "extensions": specs,
      "document": resolved.document,
    });
    println!("{}", serde_json::to_string_pretty(&payload)?);
    return Ok(());
  }

  print_human_report(&resolved, &effective, &specs)
}

/// Per-spec extension resolution, so the report answers "is this
/// extension actually being found?" without starting a server. Includes
/// the package's `ferridriver` manifest when it declares one, because
/// its `entries` decide which files load and its `requires` decide
/// whether the package can work here at all.
fn extension_report(config: &FerridriverConfig) -> Vec<serde_json::Value> {
  let specs = config.extension_specs();
  let (resolved, errors) = ferridriver_script::discover::resolve_extensions(&specs);
  let issues = requirement_issues(config, &resolved);

  let mut report: Vec<serde_json::Value> = resolved
    .iter()
    .map(|r| {
      let source = ferridriver_mcp::extension::requirements::source_label(r);
      let unmet: Vec<serde_json::Value> = issues
        .iter()
        .filter(|i| i.issue.source == source)
        .map(|i| {
          serde_json::json!({
            "message": i.issue.message,
            "blocking": i.issue.blocking,
            "hosts": i.hosts,
          })
        })
        .collect();
      serde_json::json!({
        "spec": r.spec,
        "baseDir": r.base_dir,
        "packageDir": r.package_dir,
        "files": r.paths().collect::<Vec<_>>(),
        "entries": r.files,
        "manifest": r.manifest,
        "requirements": unmet,
        "error": serde_json::Value::Null,
      })
    })
    .collect();

  report.extend(errors.iter().map(|(spec, e)| {
    let base = specs.iter().find(|s| &s.spec == spec).map(|s| s.base_dir.clone());
    serde_json::json!({
      "spec": spec,
      "baseDir": base,
      "files": [],
      "error": e.message,
    })
  }));
  report
}

/// Evaluate every resolved package's declared requirements, on every
/// host, and say which hosts each issue applies to.
///
/// Per host, because `requires` is scoped to the entries that load: an
/// entry narrowed with `hosts` states preconditions only where it runs,
/// so an issue that blocks the MCP server may be no issue at all under
/// `ferridriver test`. Reporting the union without saying WHERE would be
/// the old package-wide answer wearing a new shape.
fn requirement_issues(
  config: &FerridriverConfig,
  resolved: &[ferridriver_script::ResolvedExtension],
) -> Vec<HostedIssue> {
  let policy = config.extensions.policy();
  let settings = config.extensions.settings();
  let sidecars: Vec<String> = config.sidecars.iter().map(|s| s.name.clone()).collect();
  let env = ferridriver_mcp::extension::RequirementEnv {
    policy: &policy,
    allow_env: &config.scripting.allow_env,
    sidecars: &sidecars,
    settings: &settings,
  };

  let mut out: Vec<HostedIssue> = Vec::new();
  for host in ferridriver_script::ExtensionHost::ALL {
    for issue in ferridriver_mcp::extension::requirements::check(resolved, &env, *host) {
      if let Some(existing) = out
        .iter_mut()
        .find(|h| h.issue.source == issue.source && h.issue.message == issue.message)
      {
        existing.hosts.push(host.as_str().to_string());
      } else {
        out.push(HostedIssue {
          issue,
          hosts: vec![host.as_str().to_string()],
        });
      }
    }
  }
  out
}

/// One requirement issue plus the hosts it applies to.
struct HostedIssue {
  issue: ferridriver_mcp::extension::RequirementIssue,
  hosts: Vec<String>,
}

impl HostedIssue {
  /// How the hosts read in a report: nothing when it applies everywhere,
  /// since that is the ordinary case and naming all four would only add
  /// noise to it.
  fn host_suffix(&self) -> String {
    if self.hosts.len() == ferridriver_script::ExtensionHost::ALL.len() {
      String::new()
    } else {
      format!(" (on {})", self.hosts.join(", "))
    }
  }
}

fn print_human_report(
  resolved: &layer::Resolved,
  effective: &EffectiveBrowser,
  specs: &[serde_json::Value],
) -> anyhow::Result<()> {
  let mut out = std::io::stdout().lock();

  writeln!(out, "Layers (lowest precedence first)")?;
  if resolved.layers.is_empty() {
    writeln!(
      out,
      "  (none — running on built-in defaults; no ferridriver.{{toml,yaml,yml,json}} found)"
    )?;
  }
  for l in &resolved.layers {
    writeln!(out, "  {:<9} {}", l.kind.label(), l.path.display())?;
  }

  writeln!(out, "\nEffective browser (CLI flags override the file)")?;
  writeln!(
    out,
    "  backend    {:?}{}",
    effective.backend,
    if effective.backend_from_cli {
      "  (from --backend)"
    } else {
      ""
    }
  )?;
  writeln!(
    out,
    "  headless   {}{}",
    effective.headless,
    if effective.headless_from_cli {
      "  (from --headless/--headed)"
    } else {
      ""
    }
  )?;

  writeln!(out, "\nExtensions")?;
  if specs.is_empty() {
    writeln!(out, "  (none configured)")?;
  }
  for s in specs {
    let spec = s["spec"].as_str().unwrap_or_default();
    if let Some(err) = s["error"].as_str() {
      writeln!(out, "  {spec}\n    UNRESOLVED: {err}")?;
      continue;
    }
    let count = s["files"].as_array().map_or(0, Vec::len);
    writeln!(out, "  {spec}\n    {count} entry file(s), base {}", s["baseDir"])?;
    if s["manifest"].is_object() {
      let entries = s["manifest"]["entries"].as_array().map_or(0, Vec::len);
      writeln!(out, "    package manifest: {entries} declared entry/entries")?;
    }
    for issue in s["requirements"].as_array().into_iter().flatten() {
      let blocking = issue["blocking"] == serde_json::Value::Bool(true);
      writeln!(
        out,
        "    {} {}",
        if blocking { "UNMET:" } else { "note: " },
        issue["message"].as_str().unwrap_or_default()
      )?;
    }
  }

  writeln!(out, "\nResolved values (key = value  <- source)")?;
  for (key, origin) in &resolved.provenance {
    let value = value_at(&resolved.document, key);
    // An appended array belongs to every layer that added to it; naming
    // only the last one sent people editing the wrong file.
    let source = match resolved.contributors.get(key) {
      Some(list) if list.len() > 1 => list.iter().map(layer::Origin::describe).collect::<Vec<_>>().join(" + "),
      _ => origin.describe(),
    };
    writeln!(out, "  {key} = {value}  <- {source}")?;
  }

  if !resolved.warnings.is_empty() {
    writeln!(out, "\nWarnings")?;
    for w in &resolved.warnings {
      writeln!(out, "  {}: {}", w.source, w.message)?;
    }
  }
  Ok(())
}

/// Read a dotted key out of the merged document for display.
fn value_at(document: &serde_json::Value, dotted: &str) -> String {
  let mut current = document;
  for segment in dotted.split('.') {
    match current.get(segment) {
      Some(next) => current = next,
      None => return "?".to_string(),
    }
  }
  let rendered = current.to_string();
  // Instructions blocks and host-resolver rules are kilobytes long;
  // a report that scrolls them off the screen is not a report.
  //
  // Truncated on a CHARACTER boundary: a byte slice panics the whole
  // command the moment a value carries a multi-byte character, which
  // server instructions and any non-ASCII path routinely do.
  let count = rendered.chars().count();
  if count <= VALUE_DISPLAY_LIMIT {
    return rendered;
  }
  let head: String = rendered.chars().take(VALUE_DISPLAY_LIMIT - 3).collect();
  format!("{head}… ({count} chars)")
}

/// Longest value `ferridriver config` prints in full, in characters.
const VALUE_DISPLAY_LIMIT: usize = 120;

/// One doctor check outcome.
struct Check {
  name: &'static str,
  status: Status,
  detail: String,
}

#[derive(PartialEq, Eq)]
enum Status {
  Pass,
  Warn,
  Fail,
}

impl Status {
  fn label(&self) -> &'static str {
    match self {
      Self::Pass => "ok  ",
      Self::Warn => "warn",
      Self::Fail => "FAIL",
    }
  }
}

/// `ferridriver doctor`: verify the setup end to end and exit non-zero
/// when something will not work.
pub async fn run_doctor(
  startup: &ferridriver_config::Startup,
  defaults: Vec<(String, serde_json::Value)>,
  args: cli::DoctorArgs,
) -> anyhow::Result<()> {
  let mut checks = Vec::new();

  let mut options = load_options(startup);
  options.extension_defaults = defaults;
  let resolved = match layer::resolve(&options) {
    Ok(r) => r,
    Err(e) => {
      // A config that does not parse is the whole answer; nothing
      // downstream is meaningful.
      report(
        &[Check {
          name: "config",
          status: Status::Fail,
          detail: e.to_string(),
        }],
        args.json,
      )?;
      std::process::exit(1);
    },
  };

  checks.push(if resolved.layers.is_empty() {
    Check {
      name: "config",
      status: Status::Warn,
      detail: "no config file found; running on built-in defaults. \
               Add ferridriver.toml, or ~/.config/ferridriver/config.yaml for machine-wide settings."
        .to_string(),
    }
  } else {
    Check {
      name: "config",
      status: Status::Pass,
      detail: resolved
        .layers
        .iter()
        .map(|l| format!("{} ({})", l.path.display(), l.kind.label()))
        .collect::<Vec<_>>()
        .join(", "),
    }
  });

  for w in &resolved.warnings {
    checks.push(Check {
      name: "config keys",
      status: Status::Warn,
      detail: format!("{}: {}", w.source, w.message),
    });
  }

  checks.extend(check_extensions(&resolved.config).await);

  // The remaining checks stat the filesystem, walk PATH, and (with
  // `--instances`) run the operator's own commands, which may poll for a
  // browser for seconds. Off the reactor: a `doctor` that blocks it stalls
  // the very extension-loading tasks whose results it is about to report.
  let config = resolved.config;
  let instances = args.instances;
  let (blocking_checks, config) = tokio::task::spawn_blocking(move || {
    let mut out = check_roots(&config);
    out.extend(check_sidecars(&config));
    out.extend(check_browser(&config));
    if instances {
      out.extend(check_instances(&config));
    }
    (out, config)
  })
  .await
  .map_err(|e| anyhow::anyhow!("doctor checks failed: {e}"))?;
  drop(config);
  checks.extend(blocking_checks);

  let failed = checks.iter().any(|c| c.status == Status::Fail);
  report(&checks, args.json)?;
  if failed {
    std::process::exit(1);
  }
  Ok(())
}

/// Resolve AND load every configured extension. Loading is the only
/// honest check: a path can exist and still fail to bundle, and a file
/// that declares no tool is dead weight the MCP server would skip with
/// a log line nobody sees.
async fn check_extensions(config: &FerridriverConfig) -> Vec<Check> {
  let specs = config.extension_specs();
  if specs.is_empty() {
    return vec![Check {
      name: "extensions",
      status: Status::Pass,
      detail: "none configured".to_string(),
    }];
  }

  let mut checks = Vec::new();
  let (resolved, errors) = ferridriver_script::discover::resolve_extensions(&specs);
  for (spec, e) in &errors {
    let base = specs.iter().find(|s| &s.spec == spec).map(|s| s.base_dir.clone());
    checks.push(Check {
      name: "extensions",
      status: Status::Fail,
      detail: format!(
        "`{spec}` does not resolve from {}: {}",
        base.unwrap_or_default().display(),
        e.message
      ),
    });
  }

  // A package that declares unmet requirements is not loaded, so report
  // the requirement rather than a downstream "no tools" symptom.
  let issues = requirement_issues(config, &resolved);
  let flat: Vec<ferridriver_mcp::extension::RequirementIssue> = issues.iter().map(|h| h.issue.clone()).collect();
  // `doctor` answers "will this setup work", and a package blocked on
  // ANY host is one the operator has to hear about — the per-host detail
  // is on the message, not on whether it is reported.
  let blocked = ferridriver_mcp::extension::requirements::blocked_specs(&resolved, &flat);
  for issue in &issues {
    checks.push(Check {
      name: "extension requires",
      status: if issue.issue.blocking {
        Status::Fail
      } else {
        Status::Warn
      },
      detail: format!("{}: {}{}", issue.issue.source, issue.issue.message, issue.host_suffix()),
    });
  }

  let mut all_files = Vec::new();
  for r in &resolved {
    if r.files.is_empty() {
      checks.push(Check {
        name: "extensions",
        status: Status::Fail,
        detail: format!("`{}` resolved to no source files", r.spec),
      });
      continue;
    }
    if blocked.contains(&r.spec) {
      continue;
    }
    all_files.extend(r.files.iter().cloned());
  }

  if all_files.is_empty() {
    return checks;
  }

  let paths: Vec<std::path::PathBuf> = all_files.iter().map(|e| e.path.clone()).collect();
  let (loaded, errors) = ferridriver_mcp::extension::load_all(&paths, &config.extensions.policy()).await;
  for e in &errors {
    checks.push(Check {
      name: "extensions",
      status: Status::Fail,
      detail: e.to_string(),
    });
  }
  let tools: Vec<String> = loaded
    .iter()
    .flat_map(|f| f.tools.iter().map(|t| t.name.clone()))
    .collect();
  checks.push(Check {
    name: "extensions",
    status: if tools.is_empty() { Status::Warn } else { Status::Pass },
    detail: format!(
      "{} file(s), {} tool(s): {}",
      loaded.len(),
      tools.len(),
      tools.join(", ")
    ),
  });
  checks
}

/// Report whether the sandbox roots exist or could be created.
///
/// Read-only on purpose: a diagnostic that silently creates directories
/// makes the setup look healthier than it is, and leaves `.ferridriver/`
/// trees behind in whatever directory the operator happened to run it
/// from. The nearest existing ancestor's writability answers the same
/// question without touching the disk.
fn check_roots(config: &FerridriverConfig) -> Vec<Check> {
  let mut checks = Vec::new();
  let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
  for (name, path) in [
    ("scriptRoot", config.script_root()),
    ("artifactsRoot", config.artifacts_root()),
  ] {
    // The defaults (`.ferridriver/scripts`) are relative to the run's
    // directory. Walking a relative path's ancestors bottoms out at the
    // empty path, which is no directory at all — reported as "no existing
    // parent" for a root that would have been created fine.
    let absolute = if path.is_absolute() {
      path.clone()
    } else {
      cwd.join(&path)
    };
    let (status, detail) = if absolute.is_dir() {
      if writable(&absolute) {
        (Status::Pass, format!("{name} {}", path.display()))
      } else {
        (
          Status::Fail,
          format!("{name} {} exists but is not writable", path.display()),
        )
      }
    } else {
      match absolute.ancestors().find(|a| a.is_dir()) {
        Some(base) if writable(base) => (Status::Pass, format!("{name} {} (will be created)", path.display())),
        Some(base) => (
          Status::Fail,
          format!(
            "{name} {} cannot be created: {} is not writable",
            path.display(),
            base.display()
          ),
        ),
        None => (
          Status::Fail,
          format!("{name} {} has no existing parent directory", path.display()),
        ),
      }
    };
    checks.push(Check {
      name: "sandbox roots",
      status,
      detail,
    });
  }
  checks
}

/// Whether this process may create entries in `dir`.
///
/// A real create, because the mode bits alone answer the wrong question:
/// a directory owned by another user is `0755` and reads as writable
/// while every write fails. The probe file removes itself on drop, so
/// nothing is left behind and no configured directory is created.
fn writable(dir: &Path) -> bool {
  tempfile::Builder::new()
    .prefix(".ferridriver-doctor-")
    .tempfile_in(dir)
    .is_ok()
}

fn check_sidecars(config: &FerridriverConfig) -> Vec<Check> {
  config
    .sidecars
    .iter()
    .map(|s| {
      let program = s.command.first().cloned().unwrap_or_default();
      match which::which(&program) {
        Ok(p) => Check {
          name: "sidecars",
          status: Status::Pass,
          detail: format!("{} -> {}", s.name, p.display()),
        },
        Err(_) => Check {
          name: "sidecars",
          status: Status::Fail,
          detail: format!("{}: `{program}` is not on PATH", s.name),
        },
      }
    })
    .collect()
}

fn check_browser(config: &FerridriverConfig) -> Vec<Check> {
  let backend = config.mcp.backend_kind();
  let explicit = config.mcp.browser.executable_path.clone();
  if let Some(path) = explicit {
    let exists = Path::new(&path).is_file();
    return vec![Check {
      name: "browser",
      status: if exists { Status::Pass } else { Status::Fail },
      detail: format!(
        "{:?}: executablePath {path}{}",
        backend,
        if exists { "" } else { " does not exist" }
      ),
    }];
  }

  vec![match installed_browser(backend) {
    Some(path) => Check {
      name: "browser",
      status: Status::Pass,
      detail: format!("{backend:?} -> {path}"),
    },
    None => Check {
      name: "browser",
      status: Status::Fail,
      detail: format!(
        "{backend:?}: no browser found. Install one with `ferridriver install --with-deps {}`",
        match backend {
          ferridriver::backend::BackendKind::Bidi => "firefox",
          ferridriver::backend::BackendKind::WebKit => "webkit",
          _ => "chromium",
        }
      ),
    },
  }]
}

/// Locate the browser binary a backend would launch.
fn installed_browser(backend: ferridriver::backend::BackendKind) -> Option<String> {
  use ferridriver::backend::BackendKind;
  let installer = ferridriver::install::BrowserInstaller::new();
  match backend {
    BackendKind::Bidi => installer.find_installed_firefox(),
    // WebKit runs Playwright's `pw_run.sh`, not a bundle we install
    // ourselves, so ask the launcher where it is.
    BackendKind::WebKit => ferridriver::backend::webkit::launcher::locate_binary()
      .ok()
      .map(|p| p.display().to_string()),
    BackendKind::CdpPipe | BackendKind::CdpRaw => installer.find_installed_chromium(),
  }
}

/// Run each configured instance's args/discover command. Opt-in: these
/// shell out, and a discover command may block while it polls for a
/// browser that is not running yet.
fn check_instances(config: &FerridriverConfig) -> Vec<Check> {
  let mut names: Vec<String> = config.mcp.browser.instances.keys().cloned().collect();
  names.sort();
  if names.is_empty() && config.mcp.browser.instance_args_command.is_some() {
    // Nothing is enumerable when instances come from a command; at
    // least prove the command runs for the default instance.
    names.push("default".to_string());
  }

  let mut checks = Vec::new();
  // Instance name per resolved profile directory, so a directory two
  // instances would both launch into is named rather than discovered as
  // a Chrome "profile is already in use" failure at run time.
  let mut profiles: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();

  for name in &names {
    if let Err(e) = config.mcp.instance_health(name) {
      checks.push(Check {
        name: "instances",
        status: Status::Fail,
        detail: format!("{name}: {e}"),
      });
      continue;
    }
    let overrides = match config.mcp.instance_overrides(name) {
      Ok(o) => o,
      Err(e) => {
        checks.push(Check {
          name: "instances",
          status: Status::Fail,
          detail: format!("{name}: {e}"),
        });
        continue;
      },
    };

    if let Some(dir) = overrides.user_data_dir.clone()
      && let Some(other) = profiles.insert(dir.clone(), name.clone())
    {
      checks.push(Check {
        name: "instances",
        status: Status::Fail,
        detail: format!(
          "{name} and {other} both launch with profile {dir}; a Chrome profile serves one process. \
           Put `${{INSTANCE}}` in the path, or give each instance its own userDataDir."
        ),
      });
    }

    let resolved = config.mcp.resolve_instance(name);
    checks.push(Check {
      name: "instances",
      status: Status::Pass,
      detail: format!(
        "{name}: {} arg(s), profile {}, {}",
        overrides.args.len(),
        overrides.user_data_dir.as_deref().unwrap_or("(throwaway)"),
        match resolved {
          Some(ferridriver::state::ConnectMode::ConnectUrl(url)) => format!("connect {url}"),
          Some(_) => "connect (discovered)".to_string(),
          None => "would launch a new browser".to_string(),
        }
      ),
    });
  }
  checks
}

/// Print the checks.
///
/// Flushes explicitly: `doctor` exits through `std::process::exit`, which
/// runs no destructors, and piping the command to a file (the CI shape)
/// makes stdout block-buffered — so the whole report was discarded on
/// exactly the failing runs someone would want the log of.
fn report(checks: &[Check], json: bool) -> anyhow::Result<()> {
  let mut out = std::io::stdout().lock();
  if json {
    let payload: Vec<serde_json::Value> = checks
      .iter()
      .map(|c| {
        serde_json::json!({
          "check": c.name,
          "status": match c.status { Status::Pass => "pass", Status::Warn => "warn", Status::Fail => "fail" },
          "detail": c.detail,
        })
      })
      .collect();
    writeln!(out, "{}", serde_json::to_string_pretty(&payload)?)?;
    out.flush()?;
    return Ok(());
  }

  for c in checks {
    writeln!(out, "[{}] {:<14} {}", c.status.label(), c.name, c.detail)?;
  }
  let fails = checks.iter().filter(|c| c.status == Status::Fail).count();
  let warns = checks.iter().filter(|c| c.status == Status::Warn).count();
  writeln!(
    out,
    "\n{} check(s): {} failed, {} warning(s)",
    checks.len(),
    fails,
    warns
  )?;
  out.flush()?;
  Ok(())
}
