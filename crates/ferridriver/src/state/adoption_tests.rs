use super::*;
use crate::backend::webdriver::browser::WebDriverBrowser;
use crate::backend::webdriver::session::{CreatedSession, WebDriverSession, tests::request};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;

async fn allocated_browser(endpoint: &str, id: &str) -> AnyBrowser {
  let session = WebDriverSession::new(
    reqwest::Client::new(),
    format!("{endpoint}/session").parse().unwrap(),
    id,
  )
  .unwrap();
  AnyBrowser::WebDriver(
    WebDriverBrowser::from_created(
      CreatedSession {
        session: Arc::new(session),
        id: id.into(),
        capabilities: json!({"browserName":"safari","browserVersion":"26.2","setWindowRect":true}),
      },
      tokio::time::Instant::now() + std::time::Duration::from_secs(1),
      1000,
    )
    .await
    .unwrap(),
  )
}

async fn reply(socket: &mut tokio::net::TcpStream, status: &str, value: Value) {
  let body = json!({"value":value}).to_string();
  socket
    .write_all(
      format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
      )
      .as_bytes(),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_cancelled_launch_caller_does_not_discard_or_duplicate_its_session() {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let endpoint = format!("http://{}", listener.local_addr().unwrap());
  let (received, receipt) = tokio::sync::oneshot::channel();
  let (release, released) = tokio::sync::oneshot::channel();
  let server = tokio::spawn(async move {
    let (mut socket, _) = listener.accept().await.unwrap();
    let mut requests = vec![request(&mut socket).await.0];
    received.send(()).unwrap();
    released.await.unwrap();
    reply(
      &mut socket,
      "200 OK",
      json!({"sessionId":"retained","capabilities":{"browserName":"safari","browserVersion":"26.2"}}),
    )
    .await;
    for value in [json!([]), Value::Null] {
      let (mut socket, _) = listener.accept().await.unwrap();
      requests.push(request(&mut socket).await.0);
      reply(&mut socket, "200 OK", value).await;
    }
    requests
  });
  let mut state = tests::test_state(BackendKind::WebDriver);
  state.connect_mode = ConnectMode::WebDriver {
    endpoint,
    browser_name: "safari".into(),
    protocol: crate::backend::webdriver::WebDriverProtocol::Classic,
    capabilities: None,
    headers: None,
    timeout: Some(1000),
  };
  let state = Arc::new(tokio::sync::RwLock::new(state));
  let launching = Arc::clone(&state);
  let launch = tokio::spawn(async move { BrowserState::ensure_instance_shared(&launching, "safari").await });
  receipt.await.unwrap();
  launch.abort();
  assert!(launch.await.unwrap_err().is_cancelled());
  release.send(()).unwrap();
  BrowserState::ensure_instance_shared(&state, "safari").await.unwrap();
  assert!(state.read().await.instance_is_live("safari"));
  assert!(state.read().await.pending_cleanup.is_empty());
  assert!(state.write().await.close_instance("safari").await.unwrap());
  assert_eq!(
    server.await.unwrap(),
    [
      "POST /session HTTP/1.1",
      "GET /session/retained/window/handles HTTP/1.1",
      "DELETE /session/retained HTTP/1.1",
    ]
  );
}

#[tokio::test]
async fn failed_initialization_retains_the_raw_session_until_deletion_succeeds() {
  for capabilities in [
    Value::Null,
    json!({"browserName":"firefox","browserVersion":"contract"}),
    json!({"browserName":"firefox","browserVersion":"contract","webSocketUrl":"not-a-socket"}),
  ] {
    for cleanup_fails in [false, true] {
      let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
      let endpoint = format!("http://{}", listener.local_addr().unwrap());
      let capabilities = capabilities.clone();
      let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        let mut responses = vec![
          ("200 OK", json!({"sessionId":"partial","capabilities":capabilities})),
          if cleanup_fails {
            (
              "500 Internal Server Error",
              json!({"error":"unknown error","message":"provider cleanup unavailable"}),
            )
          } else {
            ("200 OK", Value::Null)
          },
        ];
        if cleanup_fails {
          responses.push(("200 OK", Value::Null));
        }
        for (status, value) in responses {
          let (mut socket, _) = listener.accept().await.unwrap();
          requests.push(request(&mut socket).await.0);
          reply(&mut socket, status, value).await;
        }
        requests
      });
      let mut state = tests::test_state(BackendKind::Bidi);
      state.connect_mode = ConnectMode::WebDriver {
        endpoint,
        browser_name: "firefox".into(),
        protocol: crate::backend::webdriver::WebDriverProtocol::Bidi,
        capabilities: None,
        headers: None,
        timeout: Some(1000),
      };
      let state = Arc::new(tokio::sync::RwLock::new(state));
      let error = BrowserState::ensure_instance_shared(&state, "firefox")
        .await
        .unwrap_err();
      assert_eq!(error.to_string().contains("cleanup failed"), cleanup_fails, "{error}");
      assert!(state.read().await.instances.is_empty());
      assert_eq!(state.read().await.pending_cleanup.len(), usize::from(cleanup_fails));
      if cleanup_fails {
        let error = BrowserState::ensure_instance_shared(&state, "firefox")
          .await
          .unwrap_err();
        assert!(error.to_string().contains("unfinished cleanup"), "{error}");
        assert!(state.write().await.close_instance("firefox").await.unwrap());
      }
      assert!(state.read().await.pending_cleanup.is_empty());
      let requests = server.await.unwrap();
      assert_eq!(requests[0], "POST /session HTTP/1.1");
      assert_eq!(
        requests[1..],
        vec!["DELETE /session/partial HTTP/1.1"; if cleanup_fails { 2 } else { 1 }]
      );
    }
  }
}

