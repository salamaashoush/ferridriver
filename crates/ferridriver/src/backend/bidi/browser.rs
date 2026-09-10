//! `BiDi` browser -- manages contexts, pages, and browser lifecycle.

use serde_json::json;
use std::sync::Arc;
use tracing::debug;

use super::page::BidiPage;
use super::session::BidiSession;
use crate::backend::{AnyPage, NavLifecycle};
use crate::error::{FerriError, Result};

#[derive(Clone)]
/// Browser instance using the `WebDriver` `BiDi` protocol.
pub struct BidiBrowser {
  pub(crate) session: Arc<BidiSession>,
  child: Arc<tokio::sync::Mutex<Option<crate::backend::process::ChildGroup>>>,
  /// Popup announcement subscriptions (see
  /// [`crate::backend::PopupInfo`]) fed by the connect-time
  /// `browsingContext.contextCreated` listener.
  popup_taps: crate::backend::PopupTaps,
  /// Owned Firefox `--profile` directory for launched browsers. Removed
  /// by `close()`, or by the last handle drop if nobody closed. `None`
  /// for `connect()` — we don't own the profile of a browser someone
  /// else launched.
  profile_dir: Option<Arc<crate::backend::async_tempdir::AsyncTempDir>>,
  /// One downloads directory per browser, shared by every page. Per
  /// page it cost a mkdir on each open and leaked a directory whenever
  /// teardown was skipped.
  downloads_dir: Arc<tempfile::TempDir>,
}

impl BidiBrowser {
  /// Real browser version reported in the `BiDi` session capabilities at
  /// `session.new` time. Format is `"{browserName}/{browserVersion}"` to
  /// match the CDP `Browser.getVersion().product` shape (e.g.
  /// `"firefox/135.0.1"`).
  #[must_use]
  pub fn version(&self) -> String {
    format!("{}/{}", self.session.browser_name, self.session.browser_version)
  }

  async fn wait_for_context_event(&self, method: &str, context_id: &str, timeout: std::time::Duration) -> Result<()> {
    let mut rx = self.session.transport.tap_events();
    let wait_for_event = async {
      while let Some(event) = rx.recv().await {
        if event.method != method {
          continue;
        }
        if event.params.get("context").and_then(|v| v.as_str()) == Some(context_id) {
          return Ok(());
        }
      }
      Err(FerriError::Backend(format!(
        "BiDi event stream closed while waiting for {method}"
      )))
    };
    tokio::time::timeout(timeout, wait_for_event).await.map_err(|_| {
      FerriError::timeout(
        format!("BiDi event '{method}' on {context_id}"),
        u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
      )
    })?
  }

  async fn list_context_ids_for_user_context(&self, user_context_id: &str) -> Result<Vec<String>> {
    let result = self
      .session
      .transport
      .send_command("browsingContext.getTree", json!({}))
      .await?;
    let contexts = result
      .get("contexts")
      .and_then(|v| v.as_array())
      .ok_or_else(|| FerriError::protocol("browsingContext.getTree", "missing contexts array"))?;

    Ok(
      contexts
        .iter()
        .filter(|ctx| ctx.get("userContext").and_then(|v| v.as_str()) == Some(user_context_id))
        .filter_map(|ctx| ctx.get("context").and_then(|v| v.as_str()).map(ToOwned::to_owned))
        .collect(),
    )
  }

