#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Sidecar transport + `sidecars` JS binding, exercised against a
//! self-contained fixture (`sidecar_echo`) that speaks the fd 3/4 NUL-JSON
//! protocol. No external binaries — the fixture is built by cargo and its
//! path comes from `CARGO_BIN_EXE_sidecar_echo`.

use std::sync::Arc;

use ferridriver_script::sidecar::{Sidecar, SidecarSpec};
use ferridriver_script::{
  InMemoryVars, Outcome, PathSandbox, RunContext, RunOptions, ScriptEngine, ScriptEngineConfig,
};

const FIXTURE: &str = env!("CARGO_BIN_EXE_sidecar_echo");

fn spec() -> SidecarSpec {
  SidecarSpec {
    name: "echo".into(),
    command: vec![FIXTURE.to_string()],
    env: vec![],
    cwd: None,
    startup_timeout_ms: 5000,
  }
}

// ── Transport-level ────────────────────────────────────────────────────────

#[tokio::test]
async fn ping_round_trips_over_fd_3_4() {
  let s = Sidecar::connect(&spec()).await.expect("connect");
  let r = s.send("ping", None, 5000).await.expect("ping");
  assert_eq!(
    r.get("ok").and_then(serde_json::Value::as_bool),
    Some(true),
    "ping -> {r}"
  );
  s.close().await.expect("close");
}

#[tokio::test]
async fn params_round_trip_via_echo() {
  let s = Sidecar::connect(&spec()).await.expect("connect");
  let r = s
    .send("echo", Some(serde_json::json!({ "a": 1, "b": "x" })), 5000)
    .await
    .expect("echo");
  assert_eq!(r, serde_json::json!({ "a": 1, "b": "x" }));
  s.close().await.expect("close");
}

#[tokio::test]
async fn unknown_method_is_a_remote_error_but_keeps_transport_alive() {
  let s = Sidecar::connect(&spec()).await.expect("connect");
  let err = s.send("__nope__", None, 5000).await.unwrap_err();
  assert!(err.to_string().contains("unknown method"), "err: {err}");
  // Transport still works after a remote error.
  let r = s.send("ping", None, 5000).await.expect("ping after error");
  assert_eq!(r.get("ok").and_then(serde_json::Value::as_bool), Some(true));
  s.close().await.expect("close");
}

#[tokio::test]
async fn concurrent_requests_match_by_id() {
  let s = Sidecar::connect(&spec()).await.expect("connect");
  let (a, b, c) = tokio::join!(
    s.send("echo", Some(serde_json::json!(1)), 5000),
    s.send("echo", Some(serde_json::json!(2)), 5000),
    s.send("echo", Some(serde_json::json!(3)), 5000),
  );
  assert_eq!(a.expect("a"), serde_json::json!(1));
  assert_eq!(b.expect("b"), serde_json::json!(2));
  assert_eq!(c.expect("c"), serde_json::json!(3));
  s.close().await.expect("close");
}

// ── JS binding ──────────────────────────────────────────────────────────────

fn engine() -> (ScriptEngine, tempfile::TempDir, RunContext) {
  let tmp = tempfile::tempdir().expect("tempdir");
  let sandbox = PathSandbox::new(tmp.path()).expect("sandbox");
  let context = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(sandbox),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    plugins: Vec::new(),
    trusted_modules: false,
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
  };
  let cfg = ScriptEngineConfig {
    sidecars: vec![spec()],
    ..Default::default()
  };
  (ScriptEngine::new(cfg), tmp, context)
}

#[tokio::test]
async fn connect_send_close_from_js() {
  let (eng, _tmp, ctx) = engine();
  let src = r"
    const sc = await sidecars.connect('echo');
    const ping = await sc.send('ping');
    const echoed = await sc.send('echo', { hello: 'world' });
    await sc.close();
    return { ok: ping.ok === true, echoed };
  ";
  let result = eng.run(src, &[], RunOptions::default(), ctx).await;
  match result.outcome {
    Outcome::Ok { success } => {
      assert_eq!(
        success.value,
        serde_json::json!({ "ok": true, "echoed": { "hello": "world" } })
      );
    },
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[tokio::test]
async fn unknown_sidecar_name_rejects() {
  let (eng, _tmp, ctx) = engine();
  let src = r"
    try { await sidecars.connect('does-not-exist'); return 'no-throw'; }
    catch (e) { return String(e.message || e); }
  ";
  let result = eng.run(src, &[], RunOptions::default(), ctx).await;
  match result.outcome {
    Outcome::Ok { success } => {
      let msg = success.value.as_str().unwrap_or("");
      assert!(msg.contains("unknown sidecar"), "got: {msg}");
    },
    Outcome::Error { error } => panic!("expected a caught rejection, got engine error: {error:?}"),
  }
}
