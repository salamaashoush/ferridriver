//! `AnyBrowser` — zero-cost enum dispatch across all four browser backends.

use std::sync::Arc;

use crate::error::{FerriError, Result};

use super::{AnyPage, BackendKind, PopupInfo, bidi, cdp, webdriver, webkit};

/// Browser instance — enum dispatch across backends.
#[derive(Clone)]
pub enum AnyBrowser {
  Cdp(cdp::CdpBrowser),
  WebKit(webkit::WebKitBrowser),
  Bidi(bidi::BidiBrowser),
  WebDriver(webdriver::browser::WebDriverBrowser),
}

macro_rules! browser_dispatch {
  ($self:expr, $method:ident ( $($arg:expr),* $(,)? )) => {
    match $self {
      Self::Cdp(b) => b.$method($($arg),*).await,
      Self::WebKit(b) => b.$method($($arg),*).await,
      Self::Bidi(b) => b.$method($($arg),*).await,
      Self::WebDriver(b) => b.$method($($arg),*).await,
    }
  };
}

macro_rules! browser_dispatch_sync {
  ($self:expr, $method:ident ( $($arg:expr),* $(,)? )) => {
    match $self {
      Self::Cdp(b) => b.$method($($arg),*),
      Self::WebKit(b) => b.$method($($arg),*),
      Self::Bidi(b) => b.$method($($arg),*),
      Self::WebDriver(b) => b.$method($($arg),*),
    }
  };
}

impl AnyBrowser {
  pub(crate) fn kind(&self) -> BackendKind {
    match self {
      Self::Cdp(b) => b.backend_kind(),
      Self::WebKit(_) => BackendKind::WebKit,
      Self::Bidi(_) => BackendKind::Bidi,
      Self::WebDriver(_) => BackendKind::WebDriver,
    }
  }

  pub(crate) fn supports_isolated_contexts(&self) -> bool {
    match self {
      Self::Cdp(browser) => browser.isolated_contexts,
      Self::WebKit(_) => true,
      Self::Bidi(browser) => browser.session.user_contexts,
      Self::WebDriver(_) => false,
    }
  }

  /// The lock a page holds while it is captured, when this browser
  /// draws only the tab in front. `None` when it draws every tab.
  pub(crate) fn front_tab_turn(&self) -> Option<Arc<tokio::sync::Mutex<()>>> {
    match self {
      Self::Cdp(browser) => browser.front_tab_turn(),
      Self::WebKit(_) | Self::Bidi(_) | Self::WebDriver(_) => None,
    }
  }

  /// Have a Chromium on another machine download there and each file
  /// fetched from it. The other backends keep downloads on this machine.
  pub(crate) fn set_remote_downloads(&self, remote: crate::download::RemoteDownloads) {
    if let Self::Cdp(browser) = self {
      browser.set_remote_downloads(remote);
    }
  }

  /// Whether a path on this machine names the same file to the browser.
  /// Chrome on an Android device has a filesystem of its own, and it is
  /// the one browser recognised here as being somewhere else.
  pub(crate) fn shares_host_files(&self) -> bool {
    match self {
      Self::Cdp(browser) => browser.isolated_contexts,
      Self::WebKit(_) | Self::Bidi(_) | Self::WebDriver(_) => true,
    }
  }

  /// Resolves once every popup this browser opened before the call has
  /// been registered: listed in its context and announced.
  pub(crate) async fn popups_settled(&self) -> Result<()> {
    browser_dispatch!(self, popups_settled())
  }