  /// Launch a browser with `BiDi` support.
  /// Auto-detects Firefox vs Chrome from the binary path.
  ///
  /// `user_data_dir` selects a persistent profile ferridriver does not
  /// own; without it a throwaway profile is created and removed on
  /// teardown.
  pub async fn launch_with_flags(
    browser_path: &str,
    flags: &[String],
    env: &rustc_hash::FxHashMap<String, String>,
    user_data_dir: Option<&std::path::Path>,
    proxy: Option<&crate::options::ProxyConfig>,
  ) -> Result<Self> {
    // Determine if headless from flags
    let headless = flags.iter().any(|f| f == "--headless");
    let (session, child, profile) = Box::pin(BidiSession::launch(
      browser_path,
      flags,
      headless,
      env,
      user_data_dir,
      proxy,
    ))
    .await?;
    let session = Arc::new(session);
    let downloads_dir = new_downloads_dir()?;
    let popup_taps = Self::spawn_popup_listener(&session, &downloads_dir);
    let owns_profile = profile.owned.is_some();
    let mut group = crate::backend::process::ChildGroup::recorded(child, Some(&profile.path), owns_profile);
    group.own_dir(downloads_dir.path());
    Ok(Self {
      session,
      child: Arc::new(tokio::sync::Mutex::new(Some(group))),
      popup_taps,
      // A persistent profile is the caller's, so nothing here may delete
      // it — that is the whole point of asking for one.
      profile_dir: profile
        .owned
        .map(|dir| Arc::new(crate::backend::async_tempdir::AsyncTempDir::new(dir))),
      downloads_dir,
    })
  }

  /// Connect to an existing `BiDi` endpoint via WebSocket.
  pub async fn connect(ws_url: &str) -> Result<Self> {
    let session = Arc::new(Box::pin(BidiSession::connect(ws_url)).await?);
    Self::from_session(session)
  }

  /// Create a `BiDi` browser from a `WebDriver` Classic HTTP endpoint.
  ///
  /// The endpoint creates a W3C session with `webSocketUrl: true`, then the
  /// returned `BiDi` connection carries all browser operations. This is the
  /// low-latency path for `WebDriver` servers that implement `BiDi`, including
  /// compatible Appium and Safari Technology Preview sessions.
  pub async fn connect_webdriver(
    endpoint: &str,
    browser_name: &str,
    extra_capabilities: Option<&serde_json::Value>,
    headers: Option<&rustc_hash::FxHashMap<String, String>>,
    timeout_ms: Option<u64>,
  ) -> Result<Self> {
    let session_url = webdriver_session_url(endpoint)?;
    let always_match = webdriver_capabilities(browser_name, extra_capabilities);
    let body = serde_json::json!({
      "capabilities": {
        "alwaysMatch": always_match
      }
    });
    let mut client = reqwest::Client::builder();
    if let Some(timeout_ms) = timeout_ms {
      client = client.timeout(std::time::Duration::from_millis(timeout_ms));
    }
    if let Some(headers) = headers {
      let mut request_headers = reqwest::header::HeaderMap::new();
      for (name, value) in headers {
        let name = reqwest::header::HeaderName::try_from(name)
          .map_err(|e| FerriError::invalid_argument("headers", format!("invalid header name '{name}': {e}")))?;
        let value = reqwest::header::HeaderValue::try_from(value)
          .map_err(|e| FerriError::invalid_argument("headers", format!("invalid value for '{name}': {e}")))?;
        request_headers.insert(name, value);
      }
      client = client.default_headers(request_headers);
    }
    let response = client
      .build()
      .map_err(|e| FerriError::Backend(format!("WebDriver HTTP client setup failed: {e}")))?
      .post(session_url.as_str())
      .json(&body)
      .send()
      .await
      .map_err(|e| FerriError::Backend(format!("WebDriver session request failed: {e}")))?;
    let status = response.status();
    let payload = response
      .json::<serde_json::Value>()
      .await
      .map_err(|e| FerriError::Backend(format!("WebDriver session response was not JSON: {e}")))?;
    if !status.is_success() {
      return Err(FerriError::Backend(format!(
        "WebDriver session request returned {status}: {payload}"
      )));
    }
    let value = payload.get("value").unwrap_or(&payload);
    let session_id = value
      .get("sessionId")
      .or_else(|| payload.get("sessionId"))
      .and_then(serde_json::Value::as_str)
      .ok_or_else(|| FerriError::protocol("WebDriver /session", "response omitted sessionId"))?
      .to_string();
    let capabilities = value.get("capabilities").cloned().unwrap_or_else(|| value.clone());
    let ws_url = capabilities
      .get("webSocketUrl")
      .and_then(serde_json::Value::as_str)
      .ok_or_else(|| {
        FerriError::unsupported("WebDriver server created a Classic session without a BiDi webSocketUrl capability")
      })?
      .to_string();
    let session = Arc::new(BidiSession::connect_existing(&ws_url, session_id, capabilities).await?);
    Self::from_session(session)
  }

