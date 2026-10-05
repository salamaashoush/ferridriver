use super::*;
use tokio::io::AsyncWriteExt;

async fn server(
  chrome: Value,
  extension_status: u16,
  version: Option<&str>,
) -> (String, tokio::task::JoinHandle<Vec<(String, Value)>>) {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let endpoint = format!("http://{}/wd/hub", listener.local_addr().unwrap());
  let version = version.map(str::to_owned);
  let task = tokio::spawn(async move {
    let mut requests = Vec::new();
    loop {
      let (mut socket, _) = listener.accept().await.unwrap();
      let request = crate::backend::webdriver::session::tests::request(&mut socket).await;
      let deleted = request.0.starts_with("DELETE ");
      let (status, value) = if request.0.starts_with("POST /wd/hub/session HTTP/") {
        (
          200,
          json!({"sessionId":"android","capabilities":{"browserName":"Chrome","browserVersion":version}}),
        )
      } else if request.1["script"] == "mobile: getChromeCapabilities" {
        (extension_status, chrome.clone())
      } else if request.0.starts_with("GET /wd/hub/session/android/window/handles ") {
        (200, json!(["page"]))
      } else if request.0.starts_with("GET /wd/hub/session/android/title ") {
        (200, json!("sashoush's Android session"))
      } else {
        (200, Value::Null)
      };
      requests.push(request);
      let body = json!({"value":value}).to_string();
      socket.write_all(format!("HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
      if deleted {
        return requests;
      }
    }
  });
  (endpoint, task)
}

fn capabilities() -> Value {
  json!({"platformName":"Android","appium:options":{"automationName":"UiAutomator2"}})
}

#[tokio::test]
async fn missing_debugger_address_adopts_the_session_and_chrome_version() {
  let (endpoint, server) = server(
    json!({"browserName":"chrome","browserVersion":"152.0.7977.82"}),
    200,
    None,
  )
  .await;
  let mut browser = connect_android_chrome(&endpoint, &capabilities(), None, Some(1000), WebDriverProtocol::Auto)
    .await
    .unwrap();
  assert!(matches!(browser, AnyBrowser::WebDriver(_)));
  assert_eq!(browser.version(), "Chrome/152.0.7977.82");
  let pages = browser.pages().await.unwrap();
  assert_eq!(
    pages[0].title().await.unwrap().as_deref(),
    Some("sashoush's Android session")
  );
  browser.close().await.unwrap();
  let requests = server.await.unwrap();
  assert_eq!(
    requests
      .iter()
      .filter(|r| r.0.starts_with("POST /wd/hub/session HTTP/"))
      .count(),
    1
  );
  assert_eq!(requests.iter().filter(|r| r.0.starts_with("DELETE ")).count(), 1);
}

#[tokio::test]
async fn unsupported_extension_can_use_classic_but_provider_failures_cannot() {
  for status in [404, 405, 500, 501, 401, 403, 429, 503] {
    let (endpoint, server) = server(
      json!({"error":"unsupported operation","message":"extension unavailable"}),
      status,
      Some("152.0.7977.82"),
    )
    .await;
    let result = connect_android_chrome(&endpoint, &capabilities(), None, Some(1000), WebDriverProtocol::Auto).await;
    if [404, 405, 500, 501].contains(&status) {
      let mut browser = result.unwrap();
      assert!(matches!(browser, AnyBrowser::WebDriver(_)));
      browser.close().await.unwrap();
    } else {
      assert!(matches!(result, Err(FerriError::Protocol { .. })));
    }
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[2].0.starts_with("DELETE /wd/hub/session/android "));
  }
}

#[tokio::test]
async fn remote_loopback_discovery_uses_classic_without_contacting_that_address() {
  let (endpoint, server) = server(
    json!({"goog:chromeOptions":{"debuggerAddress":"127.0.0.1:9"}}),
    200,
    Some("152.0.7977.82"),
  )
  .await;
  let created = WebDriverSession::create(
    super::super::http_client(None).unwrap(),
    super::super::session_url(&endpoint).unwrap(),
    capabilities(),
    1000,
    None,
  )
  .await
  .unwrap();
  let mut browser = attach(
    created,
    "https://acme.example/wd/hub",
    None,
    tokio::time::Instant::now() + std::time::Duration::from_secs(1),
    1000,
    WebDriverProtocol::Auto,
  )
  .await
  .unwrap();
  assert!(matches!(browser, AnyBrowser::WebDriver(_)));
  browser.close().await.unwrap();
  assert_eq!(server.await.unwrap().len(), 3);
}

#[tokio::test]
async fn malformed_capabilities_fail_and_delete_the_owned_session() {
  for chrome in [
    Value::Null,
    json!([]),
    json!({"goog:chromeOptions":null}),
    json!({"goog:chromeOptions":{"debuggerAddress":7}}),
    json!({"goog:chromeOptions":{"debuggerAddress":""}}),
    json!({"goog:chromeOptions":{"debuggerAddress":"host/path"}}),
    json!({"goog:chromeOptions":{"debuggerAddress":"user:password@host"}}),
  ] {
    let (endpoint, server) = server(chrome, 200, Some("152.0.7977.82")).await;
    let result = connect_android_chrome(&endpoint, &capabilities(), None, Some(1000), WebDriverProtocol::Auto).await;
    assert!(matches!(result, Err(FerriError::Protocol { .. })));
    assert_eq!(server.await.unwrap().len(), 3);
  }
}

#[tokio::test]
async fn fallback_requires_an_actual_reported_browser_version() {
  let (endpoint, server) = server(json!({}), 200, None).await;
  let result = connect_android_chrome(&endpoint, &capabilities(), None, Some(1000), WebDriverProtocol::Auto).await;
  assert!(matches!(result, Err(FerriError::Protocol { message, .. }) if message.contains("browserVersion")));
  assert_eq!(server.await.unwrap().len(), 3);
}

#[tokio::test]
async fn explicit_classic_only_discovers_missing_version_metadata() {
  for version in [Some("152.0.7977.82"), None] {
    let (endpoint, server) = server(
      json!({"browserVersion":"152.0.7977.82",
      "goog:chromeOptions":{"debuggerAddress":"127.0.0.1:9"}}),
      200,
      version,
    )
    .await;
    let mut browser = connect_android_chrome(&endpoint, &capabilities(), None, Some(1000), WebDriverProtocol::Classic)
      .await
      .unwrap();
    assert!(matches!(browser, AnyBrowser::WebDriver(_)));
    assert_eq!(browser.version(), "Chrome/152.0.7977.82");
    browser.close().await.unwrap();
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), if version.is_some() { 2 } else { 3 });
  }
}

#[tokio::test]
async fn discovery_timeout_does_not_downgrade_or_leak_the_session() {
  let discovery = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let (endpoint, server) = server(
    json!({"goog:chromeOptions":{"debuggerAddress":discovery.local_addr().unwrap().to_string()}}),
    200,
    Some("152.0.7977.82"),
  )
  .await;
  let result = connect_android_chrome(&endpoint, &capabilities(), None, Some(100), WebDriverProtocol::Auto).await;
  assert!(matches!(result, Err(FerriError::Timeout { timeout_ms: 100, .. })));
  assert_eq!(server.await.unwrap().len(), 3);
}

#[test]
fn discovery_addresses_preserve_ipv6_and_reject_credentials_and_paths() {
  let url = discovery_url(&json!({"goog:chromeOptions":{"debuggerAddress":"[::1]:9222"}}))
    .unwrap()
    .unwrap();
  assert_eq!(url.as_str(), "http://[::1]:9222/json/version");
  assert!(url.host_str().is_some_and(is_loopback));
  for address in [
    "host/path",
    "user@host",
    "host?token=secret",
    "host#fragment",
    "https://host",
  ] {
    assert!(discovery_url(&json!({"goog:chromeOptions":{"debuggerAddress":address}})).is_err());
  }
}
