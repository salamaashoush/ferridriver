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
//! Cheaply cloneable — the child + pipe fds live behind an `Arc<ChildHandle>`
//! whose `Drop` reaps the process, matching `WebKitBrowser`'s shape.

use super::connection::{Connection, ConnectionError, Session};
use super::launcher::{LaunchConfig, LaunchError};
use super::page::PwWebKitPage;
use super::protocol::{self, CreateContextParams, CreateContextResult, CreatePageParams, CreatePageResult};
use super::transport::Transport;
use crate::backend::AnyPage;
use crate::error::{FerriError, Result};
use serde_json::json;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::process::Child;
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

/// Owns the child process + the parent's pipe fds. `Drop` closes the
/// fds (child sees EOF) and reaps the process so test runs leave no
/// zombie.
struct ChildHandle {
  child: Mutex<Option<Child>>,
  /// Parent halves of the two socketpairs. Dropping closes them.
  ipc: Mutex<Option<(UnixStream, UnixStream)>>,
}

impl Drop for ChildHandle {
  fn drop(&mut self) {
    if let Ok(mut g) = self.ipc.lock() {
      g.take();
    }
    if let Ok(mut g) = self.child.lock() {
      if let Some(mut c) = g.take() {
        let _ = c.kill();
        let _ = c.wait();
      }
    }
  }
}

/// Playwright `WebKit` browser. Cloneable; clones share the child + connection.
#[derive(Clone)]
pub struct PwWebKitBrowser {
  conn: Arc<Connection>,
  root: Session,
  handle: Arc<ChildHandle>,
  /// Pages created through this browser. `pages()` snapshots it; the
  /// PW `WebKit` protocol has no page-list RPC.
  pages: Arc<Mutex<Vec<PwWebKitPage>>>,
  /// Context every page lands in when the caller passes no explicit
  /// `browserContextId`. PW `WebKit` non-persistent launches have no
  /// implicit default context — `Playwright.createPage` without a
  /// `browserContextId` fails with "Browser started with no default
  /// context", so we mint one at launch.
  default_context: Arc<str>,
  /// PW `WebKit` build revision (e.g. `"webkit-playwright/2272"`),
  /// derived from the binary path — a real build identifier, not a
  /// placeholder.
  version: Arc<str>,
  /// Per-context options stash, keyed by `browserContextId`. Populated
  /// by [`Self::new_context_with_options`]; consumed by
  /// [`PwWebKitPage::attach`] to apply per-page overrides before the
  /// initial document becomes scriptable.
  context_options: Arc<Mutex<rustc_hash::FxHashMap<String, crate::options::BrowserContextOptions>>>,
}

impl PwWebKitBrowser {
  /// Spawn a Playwright `WebKit` child and complete `Playwright.enable`.
  pub async fn launch(config: &LaunchConfig) -> std::result::Result<Self, BrowserError> {
    // pair A — child reads fd 3 ← parent writes. pair B — child writes
    // fd 4 → parent reads. Swapping the pairs deadlocks both ends.
    let (parent_write, child_read) = UnixStream::pair()?;
    let (parent_read, child_write) = UnixStream::pair()?;
    let child_read_fd = child_read.as_raw_fd();
    let child_write_fd = child_write.as_raw_fd();
    let child = super::launcher::spawn(config, child_write_fd, child_read_fd)?;
    drop(child_read);
    drop(child_write);

    parent_read.set_nonblocking(false)?;
    parent_write.set_nonblocking(false)?;
    let transport = Transport::new(parent_read.try_clone()?, parent_write.try_clone()?);
    let conn = Connection::spawn(transport);
    let root = conn.browser_session();
    root.send(protocol::PLAYWRIGHT_ENABLE, json!({})).await?;

    // Mint the default context — PW WebKit non-persistent launches
    // have no implicit one.
    let ctx_resp = root
      .send(
        protocol::PLAYWRIGHT_CREATE_CONTEXT,
        serde_json::to_value(CreateContextParams::default())?,
      )
      .await?;
    let default_context: Arc<str> = serde_json::from_value::<CreateContextResult>(ctx_resp)
      .map(|r| Arc::from(r.browser_context_id))
      .map_err(|e| BrowserError::Protocol(format!("default context: {e}")))?;

    let version: Arc<str> = Arc::from(format!("webkit-playwright/{}", super::launcher::binary_revision()));

    let downloads_dir = std::env::temp_dir().join(format!(
      "ferridriver-pw-webkit-downloads-{}",
      std::process::id()
    ));
    let _ = std::fs::create_dir_all(&downloads_dir);
    let downloads_dir = Arc::new(downloads_dir);
    let _ = root
      .send(
        "Playwright.setDownloadBehavior",
        json!({
          "behavior": "allow",
          "downloadPath": downloads_dir.to_string_lossy(),
          "browserContextId": default_context.to_string(),
        }),
      )
      .await;

    let pages: Arc<Mutex<Vec<PwWebKitPage>>> = Arc::new(Mutex::new(Vec::new()));
    spawn_download_listener(&root, pages.clone(), downloads_dir.clone());

    Ok(PwWebKitBrowser {
      conn,
      root,
      handle: Arc::new(ChildHandle {
        child: Mutex::new(Some(child)),
        ipc: Mutex::new(Some((parent_read, parent_write))),
      }),
      pages,
      default_context,
      version,
      context_options: Arc::new(Mutex::new(rustc_hash::FxHashMap::default())),
    })
  }

