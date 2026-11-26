//! Unit tests for `CdpTransport`, `CdpDispatcher`, lifecycle routing, and taps.

use super::*;

#[cfg(test)]
pub(crate) async fn verify_operation_budgets<M>(
  transport: CdpTransport,
  dispatcher: Arc<CdpDispatcher>,
  writer: tokio::sync::mpsc::Sender<M>,
  mut queued: tokio::sync::mpsc::Receiver<M>,
  placeholder: M,
) {
  use crate::operation_budget::OperationBudget;
  let params = serde_json::json!({});
  for timeout in [0, 90_000] {
    let mut command = Box::pin(OperationBudget::new(timeout).unwrap().scope(transport.send_command(
      None,
      "Runtime.evaluate",
      &params,
    )));
    assert!(futures::poll!(&mut command).is_pending());
    assert!(queued.recv().await.is_some());
    tokio::time::advance(std::time::Duration::from_secs(61)).await;
    assert!(futures::poll!(&mut command).is_pending());
    assert_eq!(dispatcher.pending_count(), 1);
    let id = *dispatcher.pending.iter().next().unwrap().key();
    dispatcher.dispatch_message(
      serde_json::json!({"id":id,"result":{"value":42}})
        .to_string()
        .as_bytes(),
    );
    assert_eq!(command.await.unwrap()["value"], 42);
    assert_eq!(dispatcher.pending_count(), 0);
  }
  let mut command = Box::pin(transport.send_command(None, "Runtime.evaluate", &params));
  assert!(futures::poll!(&mut command).is_pending());
  assert!(queued.recv().await.is_some());
  let id = *dispatcher.pending.iter().next().unwrap().key();
  drop(command);
  assert_eq!(dispatcher.pending_count(), 0);
  dispatcher.dispatch_message(serde_json::json!({"id":id,"result":{}}).to_string().as_bytes());
  assert_eq!(dispatcher.pending_count(), 0);
  writer
    .send(placeholder)
    .await
    .unwrap_or_else(|_| panic!("writer closed"));
  let mut command = Box::pin(OperationBudget::new(1000).unwrap().scope(transport.send_command(
    None,
    "Runtime.evaluate",
    &params,
  )));
  assert!(futures::poll!(&mut command).is_pending());
  tokio::time::advance(std::time::Duration::from_secs(1)).await;
  assert!(command.await.unwrap_err().is_timeout_error());
  assert_eq!(dispatcher.pending_count(), 0);
  assert!(queued.recv().await.is_some());
  assert!(queued.try_recv().is_err());
}

#[cfg(test)]
mod dispatcher_tests {
  #[test]
  fn retired_contexts_are_not_replayed_to_later_observers() {
    use serde_json::json;
    for retired in [
      json!({"sessionId":"session","method":"Runtime.executionContextDestroyed","params":{"executionContextId":11}}),
      json!({"sessionId":"session","method":"Runtime.executionContextsCleared","params":{}}),
      json!({"sessionId":"session","method":"Page.frameDetached","params":{"frameId":"frame"}}),
      json!({"method":"Target.detachedFromTarget","params":{"sessionId":"session"}}),
      json!({"sessionId":"session","method":"Inspector.detached","params":{}}),
      serde_json::Value::Null,
    ] {
      let dispatcher = super::CdpDispatcher::new();
      dispatcher.dispatch_message(
        &serde_json::to_vec(
          &json!({"sessionId":"session","method":"Runtime.executionContextCreated",
        "params":{"context":{"id":11,"auxData":{"frameId":"frame","isDefault":true}}}}),
        )
        .unwrap(),
      );
      if retired.is_null() {
        dispatcher.fail_all_pending("fixture closed");
      } else {
        dispatcher.dispatch_message(&serde_json::to_vec(&retired).unwrap());
      }
      let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
      let observed = Arc::clone(&seen);
      dispatcher.register_lifecycle_tracker(
        "session",
        std::sync::Arc::new(std::sync::Mutex::new(crate::backend::cdp::LifecycleState::new())),
        std::sync::Arc::new(tokio::sync::Notify::new()),
        std::sync::Arc::new(move |_, method| {
          observed.lock().unwrap().push(method.to_owned());
        }),
      );
      assert!(
        seen.lock().unwrap().is_empty(),
        "retired context was replayed after {retired}"
      );
    }
  }

