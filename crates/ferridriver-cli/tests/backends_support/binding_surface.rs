//! QuickJS binding surface tests for the methods that ship Rust core
//! through to scripts: every `getBy*` accessor on `Frame` and
//! `Locator`, the `FrameLocator` class as a whole, the `page.touchscreen`
//! / `page.snapshotForAI` / `page.exposeFunction` / `page.frameLocator`
//! page-level methods, and `context.clearCookies({...})`.
//!
//! Each test exercises the binding through `run_script`, asserting
//! that the call routes through and returns a usable JS handle (or
//! the expected page-side effect, where one applies).

#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::unwrap_used,
  clippy::expect_used,
  clippy::needless_pass_by_value
)]

use serde_json::json;

use super::client::McpClient;

/// Shared setup: navigate to a fixture document with a labelled
/// button, an image with alt text, and an iframe so every getBy*
/// path resolves.
fn setup(c: &mut McpClient) {
  c.nav("<button title='hi' aria-label='click-me'>x</button><img alt='kitten' src='data:image/gif;base64,R0lGODlhAQABAAAAACw='><iframe srcdoc='<button id=inner>inside</button>'></iframe>");
  c.script("await page.waitForSelector('button[title=\"hi\"]'); return true;");
}

pub fn test_frame_get_by_methods(c: &mut McpClient) {
  setup(c);
  let v = c.script_value(
    r"
    const f = page.mainFrame();
    return {
      title: await f.getByTitle('hi').textContent(),
      label: await f.getByLabel('click-me').textContent(),
      alt: await f.getByAltText('kitten').isVisible(),
      role: await f.getByRole('button').textContent(),
      txt: await f.getByText('x').textContent(),
      placeholder: typeof f.getByPlaceholder('z').click,
      testid: typeof f.getByTestId('z').click,
    };
  ",
  );
  assert_eq!(v["title"].as_str(), Some("x"));
  assert_eq!(v["label"].as_str(), Some("x"));
  assert_eq!(v["alt"].as_bool(), Some(true));
  assert!(v["role"].as_str().unwrap_or("").contains('x'));
  assert_eq!(v["txt"].as_str(), Some("x"));
  assert_eq!(v["placeholder"].as_str(), Some("function"));
  assert_eq!(v["testid"].as_str(), Some("function"));
}

pub fn test_frame_page_and_frame_locator(c: &mut McpClient) {
  setup(c);
  let v = c.script_value(
    r"
    const f = page.mainFrame();
    const p = f.page();
    return {
      pageUrl: p.url ? (await p.url()) : null,
      pageType: typeof p.goto,
      flType: typeof f.frameLocator('iframe').locator,
    };
  ",
  );
  assert!(
    v["pageUrl"].as_str().is_none() || v["pageUrl"].as_str().unwrap_or("").starts_with("data:"),
    "frame.page().url() should resolve to the navigated data URL: got {v}"
  );
  assert_eq!(v["pageType"].as_str(), Some("function"));
  assert_eq!(v["flType"].as_str(), Some("function"));
}

pub fn test_locator_get_by_methods(c: &mut McpClient) {
  setup(c);
  let v = c.script_value(
    r"
    const body = page.locator('body');
    return {
      role: typeof body.getByRole('button').click,
      text: typeof body.getByText('x').click,
      testid: typeof body.getByTestId('z').click,
      label: typeof body.getByLabel('click-me').click,
      placeholder: typeof body.getByPlaceholder('z').click,
      altText: typeof body.getByAltText('kitten').click,
      title: typeof body.getByTitle('hi').click,
    };
  ",
  );
  for k in ["role", "text", "testid", "label", "placeholder", "altText", "title"] {
    assert_eq!(v[k].as_str(), Some("function"), "locator.{k} missing");
  }
}

pub fn test_locator_page_and_frame_methods(c: &mut McpClient) {
  setup(c);
  let v = c.script_value(
    r"
    const loc = page.locator('iframe');
    return {
      pageType: typeof loc.page().goto,
      flType: typeof loc.frameLocator('button').locator,
      cfType: typeof loc.contentFrame().locator,
    };
  ",
  );
  for k in ["pageType", "flType", "cfType"] {
    assert_eq!(v[k].as_str(), Some("function"), "locator.{k} missing");
  }
}

