#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `defineDefaults` — an extension package contributing configuration.
//!
//! The contribution is the LOWEST layer, which is the whole claim worth
//! testing at the process boundary: it applies when the config file is
//! silent, the file overrides it, and the command line overrides the
//! file. All three are observed in ONE run each through
//! `ferridriver test --list`, which discovers and prints the corpus
//! without launching a browser.
//!
//! Requires a built `ferridriver` binary (`FERRIDRIVER_BIN` or
//! `target/{debug,release}/ferridriver`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

const SPEC: &str = "
import { test, expect } from '@ferridriver/test';
test('runs', async ({}) => { expect(1).toBe(1); });
";

/// Lay out a workspace with three spec files, each discoverable by a
/// different glob, and an extension that defaults `testMatch` to the
/// first of them.
fn scratch(case: &str, extension: &str, config_test_match: Option<&str>, policy: &str) -> PathBuf {
  let dir = std::env::temp_dir().join(format!("ferri-defaults-{case}-{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(dir.join("specs")).expect("workspace");
  std::fs::create_dir_all(dir.join("ext")).expect("ext dir");
  for name in ["from-extension", "from-config", "from-cli"] {
    std::fs::write(dir.join(format!("specs/{name}.spec.ts")), SPEC).expect("spec");
  }
  std::fs::write(dir.join("ext/plug.ts"), extension).expect("extension");
  let test_match = config_test_match
    .map(|glob| format!("testMatch = [\"{glob}\"]\n"))
    .unwrap_or_default();
  // The table shape, because a policy ceiling and the path list cannot
  // both hang off a bare `extensions = [...]` array in TOML.
  std::fs::write(
    dir.join("ferridriver.toml"),
    format!(
      "[extensions]\n\
       paths = [\"./ext/plug.ts\"]\n\
       {policy}\n\
       [test]\n\
       {test_match}workers = 1\n\
       reporter = [{{ name = \"list\" }}]\n\
       \n\
       [test.browser]\n\
       browser = \"chromium\"\n\
       backend = \"cdp-pipe\"\n\
       headless = true\n"
    ),
  )
  .expect("config");
  dir
}

fn list(dir: &Path, extra: &[&str]) -> Output {
  let mut args = vec!["test", "--list"];
  args.extend_from_slice(extra);
  Command::new(bin())
    .current_dir(dir)
    .args(args)
    .output()
    .expect("run ferridriver test --list")
}

fn combined(output: &Output) -> String {
  format!(
    "{}{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  )
}

const DEFAULTS_TEST_MATCH: &str = "
defineDefaults({ test: { testMatch: ['specs/from-extension.spec.ts'] } });
";

#[test]
fn a_contribution_applies_the_config_overrides_it_and_the_cli_overrides_both() {
  // 1. The config file says nothing about `testMatch`: the package's
  //    default is what the run discovers.
  let dir = scratch("silent", DEFAULTS_TEST_MATCH, None, "");
  let output = list(&dir, &[]);
  let text = combined(&output);
  assert!(output.status.success(), "{text}");
  assert!(text.contains("from-extension.spec.ts"), "{text}");
  assert!(!text.contains("from-config.spec.ts"), "{text}");

  // 2. The config file speaks: it wins.
  let dir = scratch("file", DEFAULTS_TEST_MATCH, Some("specs/from-config.spec.ts"), "");
  let output = list(&dir, &[]);
  let text = combined(&output);
  assert!(output.status.success(), "{text}");
  assert!(text.contains("from-config.spec.ts"), "{text}");
  assert!(!text.contains("from-extension.spec.ts"), "{text}");

  // 3. The command line wins over both.
  let output = list(&dir, &["specs/from-cli.spec.ts"]);
  let text = combined(&output);
  assert!(output.status.success(), "{text}");
  assert!(text.contains("from-cli.spec.ts"), "{text}");
  assert!(!text.contains("from-config.spec.ts"), "{text}");
  assert!(!text.contains("from-extension.spec.ts"), "{text}");
}

#[test]
fn a_typo_in_a_contribution_names_the_key_and_fails_the_run() {
  let dir = scratch(
    "typo",
    "defineDefaults({ test: { testMatchh: ['specs/*.spec.ts'] } });\n",
    None,
    "",
  );
  let output = list(&dir, &[]);
  let text = combined(&output);
  assert!(
    !output.status.success(),
    "a contributed typo must fail the run:\n{text}"
  );
  assert!(text.contains("testMatchh"), "{text}");
  assert!(text.contains("plug.ts"), "the package that set it is named: {text}");
}

#[test]
fn the_operator_can_refuse_config_defaults() {
  let dir = scratch(
    "refused",
    DEFAULTS_TEST_MATCH,
    None,
    "\n[extensions.policy]\nconfigDefaults = false\n",
  );
  let output = list(&dir, &[]);
  let text = combined(&output);
  assert!(
    !output.status.success(),
    "a policy refusal fails the run rather than dropping the package:\n{text}"
  );
  assert!(text.contains("configDefaults"), "{text}");
}

#[test]
fn a_package_may_not_configure_the_loader_that_read_it() {
  let dir = scratch(
    "loader",
    "defineDefaults({ bundler: { conditions: ['node'] } });\n",
    None,
    "",
  );
  let output = list(&dir, &[]);
  let text = combined(&output);
  assert!(!output.status.success(), "{text}");
  assert!(text.contains("bundler"), "{text}");
  assert!(text.contains("compiled this package"), "the refusal says WHY: {text}");
}