  fn from_session(session: Arc<BidiSession>) -> Result<Self> {
    let downloads_dir = new_downloads_dir()?;
    let popup_taps = Self::spawn_popup_listener(&session, &downloads_dir);
    Ok(Self {
      session,
      child: Arc::new(tokio::sync::Mutex::new(None)),
      popup_taps,
      profile_dir: None,
      downloads_dir,
    })
  }

  pub(crate) fn popup_taps(&self) -> crate::backend::PopupTaps {
    Arc::clone(&self.popup_taps)
  }

  /// Watch `browsingContext.contextCreated` for TOP-LEVEL contexts the
  /// browser created on its own — `window.open` / `target=_blank`
  /// popups carry `originalOpener` (the opener's navigable id), while
  /// contexts our `new_page` creates via `browsingContext.create`
  /// never do. Mirrors Playwright's
  /// `bidiBrowser.ts::_onBrowsingContextCreated`, which resolves the
  /// opener from the same field.
  fn spawn_popup_listener(
    session: &Arc<BidiSession>,
    downloads_dir: &Arc<tempfile::TempDir>,
  ) -> crate::backend::PopupTaps {
    let taps: crate::backend::PopupTaps = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut rx = session.transport.tap_events();
    let session = Arc::clone(session);
    let downloads_dir = Arc::clone(downloads_dir);
    let listener_taps = Arc::clone(&taps);
    tokio::spawn(async move {
      while let Some(event) = rx.recv().await {
        if event.method != "browsingContext.contextCreated" {
          continue;
        }
        let is_child = event.params.get("parent").and_then(|v| v.as_str()).is_some();
        let Some(opener) = event.params.get("originalOpener").and_then(|v| v.as_str()) else {
          continue;
        };
        if is_child {
          continue;
        }
        let Some(context_id) = event.params.get("context").and_then(|v| v.as_str()) else {
          continue;
        };
        let user_context = event
          .params
          .get("userContext")
          .and_then(|v| v.as_str())
          .unwrap_or("default")
          .to_string();
        let page = BidiPage::create(
          session.clone(),
          context_id.to_string(),
          Some(&user_context),
          Arc::clone(&downloads_dir),
        );
        // Claimed inline: BidiPage::create is pure construction (no
        // wire round-trip), and readiness is the pump's business.
        let delivered = crate::backend::push_popup(
          &listener_taps,
          crate::backend::PopupInfo {
            page: crate::backend::AnyPage::Bidi(page),
            browser_context_id: if user_context == "default" {
              None
            } else {
              Some(user_context)
            },
            opener_target_id: Some(opener.to_string()),
          },
        );
        if !delivered {
          debug!("popup {context_id} observed with no pump subscribed");
        }
      }
    });
    taps
  }

  /// Create a new isolated user context. `proxy` is wired via
  /// `browser.createUserContext({ proxy })` — `BiDi`'s proxy shape
  /// matches `WebDriver`'s capabilities (`proxyType: 'manual', httpProxy,
  /// sslProxy, socksProxy, socksVersion, noProxy`). For the common
  /// `http://host:port` / `socks5://host:port` input we decompose into
  /// the equivalent `BiDi` shape.
  pub async fn new_context(&self, proxy: Option<&crate::options::ProxyConfig>) -> Result<String> {
    let mut params = json!({});
    if let Some(p) = proxy {
      params["proxy"] = bidi_proxy_capability(p);
    }
    let result = self
      .session
      .transport
      .send_command("browser.createUserContext", params)
      .await?;
    result
      .get("userContext")
      .and_then(|v| v.as_str())
      .map(ToOwned::to_owned)
      .ok_or_else(|| FerriError::protocol("browser.createUserContext", "missing userContext id"))
  }

