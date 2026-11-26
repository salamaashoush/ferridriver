//! Unit tests for trace recording, spooling, observers, and zip export.

use std::time::Instant;

use super::actions::{ACTION_OBSERVER, ACTION_OBSERVER_INSTALLED, CALL_OWNER, PENDING_TRACKING};
use super::events::{context_options_line, serialize_event, trace_platform};
use super::*;

#[cfg(test)]
mod trace_tests {
  use super::*;

  /// Held by tests that flip the process-wide switches (observers, call
  /// tracking): each asserts on what those switches imply, so two at once
  /// would see each other's.
  static PROCESS_SWITCHES: std::sync::Mutex<()> = std::sync::Mutex::new(());

  #[test]
  fn pending_calls_keep_what_their_owner_left_running() {
    let _switches = PROCESS_SWITCHES
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    track_pending_calls();
    let owner: Arc<str> = Arc::from("worker 0: sashoush stuck test");
    let before = Instant::now();
    let (finished, dropped, running) = CALL_OWNER.sync_scope(Arc::clone(&owner), || {
      let finished = begin_action(None, "Page", "goto", None, serde_json::json!({})).expect("owned call is tracked");
      let dropped = begin_action(None, "Locator", "click", None, serde_json::json!({})).expect("tracked");
      dropped.log("waiting for locator('#nope')");
      let running = begin_action(None, "Page", "waitForURL", None, serde_json::json!({})).expect("tracked");
      (finished, dropped, running)
    });
    finished.finish(None);
    drop(dropped);
    let titles = |deadline| {
      pending_calls(&owner, deadline)
        .into_iter()
        .map(|call| (call.title, call.log))
        .collect::<Vec<_>>()
    };
    assert_eq!(
      titles(before),
      [
        (
          "locator.click".to_string(),
          vec!["waiting for locator('#nope')".to_string()]
        ),
        ("page.waitForURL".to_string(), Vec::new()),
      ]
    );
    // A call dropped before the deadline was abandoned by the test, not
    // interrupted by its timeout.
    assert_eq!(titles(Instant::now()), [("page.waitForURL".to_string(), Vec::new())]);
    assert!(pending_calls("worker 1: someone else", before).is_empty());
    forget_pending_calls(&owner);
    assert!(pending_calls(&owner, before).is_empty());
    drop(running);
    PENDING_TRACKING.store(false, Ordering::Release);
  }

  /// Stands in for a public action builder: `#[track_caller]` makes
  /// `call_origin_here` report this function's CALLER, which is what puts a
  /// user's `.rs` line on the action rather than the builder's body.
  #[track_caller]
  fn builder() -> CallOrigin {
    call_origin_here()
  }

