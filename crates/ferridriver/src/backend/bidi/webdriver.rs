pub(super) use crate::backend::webdriver::session::WebDriverSession;
pub(super) use crate::backend::webdriver::{http_client, same_origin};
#[cfg(test)]
use crate::error::FerriError;

#[cfg(test)]
mod tests {
  use super::*;
  use crate::backend::bidi::BidiBrowser;
  use futures::{SinkExt, StreamExt};
  use tokio::io::{AsyncReadExt, AsyncWriteExt};

  #[test]
  fn inherited_headers_are_confined_to_the_original_origin() {
    assert!(same_origin(
      "https://example.com/wd/hub",
      "wss://example.com/session/mobile"
    ));
    assert!(same_origin(
      "http://localhost:4444/wd/hub",
      "ws://localhost:4444/session/mobile"
    ));
    assert!(!same_origin(
      "https://example.com/wd/hub",
      "ws://example.com/session/mobile"
    ));
    assert!(!same_origin(
      "https://example.com/wd/hub",
      "wss://other.example.com/session/mobile"
    ));
    assert!(!same_origin(
      "http://localhost:4444/wd/hub",
      "ws://localhost:4445/session/mobile"
    ));
  }

  struct Server {
    endpoint: String,
    requests: tokio::sync::mpsc::UnboundedReceiver<String>,
    commands: tokio::sync::mpsc::UnboundedReceiver<String>,
    http: tokio::task::JoinHandle<()>,
    ws: tokio::task::JoinHandle<()>,
  }

  impl Drop for Server {
    fn drop(&mut self) {
      self.http.abort();
      self.ws.abort();
    }
  }

  async fn server(hang: bool, reject: bool) -> Server {
    let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/wd/hub", http.local_addr().unwrap());
    let ws_url = format!("ws://{}/session/mobile", ws.local_addr().unwrap());
    let (request_tx, requests) = tokio::sync::mpsc::unbounded_channel();
    let (command_tx, commands) = tokio::sync::mpsc::unbounded_channel();
    let http = tokio::spawn(async move {
      while let Ok((mut socket, _)) = http.accept().await {
        let mut request = Vec::new();
        let mut chunk = [0; 4096];
        loop {
          let count = socket.read(&mut chunk).await.unwrap();
          if count == 0 {
            break;
          }
          request.extend_from_slice(&chunk[..count]);
          if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
            let length = head
              .lines()
              .find_map(|line| line.strip_prefix("content-length: "))
              .map_or(0, |length| length.parse::<usize>().unwrap());
            if request.len() >= end + 4 + length {
              break;
            }
          }
        }
        let request = String::from_utf8(request).unwrap();
        let body = if request.starts_with("POST ") {
          serde_json::json!({"value":{"sessionId":"mobile","capabilities":{
            "browserName":"safari", "browserVersion":"contract", "webSocketUrl":ws_url
          }}})
        } else {
          serde_json::json!({"value":null})
        }
        .to_string();
        request_tx.send(request).unwrap();
        socket.write_all(format!(
          "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
        ).as_bytes()).await.unwrap();
      }
    });
    let ws = tokio::spawn(async move {
      let (socket, _) = ws.accept().await.unwrap();
      let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
      while let Some(Ok(message)) = socket.next().await {
        if !message.is_text() {
          continue;
        }
        let command: serde_json::Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        command_tx.send(command["method"].as_str().unwrap().into()).unwrap();
        if hang {
          continue;
        }
        let response = if reject {
          serde_json::json!({"type":"error", "id":command["id"], "error":"unknown command", "message":"contract rejection"})
        } else {
          serde_json::json!({"type":"success", "id":command["id"], "result":{}})
        };
        socket
          .send(tokio_tungstenite::tungstenite::Message::Text(
            response.to_string().into(),
          ))
          .await
          .unwrap();
      }
    });
    Server {
      endpoint,
      requests,
      commands,
      http,
      ws,
    }
  }

  async fn deleted(server: &mut Server) {
    let request = tokio::time::timeout(std::time::Duration::from_secs(2), server.requests.recv())
      .await
      .unwrap()
      .unwrap();
    assert!(
      request.starts_with("DELETE /wd/hub/session/mobile HTTP/1.1"),
      "{request}"
    );
  }

  #[tokio::test]
  async fn close_deletes_the_http_session_once_across_clones() {
    let mut server = server(false, false).await;
    let mut browser = BidiBrowser::connect_webdriver(&server.endpoint, "safari", None, None, Some(1000))
      .await
      .unwrap();
    assert_eq!(browser.version(), "safari/contract");
    assert!(server.requests.recv().await.unwrap().starts_with("POST "));
    assert_eq!(server.commands.recv().await.unwrap(), "session.subscribe");
    let mut clone = browser.clone();
    browser.close().await.unwrap();
    deleted(&mut server).await;
    clone.close().await.unwrap();
    drop(browser);
    drop(clone);
    assert!(server.requests.try_recv().is_err());
  }

  #[tokio::test]
  async fn rejected_bidi_initialization_deletes_the_http_session() {
    let mut server = server(false, true).await;
    let result = BidiBrowser::connect_webdriver(&server.endpoint, "safari", None, None, Some(1000)).await;
    assert!(matches!(result, Err(FerriError::Protocol { .. })));
    server.requests.recv().await.unwrap();
    deleted(&mut server).await;
  }

  #[tokio::test]
  async fn connect_timeout_covers_bidi_initialization_and_cleans_up() {
    let mut server = server(true, false).await;
    let result = BidiBrowser::connect_webdriver(&server.endpoint, "safari", None, None, Some(100)).await;
    assert!(matches!(result, Err(FerriError::Timeout { .. })));
    server.requests.recv().await.unwrap();
    deleted(&mut server).await;
  }

  #[tokio::test]
  async fn cancelled_bidi_initialization_deletes_the_http_session() {
    let mut server = server(true, false).await;
    let endpoint = server.endpoint.clone();
    let connect =
      tokio::spawn(async move { BidiBrowser::connect_webdriver(&endpoint, "safari", None, None, Some(0)).await });
    server.requests.recv().await.unwrap();
    assert_eq!(server.commands.recv().await.unwrap(), "session.subscribe");
    connect.abort();
    assert!(matches!(connect.await, Err(error) if error.is_cancelled()));
    deleted(&mut server).await;
  }
}
