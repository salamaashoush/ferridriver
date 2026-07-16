//! Backend tests for the `expect()` global exposed by the QuickJS
//! script layer — exercises web-first matchers (`toBeVisible`,
//! `toHaveText`, `toBeOK`, ...) and the Jest value-matcher path
//! through a live browser.
//!
//! Mirrors the layout of the other `backends_support` modules: every
//! test routes through `run_script` and asserts a real page-side
//! observation. Listed in `tests/backends.rs::run_all_tests` so each
//! backend exercises the matcher.

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

pub fn test_expect_to_be_visible(c: &mut McpClient) {
  c.nav("<button id='b'>hi</button><span id='hidden' style='display:none'>x</span>");
  let v = c.script_value(
    "await expect(page.locator('#b')).toBeVisible(); \
     await expect(page.locator('#hidden')).not.toBeVisible(); \
     return 'ok';",
  );
  assert_eq!(v, json!("ok"));
}

pub fn test_expect_to_have_text(c: &mut McpClient) {
  c.nav("<h1>Hello World</h1>");
  let v = c.script_value(
    "await expect(page.locator('h1')).toHaveText('Hello World'); \
     await expect(page.locator('h1')).toHaveText(/^Hello/); \
     return 'ok';",
  );
  assert_eq!(v, json!("ok"));
}

pub fn test_expect_to_contain_text(c: &mut McpClient) {
  c.nav("<p id='msg'>The quick brown fox</p>");
  let v = c.script_value(
    "await expect(page.locator('#msg')).toContainText('quick brown'); \
     return 'ok';",
  );
  assert_eq!(v, json!("ok"));
}

pub fn test_expect_to_have_count(c: &mut McpClient) {
  c.nav("<ul><li>a</li><li>b</li><li>c</li></ul>");
  let v = c.script_value(
    "await expect(page.locator('li')).toHaveCount(3); \
     return 'ok';",
  );
  assert_eq!(v, json!("ok"));
}

pub fn test_expect_to_have_attribute(c: &mut McpClient) {
  c.nav("<a id='lnk' href='https://example.com' data-x>link</a>");
  let v = c.script_value(
    "await expect(page.locator('#lnk')).toHaveAttribute('href', 'https://example.com'); \
     await expect(page.locator('#lnk')).toHaveAttribute('data-x'); \
     return 'ok';",
  );
  assert_eq!(v, json!("ok"));
}

pub fn test_expect_to_have_value(c: &mut McpClient) {
  c.nav("<input id='inp' value='hello' />");
  let v = c.script_value(
    "await expect(page.locator('#inp')).toHaveValue('hello'); \
     return 'ok';",
  );
  assert_eq!(v, json!("ok"));
}

pub fn test_expect_page_title_and_url(c: &mut McpClient) {
  c.nav("<title>My Page</title><h1>x</h1>");
  let v = c.script_value(
    "await expect(page).toHaveTitle('My Page'); \
     await expect(page).toHaveURL(/^data:/); \
     return 'ok';",
  );
  assert_eq!(v, json!("ok"));
}

pub fn test_expect_value_matchers_in_script(c: &mut McpClient) {
  c.nav("<body></body>");
  let v = c.script_value(
    "expect(2 + 2).toBe(4); \
     expect({a: 1, b: 2}).toEqual({a: 1, b: 2}); \
     expect([1, 2, 3]).toContain(2); \
     expect({id: 7}).toEqual({id: expect.any(Number)}); \
     expect({a: 1, b: 2, c: 3}).toEqual(expect.objectContaining({a: 1})); \
     return 'ok';",
  );
  assert_eq!(v, json!("ok"));
}

pub fn test_expect_to_throw_in_script(c: &mut McpClient) {
  c.nav("<body></body>");
  let v = c.script_value(
    "await expect(() => { throw new Error('boom: bad'); }).toThrow('bad'); \
     await expect(() => 42).not.toThrow(); \
     return 'ok';",
  );
  assert_eq!(v, json!("ok"));
}

pub fn test_expect_failure_throws(c: &mut McpClient) {
  // A failing assertion must throw a JS error that surfaces as a
  // structured script error — not a silent pass.
  c.nav("<body></body>");
  let payload = c.script("expect(1).toBe(2); return 'ok';");
  let status = payload["status"].as_str().unwrap_or_default();
  assert_ne!(
    status, "ok",
    "expected failing toBe to surface as script error; got status={status}, payload={payload}"
  );
}