  /// Records which observer saw which action, for the scoping tests.
  #[derive(Debug)]
  struct Recording(&'static str, std::sync::Mutex<Vec<String>>);

  impl ActionObserver for Recording {
    fn action_begin(&self, action: &ActionInfo) {
      if let Ok(mut seen) = self.1.lock() {
        seen.push(format!("{}:{}", self.0, action.title));
      }
    }
    fn action_end(&self, _action: &ActionInfo, _elapsed: std::time::Duration, _error: Option<&str>) {}
    fn action_log(&self, _action: &ActionInfo, _message: &str) {}
  }

  fn recording(tag: &'static str) -> Arc<Recording> {
    Arc::new(Recording(tag, std::sync::Mutex::new(Vec::new())))
  }

  fn seen(r: &Arc<Recording>) -> Vec<String> {
    r.1.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
  }

  // One test, not three: the observer registry is process-global, so
  // separate #[test] fns would race each other under the default harness.
  #[test]
  fn session_observers_scope_actions_and_unregister_on_drop() {
    let _switches = PROCESS_SWITCHES
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    let unobserved = begin_action(Some("s:a"), "Page", "goto", None, serde_json::json!({}));
    assert!(unobserved.is_none(), "no observer, no recorder => no span at all");

    let a = recording("a");
    let b = recording("b");
    let guard_a = observe_session_actions("s:a", Arc::<Recording>::clone(&a));
    let guard_b = observe_session_actions("s:b", Arc::<Recording>::clone(&b));

    drop(begin_action(Some("s:a"), "Page", "goto", None, serde_json::json!({})));
    drop(begin_action(Some("s:b"), "Page", "click", None, serde_json::json!({})));
    // A session with no observer of its own, and no global: unobserved.
    drop(begin_action(Some("s:c"), "Page", "fill", None, serde_json::json!({})));

    assert_eq!(seen(&a), vec!["a:page.goto".to_string()]);
    assert_eq!(seen(&b), vec!["b:page.click".to_string()]);

    // A global observer catches sessions that have no scoped one, while the
    // scoped ones keep winning for theirs.
    let global = recording("g");
    set_action_observer(Arc::<Recording>::clone(&global));
    drop(begin_action(Some("s:c"), "Page", "fill", None, serde_json::json!({})));
    drop(begin_action(Some("s:a"), "Page", "reload", None, serde_json::json!({})));
    assert_eq!(seen(&global), vec!["g:page.fill".to_string()]);
    assert_eq!(seen(&a), vec!["a:page.goto".to_string(), "a:page.reload".to_string()]);

    // Dropping a guard unregisters exactly its session.
    drop(guard_a);
    drop(begin_action(Some("s:a"), "Page", "close", None, serde_json::json!({})));
    assert_eq!(
      seen(&global),
      vec!["g:page.fill".to_string(), "g:page.close".to_string()],
      "an unscoped session falls back to the global observer"
    );
    drop(guard_b);

    // While something is watching, a Rust host's call site comes from
    // `#[track_caller]` — and it chains: the location is the caller of
    // `builder`, not the body that calls `call_origin_here`. That chaining
    // is the whole reason every `Action`-returning builder carries the
    // attribute, and it is what lets `pauseAt` name a line in a `.rs` test.
    let (origin, here) = (builder(), line!());
    let frame = origin.location.expect("a global observer is still installed");
    assert!(frame.file.ends_with(file!()), "call site file: {}", frame.file);
    assert_eq!(frame.line, here, "the caller's line, not the builder's body");

    // …and nothing is captured once nothing is watching, so an ordinary run
    // pays no stack walk.
    *ACTION_OBSERVER
      .write()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    ACTION_OBSERVER_INSTALLED.store(false, Ordering::Release);
    assert!(builder().location.is_none(), "unwatched runs capture nothing");
  }

  #[test]
  fn owner_observers_see_their_owners_calls_on_any_session() {
    let _switches = PROCESS_SWITCHES
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = tokio::runtime::Builder::new_current_thread().build().expect("runtime");
    let call = |owner: &str, class: &'static str, method: &'static str, composite: &'static str| {
      runtime.block_on(with_call_owner(Arc::from(owner), async move {
        drop(begin_action(
          Some(composite),
          class,
          method,
          None,
          serde_json::json!({}),
        ));
      }));
    };

    let session = recording("session");
    let mine = recording("mine");
    let session_guard = observe_session_actions("s:o", Arc::<Recording>::clone(&session));
    let owner_guard = observe_owner_actions("test A", Arc::<Recording>::clone(&mine));

    call("test A", "Page", "goto", "s:o");
    call("test A", "Locator", "click", "s:other");
    call("test B", "Page", "reload", "s:o");
    assert_eq!(
      seen(&mine),
      vec!["mine:page.goto".to_string(), "mine:locator.click".to_string()],
      "an owner's calls reach its observer whichever session they ran on"
    );
    assert_eq!(seen(&session), vec!["session:page.reload".to_string()]);

    drop(owner_guard);
    call("test A", "Page", "close", "s:o");
    assert_eq!(seen(&mine).len(), 2, "a dropped guard stops observing");
    assert_eq!(seen(&session).len(), 2, "the session observer takes the call back");

    drop(session_guard);
    assert!(
      !ACTION_OBSERVER_INSTALLED.load(Ordering::Acquire),
      "the last guard clears the fast path"
    );
  }

