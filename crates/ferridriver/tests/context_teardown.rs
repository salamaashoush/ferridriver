//! Per-context state must not survive the context.
//!
//! A process that opens and closes a context per session — a test runner, a
//! synthetic-monitoring loop, a cloud test host — accumulates whatever
//! teardown forgets. These registries are keyed by a composite session key
//! built from a process-wide counter that never reuses a value, so a missing
//! prune is unbounded growth rather than a bounded overcount.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ferridriver::chromium;
use ferridriver::options::{BrowserContextOptions, LaunchOptions};

/// Open and close N contexts, then assert every per-context registry is back
/// to the size it had before. Each context carries an options bag and opens a
/// page, so the registries are genuinely populated rather than trivially empty.
#[tokio::test(flavor = "multi_thread")]
async fn closing_contexts_leaves_no_per_context_state() {
  let browser = chromium()
    .launch(LaunchOptions {
      headless: Some(true),
      ..Default::default()
    })
    .await
    .expect("launch chromium");

  let state = browser.state().clone();
  let sizes = || {
    let state = state.clone();
    async move {
      let s = state.read().await;
      // Take every std::sync lock's length before touching the async
      // registries: holding a MutexGuard across an await is exactly the
      // shape that deadlocks under a multi-thread runtime.
      let sync_lens = [
        s.context_options.lock().unwrap().len(),
        s.context_events.lock().unwrap().len(),
        s.context_closed.lock().unwrap().len(),
        s.record_video.lock().unwrap().len(),
        s.har_recorders.lock().unwrap().len(),
        s.context_har_updates.lock().unwrap().len(),
        s.clock_installed.lock().unwrap().len(),
        s.storage_state_hydrated.lock().unwrap().len(),
      ];
      let async_lens = [
        s.context_bindings.read().await.len(),
        s.context_ws_routes.read().await.len(),
        s.context_routes.read().await.len(),
        s.context_init_scripts.read().await.len(),
      ];
      (sync_lens, async_lens)
    }
  };

  let before = sizes().await;

  for _ in 0..5 {
    let ctx = browser
      .new_context()
      .options(BrowserContextOptions {
        user_agent: Some("ferri-teardown-probe".to_string()),
        ..Default::default()
      })
      .await
      .expect("new context");
    let page = ctx.new_page().await.expect("new page");
    page.set_content("<h1>teardown</h1>").await.expect("set content");
    ctx.close().await.expect("close context");
  }

  let after = sizes().await;
  assert_eq!(
    before, after,
    "per-context registries grew across 5 open/close cycles \
     (context_options, context_events, context_closed, record_video, \
      har_recorders, context_har_updates, clock_installed, \
      storage_state_hydrated, context_bindings, context_ws_routes, \
      context_routes, context_init_scripts)"
  );

  browser.close().await.expect("close browser");
}
