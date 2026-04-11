//! Browser class -- NAPI binding for `ferridriver::Browser`.

use crate::page::Page;
use crate::types::LaunchOptions;
use ferridriver::backend::BackendKind;
use napi::Result;
use napi_derive::napi;

/// Parse browser type string to `BrowserType`.
fn parse_browser_type(s: Option<&str>) -> Option<ferridriver::options::BrowserType> {
  match s {
    None => None,
    Some("chromium" | "chrome") => Some(ferridriver::options::BrowserType::Chromium),
    Some("firefox") => Some(ferridriver::options::BrowserType::Firefox),
    Some("webkit" | "safari") => Some(ferridriver::options::BrowserType::WebKit),
    Some(_) => None,
  }
}

/// Parse backend string to `BackendKind`.
fn parse_backend(s: Option<&str>) -> Result<BackendKind> {
  match s {
    None | Some("cdp-pipe" | "cdpPipe") => Ok(BackendKind::CdpPipe),
    Some("cdp-raw" | "cdpRaw") => Ok(BackendKind::CdpRaw),
    #[cfg(target_os = "macos")]
    Some("webkit") => Ok(BackendKind::WebKit),
    Some("bidi") => Ok(BackendKind::Bidi),
    Some(other) => Err(napi::Error::from_reason(format!("Unknown backend: {other}"))),
  }
}

/// Browser instance. Manages pages and browser lifecycle.
#[napi]
pub struct Browser {
  inner: ferridriver::Browser,
}

impl Browser {
  /// Wrap a core Browser into a NAPI Browser.
  pub(crate) fn wrap(inner: ferridriver::Browser) -> Self {
    Self { inner }
  }
}

#[napi]
impl Browser {
  /// Launch a new browser with default settings.
  #[napi(factory)]
  pub async fn launch(options: Option<LaunchOptions>) -> Result<Self> {
    let opts = options.unwrap_or_default();

    let browser_type = parse_browser_type(opts.browser.as_deref());
    // If backend not explicitly set, infer from browser type
    let backend = if let Some(b) = opts.backend.as_deref() {
      parse_backend(Some(b))?
    } else {
      match browser_type {
        Some(ferridriver::options::BrowserType::Firefox) => BackendKind::Bidi,
        #[cfg(target_os = "macos")]
        Some(ferridriver::options::BrowserType::WebKit) => BackendKind::WebKit,
        _ => BackendKind::CdpPipe,
      }
    };
    let launch_opts = ferridriver::options::LaunchOptions {
      backend,
      browser: browser_type,
      headless: opts.headless.unwrap_or(true),
      ws_endpoint: opts.ws_endpoint.clone(),
      executable_path: opts.executable_path.clone(),
      args: opts.args.clone().unwrap_or_default(),
      ..Default::default()
    };
    let inner = Box::pin(ferridriver::Browser::launch(launch_opts))
      .await
      .map_err(napi::Error::from_reason)?;

    Ok(Self { inner })
  }

  /// Connect to a running browser via WebSocket URL.
  #[napi(factory)]
  pub async fn connect(ws_endpoint: String) -> Result<Self> {
    let inner = Box::pin(ferridriver::Browser::connect(&ws_endpoint))
      .await
      .map_err(napi::Error::from_reason)?;
    Ok(Self { inner })
  }

  /// Create a new page (tab).
  #[napi]
  pub async fn new_page(&self) -> Result<Page> {
    let page = Box::pin(self.inner.new_page())
      .await
      .map_err(napi::Error::from_reason)?;
    Ok(Page::wrap(page))
  }

  /// Create a new page and navigate to URL.
  #[napi]
  pub async fn new_page_with_url(&self, url: String) -> Result<Page> {
    let page = Box::pin(self.inner.new_page_with_url(&url))
      .await
      .map_err(napi::Error::from_reason)?;
    Ok(Page::wrap(page))
  }

  /// Get the active page for the default context.
  #[napi]
  pub async fn page(&self) -> Result<Page> {
    let page = Box::pin(self.inner.page()).await.map_err(napi::Error::from_reason)?;
    Ok(Page::wrap(page))
  }

  /// Create a new isolated browser context.
  /// Mirrors Playwright's `browser.newContext()`.
  #[napi]
  pub fn new_context(&self) -> crate::context::BrowserContext {
    crate::context::BrowserContext::wrap(self.inner.new_context())
  }

  /// Get the default browser context.
  #[napi]
  pub fn default_context(&self) -> crate::context::BrowserContext {
    crate::context::BrowserContext::wrap(self.inner.default_context())
  }

  /// Close the browser.
  #[napi]
  pub async fn close(&self) -> Result<()> {
    self.inner.close().await.map_err(napi::Error::from_reason)
  }

  /// List all browser contexts.
  #[napi]
  pub async fn contexts(&self) -> Result<Vec<crate::context::BrowserContext>> {
    let contexts = self.inner.contexts().await;
    Ok(contexts.into_iter().map(crate::context::BrowserContext::wrap).collect())
  }

  /// Get the browser engine name.
  #[napi(getter)]
  pub fn version(&self) -> String {
    self.inner.version().to_string()
  }

  /// Check if the browser is connected.
  #[napi]
  pub async fn is_connected(&self) -> bool {
    self.inner.is_connected().await
  }
}
