#![allow(clippy::expect_used, clippy::unwrap_used)]
//! The sandbox `NetGuard` is enforced end-to-end: the host allow-list
//! and the metadata block apply to the INITIAL url AND every redirect
//! hop (the SSRF-via-redirect bypass), and loopback stays reachable so
//! local automation is unaffected. Browser-free std-only loopback server.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use ferridriver::http_client::{HttpClient, HttpClientOptions, NetGuard, RequestOptions};

/// Routes:
/// - `/landed`            → 200 "LANDED"
/// - `/hop-offhost`       → 302 to `http://localhost:<port>/landed`
///   (a different host than `127.0.0.1`)
/// - `/hop-metadata`      → 302 to `http://169.254.169.254/latest`
fn spawn_server() -> (String, u16) {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
  let addr = listener.local_addr().expect("local addr");
  let port = addr.port();
  thread::spawn(move || {
    for stream in listener.incoming() {
      let Ok(stream) = stream else { continue };
      handle(stream, port);
    }
  });
  (format!("http://127.0.0.1:{port}"), port)
}

fn handle(mut stream: TcpStream, port: u16) {
  let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
  let mut reader = BufReader::new(stream.try_clone().expect("clone"));
  let mut request_line = String::new();
  if reader.read_line(&mut request_line).is_err() || request_line.is_empty() {
    return;
  }
  let path = request_line.split_whitespace().nth(1).unwrap_or("/").to_string();
  loop {
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line == "\r\n" || line.is_empty() {
      break;
    }
  }
  let response = match path.as_str() {
    "/landed" => "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nLANDED".to_string(),
    "/hop-offhost" => format!(
      "HTTP/1.1 302 Found\r\nLocation: http://localhost:{port}/landed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    ),
    "/hop-metadata" => {
      "HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/latest\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        .to_string()
    },
    _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
  };
  let _ = stream.write_all(response.as_bytes());
  let _ = stream.flush();
  let mut sink = Vec::new();
  let _ = reader.get_mut().read_to_end(&mut sink);
}

fn guard(allowlist: Option<&[&str]>, block_metadata: bool, block_private: bool) -> NetGuard {
  NetGuard {
    allowlist: allowlist.map(|l| Arc::from(l.iter().map(|s| (*s).to_string()).collect::<Vec<_>>())),
    block_metadata,
    block_private,
  }
}

fn opts(g: NetGuard) -> RequestOptions {
  RequestOptions {
    net_guard: Some(g),
    ..Default::default()
  }
}

#[tokio::test]
async fn allowlisted_host_direct_request_succeeds() {
  let (base, _) = spawn_server();
  let client = HttpClient::new(HttpClientOptions::default());
  let resp = client
    .get(&format!("{base}/landed"), Some(opts(guard(Some(&["127.0.0.1"]), true, false))))
    .await
    .expect("allowlisted loopback host is reachable");
  assert_eq!(resp.text().unwrap(), "LANDED");
}

#[tokio::test]
async fn redirect_to_offlist_host_is_rejected() {
  // The crux of the SSRF fix: 127.0.0.1 is allow-listed, but it 302s to
  // `localhost` (a different host). The per-hop check must reject it.
  let (base, _) = spawn_server();
  let client = HttpClient::new(HttpClientOptions::default());
  let err = client
    .get(
      &format!("{base}/hop-offhost"),
      Some(opts(guard(Some(&["127.0.0.1"]), true, false))),
    )
    .await
    .expect_err("a redirect off the allow-list must fail");
  let msg = err.to_string();
  assert!(!msg.contains("LANDED"), "must not have followed the redirect: {msg}");
}

#[tokio::test]
async fn redirect_to_metadata_is_rejected_even_without_allowlist() {
  // No allow-list (the default top-level-script posture), but the
  // metadata endpoint is blocked by default — the redirect hop to
  // 169.254.169.254 must fail.
  let (base, _) = spawn_server();
  let client = HttpClient::new(HttpClientOptions::default());
  let res = client
    .get(&format!("{base}/hop-metadata"), Some(opts(guard(None, true, false))))
    .await;
  assert!(res.is_err(), "redirect to cloud metadata must be blocked");
}

#[tokio::test]
async fn direct_metadata_request_is_rejected_at_preflight() {
  let client = HttpClient::new(HttpClientOptions::default());
  let err = client
    .get("http://169.254.169.254/latest/meta-data/", Some(opts(guard(None, true, false))))
    .await
    .expect_err("metadata IP must be denied before any I/O");
  assert!(err.to_string().contains("blocked address"), "{err}");
}

#[tokio::test]
async fn unguarded_path_is_unaffected() {
  // No guard → original behaviour (follows the loopback redirect).
  let (base, _) = spawn_server();
  let client = HttpClient::new(HttpClientOptions::default());
  let resp = client
    .get(&format!("{base}/hop-offhost"), None)
    .await
    .expect("unguarded request still follows redirects");
  assert_eq!(resp.text().unwrap(), "LANDED");
}
