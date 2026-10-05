use std::sync::Arc;

use serde_json::{Value, json};

use super::page::WebDriverPage;
use super::session::{Command, Target, WebDriverSession};
use crate::backend::process::ChildGroup;
use crate::error::{FerriError, Result};

#[derive(Clone)]
pub struct WebDriverBrowser {
  pub(crate) session: Arc<WebDriverSession>,
  pub(crate) capabilities: Value,
  pub(crate) child: Arc<tokio::sync::Mutex<Option<ChildGroup>>>,
  pub(crate) ios_owner: Option<Arc<crate::ios::IosOwner>>,
  timeout_ms: u64,
  pages: Arc<tokio::sync::Mutex<rustc_hash::FxHashMap<String, WebDriverPage>>>,
  pub(crate) popup_taps: crate::backend::PopupTaps,
  pub(crate) product: String,
  version: String,
}

impl WebDriverBrowser {
  pub async fn launch_safari(env: &rustc_hash::FxHashMap<String, String>, timeout_ms: Option<u64>) -> Result<Self> {
    let timeout_ms = timeout_ms.unwrap_or(30_000);
    let (mut browser, group) = super::launcher::launch_safari(env, timeout_ms, |endpoint| async move {
      Self::connect(&endpoint, "safari", None, None, Some(timeout_ms)).await
    })
    .await?;
    browser.child = group;
    Ok(browser)
  }

  #[must_use]
  pub fn version(&self) -> String {
    self.version.clone()
  }

  pub async fn connect(
    endpoint: &str,
    browser_name: &str,
    capabilities: Option<&Value>,
    headers: Option<&rustc_hash::FxHashMap<String, String>>,
    timeout_ms: Option<u64>,
  ) -> Result<Self> {
    let timeout_ms = timeout_ms.unwrap_or(30_000);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    if capabilities.is_some_and(|value| !value.is_object()) {
      return Err(FerriError::invalid_argument("capabilities", "expected an object"));
    }
    let mut requested = json!({"browserName":browser_name,"unhandledPromptBehavior":"ignore"});
    if let Some(extra) = capabilities.and_then(Value::as_object)
      && let Some(requested) = requested.as_object_mut()
    {
      requested.extend(extra.iter().map(|(key, value)| (key.clone(), value.clone())));
    }
    let client = super::http_client(headers)?;
    let created =
      WebDriverSession::create(client, super::session_url(endpoint)?, requested, timeout_ms, headers).await?;
    let owner = created.session.clone();
    match Self::from_created(created, deadline, timeout_ms).await {
      Ok(browser) => Ok(browser),
      Err(error) => {
        if let Err(cleanup) = owner.close().await {
          return Err(FerriError::backend(format!("{error}; cleanup failed: {cleanup}")));
        }
        Err(error)
      },
    }
  }

  pub(crate) async fn from_created(
    created: super::session::CreatedSession,
    deadline: tokio::time::Instant,
    timeout_ms: u64,
  ) -> Result<Self> {
    let Some(name) = created.capabilities["browserName"]
      .as_str()
      .filter(|name| !name.is_empty())
    else {
      return Err(FerriError::protocol(
        "WebDriver capabilities",
        "response omitted browserName",
      ));
    };
    let discovery = browser_version(&created, name, timeout_ms);
    let version = if timeout_ms == 0 {
      discovery.await
    } else {
      tokio::time::timeout_at(deadline, discovery)
        .await
        .unwrap_or_else(|_| Err(FerriError::timeout("discovering WebDriver browser version", timeout_ms)))
    };
    let version = version?;
    let product = format!("{name}/{version}");
    Ok(Self {
      session: created.session,
      capabilities: created.capabilities,
      child: Arc::new(tokio::sync::Mutex::new(None)),
      ios_owner: None,
      timeout_ms: 30_000,
      pages: Arc::default(),
      popup_taps: Arc::default(),
      product,
      version,
    })
  }