  #[must_use]
  pub fn version(&self) -> String {
    self.version.to_string()
  }

  #[must_use]
  pub fn root(&self) -> &Session {
    &self.root
  }

  #[must_use]
  pub fn connection(&self) -> &Arc<Connection> {
    &self.conn
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
      .root
      .send(protocol::PLAYWRIGHT_CREATE_CONTEXT, serde_json::to_value(&params)?)
      .await
      .map_err(BrowserError::from)?;
    let parsed: CreateContextResult =
      serde_json::from_value(resp).map_err(|e| FerriError::protocol("Playwright.createContext", e.to_string()))?;
    Ok(parsed.browser_context_id)
  }

  /// Create a context with the full [`BrowserContextOptions`] bag.
  ///
  /// Sends `Playwright.createContext` for the proxy fields, then
  /// `Playwright.setLanguages` if `locale` is set (mirroring
  /// `WKBrowserContext.initialize`), then stashes the options so
  /// [`Self::new_page`] / [`PwWebKitPage::attach`] can apply per-page
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
      if let Some(locale) = opts.locale.as_deref() {
        let _ = self
          .root
          .send(
            "Playwright.setLanguages",
            json!({ "browserContextId": ctx_id.clone(), "languages": [locale] }),
          )
          .await;
      }
      self
        .context_options
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(ctx_id.clone(), opts.clone());
    }
    Ok(ctx_id)
  }

  /// Look up stashed [`BrowserContextOptions`] for a context id. Used
  /// by [`PwWebKitPage::attach`] to apply per-page overrides before the
  /// initial document loads.
  pub(crate) fn context_options_for(&self, ctx_id: &str) -> Option<crate::options::BrowserContextOptions> {
    self
      .context_options
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .get(ctx_id)
      .cloned()
  }

  /// Delete a context. `Playwright.deleteContext`.
  pub async fn dispose_context(&self, browser_context_id: &str) -> Result<()> {
    self
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
      browser_context_id: Some(browser_context_id.unwrap_or(&self.default_context).to_string()),
    };
    let resp = self
      .root
      .send(protocol::PLAYWRIGHT_CREATE_PAGE, serde_json::to_value(&params)?)
      .await
      .map_err(BrowserError::from)?;
    let parsed: CreatePageResult =
      serde_json::from_value(resp).map_err(|e| FerriError::protocol("Playwright.createPage", e.to_string()))?;
    Ok(self.conn.page_proxy_session(parsed.page_proxy_id))
  }

  /// List all open pages.
  pub async fn pages(&self) -> Result<Vec<AnyPage>> {
    tokio::task::yield_now().await;
    Ok(
      self
        .pages
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|p| !p.is_closed())
        .cloned()
        .map(AnyPage::PwWebKit)
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
    let page = PwWebKitPage::attach(self, proxy, browser_context_id.map(str::to_string)).await?;
    if let Some(vp) = viewport {
      page.emulate_viewport(vp).await?;
    }
    if !url.is_empty() && url != "about:blank" {
      page.goto(url, crate::backend::NavLifecycle::Load, 30_000, None).await?;
    }
    self
      .pages
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(page.clone());
    Ok(AnyPage::PwWebKit(page))
  }

  /// Issue `Playwright.close` and reap the child.
  pub async fn close(&mut self) -> Result<()> {
    // `Playwright.close` carries the sentinel id the child never
    // answers — fire without awaiting a reply.
    let _ = self.conn.send_raw(&json!({
      "id": -9999, "method": protocol::PLAYWRIGHT_CLOSE, "params": {},
    }));
    // Closing the fds gives the child EOF; ChildHandle::Drop reaps it.
    if let Ok(mut g) = self.handle.ipc.lock() {
      g.take();
    }
    for _ in 0..30 {
      let exited = self
        .handle
        .child
        .lock()
        .ok()
        .and_then(|mut g| g.as_mut().map(|c| c.try_wait().ok().flatten().is_some()))
        .unwrap_or(true);
      if exited {
        return Ok(());
      }
      tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    if let Ok(mut g) = self.handle.child.lock() {
      if let Some(mut c) = g.take() {
        let _ = c.kill();
        let _ = c.wait();
      }
    }
    Ok(())
  }

  /// `Playwright.getInfo` — `{ os }`.
  pub async fn info(&self) -> Result<serde_json::Value> {
    self
      .root
      .send(protocol::PLAYWRIGHT_GET_INFO, json!({}))
      .await
      .map_err(|e| BrowserError::from(e).into())
  }
}

