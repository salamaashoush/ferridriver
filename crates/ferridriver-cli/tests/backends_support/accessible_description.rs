//! Rule-9 integration tests for accessible-description handling through
//! QuickJS `run_script`, on every backend:
//! - `locator.describe(text).description()` getter (Playwright 1.58).
//! - `getByRole(role, { description })` matcher (Playwright 1.60).
//!
//! Each test asserts a page-visible effect that only holds when the feature
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

pub fn register(set: &mut super::super::TestSet<'_>) {
  set.run(
    "backends_support::accessible_description::test_locator_description_getter",
    test_locator_description_getter,
  );
  set.run(
    "backends_support::accessible_description::test_get_by_role_description",
    test_get_by_role_description,
  );
}
