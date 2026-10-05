use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

use super::{WebDriverProtocol, connect};
use crate::backend::{AnyBrowser, BackendKind};
use crate::error::FerriError;

async fn server(capabilities: Value) -> (String, tokio::task::JoinHandle<Vec<(String, Value)>>) {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let endpoint = format!("http://{}/wd/hub", listener.local_addr().unwrap());
  let task = tokio::spawn(async move {
    let mut requests = Vec::new();
    loop {
      let (mut socket, _) = listener.accept().await.unwrap();
      let request = super::session::tests::request(&mut socket).await;
      let deleted = request.0.starts_with("DELETE ");
      let value = if request.0.starts_with("POST /wd/hub/session HTTP/") {
        json!({"sessionId":"owned","capabilities":capabilities})
      } else if request.0.starts_with("GET /wd/hub/session/owned/window/handles ") {
        json!(["page"])
      } else if request.0.starts_with("GET /wd/hub/session/owned/title ") {
        json!("sashoush's session")
      } else {
        Value::Null
      };
      requests.push(request);
      let body = json!({"value":value}).to_string();
      socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
      if deleted {
        return requests;
      }
    }
  });
  (endpoint, task)
}

#[tokio::test]
async fn automatic_connection_adopts_classic_without_creating_another_session() {
  for name in ["chrome", "firefox"] {
    let (endpoint, server) = server(json!({"browserName":name,"browserVersion":"contract"})).await;
    let mut browser = connect(&endpoint, name, None, None, Some(1000), WebDriverProtocol::Auto)
      .await
      .unwrap();
    assert!(matches!(browser, AnyBrowser::WebDriver(_)));
    let pages = browser.pages().await.unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].title().await.unwrap().as_deref(), Some("sashoush's session"));
    browser.close().await.unwrap();
    drop(browser);
    let requests = server.await.unwrap();
    assert_eq!(
      requests
        .iter()
        .filter(|r| r.0.starts_with("POST /wd/hub/session HTTP/"))
        .count(),
      1
    );
    assert_eq!(requests[0].1["capabilities"]["alwaysMatch"]["webSocketUrl"], true);
    assert_eq!(requests.iter().filter(|r| r.0.starts_with("DELETE ")).count(), 1);
    assert!(requests.last().unwrap().0.starts_with("DELETE /wd/hub/session/owned "));
  }
}

#[tokio::test]
async fn required_bidi_rejects_classic_and_releases_the_same_session() {
  for protocol in [WebDriverProtocol::Auto, WebDriverProtocol::Bidi] {
    let (endpoint, server) = server(json!({"browserName":"firefox","browserVersion":"contract"})).await;
    let result = connect(
      &endpoint,
      "firefox",
      Some(&json!({"webSocketUrl":true})),
      None,
      Some(1000),
      protocol,
    )
    .await;
    assert!(matches!(result, Err(FerriError::Unsupported { .. })));
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].0.starts_with("DELETE /wd/hub/session/owned "));
  }
}

#[tokio::test]
async fn explicit_classic_preserves_the_false_socket_capability() {
  let (endpoint, server) = server(json!({"browserName":"chrome","browserVersion":"contract"})).await;
  let mut browser = connect(
    &endpoint,
    "chrome",
    Some(&json!({"webSocketUrl":false})),
    None,
    Some(1000),
    WebDriverProtocol::Auto,
  )
  .await
  .unwrap();
  assert!(matches!(browser, AnyBrowser::WebDriver(_)));
  browser.close().await.unwrap();
  let requests = server.await.unwrap();
  assert_eq!(requests.len(), 2);
  assert_eq!(requests[0].1["capabilities"]["alwaysMatch"]["webSocketUrl"], false);
}

#[tokio::test]
async fn malformed_advertised_socket_does_not_fall_back_to_classic() {
  for socket in [json!(true), json!(7), json!(""), json!("http://127.0.0.1:1/session")] {
    let (endpoint, server) =
      server(json!({"browserName":"chrome","browserVersion":"contract","webSocketUrl":socket})).await;
    assert!(
      connect(&endpoint, "chrome", None, None, Some(1000), WebDriverProtocol::Auto)
        .await
        .is_err()
    );
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].0.starts_with("DELETE /wd/hub/session/owned "));
  }
}

#[tokio::test]
async fn failed_classic_initialization_releases_the_created_session() {
  let (endpoint, server) = server(json!({"browserName":"chrome"})).await;
  let result = connect(&endpoint, "chrome", None, None, Some(1000), WebDriverProtocol::Auto).await;
  assert!(matches!(result, Err(FerriError::Protocol { .. })));
  let requests = server.await.unwrap();
  assert_eq!(requests.len(), 2);
  assert!(requests[1].0.starts_with("DELETE /wd/hub/session/owned "));
}

#[tokio::test]
async fn conflicting_protocol_requirements_fail_before_creating_a_session() {
  for (backend, socket) in [(BackendKind::Bidi, false), (BackendKind::WebDriver, true)] {
    let result = connect(
      "http://127.0.0.1:1",
      "chrome",
      Some(&json!({"webSocketUrl":socket})),
      None,
      Some(1000),
      WebDriverProtocol::from_backend(Some(backend)),
    )
    .await;
    assert!(matches!(result, Err(FerriError::InvalidArgument { .. })));
  }
}

#[tokio::test]
async fn android_optimization_preserves_the_requested_browser() {
  for name in ["firefox", "safari"] {
    let (endpoint, server) = server(json!({"browserName":name,"browserVersion":"contract"})).await;
    let mut browser = connect(
      &endpoint,
      name,
      Some(&json!({"platformName":"Android","appium:automationName":"UiAutomator2"})),
      None,
      Some(1000),
      WebDriverProtocol::Auto,
    )
    .await
    .unwrap();
    browser.close().await.unwrap();
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].1["capabilities"]["alwaysMatch"]["browserName"], name);
  }
}
