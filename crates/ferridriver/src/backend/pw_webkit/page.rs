//! Playwright `WebKit` page handle. Wraps a [`super::PageProxySession`]
//! and the inner [`super::TargetSession`] reached via the
//! `Target.targetCreated` event.
//!
//! Lifecycle:
//!
//! 1. `Browser::create_page` returns a freshly-registered
//!    `PageProxySession`. The session has no inner target yet — the
//!    `Target.targetCreated` event has not arrived.
//! 2. [`Page::attach`] subscribes to events on the page-proxy session,
//!    waits for the first non-provisional `Target.targetCreated`
//!    payload, opens a [`super::TargetSession`] for that `targetId`,
//!    and initializes it with `Page.enable` / `Runtime.enable` /
//!    `Network.enable` / `Console.enable` / `Page.createUserWorld`
//!    (mirrors `WKPage._initializeSessionMayThrow`).
//! 3. The caller now has a fully wired page and can issue `navigate`,
//!    `evaluate`, etc.

use super::browser::{Browser, BrowserError};
use super::connection::{PageProxySession, TargetSession};
use super::protocol::{self, Envelope, NavigateParams, NavigateResult};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;

/// Name of the utility execution context Playwright registers per
/// frame. Mirrors `UTILITY_WORLD_NAME` in `wkPage.ts`.
pub const UTILITY_WORLD_NAME: &str = "__playwright_utility_world__";

pub struct Page {
  proxy: PageProxySession,
  target: TargetSession,
  proxy_id: String,
  /// Cached so `Browser::create_page` can hand the page out without
  /// the caller needing to keep a separate reference to the browser
  /// for context-id lookups.
  context_id: Option<String>,
  /// The cloned connection — used for `Playwright.navigate` calls
  /// which go through the root browser session, not the target.
  browser_session: super::connection::BrowserSession,
}

impl Page {
  /// Attach to a freshly-created page proxy. Waits for the inner
  /// `Target.targetCreated` event, opens the target session, then
  /// runs the standard set of `*.enable` initialisations.
  ///
  /// `context_id` is purely informational here — it lets the caller
  /// associate this page with a `BrowserContext` later, but the
  /// proxy itself is already bound to the right context inside the
  /// child process.
  pub async fn attach(
    browser: &Browser,
    proxy: PageProxySession,
    context_id: Option<String>,
  ) -> Result<Self, BrowserError> {
    let conn = browser.connection();
    let target_id = wait_for_first_page_target(&proxy).await?;
    let target = conn.open_target(&proxy, target_id);

    // Per WKPage._initializeSessionMayThrow: Page agent must be
    // enabled before Runtime so the executionContextCreated event
    // arrives in the right order.
    target.send("Page.enable", json!({})).await?;
    target.send("Runtime.enable", json!({})).await?;
    target.send("Network.enable", json!({})).await?;
    target.send("Console.enable", json!({})).await?;
    // Best-effort — user-world creation can fail if the page is
    // already torn down (matches PW's `.catch(_ => {})`).
    let _ = target
      .send("Page.createUserWorld", json!({ "name": UTILITY_WORLD_NAME }))
      .await;
    // Optional but cheap: prime the resource tree.
    let _ = target.send("Page.getResourceTree", json!({})).await;

    Ok(Page {
      proxy: proxy.clone(),
      target,
      proxy_id: proxy.page_proxy_id().to_string(),
      context_id,
      browser_session: browser.root().clone(),
    })
  }

  #[must_use]
  pub fn page_proxy_id(&self) -> &str {
    &self.proxy_id
  }

  #[must_use]
  pub fn target(&self) -> &TargetSession {
    &self.target
  }

  #[must_use]
  pub fn proxy(&self) -> &PageProxySession {
    &self.proxy
  }

  #[must_use]
  pub fn context_id(&self) -> Option<&str> {
    self.context_id.as_deref()
  }

  /// Navigate the page to `url`. Goes through `Playwright.navigate`
  /// on the root browser session — per `WKPage.navigateFrame` — and
  /// blocks until the `Page.loadEventFired` event for the resulting
  /// `loaderId` arrives on the inner target session.
  pub async fn navigate(&self, url: &str, referrer: Option<&str>) -> Result<Option<String>, BrowserError> {
    let params = NavigateParams {
      url: url.to_string(),
      page_proxy_id: self.proxy_id.clone(),
      frame_id: None,
      referrer: referrer.map(str::to_string),
    };
    let resp = self
      .browser_session
      .send(protocol::PLAYWRIGHT_NAVIGATE, serde_json::to_value(&params)?)
      .await?;
    let parsed: NavigateResult = serde_json::from_value(resp).unwrap_or_default();
    if let Some(ref loader_id) = parsed.loader_id {
      wait_for_load(&self.target, loader_id).await?;
    }
    Ok(parsed.loader_id)
  }