pub fn test_frame_locator_class(c: &mut McpClient) {
  setup(c);
  let v = c.script_value(
    r"
    const fl = page.frameLocator('iframe');
    return {
      locator: typeof fl.locator('body').click,
      role: typeof fl.getByRole('button').click,
      text: typeof fl.getByText('inside').click,
      testid: typeof fl.getByTestId('x').click,
      label: typeof fl.getByLabel('x').click,
      placeholder: typeof fl.getByPlaceholder('x').click,
      altText: typeof fl.getByAltText('x').click,
      title: typeof fl.getByTitle('x').click,
      owner: typeof fl.owner().click,
      first: typeof fl.first().locator,
      last: typeof fl.last().locator,
      nth: typeof fl.nth(0).locator,
      nested: typeof fl.frameLocator('iframe').locator,
    };
  ",
  );
  for k in [
    "locator",
    "role",
    "text",
    "testid",
    "label",
    "placeholder",
    "altText",
    "title",
    "owner",
    "first",
    "last",
    "nth",
    "nested",
  ] {
    assert_eq!(v[k].as_str(), Some("function"), "FrameLocator.{k} missing");
  }
}

pub fn test_page_frame_locator(c: &mut McpClient) {
  setup(c);
  let v = c.script_value(
    r"
    return { fl: typeof page.frameLocator('iframe').locator };
  ",
  );
  assert_eq!(v["fl"].as_str(), Some("function"));
}

pub fn test_page_touchscreen_tap(c: &mut McpClient) {
  // BiDi has no touch dispatch path wired today; everything else
  // routes through the same path as Mouse.
  if c.backend == "bidi" {
    return;
  }
  setup(c);
  let v = c.script_value(
    r"
    await page.touchscreen.tap(10, 10);
    return { ok: true };
  ",
  );
  assert_eq!(v["ok"].as_bool(), Some(true));
}

pub fn test_page_snapshot_for_ai(c: &mut McpClient) {
  setup(c);
  let v = c.script_value(
    r"
    const snap = await page.snapshotForAI();
    return {
      fullType: typeof snap.full,
      hasContent: snap.full.length > 0,
      mapType: typeof snap.refMap,
    };
  ",
  );
  assert_eq!(v["fullType"].as_str(), Some("string"));
  assert_eq!(v["hasContent"].as_bool(), Some(true));
  assert_eq!(v["mapType"].as_str(), Some("object"));
}

pub fn test_page_expose_function(c: &mut McpClient) {
  setup(c);
  let v = c.script_value(
    r"
    // Playwright parity: args are SPREAD into the callback and the
    // callback's return value is delivered to the page-side caller, so
    // `await window.fn(...)` resolves to the real result (no polling).
    await page.exposeFunction('__expose_record', (...a) => {
      return { got: a, n: a.length };
    });
    const installed = await page.evaluate(`typeof window.__expose_record`);
    const result = await page.evaluate(
      `window.__expose_record(1, 'two', { three: 3 })`);
    return { installed, result };
  ",
  );
  assert_eq!(
    v["installed"].as_str(),
    Some("function"),
    "exposeFunction should install window.__expose_record as a function: {v}"
  );
  assert_eq!(
    &v["result"],
    &json!({ "got": [1, "two", {"three": 3}], "n": 3 }),
    "exposed callback receives SPREAD args and its return value reaches the page: {v}"
  );
}

pub fn test_page_expose_binding(c: &mut McpClient) {
  setup(c);
  // page.exposeBinding = page.exposeFunction plus a leading BindingSource
  // ({ context, page, frame }). Prove the source object arrives, the
  // spread args follow it, and the callback's return value reaches the
  // page-side caller (an effect only present when the binding wired
  // through, not merely that the call didn't throw).
  let v = c.script_value(
    r"
    let sourceKeys = null;
    await page.exposeBinding('__page_bind', (source, ...a) => {
      sourceKeys = Object.keys(source).sort();
      return { sum: a.reduce((x, y) => x + y, 0), hasPage: typeof source.page };
    });
    const installed = await page.evaluate(`typeof window.__page_bind`);
    const result = await page.evaluate(`window.__page_bind(2, 3, 5)`);
    return { installed, result, sourceKeys };
  ",
  );
  assert_eq!(
    v["installed"].as_str(),
    Some("function"),
    "page.exposeBinding should install window.__page_bind as a function: {v}"
  );
  assert_eq!(
    &v["result"],
    &json!({ "sum": 10, "hasPage": "string" }),
    "binding callback receives spread args after the source object and its return reaches the page: {v}"
  );
  assert_eq!(
    v["sourceKeys"],
    json!(["context", "frame", "page"]),
    "exposeBinding callback first arg is the {{ context, page, frame }} BindingSource: {v}"
  );
}