  /// Dispose an isolated user context.
  pub async fn dispose_context(&self, user_context_id: &str) -> Result<()> {
    let context_ids = self
      .list_context_ids_for_user_context(user_context_id)
      .await
      .unwrap_or_default();
    let mut waiters = Vec::with_capacity(context_ids.len());
    for context_id in &context_ids {
      waiters.push(self.wait_for_context_event(
        "browsingContext.contextDestroyed",
        context_id,
        std::time::Duration::from_secs(2),
      ));
    }
    self
      .session
      .transport
      .send_command("browser.removeUserContext", json!({"userContext": user_context_id}))
      .await?;
    for waiter in waiters {
      let _ = waiter.await;
    }
    Ok(())
  }

  /// List all open pages (top-level browsing contexts).
  pub async fn pages(&self) -> Result<Vec<AnyPage>> {
    let result = self
      .session
      .transport
      .send_command("browsingContext.getTree", json!({}))
      .await?;
    let contexts = result
      .get("contexts")
      .and_then(|v| v.as_array())
      .ok_or_else(|| FerriError::protocol("browsingContext.getTree", "missing contexts array"))?;

    let mut pages = Vec::with_capacity(contexts.len());
    for ctx in contexts {
      let context_id = ctx
        .get("context")
        .and_then(|v| v.as_str())
        .ok_or_else(|| FerriError::protocol("browsingContext.getTree", "context missing 'context' field"))?;
      let user_context = ctx.get("userContext").and_then(|v| v.as_str());
      pages.push(AnyPage::Bidi(BidiPage::create(
        self.session.clone(),
        context_id.to_string(),
        user_context,
        Arc::clone(&self.downloads_dir),
      )));
    }
    Ok(pages)
  }

  /// Create a new page (tab) and optionally navigate.
  pub async fn new_page(
    &self,
    url: &str,
    user_context_id: Option<&str>,
    viewport: Option<&crate::options::ViewportConfig>,
  ) -> Result<AnyPage> {
    let mut params = json!({"type": "window"});
    if let Some(user_context_id) = user_context_id {
      params["userContext"] = json!(user_context_id);
    }
    let mut rx = self.session.transport.tap_events();
    let result = self
      .session
      .transport
      .send_command("browsingContext.create", params)
      .await?;
    let context_id = result
      .get("context")
      .and_then(|v| v.as_str())
      .ok_or_else(|| FerriError::protocol("browsingContext.create", "missing context id"))?
      .to_string();

    let wait_for_created = async {
      while let Some(event) = rx.recv().await {
        if event.method != "browsingContext.contextCreated" {
          continue;
        }
        if event.params.get("context").and_then(|v| v.as_str()) == Some(&context_id) {
          return Ok::<(), String>(());
        }
      }
      Err("BiDi event stream closed while waiting for browsingContext.contextCreated".to_string())
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), wait_for_created).await;

    debug!("BiDi new page: context={context_id}");
    let page = BidiPage::create(
      self.session.clone(),
      context_id,
      user_context_id,
      Arc::clone(&self.downloads_dir),
    );
    page.wait_until_ready().await?;

    if let Some(viewport) = viewport {
      page.emulate_viewport(viewport).await?;
    }

    if !url.is_empty() && url != "about:blank" {
      page.goto(url, NavLifecycle::Load, 30_000, None).await?;
    }

    Ok(AnyPage::Bidi(page))
  }

  /// Close the browser.
  ///
  /// Caller-owned profiles flush through `browser.close`; throwaway
  /// profiles can be killed directly because they are discarded.
  pub async fn close(&mut self) -> Result<()> {
    if let Some(mut group) = self.child.lock().await.take() {
      let mut flushed = true;
      if self.profile_dir.is_none() {
        let timeout = std::time::Duration::from_secs(5);
        let _ = tokio::time::timeout(timeout, self.session.transport.send_command("browser.close", json!({}))).await;
        flushed = group.wait_for_exit(timeout).await;
      }
      // Flag the transport first: the SIGKILLed Firefox never sends a
      // WebSocket close frame, and without the flag the reader logs the
      // resulting TCP reset as a spurious WARN on every teardown.
      self.session.transport.start_close();
      // Group kill first (helpers die with the parent), then reap so
      // the enclosing runtime carries no zombie.
      group.shutdown().await;
      if !flushed {
        return Err(FerriError::Backend(
          "Firefox did not close cleanly while flushing its persistent profile".into(),
        ));
      }
    }
    if let Some(dir) = self.profile_dir.as_ref() {
      dir.remove_now().await;
    }
    Ok(())
  }

  /// Whether the launched Firefox is still running. `None` when this
  /// handle connected to a browser it did not launch.
  pub(crate) fn child_is_running(&self) -> Option<bool> {
    let mut guard = self.child.try_lock().ok()?;
    guard.as_mut().map(crate::backend::process::ChildGroup::is_running)
  }
}

