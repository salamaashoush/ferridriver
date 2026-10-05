use super::*;
use crate::backend::webdriver::session::tests::request;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;
use tokio::net::{TcpListener, TcpStream};

async fn reply(stream: &mut TcpStream, status: &str, value: Value) {
  let body = json!({"value":value}).to_string();
  stream
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

fn options() -> ConnectOptions {
  ConnectOptions {
    timeout: Some(1000),
    ..Default::default()
  }
}

#[tokio::test]
async fn close_before_the_producer_is_polled_prevents_provider_allocation() {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let endpoint = format!("http://{}", listener.local_addr().unwrap());
  let resources = BrowserResources::default();
  let mut opening = Box::pin(resources.connect(crate::safari(), &endpoint, options()));
  assert!(futures::poll!(&mut opening).is_pending());
  assert_eq!(resources.registry.lock().unwrap().states.len(), 1);
  let mut closing = Box::pin(resources.close());
  assert!(futures::poll!(&mut closing).is_pending());
  drop(opening);
  closing.await.unwrap();
  assert!(resources.registry.lock().unwrap().states.is_empty());
  assert_eq!(
    listener.into_std().unwrap().accept().unwrap_err().kind(),
    std::io::ErrorKind::WouldBlock
  );
  assert!(matches!(
    resources.connect(crate::safari(), &endpoint, options()).await,
    Err(FerriError::TargetClosed { .. })
  ));
}

#[tokio::test]
async fn failed_factory_keeps_its_raw_session_for_explicit_cleanup() {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let endpoint = format!("http://{}", listener.local_addr().unwrap());
  let server = tokio::spawn(async move {
    let mut requests = Vec::new();
    for (status, value) in [
      ("200 OK", json!({"sessionId":"partial"})),
      (
        "500 Internal Server Error",
        json!({"error":"unknown error","message":"cleanup unavailable"}),
      ),
      ("200 OK", Value::Null),
    ] {
      let (mut socket, _) = listener.accept().await.unwrap();
      requests.push(request(&mut socket).await.0);
      reply(&mut socket, status, value).await;
    }
    requests
  });
  let resources = BrowserResources::default();
  let error = resources
    .connect(crate::safari(), &endpoint, options())
    .await
    .err()
    .unwrap();
  assert!(error.to_string().contains("cleanup unavailable"), "{error}");
  assert_eq!(resources.registry.lock().unwrap().states.len(), 1);
  resources.close().await.unwrap();
  assert!(resources.registry.lock().unwrap().states.is_empty());
  assert_eq!(
    server.await.unwrap(),
    [
      "POST /session HTTP/1.1",
      "DELETE /session/partial HTTP/1.1",
      "DELETE /session/partial HTTP/1.1"
    ]
  );
}

#[tokio::test]
async fn cancelled_factory_retains_the_eventual_session_until_close() {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let endpoint = format!("http://{}", listener.local_addr().unwrap());
  let (received, receipt) = tokio::sync::oneshot::channel();
  let (release, released) = tokio::sync::oneshot::channel();
  let server = tokio::spawn(async move {
    let (mut socket, _) = listener.accept().await.unwrap();
    let create = request(&mut socket).await.0;
    received.send(()).unwrap();
    released.await.unwrap();
    reply(
      &mut socket,
      "200 OK",
      json!({"sessionId":"late","capabilities":{"browserName":"safari","browserVersion":"contract"}}),
    )
    .await;
    let (mut socket, _) = listener.accept().await.unwrap();
    let delete = request(&mut socket).await.0;
    reply(&mut socket, "200 OK", Value::Null).await;
    [create, delete]
  });
  let resources = Arc::new(BrowserResources::default());
  let owned = Arc::clone(&resources);
  let opening = tokio::spawn(async move { owned.connect(crate::safari(), &endpoint, options()).await });
  receipt.await.unwrap();
  opening.abort();
  assert!(opening.await.err().unwrap().is_cancelled());
  let mut closing = Box::pin(resources.close());
  assert!(futures::poll!(&mut closing).is_pending());
  release.send(()).unwrap();
  closing.await.unwrap();
  assert_eq!(
    server.await.unwrap(),
    ["POST /session HTTP/1.1", "DELETE /session/late HTTP/1.1"]
  );
  assert!(resources.registry.lock().unwrap().states.is_empty());
}

#[tokio::test]
async fn failed_disposal_is_retryable_and_retained_handles_cannot_relaunch() {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let endpoint = format!("http://{}", listener.local_addr().unwrap());
  let server = tokio::spawn(async move {
    let mut requests = Vec::new();
    for (status, value) in [
      (
        "200 OK",
        json!({"sessionId":"owned","capabilities":{"browserName":"chrome","browserVersion":"contract"}}),
      ),
      ("200 OK", json!([])),
      (
        "500 Internal Server Error",
        json!({"error":"unknown error","message":"cleanup unavailable"}),
      ),
      ("200 OK", Value::Null),
    ] {
      let (mut socket, _) = listener.accept().await.unwrap();
      requests.push(request(&mut socket).await.0);
      reply(&mut socket, status, value).await;
    }
    requests
  });
  let resources = BrowserResources::default();
  let browser = resources
    .connect(crate::chromium(), &endpoint, options())
    .await
    .unwrap();
  let context = browser.default_context();
  assert_eq!(browser.version(), "chrome/contract");
  assert_eq!(browser.backend_kind(), crate::backend::BackendKind::WebDriver);
  assert!(!browser.supports_isolated_contexts());
  assert!(
    resources
      .close()
      .await
      .unwrap_err()
      .to_string()
      .contains("cleanup unavailable")
  );
  assert_eq!(resources.registry.lock().unwrap().states.len(), 1);
  assert!(matches!(context.new_page().await, Err(FerriError::TargetClosed { .. })));
  assert!(matches!(browser.new_page().await, Err(FerriError::TargetClosed { .. })));
  resources.close().await.unwrap();
  resources.close().await.unwrap();
  assert!(resources.registry.lock().unwrap().states.is_empty());
  assert!(matches!(context.new_page().await, Err(FerriError::TargetClosed { .. })));
  assert_eq!(
    server.await.unwrap(),
    [
      "POST /session HTTP/1.1",
      "GET /session/owned/window/handles HTTP/1.1",
      "DELETE /session/owned HTTP/1.1",
      "DELETE /session/owned HTTP/1.1",
    ]
  );
}
