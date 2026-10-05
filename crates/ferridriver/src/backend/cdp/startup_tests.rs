use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::Arc;

use super::{CdpBrowser, ws::WsTransport};
use crate::backend::cdp::transport::CdpTransport as _;

#[tokio::test]
async fn closing_an_attached_browser_disconnects_retained_protocol_handles() {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let endpoint = format!("ws://{}", listener.local_addr().unwrap());
  let server = tokio::spawn(async move {
    let (stream, _) = listener.accept().await.unwrap();
    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
    let mut methods = Vec::new();
    while let Some(Ok(message)) = socket.next().await {
      if message.is_close() {
        break;
      }
      if !message.is_text() {
        continue;
      }
      let request: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
      let method = request["method"].as_str().unwrap();
      methods.push(method.to_owned());
      let result = match method {
        "Browser.getVersion" => json!({"product":"Chrome/contract"}),
        "Target.getTargets" => json!({"targetInfos":[]}),
        _ => json!({}),
      };
      socket
        .send(json!({"id":request["id"],"result":result}).to_string().into())
        .await
        .unwrap();
    }
    methods
  });
  let mut browser = CdpBrowser::<WsTransport>::connect(&endpoint).await.unwrap();
  let retained = Arc::clone(&browser.transport);
  browser.close().await.unwrap();
  assert!(
    retained.is_disconnected(),
    "close left retained protocol handles connected"
  );
  assert!(matches!(
    retained.send_command(None, "Browser.getVersion", &json!({})).await,
    Err(crate::FerriError::TargetClosed { .. })
  ));
  let methods = tokio::time::timeout(std::time::Duration::from_secs(1), server)
    .await
    .unwrap()
    .unwrap();
  assert!(!methods.iter().any(|method| method == "Browser.close"));
}

#[tokio::test]
async fn connect_resumes_targets_announced_before_auto_attach_response() {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let url = format!("ws://{}", listener.local_addr().unwrap());
  let (resumed_tx, resumed_rx) = tokio::sync::oneshot::channel();
  let server = tokio::spawn(async move {
    let (stream, _) = listener.accept().await.unwrap();
    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
    let mut resumed_tx = Some(resumed_tx);
    while let Some(Ok(message)) = socket.next().await {
      if !message.is_text() {
        continue;
      }
      let request: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
      let method = request["method"].as_str().unwrap();
      if method == "Target.setAutoAttach" && request.get("sessionId").is_none() {
        socket
          .send(
            json!({
              "method":"Target.attachedToTarget",
              "params":{"sessionId":"worker-session", "waitingForDebugger":true,
                "targetInfo":{"targetId":"worker", "type":"service_worker"}}
            })
            .to_string()
            .into(),
          )
          .await
          .unwrap();
      }
      if method == "Runtime.runIfWaitingForDebugger"
        && request["sessionId"] == "worker-session"
        && let Some(sender) = resumed_tx.take()
      {
        sender.send(()).unwrap();
      }
      let result = match method {
        "Browser.getVersion" => json!({"product":"Chrome/124", "userAgent":"Android"}),
        "Target.getTargets" => json!({"targetInfos":[{"targetId":"page", "type":"page"}]}),
        "Target.attachToTarget" => json!({"sessionId":"page-session"}),
        _ => json!({}),
      };
      socket
        .send(json!({"id":request["id"], "result":result}).to_string().into())
        .await
        .unwrap();
    }
  });
  let browser = CdpBrowser::<WsTransport>::connect(&url).await.unwrap();
  let resumed = tokio::time::timeout(std::time::Duration::from_secs(2), resumed_rx).await;
  drop(browser);
  server.abort();
  assert!(
    resumed.is_ok(),
    "startup attach event was lost, leaving the worker paused"
  );
  resumed.unwrap().unwrap();
}

#[tokio::test]
async fn dropping_attach_tasks_cancels_their_transport_ownership() {
  let task = tokio::spawn(std::future::pending::<()>());
  let tasks = super::AttachTasks(vec![task.abort_handle()]);
  drop(tasks);
  assert!(task.await.unwrap_err().is_cancelled());
}
