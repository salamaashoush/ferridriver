#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `ferridriver ext check` reporting what a package actually registers.
//!
//! The report used to count `defineTool` and nothing else, so a package
//! contributing fixtures, steps or config defaults read as an MCP server
//! that had forgotten to register anything. What a package contributes
//! is a function of the host it loads under, so the report is per host.
//!
//! Requires a built `ferridriver` binary (`FERRIDRIVER_BIN` or
//! `target/{debug,release}/ferridriver`).

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> String {
  std::env::var("FERRIDRIVER_BIN").unwrap_or_else(|_| {
    let base = format!("{}/../../target", env!("CARGO_MANIFEST_DIR"));
    let debug = format!("{base}/debug/ferridriver");
    if Path::new(&debug).exists() {
      debug
    } else {
      format!("{base}/release/ferridriver")
    }
  })
}

/// A package directory with a manifest and the entry files it names.
fn scratch(case: &str, manifest: &str, files: &[(&str, &str)]) -> PathBuf {
  let dir = std::env::temp_dir().join(format!("ferri-ext-kinds-{case}-{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).expect("workspace");
  std::fs::write(dir.join("package.json"), manifest).expect("package.json");
  for (name, source) in files {
    std::fs::write(dir.join(name), source).expect("entry");
  }
  dir
}

fn check_json(dir: &Path) -> serde_json::Value {
  let output = Command::new(bin())
    .current_dir(dir)
    .args(["ext", "check", ".", "--json", "--no-typecheck"])
    .output()
    .expect("run ferridriver ext check");
  let text = String::from_utf8_lossy(&output.stdout);
  serde_json::from_str(&text).unwrap_or_else(|e| {
    panic!(
      "not JSON ({e}):\nstdout: {text}\nstderr: {}",
      String::from_utf8_lossy(&output.stderr)
    )
  })
}

/// The kinds one host reports, merged across that host's entries.
fn kinds_for(report: &serde_json::Value, host: &str) -> serde_json::Map<String, serde_json::Value> {
  let mut merged = serde_json::Map::new();
  for entry in report["hosts"][host]["entries"].as_array().into_iter().flatten() {
    for (kind, count) in entry["kinds"].as_object().into_iter().flatten() {
      let total = merged.get(kind).and_then(serde_json::Value::as_u64).unwrap_or(0);
      merged.insert(kind.clone(), (total + count.as_u64().unwrap_or(0)).into());
    }
  }
  merged
}

#[test]
fn a_fixtures_only_package_reports_its_fixtures_and_is_ok() {
  let dir = scratch(
    "fixtures-only",
    r#"{"name":"@acme/fixtures","type":"module","ferridriver":{"entries":["./fixtures.ts"]}}"#,
    &[(
      "fixtures.ts",
      "defineFixtures({ acmeUser: async ({}, use) => { await use('someone'); } });\n",
    )],
  );

  let report = check_json(&dir);
  assert_eq!(report["ok"], true, "{report:#}");

  let test_kinds = kinds_for(&report, "test");
  assert!(
    test_kinds
      .get("fixtures")
      .and_then(serde_json::Value::as_u64)
      .unwrap_or(0)
      >= 1,
    "the fixture is reported under the test host: {report:#}"
  );
  assert!(
    !test_kinds.contains_key("tools"),
    "a package that registers no tool has no tool count at all, rather than a zero to explain: {report:#}"
  );

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_host_branching_package_reports_each_branch_under_its_own_host() {
  let dir = scratch(
    "host-branch",
    r#"{"name":"@acme/branch","type":"module","ferridriver":{"entries":["./plug.ts"]}}"#,
    &[(
      "plug.ts",
      "if (ferridriver.host === 'bdd') {\n\
       \x20 Given('a cart with {int} items', async () => {});\n\
       } else if (ferridriver.host === 'mcp') {\n\
       \x20 defineTool({ name: 'acme_ping', description: 'ping', handler: async () => 'pong' });\n\
       }\n",
    )],
  );

  let report = check_json(&dir);
  let bdd = kinds_for(&report, "bdd");
  let mcp = kinds_for(&report, "mcp");

  assert_eq!(
    bdd.get("steps").and_then(serde_json::Value::as_u64),
    Some(1),
    "the bdd branch registers its step: {report:#}"
  );
  assert!(
    !bdd.contains_key("tools"),
    "and no tool under bdd, where that branch never ran: {report:#}"
  );
  assert_eq!(
    mcp.get("tools").and_then(serde_json::Value::as_u64),
    Some(1),
    "the mcp branch registers its tool: {report:#}"
  );
  assert!(!mcp.contains_key("steps"), "and no step under mcp: {report:#}");

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_entry_narrowed_to_a_host_is_absent_from_the_others() {
  let dir = scratch(
    "narrowed",
    r#"{"name":"@acme/narrowed","type":"module","ferridriver":{"entries":[
      {"path":"./mcp.ts","hosts":["mcp"]},
      "./shared.ts"
    ]}}"#,
    &[
      (
        "mcp.ts",
        "defineTool({ name: 'acme_only', description: 'x', handler: async () => 'y' });\n",
      ),
      (
        "shared.ts",
        "defineFixtures({ acmeShared: async ({}, use) => { await use(1); } });\n",
      ),
    ],
  );

  let report = check_json(&dir);
  // A group's files, flattened: a package's entries share one bundle, so
  // the report names the group and lists what went into it.
  let entry_paths = |host: &str| -> Vec<String> {
    report["hosts"][host]["entries"]
      .as_array()
      .into_iter()
      .flatten()
      .flat_map(|e| e["files"].as_array().cloned().unwrap_or_default())
      .filter_map(|f| f.as_str().map(str::to_string))
      .collect()
  };

  let mcp = entry_paths("mcp");
  assert!(mcp.iter().any(|p| p.ends_with("mcp.ts")), "{mcp:?}");
  assert!(mcp.iter().any(|p| p.ends_with("shared.ts")), "{mcp:?}");

  let test = entry_paths("test");
  assert!(
    !test.iter().any(|p| p.ends_with("mcp.ts")),
    "an mcp-only entry does not appear under the test host: {test:?}"
  );
  assert!(
    test.iter().any(|p| p.ends_with("shared.ts")),
    "the open entry still does: {test:?}"
  );

  let _ = std::fs::remove_dir_all(&dir);
}
