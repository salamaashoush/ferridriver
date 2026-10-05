//! Playwright `WebKit` browser handle.
//!
//! Owns the spawned `pw_run.sh` child, the [`Connection`], and the root
//! browser [`Session`]. Launch flow mirrors `WKBrowser`:
//!
//! 1. Spawn `pw_run.sh --inspector-pipe [--headless]` with fd 3/4 wired
//!    to a socketpair pair.
//! 2. `Playwright.enable` handshake on the root session.
//! 3. `Playwright.createContext` / `createPage` per page.
//!
//! Clones share an owner that retains the child, transport and downloads
//! directory through startup failures and retryable cleanup.

use super::connection::{Connection, ConnectionError, Session};
use super::launcher::{LaunchConfig, LaunchError};
use super::page::WebKitPage;
use super::protocol::{self, CreateContextParams, CreateContextResult, CreatePageParams, CreatePageResult};
use super::transport::Transport;
use crate::backend::AnyPage;
use crate::error::{FerriError, Result};
use serde_json::json;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BrowserError {
  #[error("launch: {0}")]
  Launch(#[from] LaunchError),
  #[error("connection: {0}")]
  Connection(#[from] ConnectionError),
  #[error("io: {0}")]
  Io(#[from] std::io::Error),
  #[error("json: {0}")]
  Json(#[from] serde_json::Error),
  #[error("protocol: {0}")]
  Protocol(String),
}

impl From<BrowserError> for FerriError {
  fn from(e: BrowserError) -> Self {
    FerriError::backend(e.to_string())
  }
}

/// Playwright `WebKit` browser. Cloneable; clones share the child + connection.
#[derive(Clone)]
pub struct WebKitBrowser {
  inner: Arc<BrowserInner>,
}

struct BrowserInner {
  conn: Arc<Connection>,
  root: Session,
  handle: Arc<super::owner::WebKitOwner>,
  /// Pages created through this browser. `pages()` snapshots it; the
  /// PW `WebKit` protocol has no page-list RPC.
  pages: Arc<Mutex<Vec<WebKitPage>>>,
  /// Context every page lands in when the caller passes no explicit
  /// `browserContextId`.
  ///
  /// A persistent profile uses the implicit context (no protocol ID).
  /// An ephemeral launch creates an isolated default context explicitly.
  default_context: Option<Arc<str>>,
  /// PW `WebKit` build revision (e.g. `"webkit-playwright/2272"`),
  /// derived from the binary path — a real build identifier, not a
  /// placeholder.
  version: Arc<str>,
  /// Per-context options stash, keyed by `browserContextId`. Populated
  /// by [`WebKitBrowser::new_context_with_options`]; consumed by
  /// [`WebKitPage::attach`] to apply per-page overrides before the
  /// initial document becomes scriptable.
  context_options: Arc<Mutex<rustc_hash::FxHashMap<String, crate::options::BrowserContextOptions>>>,
  /// Directory the browser writes downloads into; download behavior is
  /// per-context on the `WebKit` protocol, so every created context gets
  /// a `Playwright.setDownloadBehavior` pointing here.
  downloads_dir: Arc<std::path::PathBuf>,
  /// Popup announcement subscriptions (see
  /// [`crate::backend::PopupInfo`]) fed by the
  /// `Playwright.pageProxyCreated` listener.
  popup_taps: crate::backend::PopupTaps,
  /// Ownership ledger between `create_page` and the popup listener —
  /// `noopener` windows fire `pageProxyCreated` WITHOUT `openerId`,
  /// exactly like our own `Playwright.createPage` proxies, and the
  /// event can beat the create response (see
  /// [`crate::backend::CreateLedger`]).
  create_ledger: Arc<crate::backend::CreateLedger<ParkedProxy>>,
}

/// A `pageProxyCreated` without `openerId` observed while a
/// `Playwright.createPage` was in flight — held until the flush decides
/// whether it is a `noopener` popup.
#[derive(Clone)]
struct ParkedProxy {
  proxy_id: String,
  context_id: Option<String>,
}

impl WebKitBrowser {
  /// Spawn a Playwright `WebKit` child and complete `Playwright.enable`.
  pub async fn launch(config: &LaunchConfig) -> std::result::Result<Self, BrowserError> {
    let SpawnedBrowser {
      handle,
      conn,
      downloads_dir,
      version,
    } = spawn_browser_process(config)?;
    let root = conn.browser_session();
    let startup_events = config.user_data_dir.as_ref().map(|_| root.events());
    root.send(protocol::PLAYWRIGHT_ENABLE, json!({})).await?;

    let default_context = if config.user_data_dir.is_some() {
      None
    } else {
      let ctx_resp = root
        .send(
          protocol::PLAYWRIGHT_CREATE_CONTEXT,
          serde_json::to_value(CreateContextParams::default())?,
        )
        .await?;
      Some(
        serde_json::from_value::<CreateContextResult>(ctx_resp)
          .map(|r| Arc::<str>::from(r.browser_context_id))
          .map_err(|e| BrowserError::Protocol(format!("default context: {e}")))?,
      )
    };

    let mut download_params = json!({ "behavior": "allow", "downloadPath": downloads_dir.to_string_lossy() });
    if let Some(id) = &default_context {
      download_params["browserContextId"] = json!(id.to_string());
    }
    root.send("Playwright.setDownloadBehavior", download_params).await?;

    let pages: Arc<Mutex<Vec<WebKitPage>>> = Arc::new(Mutex::new(Vec::new()));
    let download_events = root.events();

    let browser = WebKitBrowser {
      inner: Arc::new(BrowserInner {
        conn,
        root,
        handle,
        pages,
        default_context,
        version,
        context_options: Arc::new(Mutex::new(rustc_hash::FxHashMap::default())),
        downloads_dir,
        popup_taps: Arc::new(Mutex::new(Vec::new())),
        create_ledger: Arc::new(crate::backend::CreateLedger::default()),
      }),
    };
    if let Some(mut events) = startup_events {
      let proxy_id = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(event) = events.recv().await {
          if event.method.as_deref() == Some("Playwright.pageProxyCreated") {
            return event
              .params
              .get("pageProxyId")
              .and_then(serde_json::Value::as_str)
              .map(String::from);
          }
        }
        None
      })
      .await
      .ok()
      .flatten()
      .ok_or_else(|| BrowserError::Protocol("persistent context did not open a startup page".into()))?;
      let page = WebKitPage::attach(&browser, browser.inner.conn.page_proxy_session(proxy_id), None, false).await?;
      browser
        .inner
        .pages
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(page);
    }
    let (shutdown, stopping) = tokio::sync::watch::channel(false);
    let downloads = spawn_download_listener(
      &browser.inner.root,
      download_events,
      browser.inner.pages.clone(),
      browser.inner.downloads_dir.clone(),
      stopping.clone(),
    );
    let popups = browser.spawn_popup_listener(stopping);
    browser
      .inner
      .handle
      .install_observers(crate::backend::transport_tasks::TransportTasks::new(
        shutdown,
        vec![downloads, popups],
      ));
    Ok(browser)
  }

  pub(crate) fn popup_taps(&self) -> crate::backend::PopupTaps {
    Arc::clone(&self.inner.popup_taps)
  }

  /// Watch `Playwright.pageProxyCreated` for pages the BROWSER opened.
  /// `openerId` is only set for `window.open()` pages (protocol doc),
  /// so our own `Playwright.createPage` proxies — which fire the same
  /// event without it — are left to the `new_page` flow. Mirrors
  /// `wkBrowser.ts::_onPageProxyCreated`, which resolves the opener
  /// from the same field.
  fn spawn_popup_listener(&self, mut stopping: tokio::sync::watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
    let mut rx = self.inner.root.events();
    let browser = Arc::downgrade(&self.inner);
    let mut flush_rx = self.inner.create_ledger.take_flush_rx();
    tokio::spawn(async move {
      let mut popups = tokio::task::JoinSet::new();
      loop {
        let env = tokio::select! {
          biased;
          _ = stopping.wait_for(|stopping| *stopping) => break,
          result = popups.join_next(), if !popups.is_empty() => {
            if let Some(Err(error)) = result {
              tracing::warn!(%error, "WebKit popup attachment task failed");
            }
            continue;
          },
          flushed = flush_rx.recv() => {
            if flushed.is_none() { break; }
            let Some(inner) = browser.upgrade() else { break };
            let browser = WebKitBrowser { inner };
            for parked in browser.inner.create_ledger.take_unowned_parked() {
              popups.spawn(browser.claim_popup_proxy(parked.proxy_id, parked.context_id, None));
            }
            continue;
          },
          env = rx.recv() => match env { Some(env) => env, None => break },
        };
        let Some(inner) = browser.upgrade() else { break };
        let browser = WebKitBrowser { inner };
        if env.method.as_deref() != Some("Playwright.pageProxyCreated") {
          continue;
        }
        let Some(proxy_id) = env.params.get("pageProxyId").and_then(serde_json::Value::as_str) else {
          continue;
        };
        let opener = env
          .params
          .get("openerId")
          .and_then(serde_json::Value::as_str)
          .map(String::from);
        let context_id = env
          .params
          .get("browserContextId")
          .and_then(serde_json::Value::as_str)
          .map(String::from);
        if opener.is_none() {
          let parked = ParkedProxy {
            proxy_id: proxy_id.to_string(),
            context_id: context_id.clone(),
          };
          if !browser.inner.create_ledger.try_claim_ownerless(proxy_id, parked) {
            continue;
          }
        }
        popups.spawn(browser.claim_popup_proxy(proxy_id.to_string(), context_id, opener));
      }
      popups.abort_all();
      while let Some(result) = popups.join_next().await {
        if let Err(error) = result
          && !error.is_cancelled()
        {
          tracing::warn!(%error, "WebKit popup attachment task failed during close");
        }
      }
    })
  }

  /// Attach one browser-created page proxy as a popup and announce it.
  /// Spawned off the listener loop — attach is a multi-command
  /// round-trip and the loop must keep consuming root events.
  fn claim_popup_proxy(
    &self,
    proxy_id: String,
    context_id: Option<String>,
    opener: Option<String>,
  ) -> impl std::future::Future<Output = ()> + Send + 'static {
    let proxy = self.inner.conn.page_proxy_session(proxy_id.clone());
    let attach = WebKitPage::attach(self, proxy, context_id.clone(), true);
    let pages = Arc::clone(&self.inner.pages);
    let taps = Arc::clone(&self.inner.popup_taps);
    let default_context = self.inner.default_context.clone();
    async move {
      match attach.await {
        Ok(page) => {
          if let Ok(mut pages) = pages.lock() {
            pages.push(page.clone());
          }
          // The launch-minted default context is "default" at the
          // state layer; popups in it carry no context id upward.
          let browser_context_id = context_id.filter(|id| Some(id.as_str()) != default_context.as_deref());
          let delivered = crate::backend::push_popup(
            &taps,
            crate::backend::PopupInfo {
              page: AnyPage::WebKit(page.clone()),
              browser_context_id,
              opener_target_id: opener,
            },
          );
          if !delivered {
            tracing::debug!("webkit popup {proxy_id} observed with no pump subscribed");
            let _ = page.resume_popup().await;
          }
        },
        Err(e) => {
          tracing::debug!("webkit popup attach failed for {proxy_id}: {e}");
        },
      }
    }
  }

  #[must_use]
  pub fn version(&self) -> String {
    self.inner.version.to_string()
  }

  #[must_use]
  pub fn root(&self) -> &Session {
    &self.inner.root
  }

  #[must_use]
  pub fn connection(&self) -> &Arc<Connection> {
    &self.inner.conn
  }

  /// Create an ephemeral browser context with proxy-only options.
  /// Equivalent to [`Self::new_context_with_options`] with the full
  /// options bag stripped to just the proxy field — kept for state.rs's
  /// legacy `new_context(None)` callsite.
  pub async fn new_context(&self, proxy: Option<&crate::options::ProxyConfig>) -> Result<String> {
    let mut params = CreateContextParams::default();
    if let Some(p) = proxy {
      params.proxy_server = Some(p.server.clone());
      params.proxy_bypass_list = p.bypass.clone();
    }
    let resp = self
      .inner
      .root
      .send(protocol::PLAYWRIGHT_CREATE_CONTEXT, serde_json::to_value(&params)?)
      .await
      .map_err(BrowserError::from)?;
    let parsed: CreateContextResult =
      serde_json::from_value(resp).map_err(|e| FerriError::protocol("Playwright.createContext", e.to_string()))?;
    // Download behavior is per-context on the WebKit protocol
    // (Playwright's WKBrowserContext.initialize sends it for every
    // context) — without it, downloads in a fresh context are denied
    // and terminate as 'cancelled'.
    self
      .inner
      .root
      .send(
        "Playwright.setDownloadBehavior",
        json!({
          "behavior": "allow",
          "downloadPath": self.inner.downloads_dir.to_string_lossy(),
          "browserContextId": parsed.browser_context_id.clone(),
        }),
      )
      .await
      .map_err(BrowserError::from)?;
    Ok(parsed.browser_context_id)
  }

  /// Create a context with the full `BrowserContextOptions` bag.
  ///
  /// Sends `Playwright.createContext` for the proxy fields, then
  /// `Playwright.setLanguages` if `locale` is set (mirroring
  /// `WKBrowserContext.initialize`), then stashes the options so
  /// [`Self::new_page`] / [`WebKitPage::attach`] can apply per-page
  /// overrides (userAgent, timezone, JS-disabled, bypassCSP, offline,
  /// permissions, extraHTTPHeaders) on the target session BEFORE the
  /// initial about:blank document becomes scriptable.
  pub async fn new_context_with_options(
    &self,
    options: Option<&crate::options::BrowserContextOptions>,
  ) -> Result<String> {
    let proxy = options.and_then(|o| o.proxy.as_ref());
    let ctx_id = self.new_context(proxy).await?;
    if let Some(opts) = options {
      // `new_context` opened this one with `allow`, which is the
      // default; a context that refuses downloads re-sends the
      // per-context command. WebKit then reports a refused download as
      // a cancelled one, and the download listener turns that into
      // Playwright's wording.
      if opts.accept_downloads == Some(false) {
        self
          .inner
          .root
          .send(
            "Playwright.setDownloadBehavior",
            json!({
              "behavior": "deny",
              "downloadPath": self.inner.downloads_dir.to_string_lossy(),
              "browserContextId": ctx_id.clone(),
            }),
          )
          .await
          .map_err(BrowserError::from)?;
      }
      if let Some(locale) = opts.locale.as_deref() {
        let _ = self
          .inner
          .root
          .send(
            "Playwright.setLanguages",
            json!({ "browserContextId": ctx_id.clone(), "languages": [locale] }),
          )
          .await;
      }
      self
        .inner
        .context_options
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(ctx_id.clone(), opts.clone());
    }
    Ok(ctx_id)
  }

  /// Look up stashed [`BrowserContextOptions`] for a context id. Used
  /// by [`WebKitPage::attach`] to apply per-page overrides before the
  /// initial document loads.
  pub(crate) fn context_options_lookup(
    &self,
  ) -> impl Fn(&str) -> Option<crate::options::BrowserContextOptions> + Send + 'static {
    let options = Arc::clone(&self.inner.context_options);
    move |ctx_id| {
      options
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(ctx_id)
        .cloned()
    }
  }

  /// Delete a context. `Playwright.deleteContext`.
  pub async fn dispose_context(&self, browser_context_id: &str) -> Result<()> {
    self
      .inner
      .root
      .send(
        protocol::PLAYWRIGHT_DELETE_CONTEXT,
        json!({ "browserContextId": browser_context_id }),
      )
      .await
      .map_err(BrowserError::from)?;
    Ok(())
  }

  /// `Playwright.createPage` → returns the registered page-proxy [`Session`].
  /// Falls back to the default context when no explicit one is given.
  pub async fn create_page(&self, browser_context_id: Option<&str>) -> Result<Session> {
    let params = CreatePageParams {
      browser_context_id: browser_context_id
        .or(self.inner.default_context.as_deref())
        .map(String::from),
    };
    // Ledger bracket: `pageProxyCreated` for this proxy can arrive
    // before the response; while the create is in flight the popup
    // listener parks no-opener proxies instead of claiming them.
    self.inner.create_ledger.begin_create();
    let resp = self
      .inner
      .root
      .send(protocol::PLAYWRIGHT_CREATE_PAGE, serde_json::to_value(&params)?)
      .await
      .map_err(BrowserError::from);
    let parsed: Result<CreatePageResult> = resp.map_err(FerriError::from).and_then(|r| {
      serde_json::from_value(r).map_err(|e| FerriError::protocol("Playwright.createPage", e.to_string()))
    });
    match parsed {
      Ok(parsed) => {
        self.inner.create_ledger.end_create(Some(&parsed.page_proxy_id));
        Ok(self.inner.conn.page_proxy_session(parsed.page_proxy_id))
      },
      Err(e) => {
        self.inner.create_ledger.end_create(None);
        Err(e)
      },
    }
  }

  /// List all open pages.
  pub async fn pages(&self) -> Result<Vec<AnyPage>> {
    tokio::task::yield_now().await;
    Ok(
      self
        .inner
        .pages
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|p| !p.is_closed())
        .cloned()
        .map(AnyPage::WebKit)
        .collect(),
    )
  }

  /// Create a new page, attach it, and optionally navigate.
  pub async fn new_page(
    &self,
    url: &str,
    browser_context_id: Option<&str>,
    viewport: Option<&crate::options::ViewportConfig>,
  ) -> Result<AnyPage> {
    let proxy = self.create_page(browser_context_id).await?;
    let resolved_ctx = browser_context_id
      .or(self.inner.default_context.as_deref())
      .map(String::from);
    let page = WebKitPage::attach(self, proxy, resolved_ctx, false).await?;
    if let Some(vp) = viewport {
      page.emulate_viewport(vp).await?;
    }
    if !url.is_empty() && url != "about:blank" {
      page.goto(url, crate::backend::NavLifecycle::Load, 30_000, None).await?;
    }
    self
      .inner
      .pages
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(page.clone());
    Ok(AnyPage::WebKit(page))
  }

  /// Issue `Playwright.close` and reap the child.
  pub async fn close(&mut self) -> Result<()> {
    self.inner.handle.close().await
  }

  pub(crate) fn child_is_running(&self) -> bool {
    self.inner.handle.is_running()
  }

  /// `Playwright.getInfo` — `{ os }`.
  pub async fn info(&self) -> Result<serde_json::Value> {
    self
      .inner
      .root
      .send(protocol::PLAYWRIGHT_GET_INFO, json!({}))
      .await
      .map_err(|e| BrowserError::from(e).into())
  }
}

