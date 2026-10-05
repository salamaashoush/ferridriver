use super::*;
use futures::{SinkExt, StreamExt};

#[tokio::test]
async fn forced_socket_close_releases_popup_listener_session_and_downloads() {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let endpoint = format!("ws://{}", listener.local_addr().unwrap());
  let (stop, mut stopped) = tokio::sync::oneshot::channel();
  let (ready, stalled) = tokio::sync::oneshot::channel();
  let server = tokio::spawn(async move {
    let (stream, _) = listener.accept().await.unwrap();
    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
    loop {
      tokio::select! {
        _ = &mut stopped => {
          ready.send(()).unwrap();
          std::future::pending::<()>().await;
          return;
        },
        message = socket.next() => {
          let message = message.unwrap().unwrap();
          let request: serde_json::Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
          let result = if request["method"] == "session.new" {
            json!({"sessionId":"sashoush","capabilities":{"browserName":"firefox","browserVersion":"contract"}})
          } else {
            json!({})
          };
          socket.send(json!({"type":"success","id":request["id"],"result":result}).to_string().into()).await.unwrap();
        },
      }
    }
  });
  let mut browser = BidiBrowser::connect(&endpoint).await.unwrap();
  let session = Arc::downgrade(&browser.session);
  let downloads = Arc::downgrade(&browser.downloads_dir);
  let directory = browser.downloads_dir.path().to_owned();
  stop.send(()).unwrap();
  stalled.await.unwrap();
  tokio::time::pause();
  let mut closing = Box::pin(browser.close());
  assert!(futures::poll!(&mut closing).is_pending());
  tokio::time::advance(std::time::Duration::from_secs(5)).await;
  closing.await.unwrap();
  tokio::time::resume();
  drop(browser);
  let released = tokio::time::timeout(std::time::Duration::from_secs(1), async {
    while session.strong_count() != 0 || downloads.strong_count() != 0 || directory.exists() {
      tokio::task::yield_now().await;
    }
  })
  .await;
  server.abort();
  assert!(server.await.unwrap_err().is_cancelled());
  assert!(
    released.is_ok(),
    "forced close retained session/listener/download ownership"
  );
}