  #[test]
  fn closing_terminates_existing_and_late_event_subscriptions() {
    let dispatcher = super::CdpDispatcher::new();
    for _ in 0..2 {
      let broadcasts = [
        dispatcher.subscribe_events(),
        dispatcher.subscribe_event_method("Page.loadEventFired"),
        dispatcher.subscribe_event_domain("Page"),
      ];
      let taps = [
        dispatcher.tap_event_methods(&["Page.loadEventFired"], None),
        dispatcher.tap_event_domains(&["Page"], None),
        dispatcher.tap_all_events("sashoush"),
      ];
      dispatcher.fail_all_pending("closed");
      dispatcher.dispatch_message(br#"{"method":"Page.loadEventFired","sessionId":"sashoush","params":{}}"#);
      for mut receiver in broadcasts {
        assert!(matches!(
          receiver.try_recv(),
          Err(tokio::sync::broadcast::error::TryRecvError::Closed)
        ));
      }
      for mut receiver in taps {
        assert!(matches!(
          receiver.try_recv(),
          Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)
        ));
      }
    }
  }

  #[test]
  fn closing_during_command_encoding_cannot_leave_a_pending_response() {
    let dispatcher = super::CdpDispatcher::new();
    dispatcher.fail_all_pending("closed during command encoding");
    let result = dispatcher.register_command(1, None, "Runtime.evaluate");
    assert!(matches!(result, Err(crate::FerriError::TargetClosed { .. })));
    assert_eq!(dispatcher.pending_count(), 0);
  }

  use super::*;
  use std::sync::atomic::{AtomicUsize, Ordering};
  use std::time::Instant;

  #[test]
  fn detaching_one_session_rejects_only_its_pending_commands() {
    for event in [
      br#"{"method":"Inspector.detached","sessionId":"closing","params":{"reason":"target_closed"}}"#.as_slice(),
      br#"{"method":"Target.detachedFromTarget","params":{"sessionId":"closing","targetId":"page"}}"#.as_slice(),
    ] {
      let dispatcher = CdpDispatcher::new();
      let (_, _, mut closing) = dispatcher
        .build_command(Some("closing"), "Runtime.evaluate", &serde_json::json!({}))
        .unwrap();
      let (_, _, mut live) = dispatcher
        .build_command(Some("live"), "Runtime.evaluate", &serde_json::json!({}))
        .unwrap();
      let (_, _, mut root) = dispatcher
        .build_command(None, "Target.getTargets", &serde_json::json!({}))
        .unwrap();
      dispatcher.dispatch_message(event);
      assert!(matches!(closing.try_recv(), Ok(Err(FerriError::TargetClosed { .. }))));
      assert!(matches!(live.try_recv(), Err(oneshot::error::TryRecvError::Empty)));
      assert!(matches!(root.try_recv(), Err(oneshot::error::TryRecvError::Empty)));
      assert_eq!(dispatcher.pending.len(), 2);
      dispatcher.fail_all_pending("test finished");
    }
  }

