#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Per-entry `hosts` and the entry-scoped `requires` that go with it.
//!
//! The gate is pure resolution plus requirement checking — no bundling,
//! no VM — so these run in milliseconds and say exactly what the gate
//! decided per host.

use std::path::{Path, PathBuf};

use ferridriver_config::ExtensionSpec;
use ferridriver_script::{ExtensionHost, RequirementEnv, ScriptCaps};

/// A binary that cannot plausibly be on PATH, so `requires.commands`
/// naming it is always unmet.
const ABSENT: &str = "definitely-not-a-real-binary-xyz";

fn scratch(tag: &str, manifest: &str) -> PathBuf {
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |d| d.as_nanos());
  let dir = std::env::temp_dir().join(format!("ferri_entry_hosts_{tag}_{nanos}"));
  std::fs::create_dir_all(&dir).expect("mkdir");
  std::fs::write(dir.join("package.json"), manifest).expect("package.json");
  for name in ["mcp.ts", "fixtures.ts"] {
    std::fs::write(dir.join(name), "export const nothing = 1;\n").expect("entry");
  }
  dir
}

fn gate_under(dir: &Path, host: ExtensionHost) -> (Vec<PathBuf>, Vec<String>) {
  let caps = ScriptCaps::default();
  let sidecars: Vec<String> = Vec::new();
  let env = RequirementEnv::from_caps(&caps, &sidecars);
  let specs = vec![ExtensionSpec {
    spec: dir.display().to_string(),
    base_dir: dir.to_path_buf(),
  }];
  let gated = ferridriver_script::gate(&specs, &env, host);
  (gated.files, gated.blocked)
}

#[test]
fn an_entry_narrowed_to_a_host_loads_only_there() {
  let dir = scratch(
    "narrow",
    r#"{
      "name": "@acme/narrow",
      "type": "module",
      "ferridriver": {
        "entries": [{ "path": "./mcp.ts", "hosts": ["mcp"] }, "./fixtures.ts"]
      }
    }"#,
  );

  let (mcp_files, _) = gate_under(&dir, ExtensionHost::Mcp);
  assert_eq!(mcp_files.len(), 2, "the mcp host loads both: {mcp_files:?}");

  let (test_files, _) = gate_under(&dir, ExtensionHost::Test);
  assert_eq!(test_files.len(), 1, "the test host loads only the open entry");
  assert!(
    test_files[0].ends_with("fixtures.ts"),
    "and it is the right one: {test_files:?}"
  );

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unmet_requirement_on_a_narrowed_entry_blocks_only_that_host() {
  // M24, which is the reason `requires` moved onto the entry: an
  // MCP-only entry naming a binary nobody has used to take its WHOLE
  // package down on every host — the fixtures and providers with it —
  // on a host where that entry does not even load.
  let dir = scratch(
    "scoped-requires",
    &format!(
      r#"{{
        "name": "@acme/scoped",
        "type": "module",
        "ferridriver": {{
          "entries": [
            {{ "path": "./mcp.ts", "hosts": ["mcp"], "requires": {{ "commands": ["{ABSENT}"] }} }},
            "./fixtures.ts"
          ]
        }}
      }}"#
    ),
  );

  let (mcp_files, mcp_blocked) = gate_under(&dir, ExtensionHost::Mcp);
  assert_eq!(mcp_blocked.len(), 1, "the mcp host blocks the package");
  assert!(mcp_files.is_empty(), "so nothing loads there: {mcp_files:?}");

  let (test_files, test_blocked) = gate_under(&dir, ExtensionHost::Test);
  assert!(
    test_blocked.is_empty(),
    "the test host has no unmet requirement to answer for: {test_blocked:?}"
  );
  assert_eq!(test_files.len(), 1, "and it still gets its fixtures: {test_files:?}");

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_package_level_requirement_still_blocks_every_host() {
  // The default is unchanged: `requires` written for the package is the
  // package's, and an entry that declares none inherits it.
  let dir = scratch(
    "package-requires",
    &format!(
      r#"{{
        "name": "@acme/wide",
        "type": "module",
        "ferridriver": {{
          "entries": ["./mcp.ts", "./fixtures.ts"],
          "requires": {{ "commands": ["{ABSENT}"] }}
        }}
      }}"#
    ),
  );

  for host in ExtensionHost::ALL {
    let (files, blocked) = gate_under(&dir, *host);
    assert_eq!(blocked.len(), 1, "{} blocks the package", host.as_str());
    assert!(files.is_empty(), "{} loads nothing", host.as_str());
  }

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_host_set_matches_the_one_a_manifest_is_validated_against() {
  // `hosts` is checked in the config crate, which cannot see this enum.
  // Nothing else keeps the two spellings of the same set together.
  let declared: Vec<&str> = ExtensionHost::ALL.iter().map(|h| h.as_str()).collect();
  assert_eq!(
    declared,
    ferridriver_config::extension_manifest::EXTENSION_HOSTS,
    "ExtensionHost::ALL and EXTENSION_HOSTS must name the same hosts, in the same order"
  );
}