#[tokio::test]
async fn close_retires_queued_launches_without_retiring_other_instances() {
  for shutdown in [false, true] {
    let state = Arc::new(tokio::sync::RwLock::new(tests::test_state(BackendKind::WebDriver)));
    let permit = state.read().await.launch_permit("safari");
    let other = state.read().await.launch_permit("other");
    let held = Arc::clone(&permit).lock_owned().await;
    let mut queued = Box::pin(BrowserState::ensure_instance_shared(&state, "safari"));
    assert!(futures::poll!(&mut queued).is_pending());
    if shutdown {
      state.write().await.shutdown().await.unwrap();
    } else {
      assert!(!state.write().await.close_instance("safari").await.unwrap());
    }
    drop(held);
    let error = queued.await.unwrap_err();
    assert!(error.to_string().contains("closed"), "{error}");
    let guard = state.read().await;
    assert!(guard.pending_cleanup.is_empty());
    assert!(!Arc::ptr_eq(&permit, &guard.launch_permit("safari")));
    assert_eq!(Arc::ptr_eq(&other, &guard.launch_permit("other")), !shutdown);
  }
}

#[tokio::test]
async fn shutdown_waits_for_inflight_allocation_and_retains_failed_deletion() {
  for (shared, cancel_caller) in [(true, false), (true, true), (false, true)] {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (received, receipt) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
      let (mut socket, _) = listener.accept().await.unwrap();
      let mut requests = vec![request(&mut socket).await.0];
      received.send(()).unwrap();
      released.await.unwrap();
      reply(
        &mut socket,
        "200 OK",
        json!({"sessionId":"late","capabilities":{"browserName":"safari","browserVersion":"26.2"}}),
      )
      .await;
      for (status, value) in [
        (
          "500 Internal Server Error",
          json!({"error":"unknown error","message":"provider cleanup unavailable"}),
        ),
        ("200 OK", Value::Null),
      ] {
        let (mut socket, _) = listener.accept().await.unwrap();
        requests.push(request(&mut socket).await.0);
        reply(&mut socket, status, value).await;
      }
      requests
    });
    let mut state = tests::test_state(BackendKind::WebDriver);
    state.connect_mode = ConnectMode::WebDriver {
      endpoint,
      browser_name: "safari".into(),
      protocol: crate::backend::webdriver::WebDriverProtocol::Classic,
      capabilities: None,
      headers: None,
      timeout: Some(1000),
    };
    let state = Arc::new(tokio::sync::RwLock::new(state));
    let launching = Arc::clone(&state);
    let launch = tokio::spawn(async move {
      if shared {
        BrowserState::ensure_instance_shared(&launching, "safari").await
      } else {
        launching.write().await.ensure_instance("safari").await
      }
    });
    receipt.await.unwrap();
    let launch = if cancel_caller {
      launch.abort();
      assert!(launch.await.unwrap_err().is_cancelled());
      None
    } else {
      Some(launch)
    };
    let mut guard = state.write().await;
    let mut closing = Box::pin(guard.shutdown());
    assert!(
      futures::poll!(&mut closing).is_pending(),
      "shutdown acknowledged an allocation whose identity is still pending"
    );
    release.send(()).unwrap();
    let error = closing.await.unwrap_err();
    assert!(error.to_string().contains("provider cleanup unavailable"), "{error}");
    assert!(guard.instances.is_empty());
    assert_eq!(guard.pending_cleanup.len(), 1);
    drop(guard);
    if let Some(launch) = launch {
      assert!(launch.await.unwrap().unwrap_err().to_string().contains("closed"));
    }
    assert!(state.write().await.close_instance("safari").await.unwrap());
    assert!(state.read().await.pending_cleanup.is_empty());
    assert_eq!(
      server.await.unwrap(),
      [
        "POST /session HTTP/1.1",
        "DELETE /session/late HTTP/1.1",
        "DELETE /session/late HTTP/1.1",
      ]
    );
  }
}

