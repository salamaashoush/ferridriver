#![allow(clippy::expect_used, clippy::unwrap_used)]
//! The `expect` block on `[test]` and on a project, end to end through
//! `ferridriver test`.
//!
//! Playwright: `TestConfig.expect` / `TestProject.expect`
//! (`playwright/types/test.d.ts:184` and `:1131`), resolved with
//! `takeFirst(projectConfig.expect, config.expect, {})`
//! (`playwright/src/common/config.ts:201`) — a whole-object take, never
//! a merge. Each case here runs a throwaway workspace so the repo's own
//! config cannot decide the outcome (`--no-inherit`).

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
  stdout: String,
  passed: bool,
  elapsed: std::time::Duration,
}

/// Build a throwaway workspace from `config` + `spec` and run it.
fn run_suite(case: &str, config: &str, spec: &str) -> Run {
  let dir = std::env::temp_dir().join(format!("ferri-expect-config-{}-{case}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(dir.join("specs")).expect("create workspace");
  std::fs::write(dir.join("specs/expect.spec.ts"), spec).expect("write spec");
  std::fs::write(
    dir.join("ferridriver.toml"),
    format!(
      "[test]\n\
       testDir = {:?}\n\
       testMatch = [\"**/*.spec.ts\"]\n\
       workers = 1\n\
       retries = 0\n\
       timeout = 20000\n\
       outputDir = {:?}\n\
       snapshotDir = {:?}\n\
       reporter = [{{ name = \"list\" }}]\n\
       {config}\n\
       \n\
       [test.browser]\n\
       browser = \"chromium\"\n\
       backend = \"cdp-pipe\"\n\
       headless = true\n",
      dir.join("specs").to_string_lossy(),
      dir.join("out").to_string_lossy(),
      dir.join("snaps").to_string_lossy(),
    ),
  )
  .expect("write config");

  let started = std::time::Instant::now();
  let out = Command::new(bin())
    .arg("test")
    .arg("--no-inherit")
    .arg("-c")
    .arg(dir.join("ferridriver.toml"))
    .output()
    .expect("spawn ferridriver test");
  let elapsed = started.elapsed();
  let stdout = format!(
    "{}{}",
    String::from_utf8_lossy(&out.stdout),
    String::from_utf8_lossy(&out.stderr)
  );
  let _ = std::fs::remove_dir_all(&dir);
  Run {
    stdout,
    passed: out.status.success(),
    elapsed,
  }
}

const NEVER_VISIBLE: &str = "import { test, expect } from '@ferridriver/test';\n\
   test('never visible', async ({ page }) => {\n\
   \x20 await page.setContent('<div>nothing here</div>');\n\
   \x20 await expect(page.locator('#missing')).toBeVisible();\n\
   });\n";

/// `[test.expect] timeout` decides how long an auto-retrying matcher
/// waits — the flat `expectTimeout` was the only spelling that reached
/// the runner before, and neither reached a JS spec at all (the `QuickJS`
/// `expect` started from a hardcoded 5s).
#[test]
fn the_expect_block_timeout_reaches_a_spec() {
  let run = run_suite("timeout", "[test.expect]\ntimeout = 900", NEVER_VISIBLE);
  assert!(!run.passed, "a never-visible element must fail the suite");
  assert!(
    run.stdout.contains("Timeout:  900ms"),
    "the failure must name the configured timeout, got:\n{}",
    run.stdout
  );
  // 900ms plus startup, nowhere near the 5s default: an ignored setting
  // would put this over 5s.
  assert!(
    run.elapsed < std::time::Duration::from_secs(15),
    "the run took {:?} — the configured 900ms was not honoured",
    run.elapsed
  );
}

/// The flat `expectTimeout` still works, and the nested key wins.
#[test]
fn the_flat_spelling_still_works_and_the_nested_one_wins() {
  let run = run_suite("flat", "expectTimeout = 700", NEVER_VISIBLE);
  assert!(
    run.stdout.contains("Timeout:  700ms"),
    "the flat expectTimeout must still reach the matcher, got:\n{}",
    run.stdout
  );

  let run = run_suite(
    "both",
    "expectTimeout = 700\n[test.expect]\ntimeout = 1100",
    NEVER_VISIBLE,
  );
  assert!(
    run.stdout.contains("Timeout:  1100ms"),
    "the nested key must win over the flat one, got:\n{}",
    run.stdout
  );
}

/// A per-project `expect` block narrows the timeout for THAT project
/// only. Two projects run the same spec; each must report its own
/// number.
#[test]
fn a_project_expect_block_applies_to_that_project_only() {
  let run = run_suite(
    "per-project",
    "[test.expect]\n\
     timeout = 2500\n\
     \n\
     [[test.projects]]\n\
     name = \"fast\"\n\
     [test.projects.expect]\n\
     timeout = 600\n\
     \n\
     [[test.projects]]\n\
     name = \"slow\"\n",
    NEVER_VISIBLE,
  );
  assert!(!run.passed, "both projects must fail the assertion");
  assert!(
    run.stdout.contains("Timeout:  600ms"),
    "the project's own timeout must apply, got:\n{}",
    run.stdout
  );
  assert!(
    run.stdout.contains("Timeout:  2500ms"),
    "the project without a block must keep the config's timeout, got:\n{}",
    run.stdout
  );
}

const SCREENSHOT_SPEC: &str = "import { test, expect } from '@ferridriver/test';\n\
   const box = (color: string) =>\n\
   \x20 `<style>body{margin:0}</style>` +\n\
   \x20 `<div id=\"target\" style=\"width:100px;height:100px;background:#ffffff\">` +\n\
   \x20 `<div style=\"width:100px;height:36px;background:${color}\"></div></div>`;\n\
   test('baseline', async ({ page }) => {\n\
   \x20 await page.setContent(box('#ffffff'));\n\
   \x20 await expect(page.locator('#target')).toHaveScreenshot('delta.png');\n\
   });\n\
   test('within the configured budget', async ({ page }) => {\n\
   \x20 await page.setContent(box('#000000'));\n\
   \x20 await expect(page.locator('#target')).toHaveScreenshot('delta.png');\n\
   });\n\
   test('a per-call option overrides the configured budget', async ({ page }) => {\n\
   \x20 await page.setContent(box('#000000'));\n\
   \x20 await expect(page.locator('#target')).toHaveScreenshot('delta.png', { maxDiffPixelRatio: 0.01 });\n\
   });\n";

/// `[test.expect.toHaveScreenshot]` supplies the comparison budget every
/// call starts from, and a per-call option layers ON TOP of it —
/// Playwright's `{ ...configOptions, ...callOptions }`
/// (`matchers/toMatchSnapshot.ts:121-127`).
#[test]
fn the_screenshot_budget_comes_from_config_and_a_call_can_override_it() {
  // 36 of every 100 pixels differ between the two documents.
  let generous = run_suite(
    "shot-generous",
    "[test.expect.toHaveScreenshot]\nmaxDiffPixelRatio = 0.5",
    SCREENSHOT_SPEC,
  );
  assert!(
    !generous.passed,
    "the third test asks for 1% and must fail:\n{}",
    generous.stdout
  );
  assert_eq!(
    generous.stdout.matches("pixels differ").count(),
    1,
    "only the per-call 1% test may fail under a 50% config budget, got:\n{}",
    generous.stdout
  );

  // Invert it: with a strict config budget the SECOND test fails too, so
  // the first assertion cannot have passed vacuously.
  let strict = run_suite(
    "shot-strict",
    "[test.expect.toHaveScreenshot]\nmaxDiffPixelRatio = 0.01",
    SCREENSHOT_SPEC,
  );
  assert!(!strict.passed, "a 1% budget must fail the delta");
  assert_eq!(
    strict.stdout.matches("pixels differ").count(),
    2,
    "both the config-budget test and the per-call test must fail under a strict config, got:\n{}",
    strict.stdout
  );
}

/// M27: a project that sets ONLY `expect.timeout` does NOT inherit the
/// config-level `toHaveScreenshot` budget — the project's object
/// replaces the config's whole block.
#[test]
fn a_project_expect_block_does_not_inherit_the_config_screenshot_budget() {
  let run = run_suite(
    "no-inherit-budget",
    "[test.expect]\n\
     timeout = 2500\n\
     [test.expect.toHaveScreenshot]\n\
     maxDiffPixelRatio = 0.9\n\
     \n\
     [[test.projects]]\n\
     name = \"narrow\"\n\
     [test.projects.expect]\n\
     timeout = 800\n",
    SCREENSHOT_SPEC,
  );
  assert!(
    !run.passed,
    "the generous config budget must NOT reach a project with its own expect block:\n{}",
    run.stdout
  );
  assert!(
    run.stdout.contains("pixels differ"),
    "the delta must be reported as a pixel mismatch, got:\n{}",
    run.stdout
  );

  // Same config, same project — but the project's block names the budget
  // itself, so the identical delta passes. Without this the first half
  // could be failing for any reason at all.
  let with_budget = run_suite(
    "own-budget",
    "[test.expect]\n\
     timeout = 2500\n\
     [test.expect.toHaveScreenshot]\n\
     maxDiffPixelRatio = 0.9\n\
     \n\
     [[test.projects]]\n\
     name = \"narrow\"\n\
     [test.projects.expect]\n\
     timeout = 800\n\
     [test.projects.expect.toHaveScreenshot]\n\
     maxDiffPixelRatio = 0.9\n",
    SCREENSHOT_SPEC,
  );
  let failures = with_budget.stdout.matches("pixels differ").count();
  assert_eq!(
    failures, 1,
    "only the per-call 1% test may fail once the project names its own budget, got:\n{}",
    with_budget.stdout
  );
}

/// A custom matcher's `this.timeout` is the same number the built-ins
/// poll with — Playwright's `MatcherContext.timeout`.
#[test]
fn a_custom_matcher_reads_the_configured_timeout() {
  const SPEC: &str = "import { test, expect } from '@ferridriver/test';\n\
     expect.extend({\n\
     \x20 toSeeTimeout(received: number) {\n\
     \x20   return {\n\
     \x20     pass: this.timeout === received,\n\
     \x20     message: () => `this.timeout was ${this.timeout}`,\n\
     \x20   };\n\
     \x20 },\n\
     });\n\
     test('matcher context', async () => {\n\
     \x20 expect(1234).toSeeTimeout(1234);\n\
     });\n";

  let matching = run_suite("matcher-ctx", "[test.expect]\ntimeout = 1234", SPEC);
  assert!(
    matching.passed,
    "this.timeout must be the configured 1234:\n{}",
    matching.stdout
  );

  // Invert it: any other configured value must make the same spec fail,
  // naming what it actually saw.
  let other = run_suite("matcher-ctx-other", "[test.expect]\ntimeout = 900", SPEC);
  assert!(!other.passed, "a different configured timeout must fail the matcher");
  assert!(
    other.stdout.contains("this.timeout was 900"),
    "the matcher must have seen the configured 900, got:\n{}",
    other.stdout
  );
}