struct SpawnedBrowser {
  version: Arc<str>,
  handle: Arc<super::owner::WebKitOwner>,
  conn: Arc<Connection>,
  downloads_dir: Arc<std::path::PathBuf>,
}

fn spawn_browser_process(config: &LaunchConfig) -> std::result::Result<SpawnedBrowser, BrowserError> {
  // pair A — child reads fd 3 ← parent writes. pair B — child writes
  // fd 4 → parent reads. Swapping the pairs deadlocks both ends.
  let (parent_write, child_read) = UnixStream::pair()?;
  let (parent_read, child_write) = UnixStream::pair()?;
  let child_read_fd = child_read.as_raw_fd();
  let child_write_fd = child_write.as_raw_fd();
  let downloads = tempfile::Builder::new()
    .prefix("ferridriver-webkit-downloads-")
    .tempdir()?;
  let downloads_dir = Arc::new(downloads.path().to_owned());
  let binary = config
    .executable_path
    .clone()
    .map_or_else(super::launcher::locate_binary, Ok)?;
  let child = super::launcher::spawn(&binary, config, child_write_fd, child_read_fd)?;
  let version = Arc::from(format!(
    "webkit-playwright/{}",
    super::launcher::revision_from_path(&binary)
  ));
  let handle = super::owner::WebKitOwner::new(child, downloads);
  drop(child_read);
  drop(child_write);

  parent_read.set_nonblocking(true)?;
  parent_write.set_nonblocking(true)?;
  let transport = Transport::new(
    tokio::net::UnixStream::from_std(parent_read)?,
    tokio::net::UnixStream::from_std(parent_write)?,
  );
  let conn = Connection::spawn(transport);
  handle.install_connection(Arc::clone(&conn));
  Ok(SpawnedBrowser {
    version,
    handle,
    conn,
    downloads_dir,
  })
}

