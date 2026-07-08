//! Session VM teardown must be clean: dropping a Session (LRU eviction,
//! poisoning rebuild, server shutdown) ends the VM event loop and frees
//! the `QuickJS` runtime without tripping its `JS_FreeRuntime` GC-list
//! assertion.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use ferridriver_script::engine::{RunContext, RunOptions, ScriptEngineConfig, Session};
use ferridriver_script::fs::PathSandbox;
use ferridriver_script::vars::InMemoryVars;

fn make_ctx(dir: &std::path::Path) -> RunContext {
  RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(dir).unwrap()),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: Vec::new(),
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::engine::ScriptCaps::default(),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn create_drop_is_clean() {
  let tmp = tempfile::tempdir().unwrap();
  let ctx = make_ctx(tmp.path());
  let session = Session::create(ScriptEngineConfig::default(), &ctx).await.unwrap();
  drop(session);
  tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn create_execute_drop_is_clean() {
  let tmp = tempfile::tempdir().unwrap();
  let ctx = make_ctx(tmp.path());
  let session = Session::create(ScriptEngineConfig::default(), &ctx).await.unwrap();
  let run = session.execute("return 1;", &[], RunOptions::default(), &ctx).await;
  assert!(matches!(run.result.outcome, ferridriver_script::Outcome::Ok { .. }));
  drop(session);
  tokio::time::sleep(Duration::from_millis(200)).await;
}
