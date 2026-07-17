#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Per-request `max_redirects` is honoured (real, not a no-op) by the
//! engine's manual-redirect loop, and the standalone reqwest jar persists
//! session cookies across requests. Browser-free: a tiny std-only HTTP
//! server on loopback.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use ferridriver::http_client::{
  HttpClient, HttpClientOptions, MultipartField, MultipartValue, RedirectMode, RequestOptions,
};

/// Minimal HTTP/1.1 test server. Routes:
/// - `GET /redirect/<n>`: `n>0` → 302 to `/redirect/<n-1>`; `n==0` → 200 "done".
/// - `GET /set`: 200, `Set-Cookie: sid=abc; Path=/`, body "set".
/// - `GET /echo`: 200, body = the received `Cookie` header (or "none").
/// - `POST /body-echo`: 200, body = the request body verbatim.
/// - `GET /ct-echo`: 200, body = the received `Content-Type` header (or "none").
fn spawn_server() -> String {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
  let addr = listener.local_addr().expect("local addr");
  thread::spawn(move || {
    for stream in listener.incoming() {
      let Ok(stream) = stream else { continue };
      thread::spawn(move || handle(stream));
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
  let mut content_type = String::from("none");
  let mut content_length = 0usize;
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
    if let Some(v) = line
      .strip_prefix("Content-Type: ")
      .or_else(|| line.strip_prefix("content-type: "))
    {
      content_type = v.trim().to_string();
    }
    if let Some(v) = line
      .strip_prefix("Content-Length: ")
      .or_else(|| line.strip_prefix("content-length: "))
    {
      content_length = v.trim().parse().unwrap_or(0);
    }
  }

  let mut body_bytes = vec![0u8; content_length];
  if content_length > 0 {
    let _ = reader.read_exact(&mut body_bytes);
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
  } else if path == "/body-echo" {
    http_ok(&String::from_utf8_lossy(&body_bytes), None)
  } else if path == "/ct-echo" {
    http_ok(&content_type, None)
  } else {
    http_ok("ok", None)
  };

  let _ = stream.write_all(response.as_bytes());
  let _ = stream.flush();
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

  // Same client, generous cap → succeeds (proves the cap is per-request,
  // not a client-level policy).
  let ok = client
    .get("/redirect/3", Some(opts(Some(5))))
    .await
    .expect("within cap");
  assert_eq!(ok.status(), 200);
  assert_eq!(ok.text().expect("utf8"), "done");
}

#[tokio::test]
async fn redirect_manual_returns_the_3xx_unfollowed() {
  let base = spawn_server();
  let client = HttpClient::new(HttpClientOptions {
    base_url: Some(base),
    ..Default::default()
  });
  let resp = client
    .get(
      "/redirect/3",
      Some(RequestOptions {
        redirect: RedirectMode::Manual,
        ..Default::default()
      }),
    )
    .await
    .expect("request ok");
  assert_eq!(resp.status(), 302, "manual must return the 3xx, not follow it");
  assert!(resp.unfollowed_redirect(), "manual 3xx must be flagged unfollowed");
  assert!(!resp.redirected(), "no hop was followed under manual");
}

#[tokio::test]
async fn redirect_error_rejects_on_a_3xx() {
  let base = spawn_server();
  let client = HttpClient::new(HttpClientOptions {
    base_url: Some(base),
    ..Default::default()
  });
  let err = client
    .get(
      "/redirect/1",
      Some(RequestOptions {
        redirect: RedirectMode::Error,
        ..Default::default()
      }),
    )
    .await;
  assert!(err.is_err(), "redirect: error must reject on a 3xx, got {err:?}");
}

#[tokio::test]
async fn redirect_follow_marks_redirected() {
  let base = spawn_server();
  let client = HttpClient::new(HttpClientOptions {
    base_url: Some(base),
    ..Default::default()
  });
  let resp = client.get("/redirect/2", None).await.expect("request ok");
  assert_eq!(resp.status(), 200);
  assert_eq!(resp.text().expect("utf8"), "done");
  assert!(resp.redirected(), "following a hop must set redirected");

  // A no-redirect request must NOT be marked redirected (guards against
  // the query-string false-positive: params add a query, not a hop).
  let plain = client
    .get(
      "/echo",
      Some(RequestOptions {
        params: Some(vec![("a".into(), "1".into())]),
        ..Default::default()
      }),
    )
    .await
    .expect("request ok");
  assert!(!plain.redirected(), "appended params must not read as a redirect");
}

#[tokio::test]
async fn multipart_body_serializes_to_form_data() {
  let base = spawn_server();
  let client = HttpClient::new(HttpClientOptions {
    base_url: Some(base),
    ..Default::default()
  });
  let resp = client
    .post(
      "/body-echo",
      Some(RequestOptions {
        multipart: Some(vec![
          MultipartField {
            name: "field".into(),
            value: MultipartValue::Text("hello".into()),
          },
          MultipartField {
            name: "upload".into(),
            value: MultipartValue::File {
              filename: "a.txt".into(),
              content_type: "text/plain".into(),
              bytes: b"file-bytes".to_vec(),
            },
          },
        ]),
        ..Default::default()
      }),
    )
    .await
    .expect("request ok");
  let echoed = resp.text().expect("utf8");
  assert!(
    echoed.contains("Content-Disposition: form-data; name=\"field\""),
    "text part: {echoed}"
  );
  assert!(echoed.contains("hello"), "text value: {echoed}");
  assert!(
    echoed.contains("filename=\"a.txt\"") && echoed.contains("Content-Type: text/plain"),
    "file part headers: {echoed}"
  );
  assert!(echoed.contains("file-bytes"), "file bytes: {echoed}");

  // The multipart boundary content-type reaches the server.
  let ct = client
    .post(
      "/ct-echo",
      Some(RequestOptions {
        multipart: Some(vec![MultipartField {
          name: "x".into(),
          value: MultipartValue::Text("y".into()),
        }]),
        ..Default::default()
      }),
    )
    .await
    .expect("request ok");
  assert!(
    ct.text().expect("utf8").starts_with("multipart/form-data; boundary="),
    "multipart content-type must carry the boundary"
  );
}

#[tokio::test]
async fn cookie_jar_persists_across_requests() {
  let base = spawn_server();
  let client = HttpClient::new(HttpClientOptions {
    base_url: Some(base),
    ..Default::default()
  });
  // First call (pinned to 0 redirects) stores the Set-Cookie into the
  // standalone reqwest jar.
  let set = client.get("/set", Some(opts(Some(0)))).await.expect("set ok");
  assert_eq!(set.text().expect("utf8"), "set");
  // Second call re-sends it from the jar (max_redirects is a per-request
  // loop budget, not a distinct client — the jar is shared).
  let echo = client.get("/echo", None).await.expect("echo ok");
  assert_eq!(
    echo.text().expect("utf8"),
    "sid=abc",
    "session cookie must persist across requests"
  );
}
