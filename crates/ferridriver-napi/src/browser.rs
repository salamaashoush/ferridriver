//! Browser class -- NAPI binding for ferridriver::Browser.

use crate::page::Page;
use crate::types::LaunchOptions;
use ferridriver::backend::BackendKind;
use napi::Result;
use napi_derive::napi;

/// Parse backend string to BackendKind.
fn parse_backend(s: Option<&str>) -> Result<BackendKind> {
  match s {
    None | Some("cdp-ws") | Some("cdpWs") => Ok(BackendKind::CdpWs),
    Some("cdp-pipe") | Some("cdpPipe") => Ok(BackendKind::CdpPipe),
    Some("cdp-raw") | Some("cdpRaw") => Ok(BackendKind::CdpRaw),
    #[cfg(target_os = "macos")]
    Some("webkit") => Ok(BackendKind::WebKit),
    Some(other) => Err(napi::Error::from_reason(format!("Unknown backend: {other}"))),
  }
}

/// Browser instance. Manages pages and browser lifecycle.
#[napi]
pub struct Browser {
  inner: ferridriver::Browser,
}

#[napi]
impl Browser {
  /// Launch a new browser with default settings.
  #[napi(factory)]
  pub async fn launch(options: Option<LaunchOptions>) -> Result<Self> {
    let opts = options.unwrap_or_default();

    let backend = parse_backend(opts.backend.as_deref())?;
    let launch_opts = ferridriver::options::LaunchOptions {
      backend,
      ws_endpoint: opts.ws_endpoint.clone(),
      ..Default::default()
    };
    let inner = ferridriver::Browser::launch(launch_opts)
      .await
      .map_err(napi::Error::from_reason)?;

    Ok(Self { inner })
  }

  /// Connect to a running browser via WebSocket URL.
  #[napi(factory)]
  pub async fn connect(ws_endpoint: String) -> Result<Self> {
    let inner = ferridriver::Browser::connect(&ws_endpoint)
      .await
      .map_err(napi::Error::from_reason)?;
    Ok(Self { inner })
  }

  /// Create a new page (tab).
  #[napi]
  pub async fn new_page(&self) -> Result<Page> {
    let page = self.inner.new_page()
      .await
      .map_err(napi::Error::from_reason)?;
    Ok(Page::wrap(page))
  }

  /// Create a new page and navigate to URL.
  #[napi]
  pub async fn new_page_with_url(&self, url: String) -> Result<Page> {
    let page = self.inner.new_page_with_url(&url)
      .await
      .map_err(napi::Error::from_reason)?;
    Ok(Page::wrap(page))
  }

  /// Get the active page for the default session.
  #[napi]
  pub async fn page(&self) -> Result<Page> {
    let page = self.inner.page()
      .await
      .map_err(napi::Error::from_reason)?;
    Ok(Page::wrap(page))
  }

  /// Close the browser.
  #[napi]
  pub async fn close(&self) -> Result<()> {
    self.inner.close()
      .await
      .map_err(napi::Error::from_reason)
  }
}