pub fn test_expect_poll_with_browser(c: &mut McpClient) {
  // Counter rises with each call; toEqual(3) becomes true on attempt 3.
  c.nav("<div id='counter'>0</div>");
  let v = c.script_value(
    "await page.evaluate(\"window.__attempt = 0\"); \
     await expect.poll(async () => { \
       const n = await page.evaluate(\"window.__attempt = (window.__attempt||0)+1\"); \
       return n; \
     }, { timeout: 3000 }).toEqual(3); \
     return 'ok';",
  );
  assert_eq!(v, json!("ok"));
}

pub fn test_expect_inline_timeout_option(c: &mut McpClient) {
  // The inline `{ timeout }` matcher option must bound the retry loop:
  // a 400ms timeout on a never-appearing element fails well under the
  // 5s default. Elapsed is measured in-script to exclude harness time.
  c.nav("<h1>x</h1>");
  let v = c.script_value(
    "const t = Date.now(); \
     let failed = false; \
     try { await expect(page.locator('#never')).toBeVisible({ timeout: 400 }); } \
     catch (e) { failed = true; } \
     return { failed, elapsed: Date.now() - t };",
  );
  assert_eq!(v["failed"], json!(true), "toBeVisible must fail: {v}");
  let elapsed = v["elapsed"].as_i64().unwrap_or(0);
  assert!(
    (300..3000).contains(&elapsed),
    "timeout 400 should fail in ~400ms, not the 5s default: {elapsed}ms"
  );
}

pub fn test_expect_to_pass_retries(c: &mut McpClient) {
  // toPass retries the callback until it stops throwing; the third
  // attempt succeeds, so exactly 3 attempts must be observed.
  c.nav("<body></body>");
  let v = c.script_value(
    "let attempts = 0; \
     await expect(async () => { \
       attempts += 1; \
       if (attempts < 3) { throw new Error('not yet'); } \
     }).toPass({ intervals: [50], timeout: 5000 }); \
     return attempts;",
  );
  assert_eq!(v, json!(3));
}

pub fn test_expect_to_pass_timeout_and_intervals(c: &mut McpClient) {
  // An always-failing callback must time out on the toPass deadline
  // (not the unbounded default), keep the last error message, and honor
  // the custom 50ms interval schedule (~8 attempts in 400ms, far more
  // than the default schedule's 3).
  c.nav("<body></body>");
  let v = c.script_value(
    "const t = Date.now(); \
     let attempts = 0; \
     let msg = ''; \
     try { \
       await expect(async () => { attempts += 1; throw new Error('always fails'); }) \
         .toPass({ intervals: [50], timeout: 400 }); \
     } catch (e) { msg = String(e.message); } \
     return { attempts, msg, elapsed: Date.now() - t };",
  );
  let msg = v["msg"].as_str().unwrap_or_default();
  assert!(msg.contains("always fails"), "last error must surface: {v}");
  let attempts = v["attempts"].as_i64().unwrap_or(0);
  assert!(attempts >= 5, "50ms intervals over 400ms should retry >=5 times: {v}");
  let elapsed = v["elapsed"].as_i64().unwrap_or(0);
  assert!(elapsed < 3000, "timeout 400 must bound the loop: {elapsed}ms");
}

pub fn test_expect_not_to_pass(c: &mut McpClient) {
  // `.not.toPass` succeeds as soon as the callback throws.
  c.nav("<body></body>");
  let v = c.script_value(
    "await expect(async () => { throw new Error('nope'); }).not.toPass({ timeout: 2000 }); \
     return 'ok';",
  );
  assert_eq!(v, json!("ok"));
}

pub fn test_expect_boolean_state_options(c: &mut McpClient) {
  // Playwright lowers `visible: false` to the hidden assertion,
  // `enabled: false` to disabled, `checked: false` to unchecked, and
  // `editable: false` to readonly (NOT plain negation — a disabled
  // input is neither editable nor readonly).
  c.nav(
    "<span id='gone' style='display:none'>x</span> \
     <button id='btn' disabled>b</button> \
     <input id='cb' type='checkbox' /> \
     <input id='ro' readonly value='r' /> \
     <input id='rw' value='w' />",
  );
  let v = c.script_value(
    "await expect(page.locator('#gone')).toBeVisible({ visible: false }); \
     await expect(page.locator('#btn')).toBeEnabled({ enabled: false }); \
     await expect(page.locator('#cb')).toBeChecked({ checked: false }); \
     await expect(page.locator('#ro')).toBeEditable({ editable: false }); \
     let rwReadonly = false; \
     try { await expect(page.locator('#rw')).toBeEditable({ editable: false, timeout: 400 }); rwReadonly = true; } \
     catch (e) {} \
     return { rwReadonly };",
  );
  assert_eq!(v["rwReadonly"], json!(false), "writable input is not readonly: {v}");
}