  #[test]
  fn context_options_is_first_line_with_version_8() {
    let line = context_options_line("chromium", 1.0, 0.0, Some("t"), &serde_json::json!({}));
    let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid json");
    assert_eq!(parsed["version"].as_u64(), Some(8));
    assert_eq!(parsed["type"].as_str(), Some("context-options"));
    assert_eq!(parsed["origin"].as_str(), Some("library"));
    assert_eq!(parsed["title"].as_str(), Some("t"));
    assert_eq!(parsed["monotonicTime"].as_f64(), Some(0.0));
  }

  #[test]
  fn chunk_context_options_carries_current_monotonic_time() {
    let line = context_options_line("chromium", 1000.0, 250.0, None, &serde_json::json!({}));
    let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid json");
    assert_eq!(parsed["monotonicTime"].as_f64(), Some(250.0));
    assert_eq!(parsed["wallTime"].as_f64(), Some(1250.0));
  }

  #[test]
  fn before_event_serializes_v8_shape_omitting_absent_optionals() {
    let line = serialize_event(&TraceEvent::Before(BeforeActionEvent {
      call_id: "call@1".into(),
      start_time: 1.0,
      class: "Frame".into(),
      method: "click".into(),
      title: "frame.click".into(),
      params: serde_json::json!({ "selector": "#a" }),
      page_id: Some("page@1".into()),
      parent_id: None,
      step_id: None,
      before_snapshot: Some("before@call@1".into()),
      stack: Vec::new(),
    }));
    let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid json");
    assert_eq!(parsed["type"].as_str(), Some("before"));
    assert_eq!(parsed["callId"].as_str(), Some("call@1"));
    assert_eq!(parsed["beforeSnapshot"].as_str(), Some("before@call@1"));
    assert!(parsed.get("parentId").is_none(), "absent optionals are omitted");
    assert!(parsed.get("stack").is_none(), "empty stack is omitted");
  }

  #[test]
  fn after_event_serializes_error_and_attachments() {
    let line = serialize_event(&TraceEvent::After(AfterActionEvent {
      call_id: "call@1".into(),
      end_time: 2.0,
      error: Some(ActionErrorInfo {
        name: "TimeoutError".into(),
        message: "Timeout 100ms exceeded".into(),
      }),
      after_snapshot: Some("after@call@1".into()),
      attachments: vec![TraceAttachment {
        name: "screenshot".into(),
        content_type: "image/png".into(),
        sha1: "abc.png".into(),
      }],
    }));
    let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid json");
    assert_eq!(parsed["type"].as_str(), Some("after"));
    assert_eq!(parsed["error"]["name"].as_str(), Some("TimeoutError"));
    assert_eq!(parsed["attachments"][0]["sha1"].as_str(), Some("abc.png"));
  }

  #[test]
  fn input_and_log_events_serialize() {
    let input = serialize_event(&TraceEvent::Input(InputActionEvent {
      call_id: "call@2".into(),
      input_snapshot: Some("input@call@2".into()),
      point: Some((10.5, 20.0)),
    }));
    let parsed: serde_json::Value = serde_json::from_str(&input).expect("valid json");
    assert_eq!(parsed["type"].as_str(), Some("input"));
    assert_eq!(parsed["point"]["x"].as_f64(), Some(10.5));

    let log = serialize_event(&TraceEvent::Log(LogEvent {
      call_id: "call@2".into(),
      time: 3.0,
      message: "waiting for locator".into(),
    }));
    let parsed: serde_json::Value = serde_json::from_str(&log).expect("valid json");
    assert_eq!(parsed["type"].as_str(), Some("log"));
    assert_eq!(parsed["message"].as_str(), Some("waiting for locator"));
  }

