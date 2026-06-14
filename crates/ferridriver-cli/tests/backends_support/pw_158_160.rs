//! Playwright 1.58-1.60 API-subset coverage through QuickJS `run_script`, on
//! every backend (Rule 9):
//! - `locator.description()` getter (1.58)
//! - `getByRole({ description })` matcher (1.60)
//! - `browserContext.setStorageState(state)` (1.59)
//! - `locator.ariaSnapshot({ boxes: true })` (1.60)
//!
//! Each test asserts a page-visible effect that only holds when the option
//! takes effect, not merely that the call didn't throw.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_pass_by_value)]

use super::client::McpClient;

/// `locator.describe(x).description()` round-trips; a plain locator returns
/// null/undefined.
pub fn test_locator_description_getter(c: &mut McpClient) {
  c.nav("<button id=go>Go</button>");
  let v = c.script_value(
    r"
    const described = page.locator('#go').describe('the go button');
    const plain = page.locator('#go');
    return { described: described.description(), plain: plain.description() };
    ",
  );
  assert_eq!(v["described"].as_str(), Some("the go button"), "{v}");
  // rquickjs maps None -> undefined; serde renders it as absent/null.
  assert!(v["plain"].is_null(), "plain locator should have no description: {v}");
}

/// `getByRole` with a `description` matcher selects only the element whose
/// accessible description matches.
pub fn test_get_by_role_description(c: &mut McpClient) {
  c.nav(
    "<button aria-description='primary action'>Save</button>\
     <button aria-description='secondary action'>Cancel</button>",
  );
  let v = c.script_value(
    r"
    const primary = page.getByRole('button', { description: 'primary action' });
    const secondary = page.getByRole('button', { description: 'secondary action' });
    return {
      primaryCount: await primary.count(),
      primaryText: await primary.textContent(),
      secondaryText: await secondary.textContent(),
    };
    ",
  );
  assert_eq!(
    v["primaryCount"].as_i64(),
    Some(1),
    "description should match exactly one: {v}"
  );
  assert_eq!(v["primaryText"].as_str(), Some("Save"), "{v}");
  assert_eq!(v["secondaryText"].as_str(), Some("Cancel"), "{v}");
}

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

/// `ariaSnapshot({ boxes: true })` appends `[box=x,y,w,h]` annotations; the
/// default snapshot does not.
pub fn test_aria_snapshot_boxes(c: &mut McpClient) {
  c.nav("<button>Boxed</button>");
  let v = c.script_value(
    r"
    const withBoxes = await page.locator('body').ariaSnapshot({ boxes: true });
    const without = await page.locator('body').ariaSnapshot();
    return { withBoxes, without };
    ",
  );
  let with_boxes = v["withBoxes"].as_str().unwrap_or_default();
  let without = v["without"].as_str().unwrap_or_default();
  assert!(
    with_boxes.contains("[box="),
    "boxes option should emit [box=...]: {with_boxes}"
  );
  assert!(
    !without.contains("[box="),
    "default snapshot must not emit boxes: {without}"
  );
}

pub fn register(set: &mut super::super::TestSet<'_>) {
  set.run(
    "backends_support::pw_158_160::test_locator_description_getter",
    test_locator_description_getter,
  );
  set.run(
    "backends_support::pw_158_160::test_get_by_role_description",
    test_get_by_role_description,
  );
  set.run(
    "backends_support::pw_158_160::test_context_set_storage_state",
    test_context_set_storage_state,
  );
  set.run(
    "backends_support::pw_158_160::test_aria_snapshot_boxes",
    test_aria_snapshot_boxes,
  );
}
