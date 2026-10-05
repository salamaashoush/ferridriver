use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::backend::cdp::{CdpBrowser, ws::WsTransport};
use crate::error::{FerriError, Result};

pub(super) async fn wait_until_ready(browser: &CdpBrowser<WsTransport>) -> Result<()> {
  let listener = TcpListener::bind("127.0.0.1:0").await?;
  let endpoint = serde_json::to_string(&format!("http://10.0.2.2:{}/", listener.local_addr()?.port()))?;
  let script = format!(
    r"new Promise(resolve => {{
      const request = new XMLHttpRequest();
      request.open('GET', {endpoint});
      request.timeout = 1000;
      request.onload = () => resolve(request.status === 204);
      request.onerror = request.ontimeout = () => resolve(false);
      request.send();
    }})"
  );
  // navigator.onLine and Android LinkProperties can report ready before netd installs IPv4 routes.
  tokio::select! {
    result = serve(listener) => result,
    result = Box::pin(wait_for_browser_network(browser, &script)) => result,
  }
}

async fn wait_for_browser_network(browser: &CdpBrowser<WsTransport>, script: &str) -> Result<()> {
  let mut page = None;
  loop {
    if page.is_none() {
      match Box::pin(browser.pages()).await {
        Ok(pages) => page = pages.into_iter().next(),
        Err(FerriError::TargetClosed { .. }) if browser.is_alive() => {},
        Err(error) => return Err(error),
      }
    }
    if let Some(current) = &page {
      match current.evaluate(script).await {
        Ok(Some(serde_json::Value::Bool(true))) => return Ok(()),
        Ok(_) => {},
        Err(FerriError::TargetClosed { .. }) if browser.is_alive() => page = None,
        Err(error) => return Err(error),
      }
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

async fn serve(listener: TcpListener) -> Result<()> {
  loop {
    let (stream, _) = listener.accept().await?;
    match tokio::time::timeout(Duration::from_secs(2), respond(stream)).await {
      Ok(Ok(())) => {},
      Ok(Err(error)) => tracing::debug!(%error, "Android network probe connection failed"),
      Err(error) => tracing::debug!(%error, "Android network probe request timed out"),
    }
  }
}

async fn respond(mut stream: TcpStream) -> std::io::Result<()> {
  let mut request = Vec::new();
  let mut buffer = [0; 1024];
  while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
    let count = stream.read(&mut buffer).await?;
    if count == 0 {
      return Err(std::io::ErrorKind::UnexpectedEof.into());
    }
    request.extend_from_slice(&buffer[..count]);
    if request.len() > 8192 {
      return Err(std::io::Error::other(
        "Android network probe request headers exceed 8192 bytes",
      ));
    }
  }
  let date = httpdate::fmt_http_date(std::time::SystemTime::now());
  let response = format!(
    "HTTP/1.1 204 No Content\r\nDate: {date}\r\nConnection: close\r\nCache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, OPTIONS\r\nAccess-Control-Allow-Private-Network: true\r\n\r\n"
  );
  stream.write_all(response.as_bytes()).await
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn the_probe_handles_preflight_and_releases_its_listener_on_cancellation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut server = Box::pin(serve(listener));
    assert!(futures::poll!(&mut server).is_pending());
    let client = async {
      let client = reqwest::Client::new();
      for method in [reqwest::Method::OPTIONS, reqwest::Method::GET] {
        let response = client
          .request(method, format!("http://{address}/"))
          .send()
          .await
          .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
        assert_eq!(response.headers()["access-control-allow-origin"], "*");
        assert_eq!(response.headers()["access-control-allow-private-network"], "true");
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(!response.headers().contains_key("content-length"));
        assert!(httpdate::parse_http_date(response.headers()["date"].to_str().unwrap()).is_ok());
        assert!(response.bytes().await.unwrap().is_empty());
      }
    };
    tokio::select! {
      result = &mut server => panic!("server unexpectedly stopped: {result:?}"),
      () = client => {},
    }
    // Linux can connect a TCP socket to itself if both ephemeral endpoints coincide.
    let probe = tokio::net::TcpSocket::new_v4().unwrap();
    probe.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    assert_ne!(probe.local_addr().unwrap(), address);
    drop(server);
    assert!(probe.connect(address).await.is_err());
    assert!(TcpListener::bind(address).await.is_ok());
  }

  #[tokio::test]
  async fn cancelling_a_stalled_request_closes_its_accepted_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut client = TcpStream::connect(address).await.unwrap();
    let (stream, _) = listener.accept().await.unwrap();
    let mut response = Box::pin(respond(stream));
    assert!(futures::poll!(&mut response).is_pending());
    drop(response);
    assert_eq!(client.read(&mut [0]).await.unwrap(), 0);
  }

  #[tokio::test]
  async fn fragmented_headers_are_read_before_the_probe_responds() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let client = async {
      let mut stream = TcpStream::connect(address).await.unwrap();
      stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n")
        .await
        .unwrap();
      let mut response = Vec::new();
      let mut reading = Box::pin(stream.read_to_end(&mut response));
      assert!(futures::poll!(&mut reading).is_pending());
      drop(reading);
      stream.write_all(b"\r\n").await.unwrap();
      stream.read_to_end(&mut response).await.unwrap();
      assert!(response.starts_with(b"HTTP/1.1 204 No Content\r\n"));
    };
    tokio::select! {
      result = serve(listener) => panic!("server unexpectedly stopped: {result:?}"),
      () = client => {},
    }
  }
}
