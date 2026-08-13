#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `ferridriver config` and `ferridriver doctor` end to end.
//!
//! These report on the operator's real setup, so their failure modes are
//! the ones nothing else catches: a value the renderer cannot print, a
//! check that mutates the tree it is inspecting, a misconfiguration that
//! only surfaces when two browsers race for one profile.

use std::path::Path;
use std::process::Command;

fn bin() -> String {
  if let Ok(path) = std::env::var("FERRIDRIVER_BIN") {
    return path;
  }
  format!("{}/../../target/debug/ferridriver", env!("CARGO_MANIFEST_DIR"))
}

struct Run {
  stdout: String,
  stderr: String,
  success: bool,
}

/// Run a subcommand with the layer stack pinned to `cwd`, so nothing
/// here reads the developer's own `~/.config/ferridriver`.
fn run(cwd: &Path, args: &[&str]) -> Run {
  let out = Command::new(bin())
    .arg("--no-inherit")
    .args(args)
    .current_dir(cwd)
    .output()
    .expect("spawn ferridriver");
  Run {
    stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    success: out.status.success(),
  }
}

fn write_config(dir: &Path, body: &str) {
  std::fs::write(dir.join("ferridriver.toml"), body).expect("write config");
}

/// A long value carrying multi-byte characters. The report truncates
/// long values, and truncating by BYTE index panicked the whole command
/// the moment the cut landed inside a character — which server
/// instructions (em dashes, arrows) reach immediately.
#[test]
fn a_long_non_ascii_value_is_truncated_not_panicked_on() {
  let dir = tempfile::tempdir().expect("tempdir");
  let instructions = "— ferridriver — drive the browser — ".repeat(12);
  write_config(
    dir.path(),
    &format!(
      "[mcp.server]\ninstructions = {}\n",
      serde_json::to_string(&instructions).expect("json")
    ),
  );

  let r = run(dir.path(), &["config"]);
  assert!(r.success, "stdout: {}\nstderr: {}", r.stdout, r.stderr);
  assert!(
    r.stdout.contains("mcp.server.instructions"),
    "the key is reported: {}",
    r.stdout
  );
  assert!(r.stdout.contains("chars)"), "and truncated: {}", r.stdout);
}

/// `doctor` inspects; it must not build the tree it is inspecting.
#[test]
fn doctor_does_not_create_the_sandbox_roots_it_reports_on() {
  let dir = tempfile::tempdir().expect("tempdir");
  write_config(dir.path(), "scriptRoot = \"./scripts\"\nartifactsRoot = \"./out\"\n");

  let r = run(dir.path(), &["doctor"]);
  assert!(r.stdout.contains("sandbox roots"), "{}", r.stdout);
  assert!(
    !dir.path().join("scripts").exists(),
    "scriptRoot must not be created by a diagnostic: {}",
    r.stdout
  );
  assert!(!dir.path().join("out").exists(), "nor artifactsRoot: {}", r.stdout);
}

/// A Chrome profile serves one process, so two instances resolving to
/// one `userDataDir` is a setup that cannot work — named here rather
/// than discovered as a launch failure later.
#[test]
fn doctor_names_two_instances_sharing_one_profile() {
  let dir = tempfile::tempdir().expect("tempdir");
  write_config(
    dir.path(),
    "[mcp.browser]\n\
     userDataDir = \"/tmp/ferridriver-shared-profile\"\n\
     \n\
     [mcp.browser.instances.staging]\n\
     args = []\n\
     \n\
     [mcp.browser.instances.dev]\n\
     args = []\n",
  );

  let r = run(dir.path(), &["doctor", "--instances"]);
  assert!(
    r.stdout.contains("both launch with profile"),
    "the collision must be named: {}",
    r.stdout
  );
  assert!(r.stdout.contains("${INSTANCE}"), "with the fix: {}", r.stdout);
  assert!(!r.success, "a setup that cannot work must exit non-zero");
}

/// `${INSTANCE}` in a section-level path gives each instance its own
/// profile, which is what makes the shared-profile case avoidable.
#[test]
fn a_section_level_instance_placeholder_gives_each_instance_its_own_profile() {
  let dir = tempfile::tempdir().expect("tempdir");
  let profiles = dir.path().join("profiles");
  write_config(
    dir.path(),
    &format!(
      "[mcp.browser]\n\
       userDataDir = {}\n\
       \n\
       [mcp.browser.instances.staging]\n\
       args = []\n\
       \n\
       [mcp.browser.instances.dev]\n\
       args = []\n",
      serde_json::to_string(&profiles.join("${INSTANCE}").display().to_string()).expect("json")
    ),
  );

  let r = run(dir.path(), &["doctor", "--instances"]);
  assert!(
    !r.stdout.contains("both launch with profile"),
    "distinct profiles: {}",
    r.stdout
  );
  for name in ["staging", "dev"] {
    let expected = profiles.join(name).display().to_string();
    assert!(
      r.stdout.contains(&expected),
      "{name} keeps its own profile: {}",
      r.stdout
    );
  }
}

/// The report exists to answer "which file set this". An additive array
/// is the concatenation of several layers, so naming only the last one
/// sent people editing a file that was not responsible.
#[test]
fn an_appended_array_names_every_layer_that_contributed() {
  let dir = tempfile::tempdir().expect("tempdir");
  std::fs::write(
    dir.path().join("base.toml"),
    "[mcp.browser]\nchromeArgs = [\"--from-base\"]\n",
  )
  .expect("write base");
  write_config(
    dir.path(),
    "extends = \"./base.toml\"\n[mcp.browser]\nchromeArgs = [\"--from-project\"]\n",
  );

  let r = run(dir.path(), &["config"]);
  assert!(r.success, "stdout: {}\nstderr: {}", r.stdout, r.stderr);
  let line = r
    .stdout
    .lines()
    .find(|l| l.contains("mcp.browser.chromeArgs"))
    .unwrap_or_else(|| panic!("chromeArgs not reported: {}", r.stdout));
  assert!(line.contains("base.toml"), "the extended file contributed: {line}");
  assert!(line.contains("ferridriver.toml"), "and so did the project file: {line}");
}

/// A directory holding two config formats silently used one of them, so
/// every edit to the other looked like it did nothing.
#[test]
fn a_shadowed_config_file_is_reported() {
  let dir = tempfile::tempdir().expect("tempdir");
  write_config(dir.path(), "[test]\nworkers = 3\n");
  std::fs::write(dir.path().join("ferridriver.yaml"), "test:\n  workers: 9\n").expect("write yaml");

  let r = run(dir.path(), &["config"]);
  assert!(
    r.stdout.contains("also present and ignored") && r.stdout.contains("ferridriver.yaml"),
    "the shadowed file must be named: {}",
    r.stdout
  );
}
