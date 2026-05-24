//! Rule-9 integration tests for `Video` as a first-class handle
//! accessible via `page.video()`. Playwright public-API contract
//! verified against
//! `/tmp/playwright/packages/playwright-core/types/types.d.ts:21621`.
//!
//! The test harness runs every test against the same ambient MCP
//! session per backend — closing the ambient page would break sibling
//! tests. So the lifecycle test uses `context.newPage()` (newly added
//! to the QuickJS `BrowserContextJs` binding) to open a SEPARATE page
//! under the `recordVideo`-enabled context, records on it, and closes
//! ONLY that new page. The ambient `page` global stays intact.
//!
//! Per-backend expectations:
//! * cdp-pipe / cdp-raw — full screencast recording. The Rust host
//!   verifies file existence + non-zero size after the new page
//!   closes.
//! * bidi — poll-based screencast polyfill. Recorded file must
//!   exist; size is not asserted (fast-close can produce a tiny
//!   file).
//! * webkit — stock `WKWebView` has no screencast primitive. The
//!   typed `Unsupported` from `AnyPage::start_screencast` funnels
//!   into the `VideoSink::finish_err` path. `page.video()` still
//!   returns a non-null handle (Playwright parity); `video.path()`
//!   rejects with the backend reason.

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

/// `recordVideo` off: `page.video()` on the ambient page is null.
pub fn test_video_null_without_recording(c: &mut McpClient) {
  let v = c.script_value(
    r"
    const video = page.video();
    return { isNull: video === null };
  ",
  );
  assert_eq!(
    v["isNull"].as_bool(),
    Some(true),
    "page.video() should be null without recordVideo: {v}"
  );
}

/// End-to-end recording lifecycle on a fresh context-opened page:
/// `setRecordVideo` → `context.newPage()` → navigate → close →
/// `video.path()`.
pub fn test_video_recording_lifecycle(c: &mut McpClient) {
  let record_dir = tempfile::tempdir().expect("allocate tempdir for recording");
  let record_dir_path = record_dir.path().to_path_buf();
  let record_dir_str = record_dir_path.to_string_lossy().into_owned();

  let script = r"
    const [recordDir] = args;
    // 1280x720 covers Firefox's BiDi polyfill output without
    // triggering ffmpeg's `Padded dimensions cannot be smaller than
    // input` error (the polyfill captures at Firefox's rendered
    // viewport size; the default 800x450 is smaller, which the
    // `pad` filter refuses to shrink). CDP backends honor the size
    // natively.
    await context.setRecordVideo({ dir: recordDir, size: { width: 1280, height: 720 } });
    // Open a FRESH page under the recordVideo-enabled context. The
    // ambient `page` global stays untouched so sibling tests aren't
    // disrupted.
    const recPage = await context.newPage();
    // Two navigations give the screencast encoder a visible state
    // transition to capture; the explicit setTimeout pad (real timer
    // via rquickjs-extra-timers) lets the CDP screencast pump flush a
    // trailing frame deterministically rather than racing goto timing.
    await recPage.goto('data:text/html,<h1>rec-1</h1>');
    await recPage.goto('data:text/html,<h1>rec-2</h1>');
    await new Promise((r) => setTimeout(r, 250));
    const video = recPage.video();
    const hasVideo = video !== null;
    if (!hasVideo) {
      return { hasVideo: false };
    }
    await recPage.close();
    try {
      const filePath = await video.path();
      return { hasVideo: true, kind: 'ok', filePath };
    } catch (e) {
      return {
        hasVideo: true,
        kind: 'rejected',
        reason: String(e && e.message ? e.message : e),
      };
    }
  ";
  let v = c.script_value_with_args(script, json!([record_dir_str.clone()]));

  match c.backend.as_str() {
    "cdp-pipe" | "cdp-raw" => {
      assert_eq!(
        v["hasVideo"].as_bool(),
        Some(true),
        "CDP should expose page.video() when recordVideo is set: {v}"
      );
      assert_eq!(v["kind"].as_str(), Some("ok"), "CDP video.path() should resolve: {v}");
      let file_path = v["filePath"].as_str().expect("filePath is a string");
      let p = std::path::Path::new(file_path);
      assert!(p.exists(), "CDP recorded file must exist: {file_path}");
      let size = std::fs::metadata(p).map_or(0, |m| m.len());
      assert!(
        size > 0,
        "CDP recorded file must be non-empty: size={size} path={file_path}"
      );
      assert!(
        file_path.contains(&*record_dir_str),
        "returned path must live inside recordDir {record_dir_str}: {file_path}"
      );
    },
    "bidi" => {
      assert_eq!(
        v["hasVideo"].as_bool(),
        Some(true),
        "BiDi should expose page.video() when recordVideo is set: {v}"
      );
      assert_eq!(
        v["kind"].as_str(),
        Some("ok"),
        "BiDi video.path() should resolve via the polyfill: {v}"
      );
      let file_path = v["filePath"].as_str().expect("filePath is a string");
      assert!(
        std::path::Path::new(file_path).exists(),
        "BiDi recorded file must exist: {file_path}"
      );
    },
    "webkit" => {
      assert_eq!(
        v["hasVideo"].as_bool(),
        Some(true),
        "WebKit page.video() should still return a handle (Playwright parity): {v}"
      );
      assert_eq!(
        v["kind"].as_str(),
        Some("rejected"),
        "WebKit video.path() should reject with the typed Unsupported reason: {v}"
      );
    },
    "pw-webkit" => {
      // PW WebKit's Inspector protocol delivers `Screencast.startScreencast`
      // frames; ferridriver wires these into a per-page recorder that
      // writes a `.webm` file, matching the CDP/BiDi observable surface.
      assert_eq!(
        v["hasVideo"].as_bool(),
        Some(true),
        "pw-webkit page.video() should expose a handle when recordVideo is set: {v}"
      );
      assert_eq!(
        v["kind"].as_str(),
        Some("ok"),
        "pw-webkit video.path() should resolve: {v}"
      );
      let file_path = v["filePath"].as_str().expect("filePath is a string");
      assert!(
        std::path::Path::new(file_path).exists(),
        "pw-webkit recorded file must exist: {file_path}"
      );
      assert!(
        file_path.contains(&*record_dir_str),
        "returned path must live inside recordDir {record_dir_str}: {file_path}"
      );
    },
    other => panic!("unknown backend in test: {other}"),
  }

  drop(record_dir);
}
