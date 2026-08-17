#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Open `use` keys and `{ option: true }` fixtures, end to end through
//! `ferridriver test`.
//!
//! Playwright: an option fixture's value comes from the config `use`
//! block, a project's `use` block, or an inner `test.use`, in that
//! order — the overrides are appended to the pool right after the
//! declaration they override, so a `test.use` still shadows them
//! (`playwright/src/common/fixtures.ts:88-111`,
//! `common/poolBuilder.ts:75-83`). A key naming a fixture that is NOT
//! an option is a load error with the message at `fixtures.ts:109`.
//!
//! The precedence cases run on all four backend projects, because the
//! bag is resolved per worker off that project's merged config. Each
//! workspace is throwaway (`--no-inherit`) so the repo's own config
//! cannot decide the outcome.

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

/// The four backend projects plus one that declares no `use` block, so
/// a run proves both halves: an override reaching a project, and the
/// config's value reaching a project that overrides nothing.
const BACKEND_PROJECTS: &str = r#"
[[test.projects]]
name = "cdp-pipe"
[test.projects.browser]
browser = "chromium"
backend = "cdp-pipe"
headless = true
[test.projects.browser.use]
profile = "cdp-pipe"

[[test.projects]]
name = "cdp-raw"
[test.projects.browser]
browser = "chromium"
backend = "cdp-raw"
headless = true
[test.projects.browser.use]
profile = "cdp-raw"

[[test.projects]]
name = "bidi"
[test.projects.browser]
browser = "firefox"
backend = "bidi"
headless = true
[test.projects.browser.use]
profile = "bidi"

[[test.projects]]
name = "webkit"
[test.projects.browser]
browser = "webkit"
backend = "webkit"
headless = true
[test.projects.browser.use]
profile = "webkit"

[[test.projects]]
name = "inherits-config"
[test.projects.browser]
browser = "chromium"
backend = "cdp-pipe"
headless = true
"#;