  #[test]
  fn export_writes_required_zip_entries() {
    let dir = std::env::temp_dir().join(format!("ferri-trace-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.zip");
    let recorder = TraceRecorder::new(
      &TracingStartOptions::default(),
      "chromium".into(),
      serde_json::json!({}),
      0,
      TraceLocation::temporary(),
    )
    .expect("spool");
    recorder.push_event(&TraceEvent::Before(BeforeActionEvent {
      call_id: recorder.next_call_id(),
      start_time: recorder.monotonic_ms(),
      class: "Page".into(),
      method: "goto".into(),
      title: "page.goto".into(),
      params: serde_json::json!({ "url": "about:blank" }),
      page_id: None,
      parent_id: None,
      step_id: None,
      before_snapshot: None,
      stack: Vec::new(),
    }));
    recorder.push_event(&TraceEvent::After(AfterActionEvent {
      call_id: "call@1".into(),
      end_time: recorder.monotonic_ms(),
      error: None,
      after_snapshot: None,
      attachments: Vec::new(),
    }));
    recorder.push_resource(&TraceResource {
      name: "page@1-1.jpeg".into(),
      bytes: vec![0xFF, 0xD8],
    });
    recorder
      .export(&path, &[serde_json::json!({ "request": {}, "response": {} })])
      .unwrap();

    let file = std::fs::File::open(&path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let names: Vec<String> = (0..archive.len())
      .map(|i| archive.by_index(i).unwrap().name().unwrap().into_owned())
      .collect();
    assert!(names.contains(&"trace.trace".to_string()));
    assert!(names.contains(&"trace.network".to_string()));
    assert!(names.contains(&"resources/page@1-1.jpeg".to_string()));

    let mut trace = String::new();
    std::io::Read::read_to_string(&mut archive.by_name("trace.trace").unwrap(), &mut trace).unwrap();
    let first: serde_json::Value = serde_json::from_str(trace.lines().next().unwrap()).unwrap();
    assert_eq!(
      first["version"].as_u64(),
      Some(8),
      "first line must be context-options v8"
    );
    assert_eq!(first["platform"].as_str(), Some(trace_platform()));
    assert!(
      first["playwrightVersion"]
        .as_str()
        .is_some_and(|v| v.starts_with("ferridriver/")),
      "the recorder identifies itself: {first}"
    );
    std::fs::remove_dir_all(&dir).ok();
  }

  /// A recording under a caller's `tracesDir` is named, left in place,
  /// and readable as it is written — the three things a viewer following
  /// a running test depends on.
  #[test]
  fn a_named_live_recording_is_readable_while_it_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let location = TraceLocation::in_dir(dir.path().to_path_buf(), "abc123-test".to_string());
    let recorder = TraceRecorder::new(
      &TracingStartOptions {
        streaming: TraceStreaming::Live,
        ..TracingStartOptions::default()
      },
      "chromium".into(),
      serde_json::json!({}),
      0,
      location,
    )
    .expect("spool");

    recorder.push_event(&TraceEvent::Before(BeforeActionEvent {
      call_id: "call@1".into(),
      start_time: 0.0,
      class: "Page".into(),
      method: "goto".into(),
      title: "page.goto".into(),
      params: serde_json::json!({ "url": "about:blank" }),
      page_id: Some("page@1".into()),
      parent_id: None,
      step_id: Some("call@1".into()),
      before_snapshot: None,
      stack: Vec::new(),
    }));

    // Still recording: no stop, no zip, and the file already has both
    // lines in it.
    let path = dir.path().join("abc123-test.trace");
    let written = std::fs::read_to_string(&path).expect("live trace file");
    assert_eq!(written.lines().count(), 2, "unflushed live trace: {written:?}");
    let action: serde_json::Value = serde_json::from_str(written.lines().nth(1).unwrap()).unwrap();
    assert_eq!(action["stepId"].as_str(), Some("call@1"), "v8 actions carry a stepId");

    drop(recorder);
    assert!(path.exists(), "a caller's tracesDir must survive the recorder");
  }

  #[test]
  fn a_temporary_recording_cleans_up_after_itself() {
    let recorder = TraceRecorder::new(
      &TracingStartOptions::default(),
      "chromium".into(),
      serde_json::json!({}),
      0,
      TraceLocation::temporary(),
    )
    .expect("spool");
    let dir = recorder.location().dir;
    assert!(dir.exists());
    drop(recorder);
    assert!(!dir.exists(), "temp spool left behind");
  }

  #[test]
  fn a_chunk_can_be_renamed_and_retitled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let recorder = TraceRecorder::new(
      &TracingStartOptions {
        title: Some("first".into()),
        streaming: TraceStreaming::Live,
        ..TracingStartOptions::default()
      },
      "chromium".into(),
      serde_json::json!({}),
      0,
      TraceLocation::in_dir(dir.path().to_path_buf(), "one".to_string()),
    )
    .expect("spool");

    recorder.start_chunk(0, Some("two".to_string()), Some("second".to_string()));
    assert_eq!(recorder.location().name, "two");
    assert_eq!(recorder.title().as_deref(), Some("second"));

    let second = std::fs::read_to_string(dir.path().join("two.trace")).expect("second chunk");
    let context: serde_json::Value = serde_json::from_str(second.lines().next().unwrap()).unwrap();
    assert_eq!(context["title"].as_str(), Some("second"));
    assert!(dir.path().join("one.trace").exists(), "previous chunk was discarded");
  }

