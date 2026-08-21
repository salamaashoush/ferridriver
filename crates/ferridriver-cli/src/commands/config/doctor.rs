//! `ferridriver doctor` — will this setup actually work?
//!
//! Every check answers a question someone would otherwise answer by running
//! the thing and reading a stack trace: is there a config, do the extensions
//! load, are the sidecars on PATH, is there a browser, will the instances
//! launch. Split from `config` because that command explains a resolution
//! and this one judges it.

use std::path::Path;

use ferridriver_config::FerridriverConfig;
use ferridriver_config::layer;

use crate::cli;
use crate::ui;

use super::{load_options, requirement_issues};

/// How many extension tools `doctor` names before it falls back to a count.
const TOOLS_NAMED: usize = 3;

/// One doctor check outcome.
struct Check {
  name: &'static str,
  status: Status,
  detail: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
  Pass,
  Warn,
  Fail,
}

impl Status {
  /// The status column: a glyph, because the eye finds a shape in a list
  /// faster than it reads a word, and the colour carries the same meaning
  /// again for anyone who has both.
  fn glyph(self) -> String {
    match self {
      Self::Pass => ui::glyph_ok(),
      Self::Warn => ui::glyph_warn(),
      Self::Fail => ui::glyph_fail(),
    }
  }
}

/// `ferridriver doctor`: verify the setup end to end and exit non-zero
/// when something will not work.
pub async fn run(
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
      report(&[Check {
        name: "config",
        status: Status::Fail,
        detail: e.to_string(),
      }])?;
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
      // Short paths: a check line is scanned, not read, and the column it
      // lives in is whatever the terminal has left after the status and the
      // name — an absolute path spends all of it on the part everyone shares.
      detail: resolved
        .layers
        .iter()
        .map(|l| format!("{} ({})", ui::rel_path(&l.path), l.kind.label()))
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
  report(&checks)?;
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
  // A count and a few names. The full list belongs to `ferridriver ext
  // check`, which is the command for reading it; naming all of them here
  // pushed every other check's detail off the line.
  let sample = if tools.len() > TOOLS_NAMED {
    format!(
      "{}, +{} more",
      tools[..TOOLS_NAMED].join(", "),
      tools.len() - TOOLS_NAMED
    )
  } else {
    tools.join(", ")
  };
  checks.push(Check {
    name: "extensions",
    status: if tools.is_empty() { Status::Warn } else { Status::Pass },
    detail: format!("{} file(s), {} tool(s): {sample}", loaded.len(), tools.len()),
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
          detail: format!("{} -> {}", s.name, ui::rel_path(&p)),
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
      detail: format!("{backend:?} -> {}", ui::rel_path(Path::new(&path))),
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
fn report(checks: &[Check]) -> anyhow::Result<()> {
  if ui::json() {
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
    return ui::print_json(&payload);
  }

  // Wrapped, not tabulated: a failing check's detail is the whole answer, and
  // a column that truncates cuts off exactly the half that says what to do.
  let name_width = checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
  let gutter = 3 + name_width + 2;
  let body = ui::width().saturating_sub(gutter);
  for c in checks {
    let lines = ui::wrap(&c.detail, body);
    let name = console::pad_str(c.name, name_width, console::Alignment::Left, None).into_owned();
    ui::say(&format!("{}  {}  {}", c.status.glyph(), ui::bold(&name), lines[0]));
    for line in &lines[1..] {
      ui::say(&format!("{}{line}", " ".repeat(gutter)));
    }
  }

  let fails = checks.iter().filter(|c| c.status == Status::Fail).count();
  let warns = checks.iter().filter(|c| c.status == Status::Warn).count();
  let total = ui::number(checks.len());
  let summary = match (fails, warns) {
    (0, 0) => ui::success(&format!("{total} check(s) passed")),
    (0, w) => ui::warning(&format!("{total} check(s), {} warning(s)", ui::number(w))),
    (f, w) => ui::failure(&format!(
      "{total} check(s), {} failed, {} warning(s)",
      ui::number(f),
      ui::number(w)
    )),
  };
  ui::say(&format!("\n{summary}"));
  if fails > 0 || warns > 0 {
    ui::next_steps(&[
      ("see what resolved", "ferridriver config".to_string()),
      ("install a browser", "ferridriver install chromium".to_string()),
    ]);
  }
  // `doctor` exits through `std::process::exit`, which runs no destructors,
  // and a piped stdout is block-buffered — so the whole report was discarded
  // on exactly the failing runs someone would want the log of.
  std::io::Write::flush(&mut std::io::stdout())?;
  Ok(())
}