/// Build a throwaway workspace from `config` + `spec` and run it.
fn run_suite(case: &str, config: &str, spec: &str) -> Run {
  let dir = std::env::temp_dir().join(format!("ferri-use-options-{}-{case}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(dir.join("specs")).expect("create workspace");
  std::fs::write(dir.join("specs/use.spec.ts"), spec).expect("write spec");
  std::fs::write(
    dir.join("ferridriver.toml"),
    format!(
      "[test]\n\
       testDir = {:?}\n\
       testMatch = [\"**/*.spec.ts\"]\n\
       workers = 1\n\
       retries = 0\n\
       timeout = 30000\n\
       maxParallelProjects = 1\n\
       outputDir = {:?}\n\
       snapshotDir = {:?}\n\
       reporter = [{{ name = \"list\" }}]\n\
       \n\
       [test.browser]\n\
       browser = \"chromium\"\n\
       backend = \"cdp-pipe\"\n\
       headless = true\n\
       {config}\n",
      dir.join("specs").to_string_lossy(),
      dir.join("out").to_string_lossy(),
      dir.join("snaps").to_string_lossy(),
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

/// Every assertion is inside the spec, so a project whose bag never
/// arrived fails its own test rather than quietly asserting nothing.
const PRECEDENCE_SPEC: &str = r"
import { test as base, describe, expect } from '@ferridriver/test';

const test = base.extend<{ profile: string; theme: string }>({
  profile: ['fixture-default', { option: true }],
  theme: ['fixture-theme', { option: true }],
});

const expected: Record<string, string> = {
  'cdp-pipe': 'cdp-pipe',
  'cdp-raw': 'cdp-raw',
  bidi: 'bidi',
  webkit: 'webkit',
  'inherits-config': 'from-config',
};

test('the project use block wins over the config one', async ({ profile }) => {
  const name = test.info().project?.name ?? '';
  expect(name).toBe(expected[name] === undefined ? 'a known project' : name);
  expect(profile).toBe(expected[name]);
});

test('a config key no project overrides still arrives', async ({ theme }) => {
  expect(theme).toBe('config-theme');
});

describe('with a describe-level use', () => {
  test.use({ profile: 'from-spec' });

  test('the spec bag wins over the project one', async ({ profile, theme }) => {
    expect(profile).toBe('from-spec');
    // The key the spec did not name keeps the config's value.
    expect(theme).toBe('config-theme');
  });
});
";

#[test]
fn use_precedence_is_spec_then_project_then_config_then_default() {
  let run = run_suite(
    "precedence",
    &format!("\n[test.browser.use]\nprofile = \"from-config\"\ntheme = \"config-theme\"\n{BACKEND_PROJECTS}"),
    PRECEDENCE_SPEC,
  );
  assert!(run.passed, "expected a green run, got:\n{}", run.output);
  // 3 tests x 5 projects: a project whose bag failed to resolve would
  // have failed, and a project that never ran would drop the count.
  assert!(
    run.output.contains("15 passed"),
    "expected 15 passing tests (3 x 5 projects):\n{}",
    run.output
  );
}

#[test]
fn an_option_fixture_with_no_override_anywhere_keeps_its_declared_default() {
  // Same spec, same projects — but the config declares no `use` block
  // at all, so `profile` must fall back to the declaration. Inverts the
  // case above: if the bag were ignored entirely, THAT run would fail
  // and this one would pass, so the pair cannot both pass vacuously.
  let run = run_suite(
    "defaults",
    "\n[[test.projects]]\nname = \"cdp-pipe\"\n[test.projects.browser]\nbrowser = \"chromium\"\nbackend = \"cdp-pipe\"\nheadless = true\n",
    r"
import { test as base, expect } from '@ferridriver/test';

const test = base.extend<{ profile: string }>({
  profile: ['fixture-default', { option: true }],
});

test('nothing overrides it', async ({ profile }) => {
  expect(profile).toBe('fixture-default');
});
",
  );
  assert!(run.passed, "expected a green run, got:\n{}", run.output);
  assert!(run.output.contains("1 passed"), "{}", run.output);
}

#[test]
fn a_use_key_naming_a_non_option_fixture_is_refused() {
  let run = run_suite(
    "non-option",
    "\n[test.browser.use]\nplain = \"from-config\"\n",
    r"
import { test as base, expect } from '@ferridriver/test';

const test = base.extend<{ plain: string }>({
  plain: async ({}, use: (v: string) => Promise<void>) => use('plain-value'),
});

test('never runs', async ({ plain }) => {
  expect(plain).toBe('plain-value');
});
",
  );
  assert!(!run.passed, "expected a failed run, got:\n{}", run.output);
  assert!(
    run.output.contains(
      "Fixture \"plain\" cannot be overridden in the configuration \"use\" section. \
       Only fixtures registered with { option: true } can be set in the config."
    ),
    "expected Playwright's message:\n{}",
    run.output
  );
}

#[test]
fn a_use_key_naming_a_built_in_fixture_is_refused() {
  let run = run_suite(
    "built-in",
    "\n[test.browser.use]\npage = \"nope\"\n",
    r"
import { test, expect } from '@ferridriver/test';

test('never runs', async ({ page }) => {
  expect(page).toBeTruthy();
});
",
  );
  assert!(!run.passed, "expected a failed run, got:\n{}", run.output);
  assert!(
    run
      .output
      .contains("Fixture \"page\" cannot be overridden in the configuration \"use\" section."),
    "expected Playwright's message:\n{}",
    run.output
  );
}

#[test]
fn a_use_key_that_names_nothing_is_reported_and_ignored() {
  let run = run_suite(
    "unknown",
    "\n[test.browser.use]\nnobodyClaimsThisKey = 7\n",
    r"
import { test, expect } from '@ferridriver/test';

test('still runs', async ({}) => {
  expect(1).toBe(1);
});
",
  );
  assert!(run.passed, "expected a green run, got:\n{}", run.output);
  assert!(
    run.output.contains("use.unknownKey") && run.output.contains("nobodyClaimsThisKey"),
    "expected the named diagnostic:\n{}",
    run.output
  );
}

#[test]
fn a_project_use_key_is_validated_too() {
  // The plan is built once from the root config, so a key only a
  // PROJECT sets has to be gathered from the projects as well.
  let run = run_suite(
    "project-key",
    "\n[[test.projects]]\nname = \"one\"\n[test.projects.browser]\nbrowser = \"chromium\"\nbackend = \"cdp-pipe\"\nheadless = true\n[test.projects.browser.use]\nplain = \"from-project\"\n",
    r"
import { test as base, expect } from '@ferridriver/test';

const test = base.extend<{ plain: string }>({
  plain: async ({}, use: (v: string) => Promise<void>) => use('plain-value'),
});

test('never runs', async ({ plain }) => {
  expect(plain).toBe('plain-value');
});
",
  );
  assert!(!run.passed, "expected a failed run, got:\n{}", run.output);
  assert!(
    run.output.contains("Fixture \"plain\" cannot be overridden"),
    "expected Playwright's message:\n{}",
    run.output
  );
}
