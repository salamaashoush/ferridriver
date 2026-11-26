//! Unit tests for `HttpClient` and `HttpResponse`.

use super::response::resource_timing;
use super::*;

#[cfg(test)]
mod response_tests {
  use super::*;

  fn cookie(url: Option<&str>, domain: &str, path: &str) -> crate::backend::CookieData {
    crate::backend::CookieData {
      name: "a".into(),
      value: "1".into(),
      domain: domain.into(),
      path: path.into(),
      secure: false,
      http_only: false,
      expires: None,
      same_site: None,
      url: url.map(str::to_string),
    }
  }

  #[test]
  fn a_cookie_url_becomes_its_host_directory_and_scheme() {
    let rewritten = |url: &str| {
      let cookie = rewrite_cookie(cookie(Some(url), "", "")).unwrap();
      (cookie.domain, cookie.path, cookie.secure, cookie.url)
    };
    assert_eq!(
      rewritten("https://example.com/app/page.html?q=1"),
      ("example.com".into(), "/app/".into(), true, None)
    );
    assert_eq!(
      rewritten("http://example.com"),
      ("example.com".into(), "/".into(), false, None)
    );
    assert_eq!(
      rewritten("http://127.0.0.1:8080/a/b/"),
      ("127.0.0.1".into(), "/a/b/".into(), false, None)
    );

    let refused = |cookie| rewrite_cookie(cookie).unwrap_err().to_string();
    assert!(refused(cookie(None, "", "/")).contains("a url or a domain/path pair"));
    assert!(refused(cookie(None, "example.com", "")).contains("a url or a domain/path pair"));
    assert!(refused(cookie(Some("http://example.com/"), "example.com", "")).contains("either url or domain"));
    assert!(refused(cookie(Some("about:blank"), "", "")).contains("Blank page can not have cookie \"a\""));
    assert!(refused(cookie(Some("data:text/plain,x"), "", "")).contains("Data URL page can not have cookie"));
    let mut expired = cookie(None, "example.com", "/");
    expired.expires = Some(-5.0);
    assert!(refused(expired).contains("valid expires"));
    let mut session = cookie(None, "example.com", "/");
    session.expires = Some(-1.0);
    assert!(rewrite_cookie(session).is_ok());

    let stored = stored_cookie(cookie(None, "example.com", "/")).unwrap();
    assert_eq!(
      (stored.expires, stored.same_site),
      (Some(-1.0), Some(crate::backend::SameSite::Lax))
    );
  }

  #[test]
  fn timing_is_milliseconds_from_the_start_with_absent_phases_at_minus_one() {
    let start = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let new_connection = resource_timing(
      &fetch::Timing {
        start_time: start,
        dns_end: Some(Duration::from_millis(3)),
        connect_end: Some(Duration::from_millis(10)),
        response_start: Duration::from_millis(25),
      },
      start + Duration::from_millis(40),
    );
    assert_eq!(
      (
        new_connection.start_time,
        new_connection.domain_lookup_start,
        new_connection.domain_lookup_end,
        new_connection.connect_start,
        new_connection.secure_connection_start,
        new_connection.connect_end,
        new_connection.request_start,
        new_connection.response_start,
        new_connection.response_end,
      ),
      (1_700_000_000_000.0, 0.0, 3.0, 3.0, -1.0, 10.0, 10.0, 25.0, 40.0)
    );

    let reused = resource_timing(
      &fetch::Timing {
        start_time: start,
        dns_end: None,
        connect_end: None,
        response_start: Duration::from_millis(5),
      },
      // A clock that stepped back never reports an end before the head.
      start,
    );
    assert_eq!(
      (
        reused.domain_lookup_start,
        reused.domain_lookup_end,
        reused.connect_start,
        reused.connect_end,
        reused.request_start,
        reused.response_start,
        reused.response_end,
      ),
      (-1.0, -1.0, -1.0, -1.0, 0.0, 5.0, 5.0)
    );

    // An address literal connects without a lookup.
    let literal = resource_timing(
      &fetch::Timing {
        start_time: start,
        dns_end: None,
        connect_end: Some(Duration::from_millis(2)),
        response_start: Duration::from_millis(4),
      },
      start + Duration::from_millis(4),
    );
    assert_eq!(
      (literal.domain_lookup_end, literal.connect_start, literal.connect_end),
      (-1.0, 0.0, 2.0)
    );
  }

