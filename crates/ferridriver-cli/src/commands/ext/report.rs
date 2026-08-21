//! Rendering one `ferridriver ext check` pass for a person.
//!
//! Separated from the pass itself because the two answer different questions:
//! `mod.rs` decides what loaded, this decides what a reader needs to see of
//! it. The JSON document is the same data unshaped, so `--format json` skips
//! everything here.

use super::Report;
use crate::ui;

/// Report what the extensions register, and where.
///
/// The same tool usually loads on every host, so a table per host printed the
/// identical list four times and buried the one row that differed. One table,
/// with the hosts a tool loads on as a column, says the same thing in a
/// quarter of the space and makes a host-scoped tool visible at a glance.
fn registrations(payload: &serde_json::Value) {
  let hosts = payload["hosts"].as_object().cloned().unwrap_or_default();

  ui::section("Hosts");
  for (host, view) in &hosts {
    let entries = view["entries"].as_array().cloned().unwrap_or_default();
    let blocked = view["blocked"].as_array().map_or(0, Vec::len);
    if entries.is_empty() {
      let why = if blocked > 0 {
        format!(" ({blocked} package(s) held back)")
      } else {
        String::new()
      };
      ui::say(&ui::list_item(&format!(
        "{host}  {}",
        ui::dim(&format!("nothing loads{why}"))
      )));
      continue;
    }
    let files: usize = entries.iter().map(|g| g["files"].as_array().map_or(0, Vec::len)).sum();
    let mut counts: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for group in &entries {
      if let Some(error) = group["error"].as_str() {
        ui::say(&ui::list_item(&format!(
          "{host}  {}",
          ui::failure(&format!("failed: {error}"))
        )));
      }
      for (kind, count) in group["kinds"].as_object().into_iter().flatten() {
        *counts.entry(kind.clone()).or_default() += count.as_u64().unwrap_or_default();
      }
    }
    let summary = if counts.is_empty() {
      ui::dim("registers nothing")
    } else {
      ui::dim(
        &counts
          .iter()
          .map(|(kind, count)| format!("{count} {kind}"))
          .collect::<Vec<_>>()
          .join(", "),
      )
    };
    ui::say(&ui::list_item(&format!(
      "{host}  {} {summary}",
      ui::dim(&format!("{files} file(s),"))
    )));
  }

  // Tool name -> (exposed as an MCP tool, allow summary, hosts it loads on).
  let mut tools: std::collections::BTreeMap<String, (bool, String, Vec<String>)> = std::collections::BTreeMap::new();
  for (host, view) in &hosts {
    for group in view["entries"].as_array().into_iter().flatten() {
      for t in group["tools"].as_array().into_iter().flatten() {
        let name = t["name"].as_str().unwrap_or_default().to_string();
        let exposed = t["exposeAsMcpTool"] == serde_json::Value::Bool(true);
        let commands = t["allow"]["commands"].as_array().map_or(0, Vec::len);
        let net = t["allow"]["net"].as_array().map_or(0, Vec::len);
        let allows = if commands > 0 || net > 0 {
          format!("{commands} command(s), {net} net host(s)")
        } else {
          String::new()
        };
        tools
          .entry(name)
          .or_insert_with(|| (exposed, allows, Vec::new()))
          .2
          .push(host.clone());
      }
    }
  }
  if tools.is_empty() {
    return;
  }

  let host_count = hosts.len();
  ui::section("Tools");
  let mut table = ui::Table::new(&["TOOL", "EXPOSED", "HOSTS", "ALLOWS"])
    .indent(2)
    .flex(0);
  for (name, (exposed, allows, on)) in &tools {
    let where_ = if on.len() == host_count {
      ui::dim("all")
    } else {
      on.join(", ")
    };
    table.row([
      name.clone(),
      if *exposed {
        ui::badge("mcp tool", &console::Style::new().on_cyan().black())
      } else {
        ui::dim("binding only")
      },
      where_,
      ui::dim(allows),
    ]);
  }
  table.print(ui::width());
}

