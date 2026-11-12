#![allow(clippy::expect_used, clippy::unwrap_used)]
//! The sidecar wire contract end to end: a child reading requests on fd 3
//! and answering on fd 4. On Windows those are C runtime descriptors the
//! child's runtime inherits, which `sidecar_echo` adopts through
//! `_get_osfhandle`, so this is also the proof that the hand-over works.

use std::time::Duration;

use ferridriver_script::sidecar::{Sidecar, SidecarSpec};
use serde_json::json;

const FIXTURE: &str = env!("CARGO_BIN_EXE_sidecar_echo");

fn spec() -> SidecarSpec {
  SidecarSpec {
    name: "echo".into(),
    command: vec![FIXTURE.into()],
    env: Vec::new(),
    cwd: None,
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_replies_and_events_cross_fds_three_and_four() {
  let sidecar = Sidecar::connect(&spec()).await.expect("connect");
  assert_eq!(
    sidecar.send("ping", None, 5_000).await.expect("ping"),
    json!({ "ok": true })
  );
  assert_eq!(
    sidecar
      .send("echo", Some(json!({ "word": "sashoush" })), 5_000)
      .await
      .expect("echo"),
    json!({ "word": "sashoush" })
  );

  let mut events = sidecar.subscribe();
  sidecar
    .send("emit", Some(json!({ "event": "ready", "payload": 7 })), 5_000)
    .await
    .expect("emit");
  let (name, payload) = tokio::time::timeout(Duration::from_secs(5), events.recv())
    .await
    .expect("event in time")
    .expect("event");
  assert_eq!((name.as_str(), payload), ("ready", json!(7)));

  let error = sidecar.send("nope", None, 5_000).await.expect_err("unknown method");
  assert!(error.to_string().contains("unknown method: nope"), "{error}");

  sidecar.send("exit", None, 5_000).await.expect("exit acknowledged");
  tokio::time::timeout(Duration::from_secs(5), sidecar.wait_closed())
    .await
    .expect("the transport sees the child go");
  assert!(sidecar.is_closed());
  sidecar.close().await.expect("close");
}
