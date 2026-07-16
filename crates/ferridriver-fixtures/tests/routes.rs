#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Round-trip coverage for every fixture route: plain HTTP via reqwest,
//! WebSocket echo via tokio-tungstenite, and proxy traversal with the
//! `/fx/proxy-log` observer.

use ferridriver_fixtures::{FixtureServer, FixtureServerOptions};
use futures::{SinkExt, StreamExt};

async fn start() -> FixtureServer {
  FixtureServer::start(FixtureServerOptions::default())
    .await
    .expect("start fixture server")
}

fn client() -> reqwest::Client {
  reqwest::Client::builder()
    .redirect(reqwest::redirect::Policy::none())
    .build()
    .expect("build client")
}

#[tokio::test]
async fn redirect_chain_lands() {
  let server = start().await;
  let base = server.url();

  let resp = client().get(format!("{base}/fx/redirect")).send().await.expect("get");
  assert_eq!(resp.status(), 302);
  assert_eq!(
    resp.headers().get("location").and_then(|v| v.to_str().ok()),
    Some("/fx/landed")
  );

  let resp = client().get(format!("{base}/fx/redirect/3")).send().await.expect("get");
  assert_eq!(resp.status(), 302);
  assert_eq!(
    resp.headers().get("location").and_then(|v| v.to_str().ok()),
    Some("/fx/redirect/2")
  );

  // With redirects enabled the chain resolves to the landing page.
  let follow = reqwest::Client::new();
  let resp = follow.get(format!("{base}/fx/redirect/3")).send().await.expect("get");
  assert_eq!(resp.status(), 200);
  assert!(resp.url().path().ends_with("/fx/landed"));
  assert_eq!(resp.text().await.expect("body"), "landed");

  server.stop().await;
}

#[tokio::test]
async fn api_routes_and_echo() {
  let server = start().await;
  let base = server.url();
  let client = client();

  let users: serde_json::Value = client
    .get(format!("{base}/fx/api/users"))
    .send()
    .await
    .expect("get")
    .json()
    .await
    .expect("json");
  assert_eq!(users, serde_json::json!({"users": ["alice", "bob"]}));

  let posts: serde_json::Value = client
    .get(format!("{base}/fx/api/posts"))
    .send()
    .await
    .expect("get")
    .json()
    .await
    .expect("json");
  assert_eq!(posts, serde_json::json!({"posts": ["first"]}));

  let resp = client
    .post(format!("{base}/fx/echo"))
    .body("hello echo")
    .send()
    .await
    .expect("post");
  assert_eq!(resp.status(), 200);
  assert_eq!(resp.text().await.expect("body"), "hello echo");

  let echoed: serde_json::Value = client
    .get(format!("{base}/fx/echo-headers"))
    .header("x-fx-marker", "42")
    .send()
    .await
    .expect("get")
    .json()
    .await
    .expect("json");
  assert_eq!(echoed["x-fx-marker"], serde_json::json!("42"));

  server.stop().await;
}