/// Print one cycle's report.
///
/// Flushes explicitly: `ext check` exits through `std::process::exit`, which
/// runs no destructors, so a report piped to a file (the CI / pre-commit
/// shape) was block-buffered and lost on the failing runs.
pub fn print(report: &Report) -> anyhow::Result<()> {
  if ui::json() {
    return ui::print_json(&report.payload);
  }

  for spec in report.payload["specs"].as_array().into_iter().flatten() {
    let name = spec["spec"].as_str().unwrap_or_default();
    let count = spec["files"].as_array().map_or(0, Vec::len);
    ui::section(&ui::short_in(name, 70));
    if let Some(dir) = spec["packageDir"].as_str() {
      let entries = spec["manifest"]["entries"].as_array().map_or(0, Vec::len);
      ui::say(&ui::kv(
        "package",
        &format!(
          "{} ({entries} declared entry/entries)",
          ui::path(&ui::short_path(std::path::Path::new(dir), 70))
        ),
      ));
    }
    ui::say(&ui::kv("entries", &format!("{count} file(s)")));
    for entry in spec["entries"].as_array().into_iter().flatten() {
      let narrowed = entry["hosts"]
        .as_array()
        .map(|hosts| {
          let names: Vec<&str> = hosts.iter().filter_map(serde_json::Value::as_str).collect();
          ui::dim(&format!("  [{}]", names.join(", ")))
        })
        .unwrap_or_default();
      let path = entry["path"].as_str().unwrap_or_default();
      ui::say(&ui::sub_item(&format!(
        "{}{narrowed}",
        ui::path(&ui::short_path(std::path::Path::new(path), 70))
      )));
    }
    for issue in spec["requirements"].as_array().into_iter().flatten() {
      let blocking = issue["blocking"] == serde_json::Value::Bool(true);
      let message = issue["message"].as_str().unwrap_or_default();
      ui::say(&format!(
        "  {}",
        if blocking {
          ui::failure(&format!("unmet: {message}"))
        } else {
          ui::dim(&format!("note: {message}"))
        }
      ));
    }
    if spec["blocked"] == serde_json::Value::Bool(true) {
      ui::say(&format!("  {}", ui::warning("skipped: requirements above are unmet")));
    }
  }

  let types = &report.payload["typecheck"];
  ui::section("Types");
  match (types["checker"].as_str(), types["skipped"].as_str()) {
    (_, Some(reason)) => ui::say(&format!("  {}", ui::dim(&format!("skipped: {reason}")))),
    (Some(checker), None) => {
      if types["passed"] == serde_json::Value::Bool(true) {
        ui::say(&format!("  {}", ui::success(&format!("{checker}: no errors"))));
      } else {
        ui::say(&format!("  {}", ui::failure(checker)));
        for d in types["diagnostics"].as_array().into_iter().flatten() {
          ui::say(&ui::sub_item(d.as_str().unwrap_or_default()));
        }
      }
    },
    (None, None) => ui::say(&format!("  {}", ui::dim("no checker available"))),
  }

  registrations(&report.payload);

  let errors = report.payload["errors"].as_array().cloned().unwrap_or_default();
  if !errors.is_empty() {
    ui::section("Errors");
    for e in &errors {
      ui::say(&format!("  {}", ui::failure(e.as_str().unwrap_or_default())));
    }
  }
  ui::say(&format!(
    "\n{}",
    if report.ok {
      ui::success("ok")
    } else {
      ui::failure("failed")
    }
  ));
  if !report.ok {
    ui::next_steps(&[
      ("see what resolved", "ferridriver config".to_string()),
      ("write the type declarations", "ferridriver ext types".to_string()),
    ]);
  }
  std::io::Write::flush(&mut std::io::stdout())?;
  Ok(())
}
