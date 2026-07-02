//! Rule-9 integration test for `page.localStorage` / `page.sessionStorage`
//! WebStorage accessors (Playwright 1.61) through QuickJS `run_script`, on
//! every backend.
//!
//! Asserts a page-visible effect that only holds when the binding hits real
//! DOM storage, cross-checked against `window.localStorage` in-page.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_pass_by_value)]

use super::client::McpClient;

/// `page.localStorage` / `page.sessionStorage` round-trip:
/// `setItem` → `getItem` → `items` → `removeItem` → `clear`, observed
/// against the live storage object on a real (`http://localhost`) origin.
pub fn test_web_storage(c: &mut McpClient) {
  let port = super::spawn_html_server();
  let v = c.script_value_with_args(
    r"
    const [url] = args;
    await page.goto(url);
    await page.localStorage.setItem('token', 'abc');
    await page.localStorage.setItem('user', 'sam');
    await page.sessionStorage.setItem('sid', 'sess-1');

    const token = await page.localStorage.getItem('token');
    const missing = await page.localStorage.getItem('nope');
    const items = await page.localStorage.items();
    // Cross-check against the live DOM API to prove we hit real storage.
    const domToken = await page.evaluate(() => window.localStorage.getItem('token'));
    const domSid = await page.evaluate(() => window.sessionStorage.getItem('sid'));

    await page.localStorage.removeItem('user');
    const afterRemove = (await page.localStorage.items()).map(i => i.name).sort();

    await page.localStorage.clear();
    const afterClear = await page.localStorage.items();

    return {
      token,
      missingIsNull: missing === null || missing === undefined,
      itemNames: items.map(i => i.name).sort(),
      tokenValue: (items.find(i => i.name === 'token') || {}).value,
      domToken,
      domSid,
      afterRemove,
      afterClearLen: afterClear.length,
    };
    ",
    serde_json::json!([format!("http://localhost:{port}/store")]),
  );
  assert_eq!(
    v["token"].as_str(),
    Some("abc"),
    "getItem should read the set value: {v}"
  );
  assert_eq!(v["missingIsNull"].as_bool(), Some(true), "absent key must be null: {v}");
  assert_eq!(
    v["domToken"].as_str(),
    Some("abc"),
    "binding must write real DOM storage: {v}"
  );
  assert_eq!(
    v["domSid"].as_str(),
    Some("sess-1"),
    "sessionStorage must be separate + real: {v}"
  );
  let names: Vec<&str> = v["itemNames"]
    .as_array()
    .unwrap()
    .iter()
    .filter_map(|n| n.as_str())
    .collect();
  assert_eq!(names, vec!["token", "user"], "items() must list both entries: {v}");
  assert_eq!(v["tokenValue"].as_str(), Some("abc"), "{v}");
  let after_remove: Vec<&str> = v["afterRemove"]
    .as_array()
    .unwrap()
    .iter()
    .filter_map(|n| n.as_str())
    .collect();
  assert_eq!(
    after_remove,
    vec!["token"],
    "removeItem must drop only the named key: {v}"
  );
  assert_eq!(v["afterClearLen"].as_i64(), Some(0), "clear must empty the store: {v}");
}

pub fn register(set: &mut super::super::TestSet<'_>) {
  set.run("backends_support::web_storage::test_web_storage", test_web_storage);
}
