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
