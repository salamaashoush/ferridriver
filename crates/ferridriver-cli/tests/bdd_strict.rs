#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `ferridriver bdd` step-matching outcomes at the process boundary:
//! undefined steps fail the run by default and only pass under
//! `--no-strict`, and a custom parameter type keeps its own regex
//! semantics (an alternation, a `\d`-style class) inside a cucumber
//! expression.
//!
//! Requires a built `ferridriver` binary (`FERRIDRIVER_BIN` or
//! `target/{debug,release}/ferridriver`) plus Chrome.

use std::path::Path;
use std::process::{Command, Output};

fn bin() -> String {
  std::env::var("FERRIDRIVER_BIN").unwrap_or_else(|_| {
    let base = format!("{}/../../target", env!("CARGO_MANIFEST_DIR"));
    let debug = format!("{base}/debug/ferridriver");
    if std::path::Path::new(&debug).exists() {
      debug
    } else {
      format!("{base}/release/ferridriver")
    }
  })
}

/// Lay out a scratch BDD project holding one fixture feature and the
/// shared step file.
fn scratch(feature: &str) -> tempfile::TempDir {
  let dir = tempfile::tempdir().expect("tempdir");
  let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bdd");
  std::fs::create_dir_all(dir.path().join("features")).expect("mkdir features");
  std::fs::create_dir_all(dir.path().join("steps")).expect("mkdir steps");
  std::fs::copy(
    fixtures.join(format!("{feature}.feature")),
    dir.path().join(format!("features/{feature}.feature")),
  )
  .expect("copy feature");
  std::fs::copy(fixtures.join("steps.js"), dir.path().join("steps/steps.js")).expect("copy steps");
  dir
}

fn run_bdd(dir: &Path, extra: &[&str]) -> Output {
  let mut args = vec!["bdd", "--headless", "--steps", "steps/*.js"];
  args.extend_from_slice(extra);
  args.push("features/");
  Command::new(bin())
    .current_dir(dir)
    .args(args)
    .output()
    .expect("run ferridriver bdd")
}

fn combined(output: &Output) -> String {
  format!(
    "{}{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  )
}

#[test]
fn undefined_steps_fail_the_run_by_default() {
  let dir = scratch("pending_steps");
  let output = run_bdd(dir.path(), &[]);
  let text = combined(&output);
  assert!(
    !output.status.success(),
    "a feature with undefined steps must exit non-zero:\n{text}"
  );
  assert!(text.contains("undefined step"), "the undefined step is named:\n{text}");
}

#[test]
fn undefined_steps_are_pending_under_no_strict() {
  let dir = scratch("pending_steps");
  let output = run_bdd(dir.path(), &["--no-strict"]);
  let text = combined(&output);
  assert!(
    output.status.success(),
    "--no-strict keeps undefined steps out of the exit code:\n{text}"
  );
}

#[test]
fn ambiguous_steps_fail_even_under_no_strict() {
  let dir = scratch("ambiguous_steps");
  let output = run_bdd(dir.path(), &["--no-strict"]);
  let text = combined(&output);
  assert!(
    !output.status.success(),
    "an ambiguous step is a definition bug and fails regardless of strictness:\n{text}"
  );
  assert!(text.contains("ambiguous step"), "the ambiguity is named:\n{text}");
}

#[test]
fn custom_parameter_types_match_and_reach_the_step() {
  let dir = scratch("custom_param_types");
  let output = run_bdd(dir.path(), &[]);
  let text = combined(&output);
  assert!(
    output.status.success(),
    "custom parameter types must match and pass their captured value to the step:\n{text}"
  );
}