#[tokio::test]
async fn an_unpolled_installation_retains_its_incoming_owner() {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let endpoint = format!("http://{}", listener.local_addr().unwrap());
  let server = tokio::spawn(async move {
    let (mut socket, _) = listener.accept().await.unwrap();
    let received = request(&mut socket).await;
    reply(&mut socket, "200 OK", Value::Null).await;
    received.0
  });
  let browser = allocated_browser(&endpoint, "incoming").await;
  let mut state = tests::test_state(BackendKind::WebDriver);
  drop(state.install_instance("default", browser, true, true));
  assert_eq!(state.pending_cleanup.len(), 1);
  assert!(state.instances.is_empty());
  assert!(state.close_instance("default").await.unwrap());
  assert!(state.pending_cleanup.is_empty());
  assert_eq!(server.await.unwrap(), "DELETE /session/incoming HTTP/1.1");
}

#[tokio::test]
async fn adoption_failure_retains_the_same_owner_when_deletion_fails() {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let endpoint = format!("http://{}", listener.local_addr().unwrap());
  let server = tokio::spawn(async move {
    let mut requests = Vec::new();
    for (status, value) in [
      ("200 OK", Value::Null),
      (
        "500 Internal Server Error",
        json!({"error":"unknown error","message":"provider cleanup unavailable"}),
      ),
      ("200 OK", Value::Null),
    ] {
      let (mut socket, _) = listener.accept().await.unwrap();
      requests.push(request(&mut socket).await.0);
      reply(&mut socket, status, value).await;
    }
    requests
  });
  let browser = allocated_browser(&endpoint, "incoming").await;
  let mut state = tests::test_state(BackendKind::WebDriver);
  let error = state
    .install_instance("default", browser, true, true)
    .await
    .unwrap_err()
    .to_string();
  assert!(error.contains("expected an array"), "{error}");
  assert!(error.contains("provider cleanup unavailable"), "{error}");
  assert_eq!(state.pending_cleanup.len(), 1);
  assert!(state.instances.is_empty());
  assert!(
    state
      .ensure_instance("default")
      .await
      .unwrap_err()
      .to_string()
      .contains("unfinished cleanup")
  );
  assert!(state.close_instance("default").await.unwrap());
  assert!(state.pending_cleanup.is_empty());
  assert_eq!(
    server.await.unwrap(),
    [
      "GET /session/incoming/window/handles HTTP/1.1",
      "DELETE /session/incoming HTTP/1.1",
      "DELETE /session/incoming HTTP/1.1",
    ]
  );
}

