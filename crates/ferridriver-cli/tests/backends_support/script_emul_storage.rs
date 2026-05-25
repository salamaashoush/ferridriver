#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::single_char_pattern,
  clippy::unwrap_used,
  clippy::expect_used
)]
//! run_script: emulation / storage tests, extracted from backends.rs.

use serde_json::json;

use super::client::McpClient;

pub fn test_script_user_agent(c: &mut McpClient) {
  // `userAgent` is a context-level option in Playwright. Construct a
  // fresh context via the `browser` global with `userAgent` set, open
  // a page there, and observe `navigator.userAgent`. WebKit skips —
  // single-context limitation (see skip_if_no_new_context in §4.1
  // tests).
  if c.backend == "webkit" || c.backend == "bidi" {
    // WebKit: no multi-context. BiDi: our backend currently returns
    // `Unsupported` for the userAgent override (Firefox BiDi wiring
    // not yet in place).
    return;
  }
  let v = c.script_value(
    "const ctx = await browser.newContext({ userAgent: 'TestBot/1.0' }); \
     try { \
       const p = await ctx.newPage(); \
       return await p.evaluate('navigator.userAgent'); \
     } finally { \
       await ctx.close(); \
     }",
  );
  let ua = v.as_str().unwrap_or("").to_string();
  assert!(ua.contains("TestBot"), "userAgent option should override UA: {ua}");
}

pub fn test_script_viewport(c: &mut McpClient) {
  c.nav("<body></body>");
  let v = c.script_value(
    "await page.setViewportSize({ width: 375, height: 812 }); \
       const w = await page.evaluate('window.innerWidth'); \
       const h = await page.evaluate('window.innerHeight'); \
       return { w: w, h: h };",
  );
  assert_eq!(v["w"], json!(375));
  assert_eq!(v["h"], json!(812));
}

pub fn test_script_geolocation(c: &mut McpClient) {
  c.nav("<body></body>");
  let v = c.script_value(
    "await context.setGeolocation(37.7749, -122.4194, 1.0); \
       const raw = await page.evaluate('typeof navigator.geolocation'); \
       return raw;",
  );
  assert_eq!(v, json!("object"), "geolocation should be available");
}

pub fn test_script_offline(c: &mut McpClient) {
  // BiDi has no `network.setEmulatedConditions` equivalent yet —
  // `context.setOffline` returns Unsupported there.
  if c.backend == "bidi" {
    return;
  }
  c.nav("<body></body>");
  let v = c.script_value(
    "await context.setOffline(true); \
       const rawOffline = await page.evaluate('navigator.onLine'); \
       await context.setOffline(false); \
       const rawOnline = await page.evaluate('navigator.onLine'); \
       return { offline: rawOffline, online: rawOnline };",
  );
  assert_eq!(v["offline"], json!(false), "should be offline");
  assert_eq!(v["online"], json!(true), "should be back online");
}

pub fn test_script_cookies(c: &mut McpClient) {
  c.nav_url("https://example.com");
  let v = c.script_value(
    "await context.addCookies([{ \
         name: 'k', value: 'v', domain: 'example.com', path: '/', \
         secure: false, httpOnly: false, sameSite: 'Lax' \
       }]); \
       const cookies = await context.cookies(); \
       const found = cookies.find(c => c.name === 'k'); \
       await context.deleteCookie('k'); \
       const after = await context.cookies(); \
       return { foundValue: found?.value ?? null, afterCount: after.filter(c => c.name === 'k').length };",
  );
  assert_eq!(v["foundValue"], json!("v"), "cookie should round-trip");
  assert_eq!(v["afterCount"], json!(0), "deleteCookie should remove it");
}

pub fn test_script_localstorage(c: &mut McpClient) {
  c.nav_url("https://example.com");
  // localStorage lives in the page, not the runner — drive it through
  // page.evaluate. page.evaluate rehydrates native JS values directly.
  let v = c.script_value(
    "await page.evaluate(\"localStorage.setItem('lk', 'lv')\"); \
       const got = await page.evaluate(\"localStorage.getItem('lk')\"); \
       const count = await page.evaluate(\"localStorage.length\"); \
       return { got, count };",
  );
  assert_eq!(v["got"], json!("lv"));
  assert!(v["count"].as_i64().unwrap_or(0) >= 1);
}

pub fn test_script_markdown(c: &mut McpClient) {
  c.nav("<h1>Title</h1><p>Hello world</p><ul><li>Item 1</li><li>Item 2</li></ul>");
  let v = c.script_value("return await page.markdown();");
  let md = v.as_str().unwrap_or("").to_string();
  assert!(md.contains("# Title"), "markdown headings: {md}");
  assert!(md.contains("Hello world"), "markdown paragraphs: {md}");
  assert!(md.contains("- Item"), "markdown lists: {md}");
}

pub fn test_script_markdown_links(c: &mut McpClient) {
  c.nav("<p>Visit <a href='https://example.com'>Example</a></p>");
  let v = c.script_value("return await page.markdown();");
  let md = v.as_str().unwrap_or("").to_string();
  assert!(md.contains("[Example](https://example.com)"), "markdown links: {md}");
}

pub fn register(set: &mut crate::TestSet<'_>) {
  set.run(
    "backends_support::script_emul_storage::test_script_user_agent",
    test_script_user_agent,
  );
  set.run(
    "backends_support::script_emul_storage::test_script_viewport",
    test_script_viewport,
  );
  set.run(
    "backends_support::script_emul_storage::test_script_geolocation",
    test_script_geolocation,
  );
  set.run(
    "backends_support::script_emul_storage::test_script_offline",
    test_script_offline,
  );
  set.run(
    "backends_support::script_emul_storage::test_script_cookies",
    test_script_cookies,
  );
  set.run(
    "backends_support::script_emul_storage::test_script_localstorage",
    test_script_localstorage,
  );
  set.run(
    "backends_support::script_emul_storage::test_script_markdown",
    test_script_markdown,
  );
  set.run(
    "backends_support::script_emul_storage::test_script_markdown_links",
    test_script_markdown_links,
  );
}