#[tokio::test]
async fn httpbin_shaped_api_echo() {
  let server = start().await;
  let base = server.url();

  let echoed: serde_json::Value = client()
    .post(format!("{base}/_api/post"))
    .header("content-type", "application/json")
    .body(r#"{"name": "Alice", "role": "admin"}"#)
    .send()
    .await
    .expect("post")
    .json()
    .await
    .expect("json");
  assert_eq!(echoed["url"], serde_json::json!("/_api/post"));
  assert_eq!(echoed["method"], serde_json::json!("POST"));
  assert_eq!(echoed["json"]["name"], serde_json::json!("Alice"));
  assert_eq!(echoed["json"]["role"], serde_json::json!("admin"));

  let deleted: serde_json::Value = client()
    .delete(format!("{base}/_api/delete"))
    .send()
    .await
    .expect("delete")
    .json()
    .await
    .expect("json");
  assert_eq!(deleted["method"], serde_json::json!("DELETE"));
  assert_eq!(deleted["url"], serde_json::json!("/_api/delete"));

  server.stop().await;
}

#[tokio::test]
async fn cookies_multi_and_set() {
  let server = start().await;
  let base = server.url();
  let client = client();

  let resp = client.get(format!("{base}/fx/multi-cookie")).send().await.expect("get");
  let cookies: Vec<&str> = resp
    .headers()
    .get_all("set-cookie")
    .iter()
    .filter_map(|v| v.to_str().ok())
    .collect();
  assert_eq!(cookies, vec!["a=1; Path=/", "b=2; Path=/"]);

  let resp = client
    .get(format!("{base}/fx/set-cookie?c=session%3Dabc%3B%20Path%3D%2F"))
    .send()
    .await
    .expect("get");
  let cookies: Vec<&str> = resp
    .headers()
    .get_all("set-cookie")
    .iter()
    .filter_map(|v| v.to_str().ok())
    .collect();
  assert_eq!(cookies, vec!["session=abc; Path=/"]);

  server.stop().await;
}

#[tokio::test]
async fn basic_auth_challenge() {
  let server = start().await;
  let base = server.url();
  let client = client();

  let resp = client.get(format!("{base}/fx/auth")).send().await.expect("get");
  assert_eq!(resp.status(), 401);
  assert_eq!(
    resp.headers().get("www-authenticate").and_then(|v| v.to_str().ok()),
    Some("Basic realm=\"fx\"")
  );
  assert_eq!(resp.text().await.expect("body"), "NOAUTH");

  let resp = client
    .get(format!("{base}/fx/auth"))
    .basic_auth("user", Some("pass"))
    .send()
    .await
    .expect("get");
  assert_eq!(resp.status(), 200);
  assert_eq!(resp.text().await.expect("body"), "AUTHED");

  let resp = client
    .get(format!("{base}/fx/auth"))
    .basic_auth("user", Some("wrong"))
    .send()
    .await
    .expect("get");
  assert_eq!(resp.status(), 401);

  server.stop().await;
}

#[tokio::test]
async fn csp_download_iframe() {
  let server = start().await;
  let base = server.url();
  let client = client();

  let resp = client.get(format!("{base}/fx/csp")).send().await.expect("get");
  assert_eq!(
    resp
      .headers()
      .get("content-security-policy")
      .and_then(|v| v.to_str().ok()),
    Some("script-src 'none'")
  );

  let resp = client.get(format!("{base}/fx/download")).send().await.expect("get");
  assert_eq!(
    resp.headers().get("content-disposition").and_then(|v| v.to_str().ok()),
    Some("attachment; filename=\"greeting.txt\"")
  );
  assert_eq!(resp.bytes().await.expect("body").as_ref(), b"fx-download-payload");

  let outer = client
    .get(format!("{base}/fx/iframe"))
    .send()
    .await
    .expect("get")
    .text()
    .await
    .expect("body");
  assert!(outer.contains("<iframe src=\"/fx/inner\">"));
  let inner = client
    .get(format!("{base}/fx/inner"))
    .send()
    .await
    .expect("get")
    .text()
    .await
    .expect("body");
  assert!(inner.contains("inner"));

  server.stop().await;
}

#[tokio::test]
async fn websocket_echo() {
  let server = start().await;
  let ws_url = format!("ws{}/fx/ws", server.url().strip_prefix("http").expect("http url"));

  let (mut socket, _) = tokio_tungstenite::connect_async(&ws_url).await.expect("ws connect");
  socket
    .send(tokio_tungstenite::tungstenite::Message::Text("ping-1".into()))
    .await
    .expect("send");
  let echoed = socket.next().await.expect("recv").expect("frame");
  assert_eq!(echoed.into_text().expect("text").as_str(), "ping-1");

  socket
    .send(tokio_tungstenite::tungstenite::Message::Binary(vec![1, 2, 3].into()))
    .await
    .expect("send");
  let echoed = socket.next().await.expect("recv").expect("frame");
  assert_eq!(echoed.into_data().as_ref(), &[1, 2, 3]);

  server.stop().await;
}

#[tokio::test]
async fn proxy_traversal_is_observable() {
  let server = start().await;
  let base = server.url();

  let info: serde_json::Value = client()
    .get(format!("{base}/fx/proxy-info"))
    .send()
    .await
    .expect("get")
    .json()
    .await
    .expect("json");
  let proxy_url = info["url"].as_str().expect("proxy url").to_string();
  assert_eq!(proxy_url, server.proxy_url());

  let proxied = reqwest::Client::builder()
    .proxy(reqwest::Proxy::http(&proxy_url).expect("proxy"))
    .build()
    .expect("client");
  let body = proxied
    .get("http://ferridriver-fixtures.invalid/behind-proxy")
    .send()
    .await
    .expect("get through proxy")
    .text()
    .await
    .expect("body");
  assert!(body.contains("PROXY:ok"), "canned proxy body, got: {body}");

  let log: serde_json::Value = client()
    .get(format!("{base}/fx/proxy-log"))
    .send()
    .await
    .expect("get")
    .json()
    .await
    .expect("json");
  assert!(log["hits"].as_u64().unwrap_or(0) >= 1, "proxy hits: {log}");
  let lines = log["lines"].as_array().expect("lines");
  assert!(
    lines
      .iter()
      .any(|l| l.as_str().is_some_and(|s| s.contains("behind-proxy"))),
    "request line recorded: {log}"
  );

  // DELETE resets the log.
  let cleared: serde_json::Value = client()
    .delete(format!("{base}/fx/proxy-log"))
    .send()
    .await
    .expect("delete")
    .json()
    .await
    .expect("json");
  assert_eq!(cleared["hits"], serde_json::json!(0));

  server.stop().await;
}

#[tokio::test]
async fn static_dir_served_alongside_fixtures() {
  let dir = std::env::temp_dir().join(format!("ferridriver-fixtures-static-{}", std::process::id()));
  std::fs::create_dir_all(&dir).expect("mkdir");
  std::fs::write(dir.join("hello.html"), "<!doctype html><body>static-ok</body>").expect("write");

  let server = FixtureServer::start(FixtureServerOptions {
    static_dir: Some(dir.clone()),
    ..Default::default()
  })
  .await
  .expect("start");
  let base = server.url();

  let body = client()
    .get(format!("{base}/hello.html"))
    .send()
    .await
    .expect("get")
    .text()
    .await
    .expect("body");
  assert!(body.contains("static-ok"));

  // Fixture routes still take precedence over the static fallback.
  let landed = client()
    .get(format!("{base}/fx/landed"))
    .send()
    .await
    .expect("get")
    .text()
    .await
    .expect("body");
  assert_eq!(landed, "landed");

  server.stop().await;
  let _ = std::fs::remove_dir_all(&dir);
}