fn webdriver_session_url(endpoint: &str) -> Result<reqwest::Url> {
  let mut url = reqwest::Url::parse(endpoint)
    .map_err(|e| FerriError::invalid_argument("endpoint", format!("invalid WebDriver endpoint: {e}")))?;
  if !url.path().ends_with("/session") {
    let mut path = url.path().trim_end_matches('/').to_string();
    path.push_str("/session");
    url.set_path(&path);
  }
  Ok(url)
}

fn webdriver_capabilities(browser_name: &str, extra: Option<&serde_json::Value>) -> serde_json::Value {
  let mut always_match = serde_json::json!({
    "browserName": browser_name,
    "acceptInsecureCerts": true,
    "webSocketUrl": true,
    "unhandledPromptBehavior": "ignore"
  });
  if let Some(extra) = extra.and_then(serde_json::Value::as_object)
    && let Some(target) = always_match.as_object_mut()
  {
    target.extend(extra.iter().map(|(key, value)| (key.clone(), value.clone())));
  }
  always_match
}

/// A Playwright-shaped proxy as the `BiDi`/`WebDriver` `proxy` capability.
///
/// The same shape serves `session.new` (browser-wide, from
/// `launch({ proxy })`) and `browser.createUserContext` (per context).
pub(crate) fn bidi_proxy_capability(proxy: &crate::options::ProxyConfig) -> serde_json::Value {
  let (proxy_type, host_port, is_socks, socks_version) = parse_bidi_proxy(&proxy.server);
  let mut capability = json!({ "proxyType": proxy_type });

  if is_socks {
    capability["socksProxy"] = json!(host_port);
    if let Some(version) = socks_version {
      capability["socksVersion"] = json!(version);
    }
  } else {
    capability["httpProxy"] = json!(host_port);
    capability["sslProxy"] = json!(host_port);
  }

  if let Some(ref bypass) = proxy.bypass {
    // WebDriver noProxy is an array of host strings.
    let list: Vec<&str> = bypass.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    capability["noProxy"] = json!(list);
  }

  capability
}

/// Decompose a Playwright-shaped proxy `server` string into the
/// BiDi/WebDriver capability fields. Returns `(proxyType, host_port,
/// is_socks, socks_version)`.
fn parse_bidi_proxy(server: &str) -> (&'static str, String, bool, Option<i64>) {
  let (scheme, rest) = server.split_once("://").unwrap_or(("", server));
  let host_port = rest.to_string();
  match scheme {
    "socks5" => ("manual", host_port, true, Some(5)),
    "socks4" => ("manual", host_port, true, Some(4)),
    _ => ("manual", host_port, false, None),
  }
}

/// One temp downloads directory per browser. `Playwright.setDownloadBehavior`
/// equivalents are per browser too, and download filenames are unique.
fn new_downloads_dir() -> Result<Arc<tempfile::TempDir>> {
  Ok(Arc::new(
    tempfile::Builder::new()
      .prefix("ferridriver-downloads-")
      .tempdir()
      .map_err(|e| FerriError::backend(format!("downloads tempdir: {e}")))?,
  ))
}

#[cfg(test)]
mod proxy_capability_tests {
  use super::bidi_proxy_capability;
  use crate::options::ProxyConfig;

