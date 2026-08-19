#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `defineConfig`'s merge, pinned rule by rule against
//! `packages/playwright/src/common/configLoader.ts:32-87`.
//!
//! Each case is a module that throws when the rule does not hold, so a
//! regression fails as an evaluation error naming the rule rather than
//! as a diffed structure nobody can read.

use std::sync::Arc;

use ferridriver_script::{
  ExtensionHost, InMemoryVars, PathSandbox, RunContext, ScriptCaps, ScriptEngineConfig, Session,
  bundle_and_compile_named, eval_bundle,
};

fn ctx(dir: &std::path::Path) -> RunContext {
  RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(dir).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: Vec::new(),
    host: ExtensionHost::Test,
    caps: ScriptCaps::default(),
    session: None,
  }
}

/// Evaluate `source` as a config-shaped module. `Ok` means every
/// assertion in it held.
async fn eval(source: &str) -> Result<(), String> {
  let dir = tempfile::tempdir().expect("tempdir");
  let entry = dir.path().join("case.ts");
  std::fs::write(&entry, source).expect("write");
  let bundle = bundle_and_compile_named(std::slice::from_ref(&entry), dir.path(), "ferridriver-config-case.js")
    .await
    .map_err(|e| e.to_string())?;
  let session = Session::create(ScriptEngineConfig::default(), &ctx(dir.path()))
    .await
    .map_err(|e| e.to_string())?;
  eval_bundle(&session.vm_handle(), &bundle)
    .await
    .map_err(|e| e.to_string())
}

/// Prelude every case shares: the import plus a terse assertion helper.
const PRELUDE: &str = r"import { defineConfig } from '@ferridriver/test';
const eq = (actual, expected, what) => {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
};
const ok = (cond, what) => { if (!cond) throw new Error(what); };
";