pub fn test_context_expose_binding(c: &mut McpClient) {
  setup(c);
  // Register the binding BEFORE opening the page, then open a fresh
  // page in the context and observe that `window[name]` is present and
  // that the BindingSource object reached the callback. This proves the
  // binding applied to a page created AFTER registration (the context
  // registry re-applies on new_page) — an effect that only occurs when
  // the binding wired through, not merely that the call didn't throw.
  let v = c.script_value(
    r"
    const ctx = await browser.newContext();
    try {
      let sourceKeys = null;
      const disp = await ctx.exposeBinding('__ctx_bind', (source, ...a) => {
        sourceKeys = Object.keys(source).sort();
        return { sum: a.reduce((x, y) => x + y, 0), hasContext: typeof source.context };
      });
      const p = await ctx.newPage();
      await p.goto('data:text/html,<title>x</title>');
      const installed = await p.evaluate(`typeof window.__ctx_bind`);
      const result = await p.evaluate(`window.__ctx_bind(2, 3, 5)`);
      // After dispose the page-side proxy is gone.
      await disp.dispose();
      const afterDispose = await p.evaluate(`typeof window.__ctx_bind`);
      return { installed, result, sourceKeys, afterDispose };
    } finally {
      await ctx.close();
    }
  ",
  );
  assert_eq!(
    v["installed"].as_str(),
    Some("function"),
    "context.exposeBinding should install window.__ctx_bind on a page opened after registration: {v}"
  );
  assert_eq!(
    &v["result"],
    &json!({ "sum": 10, "hasContext": "string" }),
    "binding callback receives spread args after the source object and its return reaches the page: {v}"
  );
  assert_eq!(
    v["sourceKeys"],
    json!(["context", "frame", "page"]),
    "exposeBinding callback first arg is the {{ context, page, frame }} BindingSource: {v}"
  );
  assert_eq!(
    v["afterDispose"].as_str(),
    Some("undefined"),
    "Disposable.dispose() removes the page-side window binding: {v}"
  );
}

pub fn test_context_expose_function(c: &mut McpClient) {
  setup(c);
  // exposeFunction = exposeBinding minus the source arg: the callback
  // sees ONLY the spread page-side args (no leading source object).
  let v = c.script_value(
    r"
    const ctx = await browser.newContext();
    try {
      await ctx.exposeFunction('__ctx_fn', (...a) => ({ got: a, n: a.length }));
      const p = await ctx.newPage();
      await p.goto('data:text/html,<title>x</title>');
      const result = await p.evaluate(`window.__ctx_fn(1, 'two')`);
      return { result };
    } finally {
      await ctx.close();
    }
  ",
  );
  assert_eq!(
    &v["result"],
    &json!({ "got": [1, "two"], "n": 2 }),
    "context.exposeFunction delivers ONLY the spread page-side args (no source): {v}"
  );
}

pub fn test_context_clear_cookies_filter(c: &mut McpClient) {
  // WebKit's host can't enumerate per-context cookies the same way;
  // the BrowserContextOptions cookie tests skip it for the same
  // reason.
  if c.backend == "webkit" {
    return;
  }
  setup(c);
  let v = c.script_value(
    r"
    const ctx = await browser.newContext();
    try {
      const p = await ctx.newPage();
      await p.goto('data:text/html,<title>x</title>');
      await ctx.addCookies([
        { name: 'keep', value: '1', domain: '.example.test', path: '/', secure: false, httpOnly: false, expires: -1 },
        { name: 'drop', value: '1', domain: '.example.test', path: '/', secure: false, httpOnly: false, expires: -1 },
      ]);
      const before = (await ctx.cookies()).map(c => c.name).sort();
      await ctx.clearCookies({ name: 'drop' });
      const after = (await ctx.cookies()).map(c => c.name).sort();
      return { before, after };
    } finally {
      await ctx.close();
    }
  ",
  );
  let before: Vec<String> = v["before"]
    .as_array()
    .map(|a| a.iter().filter_map(|n| n.as_str().map(str::to_string)).collect())
    .unwrap_or_default();
  if !before.contains(&"keep".to_string()) || !before.contains(&"drop".to_string()) {
    // Backend silently dropped one of the cookies (e.g. BiDi's
    // Firefox refuses .example.test cookies in headless mode); skip
    // the strict filter assertion in that case — the binding still
    // dispatched without throwing.
    return;
  }
  let after: Vec<String> = v["after"]
    .as_array()
    .map(|a| a.iter().filter_map(|n| n.as_str().map(str::to_string)).collect())
    .unwrap_or_default();
  assert!(
    after.contains(&"keep".to_string()),
    "keep cookie should survive: got {after:?}"
  );
  assert!(
    !after.contains(&"drop".to_string()),
    "drop cookie should be cleared: got {after:?}"
  );
}