  #[test]
  fn an_http_proxy_becomes_a_manual_webdriver_capability() {
    let capability = bidi_proxy_capability(&ProxyConfig {
      server: "http://127.0.0.1:3052".to_string(),
      bypass: Some("127.0.0.1, localhost".to_string()),
      username: None,
      password: None,
    });

    assert_eq!(capability["proxyType"], "manual");
    // WebDriver wants host:port, not a URL, and the same value for both schemes.
    assert_eq!(capability["httpProxy"], "127.0.0.1:3052");
    assert_eq!(capability["sslProxy"], "127.0.0.1:3052");
    assert_eq!(capability["noProxy"], serde_json::json!(["127.0.0.1", "localhost"]));
  }

  #[test]
  fn a_socks_proxy_carries_its_version() {
    let capability = bidi_proxy_capability(&ProxyConfig {
      server: "socks5://127.0.0.1:1080".to_string(),
      bypass: None,
      username: None,
      password: None,
    });

    assert_eq!(capability["socksProxy"], "127.0.0.1:1080");
    assert_eq!(capability["socksVersion"], 5);
    assert!(
      capability.get("httpProxy").is_none(),
      "a socks proxy is not an http one"
    );
  }
}

#[cfg(test)]
mod webdriver_url_tests {
  use super::{BidiBrowser, webdriver_capabilities, webdriver_session_url};

  #[test]
  fn appends_session_to_server_root() {
    assert_eq!(
      webdriver_session_url("http://127.0.0.1:4444/").unwrap().as_str(),
      "http://127.0.0.1:4444/session"
    );
  }

  #[test]
  fn preserves_existing_session_path() {
    assert_eq!(
      webdriver_session_url("http://127.0.0.1:4444/wd/hub/session")
        .unwrap()
        .as_str(),
      "http://127.0.0.1:4444/wd/hub/session"
    );
  }

  #[test]
  fn rejects_invalid_endpoint() {
    assert!(webdriver_session_url("not a url").is_err());
  }

  #[test]
  fn merges_vendor_capabilities_without_dropping_bidi_negotiation() {
    let caps = webdriver_capabilities(
      "safari",
      Some(&serde_json::json!({
        "platformName": "ios",
        "appium:options": { "automationName": "Safari" }
      })),
    );
    assert_eq!(caps["browserName"], "safari");
    assert_eq!(caps["platformName"], "ios");
    assert_eq!(caps["appium:options"]["automationName"], "Safari");
    assert_eq!(caps["webSocketUrl"], true);
  }

  #[tokio::test]
  async fn webdriver_headers_reach_the_session_endpoint() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock WebDriver");
    let address = listener.local_addr().expect("mock address");
    let server = tokio::spawn(async move {
      let (mut socket, _) = listener.accept().await.expect("accept session request");
      let mut request = Vec::new();
      let mut chunk = [0; 1024];
      loop {
        let read = socket.read(&mut chunk).await.expect("read session request");
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") || read == 0 {
          break;
        }
      }
      let body = br#"{"value":{"error":"session not created","message":"test response"}}"#;
      let response = format!(
        "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
      );
      socket.write_all(response.as_bytes()).await.expect("write status");
      socket.write_all(body).await.expect("write body");
      String::from_utf8_lossy(&request).into_owned()
    });

    let mut headers = rustc_hash::FxHashMap::default();
    headers.insert("authorization".to_string(), "Bearer test-token".to_string());
    let result = Box::pin(BidiBrowser::connect_webdriver(
      &format!("http://{address}"),
      "safari",
      Some(&serde_json::json!({"platformName": "ios"})),
      Some(&headers),
      Some(1_000),
    ))
    .await;
    let Err(error) = result else {
      panic!("the mock endpoint rejects the session");
    };
    assert!(error.to_string().contains("500 Internal Server Error"));
    let request = server.await.expect("mock server task");
    assert!(
      request
        .to_ascii_lowercase()
        .contains("authorization: bearer test-token")
    );
    assert!(request.contains("\"platformName\":\"ios\""));
    assert!(request.contains("\"webSocketUrl\":true"));
  }
}
