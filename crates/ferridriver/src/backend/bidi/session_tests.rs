use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

use super::session::BidiSession;
use crate::backend::webdriver::session::WebDriverSession;

#[tokio::test]
async fn missing_bidi_input_uses_the_owned_classic_session_and_caches_detection() {
  let websocket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let ws_url = format!("ws://{}/session/safari-contract", websocket.local_addr().unwrap());
  let ws_server = tokio::spawn(async move {
    let (stream, _) = websocket.accept().await.unwrap();
    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
    let mut methods = Vec::new();
    for _ in 0..4 {
      let request: Value = serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
      let method = request["method"].as_str().unwrap();
      methods.push(method.to_owned());
      let response = if method == "session.subscribe" {
        let events = request["params"]["events"].as_array().unwrap();
        assert!(events.contains(&json!("browsingContext.load")));
        assert!(!events.contains(&json!("browsingContext")));
        json!({"type":"success", "id":request["id"], "result":{"subscription":"events"}})
      } else {
        json!({"type":"error", "id":request["id"], "error":"unknown command", "message":"not implemented"})
      };
      socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
          response.to_string().into(),
        ))
        .await
        .unwrap();
    }
    methods
  });
  let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let url = reqwest::Url::parse(&format!("http://{}/session", http.local_addr().unwrap())).unwrap();
  let http_server = tokio::spawn(async move {
    let mut requests = Vec::new();
    for _ in 0..7 {
      let (stream, _) = http.accept().await.unwrap();
      let mut reader = tokio::io::BufReader::new(stream);
      let mut first = String::new();
      reader.read_line(&mut first).await.unwrap();
      let mut length = 0;
      loop {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        if line == "\r\n" {
          break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
          length = value.trim().parse().unwrap();
        }
      }
      let mut body = vec![0; length];
      reader.read_exact(&mut body).await.unwrap();
      requests.push((
        first,
        if body.is_empty() {
          Value::Null
        } else {
          serde_json::from_slice(&body).unwrap()
        },
      ));
      reader.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 14\r\nConnection: close\r\n\r\n{\"value\":null}").await.unwrap();
    }
    requests
  });
  let owner = Arc::new(WebDriverSession::new(reqwest::Client::new(), url, "safari-contract").unwrap());
  let session = BidiSession::connect_existing(
    &ws_url,
    "safari-contract".into(),
    json!({"browserName":"safari","browserVersion":"26.6.1"}),
    None,
    owner.clone(),
  )
  .await
  .unwrap();
  for window in ["page-first", "page-second"] {
    session
      .send_command("input.performActions", super::input::click(window, 20.0, 30.0))
      .await
      .unwrap();
  }
  owner.close().await.unwrap();
  let requests = http_server.await.unwrap();
  assert_eq!(requests[0].1, json!({"handle":"page-first"}));
  assert_eq!(requests[1].1, json!({"id":null}));
  assert!(requests[2].0.starts_with("POST /session/safari-contract/actions "));
  assert_eq!(requests[2].1["actions"][0]["actions"][0]["x"], 20);
  assert_eq!(requests[3].1, json!({"handle":"page-second"}));
  assert!(requests[5].0.starts_with("POST /session/safari-contract/actions "));
  assert!(requests[6].0.starts_with("DELETE /session/safari-contract "));
  assert_eq!(
    ws_server.await.unwrap(),
    [
      "session.subscribe",
      "network.addDataCollector",
      "network.addIntercept",
      "input.performActions"
    ]
  );
}