  /// Evaluate `expression` in the main world of the page's main
  /// frame. Returns the deserialized value (PW's `Runtime.evaluate`
  /// reply with `returnByValue: true`).
  pub async fn evaluate(&self, expression: &str) -> Result<Value, BrowserError> {
    let resp = self
      .target
      .send(
        protocol::RUNTIME_EVALUATE,
        json!({
          "expression": expression,
          "returnByValue": true,
          "awaitPromise": true,
        }),
      )
      .await?;
    Ok(
      resp
        .get("result")
        .and_then(|r| r.get("value"))
        .cloned()
        .unwrap_or(Value::Null),
    )
  }

  /// Capture a base64 PNG screenshot of the current viewport via the
  /// browser-session-level `Playwright.takePageScreenshot`. Bypasses
  /// the inner `Page.snapshotRect` so an unresponsive page is still
  /// observable.
  pub async fn screenshot(&self) -> Result<String, BrowserError> {
    let resp = self
      .browser_session
      .send(
        protocol::PLAYWRIGHT_TAKE_SCREENSHOT,
        json!({
          "pageProxyId": self.proxy_id,
          "x": 0, "y": 0, "width": 0, "height": 0,
          "omitDeviceScaleFactor": true,
        }),
      )
      .await?;
    Ok(
      resp
        .get("dataURL")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string(),
    )
  }

  /// Subscribe to inner-target events (`Page.*`, `Runtime.*`,
  /// `Network.*`, `Console.*`, ...).
  #[must_use]
  pub fn target_events(&self) -> tokio::sync::broadcast::Receiver<Envelope> {
    self.target.events()
  }

  /// Subscribe to page-proxy events (`Target.*`, `Dialog.*`, ...).
  #[must_use]
  pub fn proxy_events(&self) -> tokio::sync::broadcast::Receiver<Envelope> {
    self.proxy.events()
  }

  /// Close the page. Sends `Target.close` on the page proxy.
  /// Bounded: PW's `WebKit` doesn't always reply to `Target.close`
  /// before tearing the target down — wkPage uses `sendMayFail` for
  /// the same reason — so we cap the wait at 2s and treat a timeout
  /// as success.
  pub async fn close(self) -> Result<(), BrowserError> {
    let send = self.proxy.send(
      "Target.close",
      json!({ "targetId": self.target.target_id(), "runBeforeUnload": false }),
    );
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), send).await;
    Ok(())
  }
}

async fn wait_for_first_page_target(proxy: &PageProxySession) -> Result<String, BrowserError> {
  // Subscribe to live events BEFORE draining the buffer so we don't
  // miss an event that lands between drain and subscribe.
  let mut rx = proxy.events();
  for env in proxy.drain_pending() {
    if let Some(id) = page_target_id(&env) {
      return Ok(id);
    }
  }
  loop {
    match rx.recv().await {
      Ok(env) => {
        if let Some(id) = page_target_id(&env) {
          return Ok(id);
        }
      },
      Err(RecvError::Lagged(_)) => {},
      Err(RecvError::Closed) => return Err(BrowserError::Protocol("page proxy closed before target".into())),
    }
  }
}

fn page_target_id(env: &Envelope) -> Option<String> {
  let method = env.method.as_deref()?;
  if method != "Target.targetCreated" {
    return None;
  }
  let info = env.params.get("targetInfo")?;
  let is_provisional = info.get("isProvisional").and_then(Value::as_bool).unwrap_or(false);
  if info.get("type").and_then(Value::as_str) != Some("page") || is_provisional {
    return None;
  }
  Some(info.get("targetId")?.as_str()?.to_string())
}

async fn wait_for_load(target: &TargetSession, _loader_id: &str) -> Result<(), BrowserError> {
  let mut rx = target.events();
  loop {
    match rx.recv().await {
      Ok(env) => {
        let Some(ref method) = env.method else { continue };
        // PW's `Page.frameStoppedLoading` is the close-enough analogue
        // of CDP's `Page.loadEventFired`. The `loaderId` from
        // `Playwright.navigate` isn't directly carried on this event
        // — wkPage matches on its own loaderId tracking. For now we
        // accept the first `loadEventFired` after navigate as the
        // signal, which matches the lifecycle Playwright clients see.
        if method == "Page.loadEventFired" {
          return Ok(());
        }
      },
      Err(RecvError::Lagged(_)) => {},
      Err(RecvError::Closed) => return Err(BrowserError::Protocol("target closed during load".into())),
    }
  }
}

/// Convenience helper used by [`Browser::create_page`] callers that
/// want a fully-attached [`Page`] in one shot.
pub async fn create_attached(browser: &Browser, context_id: Option<&str>) -> Result<Page, BrowserError> {
  let proxy = browser.create_page(context_id).await?;
  Page::attach(browser, proxy, context_id.map(str::to_string)).await
}

/// Helper Arc wrapper for sharing across tasks without cloning the
/// underlying sessions.
pub type PageRef = Arc<Page>;