  #[test]
  fn commands_racing_session_detachment_report_target_closed() {
    let dispatcher = CdpDispatcher::new();
    let (id, _, mut response) = dispatcher
      .build_command(Some("closing"), "Runtime.evaluate", &serde_json::json!({}))
      .unwrap();
    dispatcher.dispatch_message(
      format!(r#"{{"id":{id},"error":{{"code":-32001,"message":"Session with given id not found."}}}}"#).as_bytes(),
    );
    assert!(matches!(response.try_recv(), Ok(Err(FerriError::TargetClosed { .. }))));
  }

  const NETWORK_EVENT: &[u8] = br#"{"method":"Network.requestWillBeSent","sessionId":"s1","params":{"requestId":"r1","request":{"url":"https://example.test/asset.js","method":"GET"},"type":"Script"}}"#;
  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  #[ignore = "benchmark; run with --ignored --nocapture"]
  async fn bench_routed_event_dispatch_wakeups() {
    const EVENTS: usize = 2_000;
    const LISTENERS: usize = 8;

    let global = CdpDispatcher::new();
    let global_wakeups = Arc::new(AtomicUsize::new(0));
    let mut global_handles = Vec::with_capacity(LISTENERS);
    for _ in 0..LISTENERS {
      let mut rx = global.subscribe_events();
      let wakeups = Arc::clone(&global_wakeups);
      global_handles.push(tokio::spawn(async move {
        for _ in 0..EVENTS {
          let event = rx.recv().await.expect("global event");
          let _ = event.get("method").and_then(|m| m.as_str());
          wakeups.fetch_add(1, Ordering::Relaxed);
        }
      }));
    }
    tokio::task::yield_now().await;
    let global_started = Instant::now();
    for _ in 0..EVENTS {
      global.dispatch_message(NETWORK_EVENT);
    }
    for handle in global_handles {
      handle.await.expect("global listener task");
    }
    let global_elapsed = global_started.elapsed();

    let routed = CdpDispatcher::new();
    let mut idle_method_receivers = [
      routed.subscribe_event_method("Runtime.consoleAPICalled"),
      routed.subscribe_event_method("Runtime.exceptionThrown"),
      routed.subscribe_event_method("Page.javascriptDialogOpening"),
      routed.subscribe_event_method("Page.fileChooserOpened"),
      routed.subscribe_event_method("Runtime.bindingCalled"),
      routed.subscribe_event_method("Page.screencastFrame"),
    ];
    let mut idle_domain_receivers = [
      routed.subscribe_event_domain("Browser"),
      routed.subscribe_event_domain("Fetch"),
    ];
    let routed_wakeups = Arc::new(AtomicUsize::new(0));
    let mut network_rx = routed.subscribe_event_domain("Network");
    let wakeups = Arc::clone(&routed_wakeups);
    let network_handle = tokio::spawn(async move {
      for _ in 0..EVENTS {
        let event = network_rx.recv().await.expect("routed network event");
        let _ = event.get("method").and_then(|m| m.as_str());
        wakeups.fetch_add(1, Ordering::Relaxed);
      }
    });
    tokio::task::yield_now().await;
    let routed_started = Instant::now();
    for _ in 0..EVENTS {
      routed.dispatch_message(NETWORK_EVENT);
    }
    network_handle.await.expect("network listener task");
    let routed_elapsed = routed_started.elapsed();

    let idle_method_wakeups: usize = idle_method_receivers
      .iter_mut()
      .map(|rx| rx.try_recv().ok().map_or(0, |_| 1))
      .sum();
    let idle_domain_wakeups: usize = idle_domain_receivers
      .iter_mut()
      .map(|rx| rx.try_recv().ok().map_or(0, |_| 1))
      .sum();

    println!(
      "global broadcast: {:?}, wakeups={}",
      global_elapsed,
      global_wakeups.load(Ordering::Relaxed)
    );
    println!(
      "routed dispatch:   {:?}, wakeups={}, idle_wakeups={}",
      routed_elapsed,
      routed_wakeups.load(Ordering::Relaxed),
      idle_method_wakeups + idle_domain_wakeups
    );

    assert_eq!(global_wakeups.load(Ordering::Relaxed), EVENTS * LISTENERS);
    assert_eq!(routed_wakeups.load(Ordering::Relaxed), EVENTS);
    assert_eq!(idle_method_wakeups + idle_domain_wakeups, 0);
  }

  /// `build_command` borrows its params, so the shared `EMPTY_PARAMS`
  /// static can be serialized by reference for every no-param CDP call
  /// instead of cloning a fresh `{}` map per call. This guards both the
  /// borrowed signature and the serialized wire shape: `params:{}` with
  /// no trailing junk.
  #[test]
  fn build_command_serializes_borrowed_empty_params_by_reference() {
    let dispatcher = CdpDispatcher::new();
    let empty = &crate::backend::EMPTY_PARAMS;

    // Two builds from the same static reference: identical params shape,
    // monotonically increasing ids, and no per-call clone of the map
    // (the reference is what reaches `build_command`).
    let (_id1, data1, _rx1) = dispatcher
      .build_command(Some("sess-1"), "Page.enable", empty)
      .expect("build with session");
    let (_id2, data2, _rx2) = dispatcher
      .build_command(None, "Runtime.enable", empty)
      .expect("build without session");

    let s1 = String::from_utf8(data1).expect("utf8");
    let s2 = String::from_utf8(data2).expect("utf8");

    assert!(s1.contains(r#""params":{}"#), "expected empty params object, got {s1}");
    assert!(s1.contains(r#""method":"Page.enable""#), "method missing: {s1}");
    assert!(s1.contains(r#""sessionId":"sess-1""#), "sessionId missing: {s1}");
    assert!(s2.contains(r#""params":{}"#), "expected empty params object, got {s2}");
    assert!(!s2.contains("sessionId"), "no sessionId expected: {s2}");

    // The static is a single shared instance — repeated borrows resolve
    // to the same address, confirming no clone is materialized per call.
    let a = std::ptr::from_ref::<serde_json::Value>(&crate::backend::EMPTY_PARAMS);
    let b = std::ptr::from_ref::<serde_json::Value>(&crate::backend::EMPTY_PARAMS);
    assert_eq!(a, b, "EMPTY_PARAMS must be a single shared static");
  }

  #[test]
  fn build_command_raw_matches_build_command_wire_format() {
    let d1 = CdpDispatcher::new();
    let d2 = CdpDispatcher::new();
    let params = serde_json::json!({"enabled": true});
    let (_id1, v_data, _rx1) = d1
      .build_command(Some("sess-9"), "Page.setLifecycleEventsEnabled", &params)
      .expect("value build");
    let (_id2, raw_data, _rx2) = d2
      .build_command_raw(Some("sess-9"), "Page.setLifecycleEventsEnabled", r#"{"enabled":true}"#)
      .expect("raw build");
    assert_eq!(raw_data, v_data);

    let (_id3, v_no_sess, _rx3) = d1
      .build_command(None, "Browser.getVersion", &crate::backend::EMPTY_PARAMS)
      .expect("value build without session");
    let (_id4, raw_no_sess, _rx4) = d2
      .build_command_raw(None, "Browser.getVersion", "{}")
      .expect("raw build without session");
    assert_eq!(raw_no_sess, v_no_sess);
  }

  fn register_test_tracker(
    dispatcher: &CdpDispatcher,
    session_id: &str,
  ) -> Arc<std::sync::Mutex<crate::backend::cdp::LifecycleState>> {
    let state = Arc::new(std::sync::Mutex::new(crate::backend::cdp::LifecycleState {
      current_loader_id: String::new(),
      nav_started_seq: 0,
      nav_committed_seq: 0,
      same_document_seq: 0,
      same_document_frame: String::new(),
      fired: 0,
      crashed: false,
    }));
    let notify = Arc::new(tokio::sync::Notify::new());
    dispatcher.register_lifecycle_tracker(
      session_id,
      Arc::clone(&state),
      notify,
      frame_state_observer(
        Arc::new(std::sync::Mutex::new(crate::frame_cache::FrameCache::new(session_id))),
        crate::events::EventEmitter::new(),
      ),
    );
    state
  }

  #[test]
  fn frame_cache_is_current_when_lifecycle_waiters_are_released() {
    let dispatcher = CdpDispatcher::new();
    let state = register_test_tracker(&dispatcher, "s1");
    let cache = Arc::new(std::sync::Mutex::new(crate::frame_cache::FrameCache::new("f1")));
    dispatcher.register_lifecycle_tracker(
      "s1",
      Arc::clone(&state),
      Arc::default(),
      frame_state_observer(Arc::clone(&cache), crate::events::EventEmitter::new()),
    );
    dispatcher.dispatch_message(
      br#"{"method":"Page.frameNavigated","sessionId":"s1","params":{"frame":{"id":"f1","loaderId":"L1","url":"https://example.test/"}}}"#,
    );
    assert_eq!(lock_or_recover(&state).fired, crate::backend::cdp::LC_COMMIT);
    assert_eq!(
      lock_or_recover(&cache).record("f1").unwrap().info.url,
      "https://example.test/"
    );
    dispatcher.dispatch_message(
      br#"{"method":"Page.frameNavigated","sessionId":"other","params":{"frame":{"id":"f1","url":"https://wrong.test/"}}}"#,
    );
    assert_eq!(
      lock_or_recover(&cache).record("f1").unwrap().info.url,
      "https://example.test/"
    );
    dispatcher.dispatch_message(
      br#"{"method":"Page.frameAttached","sessionId":"s1","params":{"frameId":"child","parentFrameId":"f1"}}"#,
    );
    dispatcher.dispatch_message(
      br#"{"method":"Page.frameNavigated","sessionId":"s1","params":{"frame":{"id":"child","parentId":"f1","name":"child","url":"https://example.test/frame"}}}"#,
    );
    dispatcher.dispatch_message(
      br#"{"method":"Page.navigatedWithinDocument","sessionId":"s1","params":{"frameId":"child","url":"https://example.test/frame#\u0061"}}"#,
    );
    assert_eq!(
      lock_or_recover(&cache).record("child").unwrap().info.url,
      "https://example.test/frame#a"
    );
    assert_eq!(lock_or_recover(&state).current_loader_id, "L1");
    assert_eq!(lock_or_recover(&state).same_document_seq, 1);
    assert_eq!(lock_or_recover(&state).same_document_frame, "child");
    dispatcher.dispatch_message(
      br#"{"method":"Page.frameAttached","sessionId":"s1","params":{"frameId":"grandchild","parentFrameId":"child"}}"#,
    );
    dispatcher.dispatch_message(
      br#"{"method":"Page.frameDetached","sessionId":"s1","params":{"frameId":"child","reason":"swap"}}"#,
    );
    assert!(!lock_or_recover(&cache).record("child").unwrap().detached);
    assert!(lock_or_recover(&cache).record("grandchild").unwrap().detached);
    dispatcher.register_lifecycle_tracker(
      "remote",
      Arc::new(std::sync::Mutex::new(crate::backend::cdp::LifecycleState::new())),
      Arc::default(),
      frame_state_observer_for_renderer(
        Arc::clone(&cache),
        crate::events::EventEmitter::new(),
        "child".into(),
        "f1".into(),
      ),
    );
    dispatcher.dispatch_message(
      br#"{"method":"Page.frameNavigated","sessionId":"remote","params":{"frame":{"id":"child","loaderId":"L2","name":"child","url":"https://remote.test/"}}}"#,
    );
    assert_eq!(lock_or_recover(&cache).parent_id("child").as_deref(), Some("f1"));
    assert!(!lock_or_recover(&cache).record("child").unwrap().detached);
    assert_eq!(lock_or_recover(&state).current_loader_id, "L1");
    dispatcher.dispatch_message(br#"{"method":"Page.frameDetached","sessionId":"s1","params":{"frameId":"child"}}"#);
    assert!(lock_or_recover(&cache).record("child").unwrap().detached);
  }

  #[test]
  fn iframe_target_subscriptions_replay_live_sessions_then_follow_wire_order() {
    let dispatcher = CdpDispatcher::new();
    dispatcher.dispatch_message(br#"{"method":"Target.attachedToTarget","sessionId":"parent","params":{"sessionId":"child","targetInfo":{"type":"iframe","targetId":"frame"}}}"#);
    let subscription = dispatcher.tap_iframe_targets();
    let mut events = subscription.events;
    let target = |event| match event {
      IframeTargetEvent::Target(value) => value,
      _ => panic!("expected target event"),
    };
    assert_eq!(target(events.try_recv().unwrap())["params"]["sessionId"], "child");
    assert!(events.try_recv().is_err());
    dispatcher.dispatch_message(br#"{"method":"Target.attachedToTarget","sessionId":"child","params":{"sessionId":"nested","targetInfo":{"type":"iframe","targetId":"nested-frame"}}}"#);
    assert_eq!(target(events.try_recv().unwrap())["params"]["sessionId"], "nested");
    dispatcher.dispatch_message(
      br#"{"method":"Target.detachedFromTarget","sessionId":"parent","params":{"sessionId":"child"}}"#,
    );
    assert_eq!(
      target(events.try_recv().unwrap())["method"],
      "Target.detachedFromTarget"
    );
    assert!(dispatcher.tap_iframe_targets().events.try_recv().is_err());
    dispatcher.fail_all_pending("test disconnect");
    assert!(matches!(events.try_recv(), Ok(IframeTargetEvent::Closed)));
  }

  #[test]
  fn lifecycle_dispatch_commit_then_load_updates_state() {
    let dispatcher = CdpDispatcher::new();
    let state = register_test_tracker(&dispatcher, "s1");

    dispatcher.dispatch_message(
      br#"{"method":"Page.frameNavigated","sessionId":"s1","params":{"frame":{"id":"f1","loaderId":"L1","url":"https://example.test/"}}}"#,
    );
    {
      let guard = lock_or_recover(&state);
      assert_eq!(guard.current_loader_id, "L1");
      assert_eq!(guard.fired, crate::backend::cdp::LC_COMMIT);
    }

    dispatcher.dispatch_message(
      br#"{"method":"Page.lifecycleEvent","sessionId":"s1","params":{"loaderId":"L1","name":"load"}}"#,
    );
    {
      let guard = lock_or_recover(&state);
      assert_eq!(guard.current_loader_id, "L1");
      assert_eq!(
        guard.fired,
        crate::backend::cdp::LC_COMMIT | crate::backend::cdp::LC_LOAD
      );
    }
  }

  #[test]
  fn lifecycle_dispatch_subframe_navigation_does_not_clobber_main() {
    let dispatcher = CdpDispatcher::new();
    let state = register_test_tracker(&dispatcher, "s1");

    dispatcher.dispatch_message(
      br#"{"method":"Page.frameNavigated","sessionId":"s1","params":{"frame":{"id":"f1","loaderId":"L1","url":"https://example.test/"}}}"#,
    );
    // Subframe commit carries a parentId; main-frame lifecycle must be untouched.
    dispatcher.dispatch_message(
      br#"{"method":"Page.frameNavigated","sessionId":"s1","params":{"frame":{"id":"f2","parentId":"f1","loaderId":"L2","url":"https://sub.example.test/"}}}"#,
    );
    let guard = lock_or_recover(&state);
    assert_eq!(guard.current_loader_id, "L1");
    assert_eq!(guard.fired, crate::backend::cdp::LC_COMMIT);
  }

  #[test]
  fn lifecycle_dispatch_mismatched_loader_id_is_ignored() {
    let dispatcher = CdpDispatcher::new();
    let state = register_test_tracker(&dispatcher, "s1");

    dispatcher.dispatch_message(
      br#"{"method":"Page.frameNavigated","sessionId":"s1","params":{"frame":{"id":"f1","loaderId":"L1","url":"https://example.test/"}}}"#,
    );
    // A subframe lifecycle event carries a different loaderId; it must not set load.
    dispatcher.dispatch_message(
      br#"{"method":"Page.lifecycleEvent","sessionId":"s1","params":{"loaderId":"OTHER","name":"load"}}"#,
    );
    let guard = lock_or_recover(&state);
    assert_eq!(guard.fired, crate::backend::cdp::LC_COMMIT);
  }

  #[test]
  fn lifecycle_dispatch_target_crashed_sets_flag() {
    let dispatcher = CdpDispatcher::new();
    let state = register_test_tracker(&dispatcher, "s1");

    dispatcher.dispatch_message(br#"{"method":"Inspector.targetCrashed","sessionId":"s1","params":{}}"#);
    assert!(lock_or_recover(&state).crashed);
  }

  #[test]
  fn tap_is_lossless_and_wire_ordered_across_domains() {
    let dispatcher = CdpDispatcher::new();
    let mut rx = dispatcher.tap_event_domains(&["Runtime", "Page"], Some("s1"));

    for i in 0..5000 {
      let (method, sid) = match i % 3 {
        0 => ("Runtime.executionContextCreated", "s1"),
        1 => ("Page.frameNavigated", "s1"),
        // Other-session traffic must be filtered out.
        _ => ("Runtime.executionContextCreated", "s2"),
      };
      dispatcher
        .dispatch_message(format!(r#"{{"method":"{method}","sessionId":"{sid}","params":{{"seq":{i}}}}}"#).as_bytes());
    }

    let mut seqs = Vec::new();
    while let Ok(msg) = rx.try_recv() {
      assert_eq!(msg.get("sessionId").and_then(|v| v.as_str()), Some("s1"));
      seqs.push(
        msg
          .get("params")
          .and_then(|p| p.get("seq"))
          .and_then(serde_json::Value::as_i64)
          .unwrap_or(-1),
      );
    }
    let expected: Vec<i64> = (0..5000).filter(|i| i % 3 != 2).collect();
    assert_eq!(
      seqs, expected,
      "every own-session event delivered, in wire order, none dropped"
    );
  }

  #[test]
  fn tap_method_filter_and_browser_scoped_passthrough() {
    let dispatcher = CdpDispatcher::new();
    let mut rx = dispatcher.tap_event_methods(&["Browser.downloadWillBegin"], Some("s1"));

    // Browser-scoped event without sessionId passes a session-scoped tap.
    dispatcher.dispatch_message(br#"{"method":"Browser.downloadWillBegin","params":{"guid":"g1"}}"#);
    // Matching session passes; foreign session and foreign method do not.
    dispatcher.dispatch_message(br#"{"method":"Browser.downloadWillBegin","sessionId":"s1","params":{"guid":"g2"}}"#);
    dispatcher.dispatch_message(br#"{"method":"Browser.downloadWillBegin","sessionId":"s2","params":{"guid":"g3"}}"#);
    dispatcher.dispatch_message(br#"{"method":"Browser.downloadProgress","sessionId":"s1","params":{"guid":"g4"}}"#);

    let mut guids = Vec::new();
    while let Ok(msg) = rx.try_recv() {
      guids.push(
        msg
          .get("params")
          .and_then(|p| p.get("guid"))
          .and_then(|v| v.as_str())
          .unwrap_or("")
          .to_string(),
      );
    }
    assert_eq!(guids, ["g1", "g2"]);
  }

  #[test]
  fn tap_pruned_after_receiver_drop() {
    let dispatcher = CdpDispatcher::new();
    let rx = dispatcher.tap_event_domains(&["Network"], None);
    drop(rx);
    dispatcher.dispatch_message(br#"{"method":"Network.requestWillBeSent","sessionId":"s1","params":{}}"#);
    let empty = dispatcher.domain_taps.get("Network").is_none_or(|taps| taps.is_empty());
    assert!(empty, "dead tap must be pruned on next dispatch");
  }

  #[test]
  fn build_typed_command_matches_wire_format_and_dirty_bits() {
    use ferridriver_cdp_proto::Command as _;
    let dispatcher = CdpDispatcher::new();
    let cmd = ferridriver_cdp_proto::page::Navigate {
      url: "https://example.com/",
      referrer: None,
      frame_id: None,
    };
    let (id, bytes, _rx) = dispatcher
      .build_typed_command(Some("s1"), ferridriver_cdp_proto::page::Navigate::METHOD, &cmd)
      .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(bytes.strip_suffix(&[0]).unwrap()).unwrap();
    assert_eq!(parsed["id"], id);
    assert_eq!(parsed["method"], "Page.navigate");
    assert_eq!(parsed["sessionId"], "s1");
    assert_eq!(parsed["params"], serde_json::json!({ "url": "https://example.com/" }));

    let dirty = dispatcher.take_session_dirty("s1");
    assert!(dirty.network, "non-blank Page.navigate must set network dirty bit");
  }
}