  #[test]
  fn groups_nest_the_actions_recorded_inside_them() {
    let dir = tempfile::tempdir().expect("tempdir");
    let recorder = TraceRecorder::new(
      &TracingStartOptions {
        streaming: TraceStreaming::Live,
        ..TracingStartOptions::default()
      },
      "chromium".into(),
      serde_json::json!({}),
      0,
      TraceLocation::in_dir(dir.path().to_path_buf(), "grouped".to_string()),
    )
    .expect("spool");

    recorder.begin_group(
      "checkout".to_string(),
      vec![StackFrame {
        file: "/spec.ts".into(),
        line: 3,
        column: 1,
      }],
    );
    let inner_parent = recorder.current_parent();
    recorder.end_group();
    let after_end = recorder.current_parent();

    let written = std::fs::read_to_string(dir.path().join("grouped.trace")).expect("trace");
    let events: Vec<serde_json::Value> = written.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let group = &events[1];
    assert_eq!(group["type"].as_str(), Some("before"));
    assert_eq!(group["class"].as_str(), Some("Tracing"));
    assert_eq!(group["method"].as_str(), Some("tracingGroup"));
    assert_eq!(group["title"].as_str(), Some("checkout"));
    assert_eq!(group["stack"][0]["line"].as_u64(), Some(3));

    assert_eq!(
      inner_parent.as_deref(),
      group["callId"].as_str(),
      "actions inside a group nest under it"
    );
    assert!(after_end.is_none(), "groupEnd pops the group");
    assert_eq!(events[2]["type"].as_str(), Some("after"));
    assert_eq!(events[2]["callId"], group["callId"]);
  }

  #[test]
  fn run_level_errors_serialize_for_the_errors_tab() {
    let line = serialize_event(&TraceEvent::Error(TraceErrorEvent {
      message: "expect(received).toBe(expected)".into(),
      stack: vec![StackFrame {
        file: "/spec.ts".into(),
        line: 9,
        column: 2,
      }],
    }));
    let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid json");
    assert_eq!(parsed["type"].as_str(), Some("error"));
    assert_eq!(parsed["message"].as_str(), Some("expect(received).toBe(expected)"));
    assert_eq!(parsed["stack"][0]["file"].as_str(), Some("/spec.ts"));
  }
}