#[tokio::test]
async fn cancelled_page_discovery_retains_the_incoming_owner() {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let endpoint = format!("http://{}", listener.local_addr().unwrap());
  let (received, receipt) = tokio::sync::oneshot::channel();
  let server = tokio::spawn(async move {
    let (mut held, _) = listener.accept().await.unwrap();
    let discovery = request(&mut held).await.0;
    received.send(()).unwrap();
    let (mut socket, _) = listener.accept().await.unwrap();
    let cleanup = request(&mut socket).await.0;
    reply(&mut socket, "200 OK", Value::Null).await;
    (discovery, cleanup)
  });
  let browser = allocated_browser(&endpoint, "incoming").await;
  let mut state = tests::test_state(BackendKind::WebDriver);
  tokio::select! {
    result = Box::pin(state.install_instance("default", browser, true, true)) => panic!("unexpected completion: {result:?}"),
    result = receipt => result.unwrap(),
  }
  assert_eq!(state.pending_cleanup.len(), 1);
  assert!(state.close_instance("default").await.unwrap());
  assert_eq!(
    server.await.unwrap(),
    (
      "GET /session/incoming/window/handles HTTP/1.1".into(),
      "DELETE /session/incoming HTTP/1.1".into(),
    )
  );
}

#[tokio::test]
async fn cancellation_during_eviction_retains_both_owners() {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let endpoint = format!("http://{}", listener.local_addr().unwrap());
  let (received, receipt) = tokio::sync::oneshot::channel();
  let (release, released) = tokio::sync::oneshot::channel();
  let server = tokio::spawn(async move {
    let (mut socket, _) = listener.accept().await.unwrap();
    let old = request(&mut socket).await.0;
    received.send(()).unwrap();
    released.await.unwrap();
    reply(&mut socket, "200 OK", Value::Null).await;
    let (mut socket, _) = listener.accept().await.unwrap();
    let incoming = request(&mut socket).await.0;
    reply(&mut socket, "200 OK", Value::Null).await;
    (old, incoming)
  });
  let mut state = tests::test_state(BackendKind::WebDriver);
  let old = BrowserInstance {
    browser: allocated_browser(&endpoint, "old").await,
    contexts: HashMap::default(),
    generation: state.next_instance_generation(),
  };
  state.instances.insert("default".into(), old);
  state.connected.store(true, std::sync::atomic::Ordering::Relaxed);
  let incoming = allocated_browser(&endpoint, "incoming").await;
  tokio::select! {
    result = Box::pin(state.install_instance("default", incoming, true, true)) => panic!("unexpected completion: {result:?}"),
    result = receipt => result.unwrap(),
  }
  assert_eq!(state.pending_cleanup.len(), 2);
  assert!(state.instances.is_empty());
  assert!(!state.connected.load(std::sync::atomic::Ordering::Relaxed));
  release.send(()).unwrap();
  state.shutdown().await.unwrap();
  assert!(state.pending_cleanup.is_empty());
  assert_eq!(
    server.await.unwrap(),
    (
      "DELETE /session/old HTTP/1.1".into(),
      "DELETE /session/incoming HTTP/1.1".into(),
    )
  );
}