/// Spawn a browser-level listener that translates `Playwright.downloadCreated`,
/// `Playwright.downloadFilenameSuggested`, and `Playwright.downloadFinished`
/// into per-page [`crate::download::Download`] handles.
fn spawn_download_listener(root: &Session, pages: Arc<Mutex<Vec<PwWebKitPage>>>, downloads_dir: Arc<std::path::PathBuf>) {
  let mut rx = root.events();
  tokio::spawn(async move {
    use tokio::sync::broadcast::error::RecvError;
    loop {
      let env = match rx.recv().await {
        Ok(e) => e,
        Err(RecvError::Lagged(_)) => continue,
        Err(RecvError::Closed) => break,
      };
      match env.method.as_deref() {
        Some("Playwright.downloadCreated") => {
          let Some(page_proxy_id) = env.params.get("pageProxyId").and_then(serde_json::Value::as_str) else {
            continue;
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
          let Some(page) = find_page(&pages, page_proxy_id) else {
            continue;
          };
          let Some(arc_page) = page.page_backref.upgrade() else {
            continue;
          };
          let canceler: crate::download::DownloadCanceler = std::sync::Arc::new(|| Box::pin(async { Ok(()) }));
          let download = crate::download::Download::new(
            &arc_page,
            uuid,
            url,
            String::new(),
            (*downloads_dir).clone(),
            canceler,
          );
          page.download_manager.did_open(&download);
        },
        Some("Playwright.downloadFilenameSuggested") => {
          let uuid = env
            .params
            .get("uuid")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
          let suggested = env
            .params
            .get("suggestedFilename")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
          let pages_snapshot: Vec<PwWebKitPage> = pages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
          for p in &pages_snapshot {
            if let Some(dl) = p.download_manager.peek_for_guid(uuid) {
              dl.filename_suggested(suggested);
              break;
            }
          }
        },
        Some("Playwright.downloadFinished") => {
          let uuid = env
            .params
            .get("uuid")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
          let error = env
            .params
            .get("error")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(std::string::ToString::to_string);
          let pages_snapshot: Vec<PwWebKitPage> = pages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
          for p in &pages_snapshot {
            if let Some(dl) = p.download_manager.take_for_guid(uuid) {
              let final_path = if error.is_none() {
                Some(downloads_dir.join(uuid))
              } else {
                None
              };
              dl.report_finished(final_path, error.clone());
              break;
            }
          }
        },
        _ => {},
      }
    }
  });
}

fn find_page(pages: &Arc<Mutex<Vec<PwWebKitPage>>>, page_proxy_id: &str) -> Option<PwWebKitPage> {
  pages
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .iter()
    .find(|p| p.page_proxy_id() == page_proxy_id)
    .cloned()
}
