//! Rule-9 integration test for `locator.ariaSnapshot({ boxes: true })`
//! (Playwright 1.60) through QuickJS `run_script`, on every backend.
//!
//! Asserts a page-visible effect that only holds when the option takes
//! effect, not merely that the call didn't throw.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_pass_by_value)]

use super::client::McpClient;

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
    "backends_support::aria_snapshot::test_aria_snapshot_boxes",
    test_aria_snapshot_boxes,
  );
}