fn case(body: &str) -> String {
  format!("{PRELUDE}{body}")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_later_config_overrides_an_earlier_one_key_by_key() {
  let out = eval(&case(
    r"const merged = defineConfig(
      { timeout: 1000, retries: 1, workers: 4 },
      { timeout: 2000, retries: 3 },
      { retries: 9 },
    );
    eq(merged.timeout, 2000, 'the middle config wins over the first');
    eq(merged.retries, 9, 'the last config wins over both');
    eq(merged.workers, 4, 'a key nobody overrode survives');
  ",
  ))
  .await;
  assert_eq!(out, Ok(()), "{out:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn use_expect_and_build_merge_one_level_deep() {
  let out = eval(&case(
    r"const merged = defineConfig(
      { use: { locale: 'de-DE', headless: true }, expect: { timeout: 5000 }, build: { external: ['a'] } },
      { use: { locale: 'fr-FR' }, expect: { toHaveScreenshot: { maxDiffPixels: 3 } } },
    );
    eq(merged.use.locale, 'fr-FR', 'the incoming use key wins');
    eq(merged.use.headless, true, 'a use key only the first config set survives');
    eq(merged.expect.timeout, 5000, 'expect merges rather than being replaced');
    eq(merged.expect.toHaveScreenshot.maxDiffPixels, 3, 'the incoming expect key lands');
    eq(merged.build.external, ['a'], 'build merges the same way');
  ",
  ))
  .await;
  assert_eq!(out, Ok(()), "{out:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_three_merged_blocks_exist_even_when_neither_side_had_them() {
  let out = eval(&case(
    r"const merged = defineConfig({ timeout: 1 }, { retries: 2 });
    eq(merged.use, {}, 'use is created empty');
    eq(merged.expect, {}, 'expect is created empty');
    eq(merged.build, {}, 'build is created empty');
    eq(merged.webServer, [], 'webServer is created empty');
  ",
  ))
  .await;
  assert_eq!(out, Ok(()), "{out:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_explicit_undefined_erases_the_key_beneath_it() {
  // The reason the merge is a JS-object merge and not a JSON one: a
  // config that writes `storageState: process.env.X ? p : undefined`
  // means to clear the key, and JSON would drop the override instead.
  let out = eval(&case(
    r"const merged = defineConfig(
      { use: { storageState: 'state.json' }, globalSetup: './setup.ts' },
      { use: { storageState: undefined }, globalSetup: undefined },
    );
    ok('storageState' in merged.use, 'the key is present');
    ok(merged.use.storageState === undefined, 'and its value is the override');
    ok(merged.globalSetup === undefined, 'a top-level key clears the same way');
  ",
  ))
  .await;
  assert_eq!(out, Ok(()), "{out:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn web_server_normalizes_each_side_then_concatenates() {
  let out = eval(&case(
    r"const one = defineConfig(
      { webServer: { command: 'a' } },
      { webServer: [{ command: 'b' }, { command: 'c' }] },
    );
    eq(one.webServer.map(w => w.command), ['a', 'b', 'c'], 'a lone object is normalized, then concatenated');

    const two = defineConfig({ webServer: { command: 'a' } }, { timeout: 1 });
    eq(two.webServer.map(w => w.command), ['a'], 'an absent incoming side contributes nothing');

    const three = defineConfig({ timeout: 1 }, { webServer: { command: 'b' } });
    eq(three.webServer.map(w => w.command), ['b'], 'an absent outgoing side contributes nothing');
  ",
  ))
  .await;
  assert_eq!(out, Ok(()), "{out:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn projects_merge_by_name_and_new_names_are_appended() {
  let out = eval(&case(
    r"const merged = defineConfig(
      { projects: [
        { name: 'chromium', retries: 1, use: { locale: 'de-DE', headless: true } },
        { name: 'firefox', retries: 2 },
      ] },
      { projects: [
        { name: 'firefox', retries: 5, use: { locale: 'fr-FR' } },
        { name: 'webkit', retries: 7 },
      ] },
    );
    eq(merged.projects.map(p => p.name), ['chromium', 'firefox', 'webkit'],
      'matched names keep their position and a new name is appended');
    eq(merged.projects[1].retries, 5, 'the override wins for a matched project');
    eq(merged.projects[1].use.locale, 'fr-FR', 'its use block merges rather than replacing');
    eq(merged.projects[0].use.locale, 'de-DE', 'an unmatched project is untouched');
    eq(merged.projects[2].retries, 7, 'the appended project keeps its own values');
  ",
  ))
  .await;
  assert_eq!(out, Ok(()), "{out:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_nothing_overrode_keeps_its_identity() {
  // Upstream pushes such a project through unchanged, so a `use` it
  // never had stays absent rather than becoming `{}`.
  let out = eval(&case(
    r"const chromium = { name: 'chromium', retries: 1 };
    const merged = defineConfig({ projects: [chromium] }, { projects: [{ name: 'webkit' }] });
    ok(merged.projects[0] === chromium, 'the same object comes out');
    ok(!('use' in merged.projects[0]), 'an absent use block is not created');
    ok('use' in defineConfig({ projects: [{ name: 'a' }] }, { projects: [{ name: 'a' }] }).projects[0],
      'a project that WAS overridden always has one');
  ",
  ))
  .await;
  assert_eq!(out, Ok(()), "{out:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn neither_side_declaring_projects_leaves_the_key_alone() {
  let out = eval(&case(
    r"const merged = defineConfig({ timeout: 1 }, { retries: 2 });
    ok(!('projects' in merged), 'no projects key is invented');

    const kept = defineConfig({ projects: [{ name: 'a' }] }, { retries: 2 });
    eq(kept.projects.map(p => p.name), ['a'], 'an outgoing list survives an incoming config that has none');
  ",
  ))
  .await;
  assert_eq!(out, Ok(()), "{out:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn one_argument_is_returned_as_it_was_passed() {
  let out = eval(&case(
    r"const config = { timeout: 1 };
    ok(defineConfig(config) === config, 'a single config is not copied');
    ok(!('use' in config), 'and nothing is added to it');
  ",
  ))
  .await;
  assert_eq!(out, Ok(()), "{out:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn calling_it_with_nothing_is_a_type_error() {
  let out = eval(&case(
    r"let threw = null;
    try { defineConfig(); } catch (e) { threw = e; }
    ok(threw !== null, 'defineConfig() with no arguments throws');
    eq(threw.name, 'TypeError', 'and it is a TypeError');
  ",
  ))
  .await;
  assert_eq!(out, Ok(()), "{out:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_playwright_specifier_serves_the_same_function() {
  let out = eval(
    r"import { defineConfig } from '@ferridriver/test';
    import { defineConfig as viaPlaywright } from '@playwright/test';
    import { defineConfig as viaBare } from 'playwright/test';
    if (viaPlaywright !== defineConfig || viaBare !== defineConfig)
      throw new Error('every specifier must answer with the same defineConfig');
  ",
  )
  .await;
  assert_eq!(out, Ok(()), "{out:?}");
}