pub fn test_expect_text_match_options(c: &mut McpClient) {
  // ignoreCase folds both sides; useInnerText reads rendered text
  // (display:none children are excluded, unlike textContent).
  c.nav("<div id='d'>Hello<span style='display:none'>ZZZ</span></div>");
  let v = c.script_value(
    "await expect(page.locator('#d')).toHaveText('hellozzz', { ignoreCase: true }); \
     await expect(page.locator('#d')).toHaveText('Hello', { useInnerText: true }); \
     await expect(page.locator('#d')).toContainText('HELLO', { ignoreCase: true }); \
     let exactFailed = false; \
     try { await expect(page.locator('#d')).toHaveText('Hello', { timeout: 400 }); } \
     catch (e) { exactFailed = true; } \
     return { exactFailed };",
  );
  assert_eq!(
    v["exactFailed"],
    json!(true),
    "textContent includes the hidden span, so the plain match must fail: {v}"
  );
}

pub fn test_expect_to_have_attribute_overloads(c: &mut McpClient) {
  // Playwright overloads: (name, value, options?) and (name, options?).
  // An options bag in the second slot is the presence check; ignoreCase
  // applies to the value comparison.
  c.nav("<a id='lnk' href='HTTPS://EXAMPLE.COM' data-x>link</a>");
  let v = c.script_value(
    "await expect(page.locator('#lnk')).toHaveAttribute('data-x', { timeout: 2000 }); \
     await expect(page.locator('#lnk')).toHaveAttribute('href', 'https://example.com', { ignoreCase: true }); \
     let caseFailed = false; \
     try { await expect(page.locator('#lnk')).toHaveAttribute('href', 'https://example.com', { timeout: 400 }); } \
     catch (e) { caseFailed = true; } \
     return { caseFailed };",
  );
  assert_eq!(v["caseFailed"], json!(true), "case-sensitive compare must fail: {v}");
}

pub fn test_expect_new_locator_matchers(c: &mut McpClient) {
  c.nav(
    "<input id='focus-me' /> \
     <div id='classy' class='alpha beta'>c</div> \
     <div id='styled' style='color: rgb(255, 0, 0)'>s</div> \
     <button id='role-btn'>r</button> \
     <select id='sel' multiple><option value='a' selected>a</option><option value='b' selected>b</option></select>",
  );
  let v = c.script_value(
    "await page.locator('#focus-me').focus(); \
     await expect(page.locator('#focus-me')).toBeFocused(); \
     await expect(page.locator('#classy')).toHaveClass('alpha beta'); \
     await expect(page.locator('#classy')).toContainClass('beta'); \
     await expect(page.locator('#styled')).toHaveCSS('color', 'rgb(255, 0, 0)'); \
     await expect(page.locator('#classy')).toHaveId('classy'); \
     await expect(page.locator('#role-btn')).toHaveRole('button'); \
     await expect(page.locator('#focus-me')).toHaveJSProperty('type', 'text'); \
     await expect(page.locator('#classy')).toBeInViewport(); \
     await expect(page.locator('#sel')).toHaveValues(['a', 'b']); \
     return 'ok';",
  );
  assert_eq!(v, json!("ok"));
}

pub fn test_expect_to_have_url_ignore_case(c: &mut McpClient) {
  c.nav("<h1>x</h1>");
  let v = c.script_value(
    "await expect(page).toHaveURL(/^DATA:/i); \
     await expect(page).toHaveURL(/^DATA:/, { ignoreCase: true }); \
     let strictFailed = false; \
     try { await expect(page).toHaveURL(/^DATA:/, { timeout: 400 }); } \
     catch (e) { strictFailed = true; } \
     return { strictFailed };",
  );
  assert_eq!(v["strictFailed"], json!(true), "case-sensitive regex must fail: {v}");
}

pub fn test_expect_poll_intervals_option(c: &mut McpClient) {
  // A 50ms interval schedule reaches attempt 4 well inside 3s; the
  // default schedule (100/250/500/1000) would take ~1.85s — both pass,
  // so assert the tight schedule finished fast.
  c.nav("<body></body>");
  let v = c.script_value(
    "const t = Date.now(); \
     let n = 0; \
     await expect.poll(() => { n += 1; return n; }, { intervals: [50], timeout: 3000 }).toEqual(4); \
     return Date.now() - t;",
  );
  let elapsed = v.as_i64().unwrap_or(i64::MAX);
  assert!(
    elapsed < 1500,
    "4 attempts at 50ms intervals should be fast: {elapsed}ms"
  );
}