  fn response(headers: Vec<(&str, &str)>, body: &[u8]) -> HttpResponse {
    HttpResponse {
      status_code: 200,
      status_text: "OK".into(),
      response_url: "http://example.test/".into(),
      response_headers: fetch::Headers::from_pairs(
        headers
          .into_iter()
          .map(|(k, v)| (k.to_string(), v.to_string()))
          .collect(),
      ),
      body_bytes: bytes::Bytes::copy_from_slice(body),
      server_addr: None,
      redirected: false,
      unfollowed_redirect: false,
      response_type: ResponseType::Basic,
      timing: crate::network::RequestTiming::default(),
      peer_certificate: None,
      disposed: false,
    }
  }

  #[test]
  fn header_combines_duplicates_like_playwright_rawheaders() {
    let r = response(
      vec![
        ("X-Dup", "one"),
        ("Set-Cookie", "a=1"),
        ("x-dup", "two"),
        ("set-cookie", "b=2"),
      ],
      b"",
    );
    assert_eq!(r.header("x-dup").as_deref(), Some("one, two"));
    assert_eq!(r.header("X-DUP").as_deref(), Some("one, two"), "case-insensitive");
    assert_eq!(r.header("set-cookie").as_deref(), Some("a=1\nb=2"));
    assert_eq!(r.header("absent"), None);
  }

  #[test]
  fn headers_array_stays_verbatim_while_headers_object_flattens() {
    let r = response(
      vec![("Content-Type", "text/plain"), ("X-Dup", "one"), ("x-dup", "two")],
      b"",
    );
    assert_eq!(
      r.headers(),
      [
        ("Content-Type".to_string(), "text/plain".to_string()),
        ("X-Dup".to_string(), "one".to_string()),
        ("x-dup".to_string(), "two".to_string()),
      ]
    );
    assert_eq!(
      r.headers_object(),
      vec![
        ("content-type".to_string(), "text/plain".to_string()),
        ("x-dup".to_string(), "one, two".to_string()),
      ]
    );
  }

