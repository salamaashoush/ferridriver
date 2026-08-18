#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Every host loads extensions through the one gate, proven where it is
//! observable: a `ferridriver test` spec seeing a value only an
//! extension contributes.
//!
//! The Playwright-spec host loaded NOTHING before this — both of its
//! `RunContext` sites hardcoded `extensions: Vec::new()` while the CLI had
//! been populating `CliOverrides.extensions` since extensions existed.
//! The negative case is the same workspace with the `extensions` key
//! removed, so a spec that passes only because the extension ran cannot
//! be mistaken for one that passes anyway.

use std::process::Command;

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

struct Run {
  output: String,
  passed: bool,
}

/// A spec that only passes when the extension's top level ran in the
/// same VM: the extension is what puts `__fromExtension` there, and it
/// records which host it saw.
const SPEC: &str = "
import { test, expect } from '@ferridriver/test';

test('sees what the extension contributed', async ({}) => {
  const contributed = (globalThis as Record<string, unknown>).__fromExtension;
  expect(contributed).toEqual({ value: 'from-extension', host: 'test' });
});
";

const EXTENSION: &str = "
(globalThis as Record<string, unknown>).__fromExtension = {
  value: 'from-extension',
  host: ferridriver.host,
};
";

/// Build a throwaway workspace and run `ferridriver test` in it.
/// `extensions` is the config line, or empty for the negative case.
fn run_suite(case: &str, extensions: &str, package_json: Option<(&str, &str, &str)>) -> Run {
  let dir = std::env::temp_dir().join(format!("ferri-ext-hosts-{}-{case}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(dir.join("specs")).expect("create workspace");
  std::fs::create_dir_all(dir.join("ext")).expect("create ext dir");
  std::fs::write(dir.join("specs/ext.spec.ts"), SPEC).expect("write spec");
  std::fs::write(dir.join("ext/plug.ts"), EXTENSION).expect("write extension");
  if let Some((pkg_dir, manifest, entry)) = package_json {
    std::fs::create_dir_all(dir.join(pkg_dir)).expect("create package dir");
    std::fs::write(dir.join(pkg_dir).join("package.json"), manifest).expect("write package.json");
    std::fs::write(dir.join(pkg_dir).join("index.ts"), entry).expect("write package entry");
  }
  std::fs::write(
    dir.join("ferridriver.toml"),
    format!(
      "{extensions}\n\
       [test]\n\
       testDir = {:?}\n\
       testMatch = [\"**/*.spec.ts\"]\n\
       workers = 1\n\
       retries = 0\n\
       timeout = 30000\n\
       outputDir = {:?}\n\
       reporter = [{{ name = \"list\" }}]\n\
       \n\
       [test.browser]\n\
       browser = \"chromium\"\n\
       backend = \"cdp-pipe\"\n\
       headless = true\n",
      dir.join("specs").to_string_lossy(),
      dir.join("out").to_string_lossy(),
    ),
  )
  .expect("write config");

  let out = Command::new(bin())
    .arg("test")
    .arg("--no-inherit")
    .arg("-c")
    .arg(dir.join("ferridriver.toml"))
    .output()
    .expect("spawn ferridriver test");
  let output = format!(
    "{}{}",
    String::from_utf8_lossy(&out.stdout),
    String::from_utf8_lossy(&out.stderr)
  );
  let _ = std::fs::remove_dir_all(&dir);
  Run {
    output,
    passed: out.status.success(),
  }
}

#[test]
fn a_spec_sees_what_an_extension_contributed() {
  let run = run_suite("loaded", "extensions = [\"./ext/plug.ts\"]", None);
  assert!(run.passed, "expected a green run, got:\n{}", run.output);
  assert!(run.output.contains("1 passed"), "{}", run.output);
}

#[test]
fn without_the_extension_the_same_spec_fails() {
  // The inversion: nothing else in the workspace can put
  // `__fromExtension` on the VM's globals.
  let run = run_suite("absent", "", None);
  assert!(!run.passed, "expected a red run, got:\n{}", run.output);
  assert!(
    run.output.contains("1 failed"),
    "expected the spec itself to fail:\n{}",
    run.output
  );
}

/// The four backend projects, so a provided specifier is proven to
/// resolve on every engine the runner drives rather than on whichever
/// one happens to run first.
const BACKEND_PROJECTS: &str = r#"
[[test.projects]]
name = "cdp-pipe"
[test.projects.browser]
browser = "chromium"
backend = "cdp-pipe"
headless = true

[[test.projects]]
name = "cdp-raw"
[test.projects.browser]
browser = "chromium"
backend = "cdp-raw"
headless = true

[[test.projects]]
name = "bidi"
[test.projects.browser]
browser = "firefox"
backend = "bidi"
headless = true

[[test.projects]]
name = "webkit"
[test.projects.browser]
browser = "webkit"
backend = "webkit"
headless = true
"#;

/// A spec importing a specifier only a package serves. The import is
/// the whole point: a suite written against another package runs with
/// no edit to its own source.
const PROVIDED_SPEC: &str = "
import { test, expect } from '@ferridriver/test';
import { greet, calls } from 'fake-vendor';

test('imports a specifier an extension provides', async ({}) => {
  expect(greet('world')).toBe('hello world');
  // The module the extension's own entry already used: one instance,
  // so the entry's call is visible here.
  expect(calls).toEqual(['from-entry', 'hello world']);
});
";

#[test]
fn a_spec_imports_a_specifier_only_an_extension_provides() {
  let dir = std::env::temp_dir().join(format!("ferri-ext-provided-{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(dir.join("specs")).expect("create workspace");
  std::fs::create_dir_all(dir.join("pkg")).expect("create package");
  std::fs::write(dir.join("specs/provided.spec.ts"), PROVIDED_SPEC).expect("write spec");
  std::fs::write(
    dir.join("pkg/package.json"),
    r#"{"name":"vendor","ferridriver":{"apiVersion":2,"name":"vendor","entries":["entry.ts"],
        "provides":{"modules":{"fake-vendor":"vendor.ts"}}}}"#,
  )
  .expect("write package.json");
  std::fs::write(
    dir.join("pkg/vendor.ts"),
    "export const calls: string[] = [];\n\
     export function greet(who: string) { const out = `hello ${who}`; calls.push(out); return out; }\n",
  )
  .expect("write provider");
  std::fs::write(
    dir.join("pkg/entry.ts"),
    "import { calls } from 'fake-vendor';\ncalls.push('from-entry');\n",
  )
  .expect("write entry");
  std::fs::write(
    dir.join("ferridriver.toml"),
    format!(
      "extensions = [\"./pkg\"]\n\
       [test]\n\
       testDir = {:?}\n\
       testMatch = [\"**/*.spec.ts\"]\n\
       workers = 1\n\
       retries = 0\n\
       timeout = 30000\n\
       maxParallelProjects = 1\n\
       outputDir = {:?}\n\
       reporter = [{{ name = \"list\" }}]\n\
       {BACKEND_PROJECTS}\n",
      dir.join("specs").to_string_lossy(),
      dir.join("out").to_string_lossy(),
    ),
  )
  .expect("write config");

  let out = Command::new(bin())
    .arg("test")
    .arg("--no-inherit")
    .arg("-c")
    .arg(dir.join("ferridriver.toml"))
    .output()
    .expect("spawn ferridriver test");
  let output = format!(
    "{}{}",
    String::from_utf8_lossy(&out.stdout),
    String::from_utf8_lossy(&out.stderr)
  );
  let _ = std::fs::remove_dir_all(&dir);
  assert!(out.status.success(), "expected a green run, got:\n{output}");
  assert!(
    output.contains("4 passed"),
    "the spec must pass on all four backend projects:\n{output}"
  );
}

#[test]
fn the_test_host_gates_a_package_whose_requirements_are_unmet() {
  // The gate the test host never had: a package declaring a binary that
  // is not on PATH must not load, and must not take the run down either.
  let run = run_suite(
    "blocked",
    "extensions = [\"./ext/plug.ts\", \"./pkg\"]",
    Some((
      "pkg",
      r#"{"name":"blocked-pkg","ferridriver":{"entries":["index.ts"],"requires":{"commands":["ferri-not-a-real-binary"]}}}"#,
      "(globalThis as Record<string, unknown>).__fromBlockedPackage = true;\n",
    )),
  );
  assert!(run.passed, "one blocked package must not fail the run:\n{}", run.output);
  assert!(
    run.output.contains("ferri-not-a-real-binary"),
    "the blocked package must be named in the diagnostics:\n{}",
    run.output
  );
}

/// A spec that names a fixture nothing in its own file registers. The
/// extension put it on the base chain with `defineFixtures`, so the
/// suite receives it through the `test` it already imports — no edit to
/// the suite, no import of the package.
const FIXTURE_SPEC: &str = "
import { test, expect } from '@ferridriver/test';

test('receives a contributed fixture', async ({ page, deployment }) => {
  expect(deployment).toBe('staging');
  await page.goto('about:blank');
  expect(await page.evaluate(() => 1 + 1)).toBe(2);
});
";

const FIXTURE_EXTENSION: &str = "
defineFixtures({
  deployment: async ({}, use) => { await use('staging'); },
});
";

fn run_fixture_suite(case: &str, extensions: &str) -> Run {
  let dir = std::env::temp_dir().join(format!("ferri-ext-fixtures-{}-{case}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(dir.join("specs")).expect("create workspace");
  std::fs::create_dir_all(dir.join("ext")).expect("create ext dir");
  std::fs::write(dir.join("specs/fixtures.spec.ts"), FIXTURE_SPEC).expect("write spec");
  std::fs::write(dir.join("ext/fixtures.ts"), FIXTURE_EXTENSION).expect("write extension");
  std::fs::write(
    dir.join("ferridriver.toml"),
    format!(
      "{extensions}\n\
       [test]\n\
       testDir = {:?}\n\
       testMatch = [\"**/*.spec.ts\"]\n\
       workers = 1\n\
       retries = 0\n\
       timeout = 30000\n\
       maxParallelProjects = 1\n\
       outputDir = {:?}\n\
       reporter = [{{ name = \"list\" }}]\n\
       {BACKEND_PROJECTS}\n",
      dir.join("specs").to_string_lossy(),
      dir.join("out").to_string_lossy(),
    ),
  )
  .expect("write config");

  let out = Command::new(bin())
    .arg("test")
    .arg("--no-inherit")
    .arg("-c")
    .arg(dir.join("ferridriver.toml"))
    .output()
    .expect("spawn ferridriver test");
  let output = format!(
    "{}{}",
    String::from_utf8_lossy(&out.stdout),
    String::from_utf8_lossy(&out.stderr)
  );
  let _ = std::fs::remove_dir_all(&dir);
  Run {
    output,
    passed: out.status.success(),
  }
}

#[test]
fn a_spec_receives_a_fixture_an_extension_contributed() {
  let run = run_fixture_suite("loaded", "extensions = [\"./ext/fixtures.ts\"]");
  assert!(run.passed, "expected a green run, got:\n{}", run.output);
  assert!(
    run.output.contains("4 passed"),
    "the contributed fixture must resolve on all four backend projects:\n{}",
    run.output
  );
}

#[test]
fn without_the_extension_the_contributed_fixture_is_absent() {
  // The inversion: `deployment` is not a built-in and the spec declares
  // nothing, so the only thing that can supply it is the extension.
  // Without it the name resolves to no registration, which is
  // Playwright's unknown-parameter error rather than a silent
  // `undefined` the assertion then trips over.
  let run = run_fixture_suite("absent", "");
  assert!(!run.passed, "expected a red run, got:\n{}", run.output);
  assert!(
    run.output.contains("4 failed") && run.output.contains(r#"Test has unknown parameter "deployment"."#),
    "every project must fail, naming the parameter nothing registers:\n{}",
    run.output
  );
}
