#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `devices` on the JS module surface.
//!
//! Playwright exports one `devices` object from `playwright` and
//! re-exports that same object from `@playwright/test`
//! (`packages/playwright/test.mjs:24`), so a suite spreads
//! `devices['iPhone 15']` into `use` and both specifiers answer with the
//! same table. Every case is a module that throws when a rule does not
//! hold.

use std::sync::Arc;

use ferridriver_script::{
  ExtensionHost, InMemoryVars, RunContext, ScriptCaps, ScriptEngineConfig, Session, bundle_and_compile_named,
  eval_bundle,
};

fn ctx(dir: &std::path::Path) -> RunContext {
  RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: dir.into(),
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

async fn eval(source: &str) -> Result<(), String> {
  let dir = tempfile::tempdir().expect("tempdir");
  let entry = dir.path().join("case.ts");
  std::fs::write(&entry, source).expect("write");
  let bundle = bundle_and_compile_named(std::slice::from_ref(&entry), dir.path(), "ferridriver-devices-case.js")
    .await
    .map_err(|e| e.to_string())?;
  let session = Session::create(ScriptEngineConfig::default(), &ctx(dir.path()))
    .await
    .map_err(|e| e.to_string())?;
  eval_bundle(&session.vm_handle(), &bundle)
    .await
    .map_err(|e| e.to_string())
}

const OK: &str = "const ok = (cond, what) => { if (!cond) throw new Error(what); };\n";

#[tokio::test(flavor = "multi_thread")]
async fn a_descriptor_carries_the_keys_a_spread_needs() {
  eval(&format!(
    "{OK}import {{ devices }} from '@ferridriver/test';
const d = devices['iPhone 15'];
ok(d, 'iPhone 15 is in the table');
ok(d.userAgent.includes('iPhone'), 'userAgent');
ok(d.viewport.width === 393 && d.viewport.height === 659, 'viewport');
ok(d.screen.width === 393 && d.screen.height === 852, 'screen survives the spread');
ok(d.deviceScaleFactor === 3, 'deviceScaleFactor');
ok(d.isMobile === true && d.hasTouch === true, 'mobile flags');
ok(d.defaultBrowserType === 'webkit', 'defaultBrowserType');
const spread = {{ ...d, hasTouch: false }};
ok(spread.userAgent === d.userAgent, 'a spread copies the descriptor');
ok(spread.hasTouch === false, 'a key beside the spread wins');
"
  ))
  .await
  .expect("case");
}

#[tokio::test(flavor = "multi_thread")]
async fn every_specifier_answers_with_the_same_object() {
  eval(&format!(
    "{OK}import {{ devices as fromTest }} from '@ferridriver/test';
import {{ devices as fromPlaywright }} from '@playwright/test';
import {{ devices as fromFerridriver }} from 'ferridriver';
ok(fromTest === fromPlaywright, '@playwright/test re-exports the same table');
ok(fromTest === fromFerridriver, 'the ferridriver module exports the same table');
ok(fromTest === require('@playwright/test').devices, 'require sees it too');
"
  ))
  .await
  .expect("case");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_table_is_the_whole_vendored_registry() {
  eval(&format!(
    "{OK}import {{ devices }} from 'ferridriver';
const names = Object.keys(devices);
ok(names.length === 207, `expected 207 devices, got ${{names.length}}`);
ok(devices['Nokia 3310'] === undefined, 'an unknown name is undefined, not a fallback');
ok(devices['Desktop Chrome'].defaultBrowserType === 'chromium', 'Desktop Chrome');
ok(devices['Desktop Safari'].defaultBrowserType === 'webkit', 'Desktop Safari');
ok(devices['Desktop Firefox'].defaultBrowserType === 'firefox', 'Desktop Firefox');
"
  ))
  .await
  .expect("case");
}