  #[test]
  fn dispose_releases_the_body_but_keeps_the_metadata() {
    let mut r = response(vec![("content-type", "application/json")], br#"{"a":1}"#);
    assert_eq!(r.text().expect("text before dispose"), r#"{"a":1}"#);

    r.dispose();

    assert!(matches!(r.body(), Err(crate::error::FerriError::Disposed("Response"))));
    assert!(r.text().is_err());
    assert!(r.json_value().is_err());
    assert_eq!(
      r.body().unwrap_err().to_string(),
      "Response has been disposed",
      "message mirrors Playwright's"
    );
    // Metadata survives disposal, as it does in Playwright.
    assert_eq!(r.status(), 200);
    assert_eq!(r.url(), "http://example.test/");
    assert_eq!(r.header("content-type").as_deref(), Some("application/json"));
  }
}

#[cfg(test)]
mod context_bound_tests {
  use super::*;
  use std::io::{Read as _, Write as _};
  use std::sync::Mutex as StdMutex;

  /// In-memory stand-in for the browser context: a cookie store with
  /// browser-like (name, domain, path) replacement semantics.
  struct MockBridge {
    defaults: ContextDefaults,
    cookies: StdMutex<Vec<crate::backend::CookieData>>,
  }

  impl MockBridge {
    fn new() -> Arc<Self> {
      Arc::new(Self {
        defaults: ContextDefaults::default(),
        cookies: StdMutex::new(Vec::new()),
      })
    }

    fn seed(self: &Arc<Self>, cookie: crate::backend::CookieData) {
      self.cookies.lock().unwrap().push(cookie);
    }

    fn cookie_value(&self, name: &str) -> Option<String> {
      self
        .cookies
        .lock()
        .unwrap()
        .iter()
        .find(|c| c.name == name)
        .map(|c| c.value.clone())
    }
  }

  impl ContextBridge for MockBridge {
    fn defaults(&self) -> BridgeFuture<'_, ContextDefaults> {
      let d = self.defaults.clone();
      Box::pin(async move { Ok(d) })
    }

    fn cookies(&self) -> BridgeFuture<'_, Vec<crate::backend::CookieData>> {
      let c = self.cookies.lock().unwrap().clone();
      Box::pin(async move { Ok(c) })
    }

    fn add_cookies(&self, new: Vec<crate::backend::CookieData>) -> BridgeFuture<'_, ()> {
      let mut store = self.cookies.lock().unwrap();
      for cookie in new {
        store.retain(|e| !(e.name == cookie.name && e.domain == cookie.domain && e.path == cookie.path));
        store.push(cookie);
      }
      Box::pin(async move { Ok(()) })
    }
  }

  fn cookie(name: &str, value: &str, domain: &str) -> crate::backend::CookieData {
    crate::backend::CookieData {
      name: name.into(),
      value: value.into(),
      domain: domain.into(),
      path: "/".into(),
      secure: false,
      http_only: false,
      expires: None,
      same_site: None,
      url: None,
    }
  }

  /// One scripted route: status, extra headers, body.
  type RouteResponse = (u16, Vec<(String, String)>, &'static str);

  /// Thread-per-connection test server (a serial accept loop starves
  /// behind browser-style idle preconnections; see CLAUDE.md). Routes:
  /// scripted per test via a match on the request path. Records every
  /// "METHOD path COOKIE:<header> CT:<header> CL:<header>" line.
  fn spawn_server(routes: fn(&str) -> RouteResponse) -> (String, Arc<StdMutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let log: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
    let log_srv = Arc::clone(&log);
    std::thread::spawn(move || {
      for stream in listener.incoming() {
        let Ok(mut stream) = stream else { break };
        let log = Arc::clone(&log_srv);
        std::thread::spawn(move || {
          let mut buf = Vec::new();
          let mut byte = [0u8; 1];
          // Read until end of headers.
          while !buf.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
            buf.push(byte[0]);
          }
          let text = String::from_utf8_lossy(&buf).to_string();
          let mut lines = text.lines();
          let request_line = lines.next().unwrap_or("").to_string();
          let mut method_path = request_line.split(' ');
          let method = method_path.next().unwrap_or("").to_string();
          let path = method_path.next().unwrap_or("").to_string();
          let mut cookie_header = String::new();
          let mut content_type = String::new();
          let mut content_length = 0usize;
          let mut user_agent = String::new();
          let mut x_ctx = String::new();
          for line in lines {
            let Some((k, v)) = line.split_once(':') else { continue };
            match k.to_ascii_lowercase().as_str() {
              "cookie" => cookie_header = v.trim().to_string(),
              "content-type" => content_type = v.trim().to_string(),
              "content-length" => content_length = v.trim().parse().unwrap_or(0),
              "user-agent" => user_agent = v.trim().to_string(),
              "x-ctx" => x_ctx = v.trim().to_string(),
              _ => {},
            }
          }
          let mut body = vec![0u8; content_length];
          if content_length > 0 {
            let _ = stream.read_exact(&mut body);
          }
          log.lock().unwrap().push(format!(
            "{method} {path} COOKIE:{cookie_header} CT:{content_type} CL:{content_length} UA:{user_agent} XCTX:{x_ctx}"
          ));
          let (status, headers, body) = routes(&path);
          let mut resp = format!(
            "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
          );
          for (k, v) in headers {
            use std::fmt::Write as _;
            let _ = write!(resp, "{k}: {v}\r\n");
          }
          resp.push_str("\r\n");
          resp.push_str(body);
          let _ = stream.write_all(resp.as_bytes());
          let _ = stream.shutdown(std::net::Shutdown::Both);
        });
      }
    });
    (format!("http://{addr}"), log)
  }

  /// A server that wants `user:pass` and challenges with `scheme`. The log
  /// holds the `authorization` header of each request, empty when absent.
  fn spawn_auth_server(scheme: &'static str) -> (String, Arc<StdMutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let log: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
    let log_srv = Arc::clone(&log);
    std::thread::spawn(move || {
      for stream in listener.incoming() {
        let Ok(mut stream) = stream else { break };
        let log = Arc::clone(&log_srv);
        std::thread::spawn(move || {
          let mut buf = Vec::new();
          let mut byte = [0u8; 1];
          while !buf.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
            buf.push(byte[0]);
          }
          let text = String::from_utf8_lossy(&buf).to_string();
          let authorization = text
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.trim().to_string())
            .unwrap_or_default();
          log.lock().unwrap().push(authorization.clone());
          // base64("user:pass")
          let resp = if authorization == "Basic dXNlcjpwYXNz" {
            "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nAUTHED".to_string()
          } else {
            format!(
              "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: {scheme} realm=\"x\"\r\nContent-Length: 6\r\nConnection: close\r\n\r\nNOAUTH"
            )
          };
          let _ = stream.write_all(resp.as_bytes());
          let _ = stream.shutdown(std::net::Shutdown::Both);
        });
      }
    });
    (format!("http://{addr}"), log)
  }

  fn credentials(password: &str, origin: Option<&str>, send: Option<HttpCredentialsSend>) -> HttpCredentials {
    HttpCredentials {
      username: "user".into(),
      password: password.into(),
      origin: origin.map(str::to_string),
      send,
    }
  }

  #[test]
  fn fail_on_status_code_lets_redirects_through() {
    assert!(!fails_status(200));
    assert!(!fails_status(302), "3xx is not a failure in Playwright");
    assert!(fails_status(404));
    assert!(fails_status(500));
    assert!(fails_status(101));
  }

  fn client_with(http_credentials: Vec<HttpCredentials>) -> HttpClient {
    HttpClient::new(HttpClientOptions {
      http_credentials,
      ..HttpClientOptions::default()
    })
  }

  #[tokio::test]
  async fn a_basic_challenge_is_answered_once_with_the_matching_credentials() {
    let (base, log) = spawn_auth_server("Basic");
    let client = client_with(vec![
      credentials("elsewhere", Some("http://elsewhere.example.com"), None),
      credentials("pass", Some(&base), None),
    ]);
    let resp = client.get(&format!("{base}/")).await.unwrap();
    assert_eq!((resp.status(), resp.text().unwrap().as_str()), (200, "AUTHED"));
    assert_eq!(*log.lock().unwrap(), ["", "Basic dXNlcjpwYXNz"]);
  }

  #[tokio::test]
  async fn credentials_sent_always_go_out_before_any_challenge() {
    let (base, log) = spawn_auth_server("Basic");
    let client = client_with(vec![credentials("pass", None, Some(HttpCredentialsSend::Always))]);
    let resp = client.get(&format!("{base}/")).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(*log.lock().unwrap(), ["Basic dXNlcjpwYXNz"]);
  }

  #[tokio::test]
  async fn refused_credentials_are_not_tried_again() {
    let (base, log) = spawn_auth_server("Basic");
    let client = client_with(vec![credentials("wrong", None, None)]);
    let resp = client.get(&format!("{base}/")).await.unwrap();
    assert_eq!(resp.status(), 401);
    // base64("user:wrong")
    assert_eq!(*log.lock().unwrap(), ["", "Basic dXNlcjp3cm9uZw=="]);
  }

  #[tokio::test]
  async fn a_challenge_nothing_matches_is_returned_as_it_came() {
    for (scheme, list, headers) in [
      (
        "Basic",
        vec![credentials("pass", Some("http://elsewhere.example.com"), None)],
        None,
      ),
      ("Basic", Vec::new(), None),
      ("Digest", vec![credentials("pass", None, None)], None),
      // The caller's own header is the answer it chose.
      (
        "Basic",
        vec![credentials("pass", None, None)],
        Some(vec![("Authorization".to_string(), "Bearer mine".to_string())]),
      ),
    ] {
      let (base, log) = spawn_auth_server(scheme);
      let sent = headers.as_ref().map_or("", |headers| headers[0].1.as_str()).to_string();
      let options = RequestOptions {
        headers,
        ..RequestOptions::default()
      };
      let resp = client_with(list)
        .get(&format!("{base}/"))
        .options(options)
        .await
        .unwrap();
      assert_eq!(resp.status(), 401, "{scheme}");
      assert_eq!(*log.lock().unwrap(), [sent], "{scheme}");
    }
  }

  #[tokio::test]
  async fn a_context_bound_client_reads_its_credentials_when_challenged() {
    let (base, log) = spawn_auth_server("Basic");
    let current: Arc<StdMutex<Vec<HttpCredentials>>> = Arc::default();
    let read = Arc::clone(&current);
    let client = HttpClient::context_bound(MockBridge::new()).with_live_http_credentials(Arc::new(move || {
      let list = read.lock().unwrap().clone();
      Box::pin(async move { list })
    }));
    assert_eq!(client.get(&format!("{base}/")).await.unwrap().status(), 401);
    *current.lock().unwrap() = vec![credentials("pass", None, None)];
    assert_eq!(client.get(&format!("{base}/")).await.unwrap().status(), 200);
    assert_eq!(*log.lock().unwrap(), ["", "", "Basic dXNlcjpwYXNz"]);
  }

  #[tokio::test]
  async fn bridge_cookie_header_injected() {
    let (base, log) = spawn_server(|_| (200, vec![], "ok"));
    let bridge = MockBridge::new();
    bridge.seed(cookie("sid", "abc", "127.0.0.1"));
    // A cookie for another domain must not leak in.
    bridge.seed(cookie("other", "x", "example.com"));
    let client = HttpClient::context_bound(bridge);
    let resp = client.get(&format!("{base}/echo")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 1);
    assert!(log[0].contains("COOKIE:sid=abc "), "got {:?}", log[0]);
  }

  #[tokio::test]
  async fn set_cookie_written_back_to_bridge() {
    let (base, _log) = spawn_server(|path| {
      if path == "/set" {
        (200, vec![("Set-Cookie".into(), "sid=fresh; Path=/".into())], "ok")
      } else {
        (200, vec![], "ok")
      }
    });
    let bridge = MockBridge::new();
    let client = HttpClient::context_bound(Arc::clone(&bridge) as Arc<dyn ContextBridge>);
    client.get(&format!("{base}/set")).await.unwrap();
    assert_eq!(bridge.cookie_value("sid").as_deref(), Some("fresh"));
  }

  #[tokio::test]
  async fn redirect_hop_set_cookie_captured_and_reinjected() {
    let (base, log) = spawn_server(|path| {
      if path == "/hop" {
        (
          302,
          vec![
            ("Set-Cookie".into(), "hop=1; Path=/".into()),
            ("Location".into(), "/after".into()),
          ],
          "",
        )
      } else {
        (200, vec![], "landed")
      }
    });
    let bridge = MockBridge::new();
    let client = HttpClient::context_bound(Arc::clone(&bridge) as Arc<dyn ContextBridge>);
    let resp = client.get(&format!("{base}/hop")).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().unwrap(), "landed");
    // The redirect hop's Set-Cookie reached the bridge...
    assert_eq!(bridge.cookie_value("hop").as_deref(), Some("1"));
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 2);
    // ...and was already sent on the SECOND hop.
    assert!(log[1].contains("GET /after COOKIE:hop=1 "), "got {:?}", log[1]);
  }

  #[tokio::test]
  async fn post_303_rewrites_to_bodyless_get() {
    let (base, log) = spawn_server(|path| {
      if path == "/submit" {
        (303, vec![("Location".into(), "/done".into())], "")
      } else {
        (200, vec![], "done")
      }
    });
    let bridge = MockBridge::new();
    let client = HttpClient::context_bound(bridge);
    let resp = client
      .post(&format!("{base}/submit"))
      .json(serde_json::json!({"a": 1}))
      .await
      .unwrap();
    assert_eq!(resp.status(), 200);
    let log = log.lock().unwrap();
    assert!(log[0].starts_with("POST /submit"), "got {:?}", log[0]);
    assert!(log[0].contains("CT:application/json"), "got {:?}", log[0]);
    // 303 → GET with the body and its content headers dropped.
    assert!(log[1].starts_with("GET /done"), "got {:?}", log[1]);
    assert!(log[1].contains("CT: CL:0"), "got {:?}", log[1]);
  }

  #[tokio::test]
  async fn explicit_cookie_header_applies_to_first_hop_only() {
    let (base, log) = spawn_server(|path| {
      if path == "/hop" {
        (302, vec![("Location".into(), "/after".into())], "")
      } else {
        (200, vec![], "ok")
      }
    });
    let bridge = MockBridge::new();
    bridge.seed(cookie("ctx", "1", "127.0.0.1"));
    let client = HttpClient::context_bound(Arc::clone(&bridge) as Arc<dyn ContextBridge>);
    client
      .get(&format!("{base}/hop"))
      .header("cookie", "manual=1")
      .await
      .unwrap();
    let log = log.lock().unwrap();
    // First hop: the caller's explicit header, untouched.
    assert!(log[0].contains("COOKIE:manual=1 "), "got {:?}", log[0]);
    // Redirect hop: re-derived from the context (Playwright drops the
    // explicit header on redirects).
    assert!(log[1].contains("COOKIE:ctx=1 "), "got {:?}", log[1]);
  }

  #[tokio::test]
  async fn max_redirects_zero_returns_the_redirect() {
    let (base, _log) = spawn_server(|_| (302, vec![("Location".into(), "/next".into())], ""));
    let client = HttpClient::context_bound(MockBridge::new());
    let resp = client.get(&format!("{base}/hop")).max_redirects(0).await.unwrap();
    assert_eq!(resp.status(), 302);
  }

  #[tokio::test]
  async fn redirect_loop_errors_at_budget() {
    let (base, _log) = spawn_server(|_| (302, vec![("Location".into(), "/again".into())], ""));
    let client = HttpClient::context_bound(MockBridge::new());
    let err = client.get(&format!("{base}/hop")).max_redirects(3).await.unwrap_err();
    assert!(err.to_string().contains("too many redirects"), "got {err}");
  }

  #[tokio::test]
  async fn base_url_and_extra_headers_come_from_bridge_defaults() {
    let (base, log) = spawn_server(|_| (200, vec![], "ok"));
    let bridge = Arc::new(MockBridge {
      defaults: ContextDefaults {
        base_url: Some(base.clone()),
        extra_http_headers: vec![("x-ctx".into(), "live".into())],
        user_agent: Some("FerriUA/9.9".into()),
        ignore_https_errors: false,
      },
      cookies: StdMutex::new(Vec::new()),
    });
    let client = HttpClient::context_bound(bridge);
    let resp = client.get("/relative").await.unwrap();
    assert_eq!(resp.status(), 200);
    let log = log.lock().unwrap();
    assert!(log[0].starts_with("GET /relative"), "got {:?}", log[0]);
    assert!(log[0].contains("UA:FerriUA/9.9"), "got {:?}", log[0]);
    assert!(log[0].contains("XCTX:live"), "got {:?}", log[0]);
  }
}
