//! `ferridriver config` — explain the configuration a run would see.
//!
//! It exists because the config system had no observable surface: the loader
//! picked files, resolved paths and discovered extensions entirely in
//! silence, so a dangling extension path or a config file that was never
//! being read looked identical to a working setup.
//!
//! The report answers one question per section: which files were read, what
//! they add up to, and which file set each key. [`doctor`] is the other half
//! — it takes the same resolution and asks whether it will actually work.

pub mod doctor;

use ferridriver_config::FerridriverConfig;
use ferridriver_config::layer::{self, LoadOptions};

use crate::cli::{self, EffectiveBrowser, effective_browser};
use crate::ui;

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
    let out = match (ui::json(), toml::to_string_pretty(&resolved.document)) {
      (false, Ok(text)) => text,
      _ => serde_json::to_string_pretty(&resolved.document)?,
    };
    println!("{out}");
    return Ok(());
  }

  let specs = extension_report(&resolved.config);

  if ui::json() {
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

  print_human_report(&resolved, &effective, &specs);
  Ok(())
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

fn print_human_report(resolved: &layer::Resolved, effective: &EffectiveBrowser, specs: &[serde_json::Value]) {
  print_layers(resolved);
  print_effective_browser(effective);
  print_extensions(specs);
  print_resolved_values(resolved);
  if !resolved.warnings.is_empty() {
    ui::section("Warnings");
    for w in &resolved.warnings {
      ui::say(&ui::warning(&format!("{}: {}", w.source, w.message)));
    }
  }
}

/// Which files were read, in the order they were folded.
fn print_layers(resolved: &layer::Resolved) {
  ui::section("Layers");
  if resolved.layers.is_empty() {
    ui::say(&ui::dim(
      "  none — running on built-in defaults; no ferridriver.{toml,yaml,yml,json} found",
    ));
    ui::next_steps(&[("write one", "ferridriver init".to_string())]);
    return;
  }
  let mut table = ui::Table::new(&["PRECEDENCE", "FILE"]).flex(1);
  for l in &resolved.layers {
    table.row([ui::dim(l.kind.label()), ui::path(&ui::short_path(&l.path, 90))]);
  }
  table.print(ui::width());
}

/// What a run with these flags would actually launch.
fn print_effective_browser(effective: &EffectiveBrowser) {
  ui::section("Effective browser");
  let from = |from_cli: bool, flag: &str| {
    if from_cli {
      ui::dim(&format!("  (from {flag})"))
    } else {
      String::new()
    }
  };
  ui::say(&ui::kv_padded(
    "backend",
    &format!(
      "{:?}{}",
      effective.backend,
      from(effective.backend_from_cli, "--backend")
    ),
    8,
  ));
  ui::say(&ui::kv_padded(
    "headless",
    &format!(
      "{}{}",
      effective.headless,
      from(effective.headless_from_cli, "--headless/--headed")
    ),
    8,
  ));
}

/// Which extension specs resolved, to what, and what is holding any back.
fn print_extensions(specs: &[serde_json::Value]) {
  ui::section("Extensions");
  if specs.is_empty() {
    ui::say(&ui::dim("  none configured"));
  }
  for s in specs {
    let spec = s["spec"].as_str().unwrap_or_default();
    ui::say(&ui::list_item(&ui::bold(&ui::short_in(spec, 70))));
    if let Some(err) = s["error"].as_str() {
      ui::say(&ui::sub_item(&ui::failure(&format!("unresolved: {err}"))));
      continue;
    }
    let count = s["files"].as_array().map_or(0, Vec::len);
    ui::say(&ui::sub_item(&format!(
      "{count} entry file(s), base {}",
      ui::path(&ui::short_path(
        std::path::Path::new(s["baseDir"].as_str().unwrap_or_default()),
        70
      ))
    )));
    if s["manifest"].is_object() {
      let entries = s["manifest"]["entries"].as_array().map_or(0, Vec::len);
      ui::say(&ui::sub_item(&format!(
        "package manifest: {entries} declared entry/entries"
      )));
    }
    for issue in s["requirements"].as_array().into_iter().flatten() {
      let blocking = issue["blocking"] == serde_json::Value::Bool(true);
      let message = issue["message"].as_str().unwrap_or_default();
      ui::say(&ui::sub_item(&if blocking {
        ui::failure(&format!("unmet: {message}"))
      } else {
        ui::dim(&format!("note: {message}"))
      }));
    }
  }
}

/// Every key the fold produced, its value, and the file that set it.
fn print_resolved_values(resolved: &layer::Resolved) {
  ui::section("Resolved values");
  // Sources are named by file, because the question this column answers is
  // "which file set this" — but by file NAME, since the same two or three
  // absolute paths repeated down a column crowd out the values and the
  // Layers table above already maps each name to its full path. A name that
  // is not unique (two `ferridriver.toml`, one per layer) keeps its layer as
  // a prefix so it still identifies exactly one file.
  let name_of = |origin: &layer::Origin| match origin {
    layer::Origin::File(path) => file_label(path, &resolved.layers),
    other => other.describe(),
  };
  // The value is capped to what its column can hold, so the "(N chars)" mark
  // that says a value was cut is itself never cut off.
  let key_width = resolved.provenance.keys().map(String::len).max().unwrap_or(0);
  let from_width = resolved
    .provenance
    .values()
    .map(|o| name_of(o).len())
    .max()
    .unwrap_or(0)
    .max("FROM".len());
  let value_width = ui::width().saturating_sub(key_width + from_width + 4).max(24);
  let mut table = ui::Table::new(&["KEY", "VALUE", "FROM"]).flex(1);
  for (key, origin) in &resolved.provenance {
    let value = value_at(&resolved.document, key, value_width);
    // An appended array belongs to every layer that added to it; naming
    // only the last one sent people editing the wrong file.
    let source = match resolved.contributors.get(key) {
      Some(list) if list.len() > 1 => list.iter().map(name_of).collect::<Vec<_>>().join(" + "),
      _ => name_of(origin),
    };
    table.row([key.clone(), value, ui::dim(&source)]);
  }
  table.print(ui::width());
}

/// How one config file is named in the provenance column.
///
/// Its file name, unless another layer has the same one — a repository and a
/// working directory both holding a `ferridriver.toml` is the normal case —
/// in which case the layer it came from goes in front.
fn file_label(path: &std::path::Path, layers: &[layer::ConfigLayer]) -> String {
  let name = path
    .file_name()
    .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
  let same_name = layers.iter().filter(|l| l.path.file_name() == path.file_name()).count();
  if same_name > 1
    && let Some(layer) = layers.iter().find(|l| l.path == path)
  {
    return format!("{}:{name}", layer.kind.label());
  }
  name
}

/// Read a dotted key out of the merged document for display.
fn value_at(document: &serde_json::Value, dotted: &str, budget: usize) -> String {
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
  // `budget` is what the value column can actually hold, so the "(N chars)"
  // mark that says a value WAS cut is not itself cut off by the table.
  //
  // Truncated on a CHARACTER boundary: a byte slice panics the whole
  // command the moment a value carries a multi-byte character, which
  // server instructions and any non-ASCII path routinely do.
  let count = rendered.chars().count();
  let limit = budget.min(VALUE_DISPLAY_LIMIT);
  if count <= limit {
    return rendered;
  }
  let mark = format!("… ({count} chars)");
  let head: String = rendered
    .chars()
    .take(limit.saturating_sub(mark.chars().count()).max(8))
    .collect();
  format!("{head}{mark}")
}

/// Longest value `ferridriver config` prints in full, in characters, however
/// wide the terminal is.
const VALUE_DISPLAY_LIMIT: usize = 120;
