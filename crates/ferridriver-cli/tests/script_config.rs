#![allow(clippy::expect_used, clippy::unwrap_used)]
//! A config written as a module.
//!
//! `.ts` / `.js` is a config FORMAT, not a `--config` special case: a
//! `ferridriver.config.ts` is discovered in the slots a
//! `ferridriver.toml` is discovered in, holds the same document, layers
//! by the same rules, and shadows the same way when two formats sit in
//! one directory. These tests are that equivalence, observed through
//! `ferridriver test --list`, which resolves config, bundles and
//! discovers without launching a browser.
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

/// The settings every case needs, in each format, so a case can choose
/// which format carries them without changing what they say.
const TOML_BASE: &str = "[test]\nworkers = 1\nreporter = [{ name = \"list\" }]\ntestMatch = [\"specs/alpha.spec.ts\"]\n\n[test.browser]\nbrowser = \"chromium\"\nbackend = \"cdp-pipe\"\nheadless = true\n";

const MODULE_BASE: &str = "
export default {
  test: {
    workers: 1,
    reporter: [{ name: 'list' }],
    testMatch: ['specs/alpha.spec.ts'],
    browser: { browser: 'chromium', backend: 'cdp-pipe', headless: true },
  },
};
";

/// A workspace with two spec files and whichever config files the case
/// names.
fn scratch(case: &str, files: &[(&str, &str)]) -> PathBuf {
  let dir = std::env::temp_dir().join(format!("ferri-script-config-{case}-{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(dir.join("specs")).expect("workspace");
  for name in ["alpha", "beta"] {
    std::fs::write(dir.join(format!("specs/{name}.spec.ts")), SPEC).expect("spec");
  }
  for (name, source) in files {
    std::fs::write(dir.join(name), source).expect("config");
  }
  dir
}

fn list(dir: &Path) -> Output {
  Command::new(bin())
    .current_dir(dir)
    .args(["test", "--list"])
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

/// The listed corpus, sorted, so two runs compare without depending on
/// discovery order.
fn corpus(text: &str) -> Vec<String> {
  let mut lines: Vec<String> = text
    .lines()
    .filter(|l| l.contains(".spec.ts"))
    .map(|l| l.trim().to_string())
    .collect();
  lines.sort();
  lines
}

#[test]
fn a_module_and_the_toml_that_says_the_same_thing_discover_the_same_corpus() {
  let from_module = scratch("module", &[("ferridriver.config.ts", MODULE_BASE)]);
  let from_toml = scratch("toml", &[("ferridriver.toml", TOML_BASE)]);

  let module_run = list(&from_module);
  let toml_run = list(&from_toml);
  let module_text = combined(&module_run);
  let toml_text = combined(&toml_run);

  assert!(module_run.status.success(), "{module_text}");
  assert!(toml_run.status.success(), "{toml_text}");
  assert_eq!(
    corpus(&module_text),
    corpus(&toml_text),
    "module:\n{module_text}\n\ntoml:\n{toml_text}"
  );
  assert!(
    module_text.contains("alpha.spec.ts") && !module_text.contains("beta.spec.ts"),
    "the module's own testMatch took effect: {module_text}"
  );

  let _ = std::fs::remove_dir_all(&from_module);
  let _ = std::fs::remove_dir_all(&from_toml);
}

#[test]
fn a_module_is_a_discovered_layer_and_is_named_as_the_source_of_its_values() {
  let dir = scratch(
    "provenance",
    &[(
      "ferridriver.config.ts",
      "
import { defineConfig } from '@ferridriver/test';

// `defineConfig` is Playwright's function and folds Playwright's shape,
// which is ferridriver's `[test]` section. The document around it is
// ferridriver's, exactly as a `.toml` layer's is.
export default {
  test: defineConfig({
    workers: 1,
    projects: [{ name: 'one' }, { name: 'two' }],
  }),
};
",
    )],
  );

  let output = Command::new(bin())
    .current_dir(&dir)
    .args(["config"])
    .output()
    .expect("run ferridriver config");
  let text = combined(&output);
  assert!(output.status.success(), "{text}");
  assert!(
    text.contains("ferridriver.config.ts"),
    "the module is a layer like any other: {text}"
  );
  assert!(
    text.contains(r#"test.projects = [{"name":"one"},{"name":"two"}]"#),
    "its values resolved: {text}"
  );

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn two_formats_in_one_directory_shadow_by_basename_order() {
  // The rule that already governed `.toml` beside `.yaml`, now covering
  // `.ts` too — because it is a format, not a layer of its own.
  let dir = scratch(
    "shadow",
    &[
      ("ferridriver.toml", TOML_BASE),
      (
        "ferridriver.config.ts",
        "export default { test: { testMatch: ['specs/beta.spec.ts'] } };\n",
      ),
    ],
  );

  let output = list(&dir);
  let text = combined(&output);
  assert!(output.status.success(), "{text}");
  assert!(
    text.contains("alpha.spec.ts") && !text.contains("beta.spec.ts"),
    "the earlier basename wins: {text}"
  );

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_module_may_not_configure_the_loader_that_compiled_it() {
  let dir = scratch(
    "refused",
    &[(
      "ferridriver.config.ts",
      "export default { test: { moduleAliases: { '@acme/test': '@ferridriver/test' } } };\n",
    )],
  );
  let output = list(&dir);
  let text = combined(&output);
  assert!(!output.status.success(), "a refusal must fail the run: {text}");
  assert!(text.contains("test.moduleAliases"), "the key is named: {text}");

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_module_with_no_default_export_says_so() {
  let dir = scratch(
    "no-default",
    &[("ferridriver.config.ts", "export const config = { test: {} };\n")],
  );
  let output = list(&dir);
  let text = combined(&output);
  assert!(!output.status.success(), "{text}");
  assert!(text.contains("no default export"), "{text}");

  let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_module_that_throws_fails_the_run_with_its_own_error() {
  let dir = scratch(
    "throws",
    &[(
      "ferridriver.config.ts",
      "throw new Error('the config could not decide');\nexport default {};\n",
    )],
  );
  let output = list(&dir);
  let text = combined(&output);
  assert!(!output.status.success(), "{text}");
  assert!(text.contains("the config could not decide"), "{text}");

  let _ = std::fs::remove_dir_all(&dir);
}
