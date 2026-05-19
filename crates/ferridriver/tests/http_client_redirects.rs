#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Per-request `max_redirects` is honoured (real, not a no-op), and the
//! per-redirect-policy clients share one cookie jar so session cookies
//! still persist. Browser-free: a tiny std-only HTTP server on loopback.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use ferridriver::http_client::{HttpClient, HttpClientOptions, RequestOptions};

/// Minimal HTTP/1.1 test server. Routes:
/// - `GET /redirect/<n>`: `n>0` → 302 to `/redirect/<n-1>`; `n==0` → 200 "done".
/// - `GET /set`: 200, `Set-Cookie: sid=abc; Path=/`, body "set".
/// - `GET /echo`: 200, body = the received `Cookie` header (or "none").
fn spawn_server() -> String {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
  let addr = listener.local_addr().expect("local addr");
  thread::spawn(move || {
    for stream in listener.incoming() {
      let Ok(stream) = stream else { continue };
      handle(stream);
    }
  });
  format!("http://{addr}")
}

fn handle(mut stream: TcpStream) {
  let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
  let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
  let mut request_line = String::new();
  if reader.read_line(&mut request_line).is_err() || request_line.is_empty() {
    return;
  }
  let path = request_line.split_whitespace().nth(1).unwrap_or("/").to_string();

  let mut cookie = String::from("none");
  loop {
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
      break;
    }
    if line == "\r\n" || line.is_empty() {
      break;
    }
    if let Some(v) = line.strip_prefix("Cookie: ").or_else(|| line.strip_prefix("cookie: ")) {
      cookie = v.trim().to_string();
    }
  }

  let response = if let Some(rest) = path.strip_prefix("/redirect/") {
    let n: u32 = rest.parse().unwrap_or(0);
    if n == 0 {
      http_ok("done", None)
    } else {
      format!(
        "HTTP/1.1 302 Found\r\nLocation: /redirect/{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        n - 1
      )
    }
  } else if path == "/set" {
    http_ok("set", Some("sid=abc; Path=/"))
  } else if path == "/echo" {
    http_ok(&cookie, None)
  } else {
    http_ok("ok", None)
  };

  let _ = stream.write_all(response.as_bytes());
  let _ = stream.flush();
  // Drain anything still inbound so the client sees a clean close.
  let mut sink = Vec::new();
  let _ = reader.get_mut().read_to_end(&mut sink);
}

fn http_ok(body: &str, set_cookie: Option<&str>) -> String {
  let cookie_hdr = set_cookie.map(|c| format!("Set-Cookie: {c}\r\n")).unwrap_or_default();
  format!(
    "HTTP/1.1 200 OK\r\n{cookie_hdr}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
    body.len()
  )
}

fn opts(max_redirects: Option<u32>) -> RequestOptions {
  RequestOptions {
    max_redirects,
    ..Default::default()
  }
}

#[tokio::test]
async fn max_redirects_none_follows_chain_to_completion() {
  let base = spawn_server();
  let client = HttpClient::new(HttpClientOptions {
    base_url: Some(base),
    ..Default::default()
  });
  let resp = client.get("/redirect/3", None).await.expect("request ok");
  assert_eq!(resp.status(), 200);
  assert_eq!(resp.text().expect("utf8"), "done");
}

#[tokio::test]
async fn max_redirects_zero_does_not_follow() {
  let base = spawn_server();
  let client = HttpClient::new(HttpClientOptions {
    base_url: Some(base),
    ..Default::default()
  });
  // Pinned to 0: the 302 itself must come back, not the followed body.
  let resp = client
    .get("/redirect/3", Some(opts(Some(0))))
    .await
    .expect("request ok");
  assert_eq!(resp.status(), 302, "0 redirects must return the 3xx as-is");
  assert_ne!(resp.text().unwrap_or_default(), "done");
}

#[tokio::test]
async fn max_redirects_limit_exceeded_errors() {
  let base = spawn_server();
  let client = HttpClient::new(HttpClientOptions {
    base_url: Some(base),
    ..Default::default()
  });
  // 3-hop chain, cap of 2 → exceeded → error (proves the cap binds).
  let err = client.get("/redirect/3", Some(opts(Some(2)))).await;
  assert!(err.is_err(), "exceeding the per-request cap must error, got {err:?}");

  // Same client, generous cap → succeeds (proves it is per-request,
  // and that the cached per-limit clients are independent).
  let ok = client
    .get("/redirect/3", Some(opts(Some(5))))
    .await
    .expect("within cap");
  assert_eq!(ok.status(), 200);
  assert_eq!(ok.text().expect("utf8"), "done");
}

#[tokio::test]
async fn cookie_jar_is_shared_across_redirect_policy_clients() {
  let base = spawn_server();
  let client = HttpClient::new(HttpClientOptions {
    base_url: Some(base),
    ..Default::default()
  });
  // First call uses the max_redirects=0 client (a distinct reqwest
  // Client); it must store the Set-Cookie into the shared jar.
  let set = client.get("/set", Some(opts(Some(0)))).await.expect("set ok");
  assert_eq!(set.text().expect("utf8"), "set");
  // Second call uses the default-policy client; if the jar were not
  // shared it would send no cookie.
  let echo = client.get("/echo", None).await.expect("echo ok");
  assert_eq!(
    echo.text().expect("utf8"),
    "sid=abc",
    "session cookie must persist across per-redirect-policy clients"
  );
}