  pub async fn pages(&self) -> Result<Vec<WebDriverPage>> {
    let mut pages = self.pages.lock().await;
    let handles = self
      .session
      .execute(Target::default(), Command::get(&["window", "handles"]), self.timeout_ms)
      .await?;
    let handles = handles
      .as_array()
      .ok_or_else(|| FerriError::protocol("WebDriver window handles", "expected an array"))?
      .iter()
      .map(|handle| {
        let handle = handle
          .as_str()
          .ok_or_else(|| FerriError::protocol("WebDriver window handles", "expected a string handle"))?;
        Ok(handle)
      })
      .collect::<Result<Vec<_>>>()?;
    pages.retain(|handle, page| {
      if handles.contains(&handle.as_str()) {
        return true;
      }
      page.dispose_local();
      false
    });
    Ok(
      handles
        .into_iter()
        .map(|handle| {
          pages
            .entry(handle.to_owned())
            .or_insert_with(|| self.page(handle))
            .clone()
        })
        .collect(),
    )
  }

  pub async fn new_page(&self, url: &str) -> Result<WebDriverPage> {
    let browser = self.clone();
    let url = url.to_owned();
    let pending = tokio::spawn(async move { browser.create_page(&url).await.map(PendingPage::new) })
      .await
      .map_err(|error| FerriError::backend(format!("Creating WebDriver page failed: {error}")))??;
    pending.take()
  }

  async fn create_page(&self, url: &str) -> Result<WebDriverPage> {
    let target = self
      .pages()
      .await?
      .first()
      .map(|page| page.target.clone())
      .ok_or_else(|| FerriError::target_closed(Some("WebDriver session has no open windows".into())))?;
    let mut pages = self.pages.lock().await;
    let result = self
      .session
      .execute(
        target,
        Command::post(&["window", "new"], json!({"type":"tab"})),
        self.timeout_ms,
      )
      .await?;
    let handle = result
      .get("handle")
      .and_then(Value::as_str)
      .ok_or_else(|| FerriError::protocol("WebDriver new window", "response omitted handle"))?;
    let page = self.page(handle);
    pages.insert(handle.to_owned(), page.clone());
    if url != "about:blank"
      && let Err(error) = page.navigate(url, self.timeout_ms).await
    {
      if let Err(cleanup) = page.close().await {
        return Err(FerriError::backend(format!(
          "Creating WebDriver page failed: {error}; closing its window failed: {cleanup}"
        )));
      }
      pages.remove(handle);
      return Err(error);
    }
    Ok(page)
  }

  fn page(&self, handle: &str) -> WebDriverPage {
    let mut page = WebDriverPage::new(
      self.session.clone(),
      Target {
        appium_context: super::capabilities::uses_xcuitest(&self.capabilities)
          .then(|| format!("WEBVIEW_{}", handle.trim_start_matches("WEBVIEW_"))),
        window: Some(handle.to_owned()),
        ..Default::default()
      },
      self.timeout_ms,
    );
    page.capabilities = Arc::new(self.capabilities.clone());
    page
  }

  pub async fn close(&self) -> Result<()> {
    let browser = self.clone();
    tokio::spawn(async move { browser.close_owned().await })
      .await
      .map_err(|error| FerriError::backend(format!("Closing WebDriver browser failed: {error}")))?
  }

  async fn close_owned(&self) -> Result<()> {
    self.session.close().await?;
    for (_, page) in self.pages.lock().await.drain() {
      page.dispose_local();
    }
    let mut child = self.child.lock().await;
    if let Some(group) = child.as_mut() {
      group.shutdown().await?;
      child.take();
    }
    drop(child);
    if let Some(owner) = &self.ios_owner
      && let Err(cleanup) = owner.close().await
    {
      return Err(FerriError::backend(format!("Simulator cleanup failed: {cleanup}")));
    }
    Ok(())
  }
}

struct PendingPage(Option<WebDriverPage>);