/// Spawn a browser-level listener that translates `Playwright.downloadCreated`,
/// `Playwright.downloadFilenameSuggested`, and `Playwright.downloadFinished`
/// into per-page [`crate::download::Download`] handles.
fn spawn_download_listener(
  root: &Session,
  mut rx: tokio::sync::mpsc::UnboundedReceiver<protocol::Envelope>,
  pages: Arc<Mutex<Vec<WebKitPage>>>,
  downloads_dir: Arc<std::path::PathBuf>,
  mut stopping: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
  let cancel_session = root.clone();
  tokio::spawn(async move {
    loop {
      let env = tokio::select! {
        biased;
        _ = stopping.wait_for(|stopping| *stopping) => break,
        env = rx.recv() => match env { Some(env) => env, None => break },
      };
      handle_download_event(&env, &pages, &downloads_dir, &cancel_session);
    }
  })
}

fn handle_download_event(
  env: &protocol::Envelope,
  pages: &Arc<Mutex<Vec<WebKitPage>>>,
  downloads_dir: &std::path::Path,
  cancel_session: &Session,
) {
  match env.method.as_deref() {
    Some("Playwright.downloadCreated") => {
      let Some(page_proxy_id) = env.params.get("pageProxyId").and_then(serde_json::Value::as_str) else {
        return;
      };
      let uuid = env
        .params
        .get("uuid")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
      let url = env
        .params
        .get("url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
      let Some(page) = find_page(pages, page_proxy_id) else {
        return;
      };
      let Some(arc_page) = page.page_backref.upgrade() else {
        return;
      };
      // Mirrors Playwright's `wkBrowser.ts::cancelDownload` —
      // `Playwright.cancelDownload {uuid}` on the browser session;
      // the terminal state then arrives via the regular
      // `Playwright.downloadFinished` event with error 'canceled'.
      let cancel_uuid = uuid.clone();
      let session_for_cancel = cancel_session.clone();
      let canceler: crate::download::DownloadCanceler = std::sync::Arc::new(move || {
        let session = session_for_cancel.clone();
        let uuid = cancel_uuid.clone();
        Box::pin(async move {
          session
            .send("Playwright.cancelDownload", json!({ "uuid": uuid }))
            .await
            .map(|_| ())
            .map_err(|e| crate::error::FerriError::Backend(format!("Playwright.cancelDownload: {e}")))
        })
      });
      let download = crate::download::Download::new(
        &arc_page,
        uuid,
        url,
        String::new(),
        downloads_dir.to_path_buf(),
        canceler,
      );
      // Register internally only; do NOT fire `did_open` (which
      // emits the Download event to JS waitForEvent listeners) yet.
      // PW WebKit reports the suggested filename in a SEPARATE
      // `Playwright.downloadFilenameSuggested` event that arrives
      // after `downloadCreated`. Mirrors Playwright's own
      // `server/download.ts::Download` constructor which only fires
      // the Page.Events.Download event once the filename is known.
      page.download_manager.register_pending(&download);
    },
    Some("Playwright.provisionalLoadFailed") => {
      // Main-frame provisional load failed (aborted route,
      // interception error, ...). The failing request lives on the
      // provisional target session whose events never reach the
      // committed-target listener, and this event carries its
      // `pageProxyId` inside `params` (browser-session routing) —
      // so the pending `goto` is failed from here. Mirrors
      // `wkPage._onProvisionalLoadFailed`.
      let Some(page_proxy_id) = env.params.get("pageProxyId").and_then(serde_json::Value::as_str) else {
        return;
      };
      let err = env
        .params
        .get("error")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("provisional load failed")
        .to_string();
      if let Some(page) = find_page(pages, page_proxy_id) {
        page.lifecycle.mark_provisional_failed(err);
      }
    },
    Some("Playwright.downloadFilenameSuggested") => {
      let uuid = env.params.get("uuid").and_then(serde_json::Value::as_str).unwrap_or("");
      let suggested = env
        .params
        .get("suggestedFilename")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
      let pages_snapshot: Vec<WebKitPage> = pages.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
      for p in &pages_snapshot {
        if let Some(dl) = p.download_manager.peek_for_guid(uuid) {
          dl.filename_suggested(suggested);
          // Now that the filename is known, fire the Download
          // event to JS listeners (Playwright parity — see
          // `server/download.ts::Download._fireDownloadEvent`).
          p.download_manager.fire_download_event(&dl);
          break;
        }
      }
    },
    Some("Playwright.downloadFinished") => {
      let uuid = env.params.get("uuid").and_then(serde_json::Value::as_str).unwrap_or("");
      let error = env
        .params
        .get("error")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(std::string::ToString::to_string);
      let pages_snapshot: Vec<WebKitPage> = pages.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
      for p in &pages_snapshot {
        if let Some(dl) = p.download_manager.take_for_guid(uuid) {
          match error.as_deref() {
            None => dl.report_finished(Some(downloads_dir.join(uuid)), None),
            Some(reason) => p.download_manager.report_canceled(&dl, reason),
          }
          break;
        }
      }
    },
    _ => {},
  }
}

fn find_page(pages: &Arc<Mutex<Vec<WebKitPage>>>, page_proxy_id: &str) -> Option<WebKitPage> {
  pages
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .iter()
    .find(|p| p.page_proxy_id() == page_proxy_id)
    .cloned()
}

#[cfg(test)]
mod ownership_tests {
  use super::*;
  use crate::backend::process::ChildGroup;
  use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

  #[tokio::test]
  async fn last_browser_drop_releases_idle_and_attaching_popup_owners() {
    for popup in [false, true] {
      let downloads = tempfile::tempdir().unwrap();
      let path = downloads.path().to_owned();
      let mut child = tokio::process::Command::new("sh")
        .args(["-c", "exec sleep 30"])
        .stdout(std::process::Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
      let mut output = child.stdout.take().unwrap();
      let handle = super::super::owner::WebKitOwner::new(ChildGroup::unregistered(child), downloads);
      let witness = Arc::downgrade(&handle);
      let (read, mut input) = tokio::io::duplex(4096);
      let (write, wire) = tokio::io::duplex(4096);
      let conn = Connection::spawn(Transport::new(read, write));
      handle.install_connection(Arc::clone(&conn));
      let browser = WebKitBrowser {
        inner: Arc::new(BrowserInner {
          root: conn.browser_session(),
          conn,
          handle,
          pages: Arc::new(Mutex::new(Vec::new())),
          default_context: None,
          version: Arc::from("webkit-test"),
          context_options: Arc::new(Mutex::new(rustc_hash::FxHashMap::default())),
          downloads_dir: Arc::new(path.clone()),
          popup_taps: Arc::new(Mutex::new(Vec::new())),
          create_ledger: Arc::new(crate::backend::CreateLedger::default()),
        }),
      };
      let (shutdown, stopping) = tokio::sync::watch::channel(false);
      let observer = browser.spawn_popup_listener(stopping);
      browser
        .inner
        .handle
        .install_observers(crate::backend::transport_tasks::TransportTasks::new(
          shutdown,
          vec![observer],
        ));
      if popup {
        for event in [
          json!({"method":"Playwright.pageProxyCreated","params":{"pageProxyId":"popup"}}),
          json!({"method":"Target.targetCreated","pageProxyId":"popup","params":{"targetInfo":{"targetId":"target","type":"page"}}}),
        ] {
          let mut bytes = serde_json::to_vec(&event).unwrap();
          bytes.push(0);
          input.write_all(&bytes).await.unwrap();
        }
        let mut wire = tokio::io::BufReader::new(wire);
        let mut frame = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(3), wire.read_until(0, &mut frame))
          .await
          .unwrap()
          .unwrap();
        assert!(!frame.is_empty(), "popup must be waiting on a real protocol reply");
        // Keep the peer open while dropping the browser, so EOF cannot do its cleanup for it.
        let retained = browser.clone();
        drop(browser);
        assert!(witness.upgrade().is_some());
        drop(retained);
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
          while witness.upgrade().is_some() {
            tokio::task::yield_now().await;
          }
          assert_eq!(wire.read(&mut [0]).await.unwrap(), 0);
        })
        .await
        .unwrap();
      } else {
        drop(browser);
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
          while witness.upgrade().is_some() {
            tokio::task::yield_now().await;
          }
        })
        .await
        .unwrap();
        drop(wire);
      }
      tokio::time::timeout(std::time::Duration::from_secs(3), async {
        assert_eq!(output.read(&mut [0]).await.unwrap(), 0);
        while path.exists() {
          tokio::task::yield_now().await;
        }
      })
      .await
      .unwrap();
    }
  }
}
