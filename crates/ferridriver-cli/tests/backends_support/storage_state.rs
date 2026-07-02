//! Rule-9 integration test for `browserContext.setStorageState(state)`
//! (Playwright 1.59) through QuickJS `run_script`, on every backend.
//!
//! Asserts a page-visible effect that only holds when the call takes
//! effect, not merely that it didn't throw.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_pass_by_value)]

use super::client::McpClient;

/// `context.setStorageState` clears existing cookies and applies the new
/// state: a pre-seeded cookie is gone, the state's cookie is present.
/// (Cookies work on the data: origin where localStorage is opaque.)
pub fn test_context_set_storage_state(c: &mut McpClient) {
  c.nav("<body>storage</body>");
  let v = c.script_value(
    r"
    // Seed a cookie that setStorageState must clear.
    await context.addCookies([{ name: 'stale', value: 'yes', domain: 'example.com', path: '/' }]);
    await context.setStorageState({
      cookies: [{ name: 'seeded', value: 'fromState', domain: 'example.com', path: '/' }],
      origins: [],
    });
    const cookies = await context.cookies();
    return { names: cookies.map(c => c.name) };
    ",
  );
  let names: Vec<String> = v["names"]
    .as_array()
    .map(|a| a.iter().filter_map(|n| n.as_str().map(String::from)).collect())
    .unwrap_or_default();
  assert!(
    !names.contains(&"stale".to_string()),
    "stale cookie should be cleared: {names:?}"
  );
  assert!(
    names.contains(&"seeded".to_string()),
    "seeded cookie missing: {names:?}"
  );
}

pub fn register(set: &mut super::super::TestSet<'_>) {
  set.run(
    "backends_support::storage_state::test_context_set_storage_state",
    test_context_set_storage_state,
  );
}