impl PendingPage {
  fn new(page: WebDriverPage) -> Self {
    Self(Some(page))
  }

  fn take(mut self) -> Result<WebDriverPage> {
    self
      .0
      .take()
      .ok_or_else(|| FerriError::backend("New WebDriver page was already consumed"))
  }
}

impl Drop for PendingPage {
  fn drop(&mut self) {
    if let Some(page) = self.0.take() {
      // A cancelled JoinHandle drops its eventual result, including when the
      // driver created the window after the original caller disappeared.
      tokio::spawn(async move {
        if let Err(error) = page.close().await {
          tracing::warn!(%error, "Closing an unclaimed WebDriver page failed");
        }
      });
    }
  }
}

async fn browser_version(created: &super::session::CreatedSession, name: &str, timeout_ms: u64) -> Result<String> {
  if let Some(version) = created.capabilities["browserVersion"]
    .as_str()
    .filter(|v| !v.is_empty())
  {
    return Ok(version.to_owned());
  }
  if name.eq_ignore_ascii_case("safari") && super::capabilities::uses_xcuitest(&created.capabilities) {
    let agent = created
      .session
      .execute(
        Target::default(),
        Command::post(
          &["execute", "sync"],
          json!({"script":"return navigator.userAgent;", "args":[]}),
        ),
        timeout_ms,
      )
      .await?;
    if let Some(version) = agent
      .as_str()
      .and_then(|agent| agent.split_whitespace().find_map(|part| part.strip_prefix("Version/")))
      .filter(|v| !v.is_empty())
    {
      return Ok(version.to_owned());
    }
  }
  Err(FerriError::protocol(
    "WebDriver capabilities",
    "response omitted browserVersion and no browser version could be discovered",
  ))
}

#[cfg(test)]
mod tests {
  use super::*;
  use tokio::io::AsyncWriteExt;