  /// Subscribe to browser-created pages (popups). Lossless unbounded
  /// tap — the popup pump must never miss one (a missed CDP popup is a
  /// target parked on `waitForDebuggerOnStart` forever).
  pub(crate) fn tap_popups(&self) -> tokio::sync::mpsc::UnboundedReceiver<PopupInfo> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let taps = browser_dispatch_sync!(self, popup_taps());
    taps.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(tx);
    rx
  }

  /// Attach a raw [`crate::cdp_session::CdpSession`] to the browser
  /// target (`Target.attachToBrowserTarget`). Chromium-only, mirroring
  /// Playwright's `browser.newBrowserCDPSession()`.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::Unsupported`] on `WebKit`/`BiDi`, or
  /// the protocol error if the attach fails.
  pub async fn new_browser_cdp_session(&self) -> Result<crate::cdp_session::CdpSession> {
    use crate::cdp_session::CdpSession;
    match self {
      Self::Cdp(b) => CdpSession::attach_to_browser_target(b.transport_arc()).await,
      Self::WebKit(_) | Self::Bidi(_) | Self::WebDriver(_) => Err(crate::error::FerriError::unsupported(
        "CDP session is only available on Chromium (CDP backends)",
      )),
    }
  }

  /// List the open pages of this browser's default context. Pages of a
  /// context another client of the same browser created are that client's
  /// and are not listed.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to enumerate targets or pages.
  pub async fn pages(&self) -> Result<Vec<AnyPage>> {
    match self {
      Self::Cdp(b) => Box::pin(b.pages()).await,
      Self::WebKit(b) => Box::pin(b.pages()).await,
      Self::Bidi(b) => Box::pin(b.pages()).await,
      Self::WebDriver(b) => Ok(b.pages().await?.into_iter().map(AnyPage::WebDriver).collect()),
    }
  }

  /// The `DevTools` WebSocket endpoint of a Chromium driven over one, which a
  /// second CDP client can dial. `None` on every other transport.
  #[must_use]
  pub fn cdp_ws_endpoint(&self) -> Option<&str> {
    match self {
      Self::Cdp(b) => b.ws_endpoint(),
      _ => None,
    }
  }

  /// The `WebDriver` `BiDi` session of a browser driven over one, which a
  /// served browser shares with its clients. `None` on every other
  /// transport.
  pub(crate) fn bidi_session(&self) -> Option<std::sync::Arc<bidi::session::BidiSession>> {
    match self {
      Self::Bidi(b) => Some(std::sync::Arc::clone(&b.session)),
      _ => None,
    }
  }

  /// Whether the browser is still reachable.
  ///
  /// CDP knows from its transport (EOF on the pipe / socket). `BiDi`
  /// and `WebKit` have no equivalent protocol signal, so they answer
  /// from the child process instead: a browser that exited is dead
  /// however it died. A handle that did not launch the process
  /// (`connect`) has nothing to check and reports alive, as before.
  #[must_use]
  pub fn is_alive(&self) -> bool {
    match self {
      Self::Cdp(b) => b.is_alive() && b.child_is_running().unwrap_or(true),
      Self::WebKit(b) => b.child_is_running(),
      Self::Bidi(b) => b.child_is_running().unwrap_or(true),
      Self::WebDriver(b) => !b.session.is_closed(),
    }
  }

  /// Resolves once the connection to the browser has ended, by our
  /// close or because the browser went away. A `WebDriver` session is
  /// plain HTTP with no stream to end, so only its own close settles it.
  pub fn closed(&self) -> futures::future::BoxFuture<'static, ()> {
    match self {
      Self::Cdp(b) => b.closed(),
      Self::WebKit(b) => b.closed(),
      Self::Bidi(b) => b.closed(),
      Self::WebDriver(_) => Box::pin(std::future::pending()),
    }
  }

  /// Create a new browser context (isolated cookies, storage, cache).
  ///
  /// `options` carries the full `BrowserContextOptions` bag — most
  /// backends only need `proxy`, but webkit applies `locale` (via
  /// `Playwright.setLanguages`) at context-creation time and stashes
  /// the remainder so per-page settings (userAgent, timezone, JS-disabled)
  /// can be sent during `attach()` BEFORE the about:blank document is
  /// live. Mirrors PW's `WKBrowserContext.initialize` flow.
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's context creation, and returns
  /// `FerriError::Unsupported` on Classic `WebDriver`, whose session has one context.
  pub async fn new_context(&self, options: Option<&crate::options::BrowserContextOptions>) -> Result<String> {
    let proxy = options.and_then(|o| o.proxy.as_ref());
    match self {
      Self::Cdp(b) => b.new_context(proxy).await,
      Self::WebKit(b) => b.new_context_with_options(options).await,
      Self::Bidi(b) => {
        b.new_context(proxy, options.is_some_and(|o| o.ignore_https_errors == Some(true)))
          .await
      },
      Self::WebDriver(_) => Err(FerriError::unsupported(
        "A Classic WebDriver session has one browser context; use the session's default context",
      )),
    }
  }

  /// Close a context whose user is done with it, in whichever way lets
  /// the next one start soonest. Chromium empties the context and keeps
  /// it for the next [`Self::new_context`] (`cdp::recycle`); the other
  /// backends have no way to drop what a session installed without
  /// dropping the page, and dispose it.
  ///
  /// # Errors
  ///
  /// Returns an error if the context had to be disposed and that failed.
  pub async fn retire_context(
    &self,
    browser_context_id: &str,
    origins: &[String],
    hints: cdp::RetireHints,
  ) -> Result<()> {
    match self {
      Self::Cdp(b) => Box::pin(b.retire_context(browser_context_id, origins, hints)).await,
      Self::WebKit(_) | Self::Bidi(_) | Self::WebDriver(_) => self.dispose_context(browser_context_id).await,
    }
  }

  /// Dispose a browser context.
  ///
  /// # Errors
  ///
  /// Forwards the error of the active backend's `dispose_context`, and returns
  /// `FerriError::Unsupported` on Classic `WebDriver`.
  pub async fn dispose_context(&self, browser_context_id: &str) -> Result<()> {
    match self {
      Self::Cdp(b) => b.dispose_context(browser_context_id).await,
      Self::WebKit(b) => b.dispose_context(browser_context_id).await,
      Self::Bidi(b) => b.dispose_context(browser_context_id).await,
      Self::WebDriver(_) => Err(FerriError::unsupported(
        "Classic WebDriver cannot dispose a browser context independently of its session",
      )),
    }
  }

  /// Open a new page, optionally in a specific browser context.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to create a new target or navigate to the URL.
  pub async fn new_page(
    &self,
    url: &str,
    browser_context_id: Option<&str>,
    viewport: Option<&crate::options::ViewportConfig>,
  ) -> Result<AnyPage> {
    match self {
      Self::Cdp(b) => Box::pin(b.new_page(url, browser_context_id, viewport)).await,
      Self::WebKit(b) => Box::pin(b.new_page(url, browser_context_id, viewport)).await,
      Self::Bidi(b) => Box::pin(b.new_page(url, browser_context_id, viewport)).await,
      Self::WebDriver(b) => {
        if browser_context_id.is_some() {
          return Err(FerriError::unsupported(
            "Classic WebDriver cannot select an isolated browser context",
          ));
        }
        let page = b.new_page(url).await?;
        if let Some(viewport) = viewport
          && page.supports_window_resize()
        {
          page.emulate_viewport(viewport).await?;
        }
        Ok(AnyPage::WebDriver(page))
      },
    }
  }

  /// Close the browser and all its pages.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser process fails to shut down cleanly.
  pub async fn close(&mut self) -> Result<()> {
    browser_dispatch!(self, close())
  }

  /// Real product version string for the running browser, captured at
  /// handshake/session-open time. Every backend surfaces a genuine value
  /// — no placeholders:
  ///
  /// * `cdp` → CDP `Browser.getVersion().product`
  ///   (e.g. `"HeadlessChrome/120.0.6099.109"`).
  /// * `webkit` → `Op::GetWebKitVersion` IPC → `CFBundleShortVersionString`
  ///   from the running `WebKit.framework` (e.g. `"WebKit/617.1.2 (17618)"`).
  /// * `bidi` → `BiDi` `session.new` response capabilities, formatted as
  ///   `"{browserName}/{browserVersion}"` (e.g. `"firefox/135.0.1"`).
  #[must_use]
  pub fn version(&self) -> String {
    match self {
      Self::Cdp(b) => b.version().to_string(),
      Self::WebKit(b) => b.version(),
      Self::Bidi(b) => b.version(),
      Self::WebDriver(b) => b.product.clone(),
    }
  }
}
