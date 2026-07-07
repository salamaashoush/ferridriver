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
  /// Owned Firefox `--profile` directory for launched browsers. Held as
  /// `Arc<TempDir>` so cheap `Clone`s share ownership; the directory is
  /// removed when the last handle drops. `None` for `connect()` — we don't
  /// own the profile of a browser someone else launched.
  #[allow(
    dead_code,
    reason = "held so TempDir::Drop removes the profile dir on last Arc release"
  )]
  profile_dir: Option<Arc<tempfile::TempDir>>,
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
    let mut rx = self.session.transport.subscribe_events();
    let wait_for_event = async {
      while let Some(event) = crate::events::recv_tolerant(&mut rx).await {
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
  pub async fn launch_with_flags(browser_path: &str, flags: &[String]) -> Result<Self> {
    // Determine if headless from flags
    let headless = flags.iter().any(|f| f == "--headless");
    let (session, child, profile_dir) = Box::pin(BidiSession::launch(browser_path, flags, headless)).await?;
    Ok(Self {
      session: Arc::new(session),
      child: Arc::new(tokio::sync::Mutex::new(Some(crate::backend::process::ChildGroup::new(
        child,
      )))),
      profile_dir: Some(Arc::new(profile_dir)),
    })
  }

  /// Connect to an existing `BiDi` endpoint via WebSocket.
  pub async fn connect(ws_url: &str) -> Result<Self> {
    let session = BidiSession::connect(ws_url).await?;
    Ok(Self {
      session: Arc::new(session),
      child: Arc::new(tokio::sync::Mutex::new(None)),
      profile_dir: None,
    })
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
      // Parse the server URL into BiDi's proxy capability fields.
      let (proxy_type, host_port, is_socks, socks_version) = parse_bidi_proxy(&p.server);
      let mut bidi_proxy = json!({ "proxyType": proxy_type });
      if is_socks {
        bidi_proxy["socksProxy"] = json!(host_port);
        if let Some(v) = socks_version {
          bidi_proxy["socksVersion"] = json!(v);
        }
      } else {
        bidi_proxy["httpProxy"] = json!(host_port);
        bidi_proxy["sslProxy"] = json!(host_port);
      }
      if let Some(ref bypass) = p.bypass {
        // WebDriver noProxy is an array of host strings.
        let list: Vec<&str> = bypass.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
        bidi_proxy["noProxy"] = json!(list);
      }
      params["proxy"] = bidi_proxy;
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
      pages.push(AnyPage::Bidi(BidiPage::create(
        self.session.clone(),
        context_id.to_string(),
      )?));
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
    let mut rx = self.session.transport.subscribe_events();
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
      while let Some(event) = crate::events::recv_tolerant(&mut rx).await {
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
    let page = BidiPage::create(self.session.clone(), context_id)?;
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
  /// SIGKILLs firefox directly via the held `ChildGroup`. The
  /// graceful `browser.close` `BiDi` command is intentionally skipped —
  /// for test runs the user-data-dir tempdir is removed regardless,
  /// and waiting for the `BiDi` response is wasted wall-clock. Mirrors
  /// the `CdpBrowser::close` fast-path.
  pub async fn close(&mut self) -> Result<()> {
    if let Some(mut group) = self.child.lock().await.take() {
      // Group kill first (helpers die with the parent), then reap so
      // the enclosing runtime carries no zombie.
      group.shutdown().await;
    }
    Ok(())
  }
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
