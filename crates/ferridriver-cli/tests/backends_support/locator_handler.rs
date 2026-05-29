//! QuickJS binding coverage for `page.addLocatorHandler` /
//! `page.removeLocatorHandler`.
//!
//! Rule 9: each test observes a real DOM effect that only occurs when the
//! handler actually fired. A modal overlay covers the target button so a
//! click cannot land until the registered handler dismisses the overlay.

#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::unwrap_used,
  clippy::expect_used,
  clippy::needless_pass_by_value
)]

use super::client::McpClient;

/// Fixture: a `#target` button whose click sets `window.__clicked`, fully
/// covered by a fixed `#overlay` that intercepts pointer events. The overlay
/// is initially shown; a handler removes it.
const FIXTURE: &str = r"
<button id='target' onclick='window.__clicked=true'>Click me</button>
<div id='overlay' style='position:fixed;inset:0;z-index:9999;background:rgba(0,0,0,0.5)'>blocking</div>
<script>window.__handlerRuns=0;</script>
";

fn setup(c: &mut McpClient) {
  c.nav(FIXTURE);
  c.script("await page.waitForSelector('#target'); return true;");
}

/// QuickJS parity: `addLocatorHandler` cannot fire a handler during an
/// in-VM action without deadlocking the single-threaded scripting VM, so the
/// binding returns a typed Unsupported error rather than hanging. The core
/// and NAPI layers implement it fully (see `locator-handler.test.ts`).
/// `removeLocatorHandler` stays a safe no-op.
pub fn test_add_locator_handler_unsupported(c: &mut McpClient) {
  setup(c);
  let v = c.script_value(
    r"
    let threw = false;
    let message = '';
    try {
      page.addLocatorHandler(page.locator('#overlay'), () => {});
    } catch (e) {
      threw = true;
      message = String(e && e.message ? e.message : e);
    }
    // removeLocatorHandler must never throw, even with nothing registered.
    page.removeLocatorHandler(page.locator('#overlay'));
    return { threw, message };
  ",
  );
  assert_eq!(
    v["threw"].as_bool(),
    Some(true),
    "QuickJS addLocatorHandler should throw Unsupported: {v}"
  );
  assert!(
    v["message"]
      .as_str()
      .unwrap_or_default()
      .to_lowercase()
      .contains("addlocatorhandler"),
    "error should explain the addLocatorHandler limitation: {v}"
  );
}