  #[cfg(unix)]
  #[tokio::test]
  async fn failed_session_deletion_retains_the_owned_driver_for_retry() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
      let mut requests = Vec::new();
      for (status, value) in [
        (
          "200 OK",
          json!({"sessionId":"owned","capabilities":{"browserName":"safari","browserVersion":"26.2"}}),
        ),
        (
          "500 Internal Server Error",
          json!({"error":"unknown error","message":"provider cleanup unavailable"}),
        ),
        ("200 OK", Value::Null),
      ] {
        let (mut socket, _) = listener.accept().await.unwrap();
        requests.push(super::super::session::tests::request(&mut socket).await.0);
        let body = json!({"value":value}).to_string();
        socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
      }
      requests
    });
    let browser = WebDriverBrowser::connect(&endpoint, "safari", None, None, Some(1000))
      .await
      .unwrap();
    let child = tokio::process::Command::new("sleep")
      .arg("60")
      .process_group(0)
      .kill_on_drop(true)
      .spawn()
      .unwrap();
    *browser.child.lock().await = Some(ChildGroup::new(child));
    let error = browser.close().await.unwrap_err();
    let driver_retained = browser.child.lock().await.as_mut().is_some_and(ChildGroup::is_running);
    browser.close().await.unwrap();
    assert!(error.to_string().contains("provider cleanup unavailable"));
    assert!(
      driver_retained,
      "failed DELETE disposed the transport needed by the retry"
    );
    assert!(browser.child.lock().await.is_none());
    assert_eq!(
      server.await.unwrap(),
      [
        "POST /session HTTP/1.1",
        "DELETE /session/owned HTTP/1.1",
        "DELETE /session/owned HTTP/1.1",
      ]
    );
  }

  #[tokio::test]
  async fn navigation_updates_synchronous_urls_before_returning() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
      for value in [
        json!({"sessionId":"navigation","capabilities":{"browserName":"safari","browserVersion":"26.2"}}),
        json!(["initial"]),
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        json!({"name":"","url":"https://example.com/redirected?query=1#fragment","state":"complete"}),
        Value::Null,
      ] {
        let (mut socket, _) = listener.accept().await.unwrap();
        super::super::session::tests::request(&mut socket).await;
        let encoded = json!({"value":value}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{encoded}",encoded.len()).as_bytes()).await.unwrap();
      }
    });
    let browser = WebDriverBrowser::connect(&endpoint, "safari", None, None, Some(1000))
      .await
      .unwrap();
    let page = crate::Page::new(crate::backend::AnyPage::WebDriver(
      browser.pages().await.unwrap().remove(0),
    ));
    page
      .inner
      .goto(
        "https://example.com/requested",
        crate::backend::NavLifecycle::Load,
        1000,
        None,
      )
      .await
      .unwrap();
    assert_eq!(page.url(), "https://example.com/redirected?query=1#fragment");
    assert_eq!(page.main_frame().url(), page.url());
    browser.close().await.unwrap();
    server.await.unwrap();
  }

  async fn unclaimed_page_case(cancel: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let cleaned = Arc::new(tokio::sync::Notify::new());
    let signals = (started.clone(), release.clone(), cleaned.clone());
    let server = tokio::spawn(async move {
      let mut selected = String::new();
      let mut deleted = Vec::new();
      loop {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (request, body) = super::super::session::tests::request(&mut socket).await;
        let mut status = "200 OK";
        let value = match request.as_str() {
          "POST /session HTTP/1.1" => {
            json!({"sessionId":"owned","capabilities":{"browserName":"safari","browserVersion":"26.2"}})
          },
          "GET /session/owned/window/handles HTTP/1.1" => json!(["initial"]),
          "POST /session/owned/window HTTP/1.1" => {
            selected = body["handle"].as_str().unwrap().to_owned();
            Value::Null
          },
          "POST /session/owned/frame HTTP/1.1"
          | "POST /session/owned/timeouts HTTP/1.1"
          | "DELETE /session/owned HTTP/1.1" => Value::Null,
          "POST /session/owned/window/new HTTP/1.1" => {
            signals.0.notify_one();
            signals.1.notified().await;
            json!({"handle":"new","type":"tab"})
          },
          "POST /session/owned/url HTTP/1.1" => {
            assert_eq!(selected, "new");
            status = "500 Internal Server Error";
            json!({"error":"timeout","message":"navigation failed"})
          },
          "DELETE /session/owned/window HTTP/1.1" => {
            deleted.push(selected.clone());
            json!(["initial"])
          },
          _ => panic!("unexpected request {request}"),
        };
        let encoded = json!({"value":value}).to_string();
        socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{encoded}",encoded.len()).as_bytes()).await.unwrap();
        if request == "DELETE /session/owned/window HTTP/1.1" {
          signals.2.notify_one();
        }
        if request == "DELETE /session/owned HTTP/1.1" {
          return deleted;
        }
      }
    });
    let browser = WebDriverBrowser::connect(&endpoint, "safari", None, None, Some(1000))
      .await
      .unwrap();
    let worker = browser.clone();
    let caller = tokio::spawn(async move {
      worker
        .new_page(if cancel { "about:blank" } else { "https://example.com" })
        .await
    });
    started.notified().await;
    if cancel {
      caller.abort();
      assert!(matches!(caller.await, Err(error) if error.is_cancelled()));
    } else {
      release.notify_one();
      assert!(matches!(caller.await.unwrap(), Err(FerriError::Timeout { .. })));
    }
    release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(2), cleaned.notified())
      .await
      .unwrap();
    browser.close().await.unwrap();
    assert_eq!(server.await.unwrap(), ["new"]);
  }

  #[tokio::test]
  async fn cancelled_page_creation_closes_the_eventual_window() {
    unclaimed_page_case(true).await;
  }

  #[tokio::test]
  async fn failed_initial_navigation_closes_only_the_created_window() {
    unclaimed_page_case(false).await;
  }

  #[tokio::test]
  async fn xcuitest_discovers_version_and_preserves_async_results_and_errors() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
      let mut requests = Vec::new();
      for value in [
        json!({"sessionId":"xcuitest","capabilities":{"browserName":"Safari","platformName":"iOS","automationName":"XCUITest"}}),
        json!("Mozilla/5.0 Version/26.2 Mobile/15E148 Safari/604.1"),
        Value::Null,
        json!({"ok":true,"value":{"answer":42}}),
        Value::Null,
        json!({"ok":false,"error":"Error: rejected promise"}),
        Value::Null,
      ] {
        let (mut socket, _) = listener.accept().await.unwrap();
        requests.push(super::super::session::tests::request(&mut socket).await);
        let body = json!({"value":value}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
      }
      requests
    });
    let browser = WebDriverBrowser::connect(&endpoint, "safari", None, None, Some(1000))
      .await
      .unwrap();
    assert_eq!(browser.version(), "26.2");
    assert!(browser.capabilities.get("browserVersion").is_none());
    let mut page = browser.page("device");
    page.target = Target::default();
    assert!(!page.supports_window_resize());
    assert_eq!(
      page
        .execute_script("return Promise.resolve(arguments[0]);", vec![json!({"answer":42})])
        .await
        .unwrap(),
      json!({"answer":42})
    );
    let error = page
      .evaluate("Promise.reject(new Error('rejected promise'))")
      .await
      .unwrap_err();
    assert!(matches!(error, FerriError::Evaluation(_)));
    page.press_modifiers(&[]).await.unwrap();
    page.release_modifiers(&[]).await.unwrap();
    browser.close().await.unwrap();
    let requests = server.await.unwrap();
    assert_eq!(requests[1].0, "POST /session/xcuitest/execute/sync HTTP/1.1");
    assert_eq!(requests[2].1, json!({"script":30000}));
    assert_eq!(requests[3].0, "POST /session/xcuitest/execute/async HTTP/1.1");
    assert_eq!(requests[3].1["args"], json!([{"answer":42}]));
    assert_eq!(requests[6].0, "DELETE /session/xcuitest HTTP/1.1");
  }

  #[tokio::test]
  async fn page_enumeration_shares_closure_and_events_across_handles() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
      let mut requests = Vec::new();
      for value in [
        json!({"sessionId":"pages","capabilities":{"browserName":"safari","browserVersion":"26.6.1"}}),
        json!(["first", "second"]),
        json!(["first", "second"]),
        Value::Null,
        Value::Null,
        json!(["second"]),
        json!([]),
        json!(["remaining"]),
        Value::Null,
        Value::Null,
        json!({"handle":"third","type":"tab"}),
        json!(["third"]),
        Value::Null,
      ] {
        let (mut socket, _) = listener.accept().await.unwrap();
        requests.push(super::super::session::tests::request(&mut socket).await);
        let body = json!({"value":value}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
      }
      requests
    });
    let browser = WebDriverBrowser::connect(&endpoint, "safari", None, None, Some(1000))
      .await
      .unwrap();
    let initial = browser.pages().await.unwrap();
    let repeated = browser.clone().pages().await.unwrap();
    let closed = initial[0]
      .events
      .wait_for(|event| matches!(event, crate::events::PageEvent::Close), 1000);
    repeated[0].close().await.unwrap();
    closed.await.unwrap();
    assert!(initial[0].is_closed());
    assert!(!initial[1].is_closed());
    initial[0].close().await.unwrap();
    assert!(initial[0].title().await.unwrap_err().is_target_closed_error());
    assert!(
      initial[0]
        .navigate("https://example.com", 1000)
        .await
        .unwrap_err()
        .is_target_closed_error()
    );
    let disappeared = initial[1]
      .events
      .wait_for(|event| matches!(event, crate::events::PageEvent::Close), 1000);
    assert!(browser.pages().await.unwrap().is_empty());
    disappeared.await.unwrap();
    assert!(repeated[1].is_closed());
    let created = browser.new_page("about:blank").await.unwrap();
    let enumerated = browser.pages().await.unwrap();
    let shutdown = created
      .events
      .wait_for(|event| matches!(event, crate::events::PageEvent::Close), 1000);
    browser.close().await.unwrap();
    shutdown.await.unwrap();
    assert!(enumerated[0].is_closed());
    let requests = server.await.unwrap();
    assert_eq!(requests[5].0, "DELETE /session/pages/window HTTP/1.1");
    assert_eq!(requests[8].0, "POST /session/pages/window HTTP/1.1");
    assert_eq!(requests[8].1, json!({"handle":"remaining"}));
    assert_eq!(requests[10].0, "POST /session/pages/window/new HTTP/1.1");
    assert_eq!(requests[12].0, "DELETE /session/pages HTTP/1.1");
  }

  #[tokio::test]
  async fn connection_timeout_does_not_limit_later_commands() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
      for (delay, value) in [
        (
          0,
          json!({"sessionId":"timeout-session","capabilities":{"browserName":"safari","browserVersion":"26.6.1"}}),
        ),
        (200, json!("completed")),
        (0, Value::Null),
      ] {
        let (mut socket, _) = listener.accept().await.unwrap();
        super::super::session::tests::request(&mut socket).await;
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        let body = json!({"value":value}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
      }
    });
    let browser = WebDriverBrowser::connect(&endpoint, "safari", None, None, Some(100))
      .await
      .unwrap();
    let result = browser
      .session
      .execute(Target::default(), Command::get(&["title"]), 1000)
      .await;
    browser.close().await.unwrap();
    server.await.unwrap();
    assert_eq!(result.unwrap(), json!("completed"));
  }

  #[tokio::test]
  async fn classic_session_preserves_capabilities_targeting_and_cleanup() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/wd/hub", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
      let responses = [
        json!({"sessionId":"safari-session","capabilities":{"browserName":"safari","browserVersion":"26.6.1"}}),
        json!(["safari-window"]),
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        json!("Safari form"),
        Value::Null,
      ];
      let mut requests = Vec::new();
      for value in responses {
        let (mut socket, _) = listener.accept().await.unwrap();
        requests.push(super::super::session::tests::request(&mut socket).await);
        let body = json!({"value":value}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
      }
      requests
    });
    let browser = WebDriverBrowser::connect(
      &endpoint,
      "safari",
      Some(&json!({"bst:options":{"os":"OS X"}})),
      None,
      Some(2000),
    )
    .await
    .unwrap();
    assert_eq!(browser.version(), "26.6.1");
    let pages = browser.pages().await.unwrap();
    assert_eq!(pages.len(), 1);
    pages[0].navigate("https://example.com/form", 1500).await.unwrap();
    assert_eq!(pages[0].title().await.unwrap().as_deref(), Some("Safari form"));
    browser.close().await.unwrap();
    let requests = server.await.unwrap();
    assert_eq!(requests[0].0, "POST /wd/hub/session HTTP/1.1");
    let capabilities = &requests[0].1["capabilities"]["alwaysMatch"];
    assert_eq!(capabilities["browserName"], "safari");
    assert_eq!(capabilities["bst:options"]["os"], "OS X");
    assert!(capabilities.get("webSocketUrl").is_none());
    assert!(capabilities.get("safari:experimentalWebSocketUrl").is_none());
    assert_eq!(requests[2].1, json!({"handle":"safari-window"}));
    assert_eq!(requests[3].1, json!({"id":null}));
    assert_eq!(requests[4].1, json!({"pageLoad":1500}));
    assert_eq!(requests[5].1, json!({"url":"https://example.com/form"}));
    assert_eq!(requests[9].0, "DELETE /wd/hub/session/safari-session HTTP/1.1");
  }
}
