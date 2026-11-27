//! A browser context per CDP connection, for a server that holds only one
//! context on each connection it serves (Lightpanda).
//!
//! Each context is created on a connection of its own to the same server
//! and driven entirely through it: its pages hold that connection's
//! transport. Such a server numbers contexts, targets and sessions per
//! connection, so every connection's first context has the same id; the
//! id handed upward carries the connection it lives on, and is translated
//! back before it reaches the server.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rustc_hash::FxHashMap;

use super::{CdpBrowser, CdpFlavor, DownloadsDir};
use crate::backend::CookieData;
use crate::error::Result;

/// The futures these methods return. [`CdpBrowser`]'s own methods await
/// them and they await [`CdpBrowser`]'s, so their type is declared `Send`
/// here rather than inferred around that cycle, which the compiler cannot
/// do.
pub(crate) type Erased<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T>> + Send + 'a>>;

#[derive(Clone)]
pub(crate) struct Routed {
  pub(crate) browser: CdpBrowser,
  /// The context's id on its own connection.
  pub(crate) raw: String,
}

pub(crate) struct ContextConnections {
  server: String,
  version: Arc<str>,
  downloads_dir: Arc<DownloadsDir>,
  flavor: CdpFlavor,
  next: AtomicU64,
  contexts: std::sync::Mutex<FxHashMap<String, Routed>>,
}

impl ContextConnections {
  pub(crate) fn new(server: String, version: Arc<str>, downloads_dir: Arc<DownloadsDir>, flavor: CdpFlavor) -> Self {
    Self {
      server,
      version,
      downloads_dir,
      flavor,
      next: AtomicU64::new(0),
      contexts: std::sync::Mutex::default(),
    }
  }

  fn contexts(&self) -> std::sync::MutexGuard<'_, FxHashMap<String, Routed>> {
    self.contexts.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  /// The connection a context id handed out by [`Self::new_context`]
  /// lives on.
  pub(crate) fn route(&self, browser_context_id: &str) -> Option<Routed> {
    self.contexts().get(browser_context_id).cloned()
  }

  /// Open a connection, create the context on it, and have the popups it
  /// opens reach `taps` under the id handed out. Fails with the
  /// connection or `Target.createBrowserContext` error.
  pub(crate) fn new_context<'a>(
    &'a self,
    proxy: Option<&'a crate::options::ProxyConfig>,
    taps: &'a crate::backend::PopupTaps,
  ) -> Erased<'a, String> {
    Box::pin(async move {
      let (browser, raw) = CdpBrowser::connect_context_connection(
        &self.server,
        Arc::clone(&self.version),
        Arc::clone(&self.downloads_dir),
        self.flavor,
        proxy,
      )
      .await?;
      let id = format!("{raw}@{}", self.next.fetch_add(1, Ordering::Relaxed) + 1);
      forward_popups(&browser, &raw, &id, Arc::clone(taps));
      self.contexts().insert(id.clone(), Routed { browser, raw });
      Ok(id)
    })
  }

  /// Open a page in a context on a connection of its own; `None` for a
  /// context that is not one of these.
  pub(crate) fn new_page<'a>(
    &'a self,
    browser_context_id: &str,
    url: &'a str,
    viewport: Option<&'a crate::options::ViewportConfig>,
  ) -> Option<Erased<'a, crate::backend::AnyPage>> {
    let routed = self.route(browser_context_id)?;
    Some(Box::pin(async move {
      Box::pin(routed.browser.new_page(url, Some(&routed.raw), viewport)).await
    }))
  }

  /// Read cookies from a routed context on its own connection.
  pub(crate) fn context_cookies(&self, browser_context_id: &str) -> Option<Erased<'_, Vec<CookieData>>> {
    let routed = self.route(browser_context_id)?;
    Some(Box::pin(async move {
      routed.browser.context_cookies(Some(&routed.raw)).await
    }))
  }

  /// Add cookies to a routed context on its own connection.
  pub(crate) fn add_context_cookies<'a>(
    &'a self,
    browser_context_id: &str,
    cookies: &'a [CookieData],
  ) -> Option<Erased<'a, ()>> {
    let routed = self.route(browser_context_id)?;
    Some(Box::pin(async move {
      routed.browser.add_context_cookies(Some(&routed.raw), cookies).await
    }))
  }

  /// Clear cookies in a routed context on its own connection.
  pub(crate) fn clear_context_cookies(&self, browser_context_id: &str) -> Option<Erased<'_, ()>> {
    let routed = self.route(browser_context_id)?;
    Some(Box::pin(async move {
      routed.browser.clear_context_cookies(Some(&routed.raw)).await
    }))
  }

  /// Dispose a context and close the connection it had. `false` for an
  /// id that is not one of these; the `Target.disposeBrowserContext`
  /// failure otherwise.
  pub(crate) fn dispose<'a>(&'a self, browser_context_id: &'a str) -> Erased<'a, bool> {
    Box::pin(async move {
      let Some(routed) = self.contexts().remove(browser_context_id) else {
        return Ok(false);
      };
      let disposed = routed.browser.dispose_context(&routed.raw).await;
      let _ = routed.browser.close().await;
      disposed.map(|()| true)
    })
  }

  /// Wait until every connection has registered the popups it announced.
  pub(crate) fn popups_settled(&self) -> Erased<'_, ()> {
    Box::pin(async move {
      let browsers: Vec<_> = self.contexts().values().map(|routed| routed.browser.clone()).collect();
      futures::future::try_join_all(
        browsers
          .into_iter()
          .map(|browser| async move { browser.popups_settled().await }),
      )
      .await?;
      Ok(())
    })
  }

  pub(crate) fn close(&self) -> Erased<'_, ()> {
    Box::pin(async move {
      let routed: Vec<_> = self.contexts().drain().map(|(_, routed)| routed).collect();
      futures::future::join_all(routed.into_iter().map(|routed| async move {
        let _ = routed.browser.close().await;
      }))
      .await;
      Ok(())
    })
  }
}

/// Hand the popups a connection announces to the browser's own taps, with
/// the context id the state layer knows its context by.
fn forward_popups(browser: &CdpBrowser, raw: &str, id: &str, taps: crate::backend::PopupTaps) {
  let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
  browser
    .popup_taps()
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .push(tx);
  let (raw, id) = (raw.to_owned(), id.to_owned());
  tokio::spawn(async move {
    while let Some(mut popup) = rx.recv().await {
      if popup.browser_context_id.as_deref() == Some(raw.as_str()) {
        popup.browser_context_id = Some(id.clone());
      }
      crate::backend::push_popup(&taps, popup);
    }
  });
}