#[tokio::test]
async fn cancelled_viewport_adoption_retains_pages_for_disposal() {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let endpoint = format!("http://{}", listener.local_addr().unwrap());
  let (received, receipt) = tokio::sync::oneshot::channel();
  let server = tokio::spawn(async move {
    let mut received = Some(received);
    let mut held = Vec::new();
    let mut requests = Vec::new();
    loop {
      let (mut socket, _) = listener.accept().await.unwrap();
      let (line, body) = request(&mut socket).await;
      requests.push(line.clone());
      let value = match line.as_str() {
        "GET /session/incoming/window/handles HTTP/1.1" => json!(["page"]),
        "POST /session/incoming/execute/sync HTTP/1.1" => json!({"width":0,"height":0}),
        "POST /session/incoming/window/rect HTTP/1.1" => {
          assert_eq!(body, json!({"width":800,"height":600}));
          held.push(socket);
          received.take().unwrap().send(()).unwrap();
          continue;
        },
        _ => Value::Null,
      };
      reply(&mut socket, "200 OK", value).await;
      if line.starts_with("DELETE ") {
        break;
      }
    }
    requests
  });
  let browser = allocated_browser(&endpoint, "incoming").await;
  let mut state = tests::test_state(BackendKind::WebDriver);
  state.default_viewport = Some(crate::options::ViewportConfig {
    width: 800,
    height: 600,
    ..Default::default()
  });
  tokio::select! {
    result = Box::pin(state.install_instance("default", browser, true, false)) => panic!("unexpected completion: {result:?}"),
    result = receipt => result.unwrap(),
  }
  assert_eq!(state.pending_cleanup.len(), 1);
  let pages = &state.pending_cleanup[0].1.instance().unwrap().contexts["default"].pages;
  assert_eq!(pages.len(), 1);
  let page = pages[0].clone();
  assert!(!page.is_closed());
  assert!(state.close_instance("default").await.unwrap());
  assert!(page.is_closed());
  assert!(state.pending_cleanup.is_empty());
  let requests = server.await.unwrap();
  assert_eq!(requests.iter().filter(|line| line.starts_with("DELETE ")).count(), 1);
  assert_eq!(requests.last().unwrap(), "DELETE /session/incoming HTTP/1.1");
}

#[tokio::test]
async fn successful_adoption_promotes_the_right_owner_after_cleanup_moves_entries() {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let endpoint = format!("http://{}", listener.local_addr().unwrap());
  let server = tokio::spawn(async move {
    let mut requests = Vec::new();
    for value in [Value::Null, json!(["page"]), Value::Null, Value::Null] {
      let (mut socket, _) = listener.accept().await.unwrap();
      requests.push(request(&mut socket).await.0);
      reply(&mut socket, "200 OK", value).await;
    }
    requests
  });
  let mut state = tests::test_state(BackendKind::WebDriver);
  for (name, id) in [("default", "old"), ("unrelated", "unrelated")] {
    let instance = BrowserInstance {
      browser: allocated_browser(&endpoint, id).await,
      contexts: HashMap::default(),
      generation: state.next_instance_generation(),
    };
    state.pending_cleanup.push((name.into(), instance.into()));
  }
  let incoming = allocated_browser(&endpoint, "incoming").await;
  state.install_instance("default", incoming, true, true).await.unwrap();
  assert_eq!(state.instance_generation("default"), Some(3));
  assert_eq!(state.instances["default"].contexts["default"].pages.len(), 1);
  assert_eq!(state.pending_cleanup.len(), 1);
  assert_eq!(state.pending_cleanup[0].0, "unrelated");
  assert!(state.connected.load(std::sync::atomic::Ordering::Relaxed));
  assert!(state.close_instance("default").await.unwrap());
  assert_eq!(state.pending_cleanup[0].0, "unrelated");
  assert!(state.close_instance("unrelated").await.unwrap());
  assert!(state.pending_cleanup.is_empty());
  assert_eq!(
    server.await.unwrap(),
    [
      "DELETE /session/old HTTP/1.1",
      "GET /session/incoming/window/handles HTTP/1.1",
      "DELETE /session/incoming HTTP/1.1",
      "DELETE /session/unrelated HTTP/1.1",
    ]
  );
}
