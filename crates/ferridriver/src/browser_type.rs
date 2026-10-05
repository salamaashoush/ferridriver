//! `BrowserType` — Playwright-shaped factory for launching and
//! connecting to browsers.
//!
//! Mirrors `/tmp/playwright/packages/playwright-core/src/client/browserType.ts`.
//! Three top-level `BrowserType` instances are exposed via the
//! [`chromium`], [`firefox`], and [`webkit`] free functions; each
//! carries its own `name()` / `executable_path()` plus the shared
//! plumbing for launch and connect.
//!
//! ```ignore
//! use ferridriver::{chromium, firefox, webkit};
//! use ferridriver::options::LaunchOptions;
//!
//! let browser = chromium().launch(LaunchOptions::default()).await?;
//! let firefox_browser = firefox().launch(LaunchOptions::default()).await?;
//! ```
//!
//! The Chromium factory accepts an optional
//! [`crate::options::BrowserTypeOptions`] that
//! switches the wire transport — `chromium()` defaults to CDP-pipe;
//! `chromium_with(BrowserTypeOptions { transport: Some(Ws), .. })`
//! drives CDP over WebSocket. This is an explicit ferridriver
//! extension over Playwright's pipe-only `chromium`.

use std::path::Path;

use crate::backend::BackendKind;
use crate::browser::Browser;
use crate::context::ContextRef;
use crate::error::Result;
use crate::options::{
  BrowserKind, BrowserTypeOptions, ChromiumTransport, ConnectOptions, ConnectOverCdpOptions, LaunchOptions,
  LaunchPersistentContextOptions, LaunchPlan,
};
use crate::state::{BrowserState, ConnectMode};

/// Playwright-shaped browser factory. Construct via [`chromium`] /
/// [`firefox`] / [`webkit`] (top-level free functions in this crate)
/// or [`BrowserType::chromium_with`] for the Chromium transport
/// override.
///
/// `Copy` because the type is just two enum tags — keeping it copy
/// lets callers write `chromium().launch(opts)` without having to
/// bind the factory to a `let` first when reusing it across multiple
/// launches.
#[derive(Debug, Clone, Copy)]
pub struct BrowserType {
  kind: BrowserKind,
  transport: Option<ChromiumTransport>,
  backend: Option<BackendKind>,
}

impl BrowserType {
  /// Construct a Chromium `BrowserType`. Equivalent to the top-level
  /// [`chromium`] function. Defaults to the CDP-pipe transport.
  #[must_use]
  pub fn chromium() -> Self {
    Self {
      kind: BrowserKind::Chromium,
      transport: None,
      backend: None,
    }
  }

  /// Construct a Chromium `BrowserType` with explicit
  /// [`BrowserTypeOptions`]. `transport: Some(Ws)` switches to the
  /// CDP-over-WebSocket backend (`CdpWs`) instead of the pipe default.
  #[must_use]
  pub fn chromium_with(opts: &BrowserTypeOptions) -> Self {
    Self {
      kind: BrowserKind::Chromium,
      transport: opts.transport,
      backend: None,
    }
  }

  /// Construct a Firefox `BrowserType`. Equivalent to the top-level
  /// [`firefox`] function.
  #[must_use]
  pub fn firefox() -> Self {
    Self {
      kind: BrowserKind::Firefox,
      transport: None,
      backend: None,
    }
  }

  /// Construct a `WebKit` `BrowserType`. Equivalent to the top-level [`webkit`] function.
  #[must_use]
  pub fn webkit() -> Self {
    Self {
      kind: BrowserKind::WebKit,
      transport: None,
      backend: None,
    }
  }

  #[must_use]
  pub fn safari() -> Self {
    Self {
      kind: BrowserKind::Safari,
      transport: None,
      backend: None,
    }
  }

  /// Rust-only escape hatch used by the test runner to pin both the
  /// product and the wire backend explicitly. NOT exposed in the JS
  /// bindings — it's intended for the test scaffolding that needs to
  /// hold `BrowserKind::Chromium + BackendKind::CdpWs` (etc.) without
  /// going through `chromium_with({ transport: Ws })`.
  #[must_use]
  pub fn with_backend(kind: BrowserKind, backend: BackendKind) -> Self {
    let transport = match (kind, backend) {
      (BrowserKind::Chromium, BackendKind::CdpWs) => Some(ChromiumTransport::Ws),
      (BrowserKind::Chromium, BackendKind::CdpPipe) => Some(ChromiumTransport::Pipe),
      _ => None,
    };
    Self {
      kind,
      transport,
      backend: Some(backend),
    }
  }

  /// Playwright `BrowserType.name()` — `"chromium"` / `"firefox"` /
  /// `"webkit"`.
  #[must_use]
  pub fn name(self) -> &'static str {
    self.kind.name()
  }

  /// Underlying [`BrowserKind`].
  #[must_use]
  pub fn kind(self) -> BrowserKind {
    self.kind
  }

  /// Path where ferridriver expects to find a bundled browser
  /// executable. Mirrors Playwright's `BrowserType.executablePath()`.
  /// Returns `None` if no bundled binary is available for this
  /// product on the current platform.
  #[must_use]
  pub fn executable_path(self) -> Option<std::path::PathBuf> {
    match self.kind {
      BrowserKind::Firefox => std::env::var("FIREFOX_PATH")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| crate::state::detect_firefox().ok().map(std::path::PathBuf::from)),
      BrowserKind::Chromium => Some(std::path::PathBuf::from(crate::state::resolve_chromium(true))),
      BrowserKind::WebKit => crate::backend::webkit::locate_binary().ok(),
      BrowserKind::Safari => {
        cfg!(target_os = "macos").then(|| std::path::PathBuf::from("/Applications/Safari.app/Contents/MacOS/Safari"))
      },
    }
  }

  /// Playwright: `browserType.launch(options?) -> Browser`.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser process fails to start.
  pub async fn launch(self, options: LaunchOptions) -> Result<Browser> {
    self.launch_with_resources(options, None).await
  }

  pub(crate) async fn launch_with_resources(
    self,
    options: LaunchOptions,
    resources: Option<&crate::BrowserResources>,
  ) -> Result<Browser> {
    let mut plan = LaunchPlan::from_public(self.kind, self.transport, options);
    plan.backend =
      crate::options::BrowserSelection::resolve(Some(self.kind), self.backend.or(Some(plan.backend)))?.backend;
    let state = BrowserState::with_plan(ConnectMode::Launch, plan)
      .start_owned(resources)?
      .await?;
    Browser::from_ready_state(state, "default").await
  }

  /// Playwright: `browserType.connect(wsEndpoint, options?) -> Browser`.
  ///
  /// A `ws://` endpoint uses the product's native protocol. An HTTP endpoint
  /// is treated as a W3C `WebDriver` server, using `BiDi` when advertised and
  /// Classic otherwise. `capabilities.webSocketUrl` explicitly selects either
  /// protocol; Safari defaults to Classic.
  ///
  /// # Errors
  ///
  /// Returns an error if the WebSocket handshake fails.
  pub async fn connect(self, ws_endpoint: &str, options: ConnectOptions) -> Result<Browser> {
    self.connect_with_resources(ws_endpoint, options, None).await
  }

  pub(crate) async fn connect_with_resources(
    self,
    ws_endpoint: &str,
    options: ConnectOptions,
    resources: Option<&crate::BrowserResources>,
  ) -> Result<Browser> {
    if ws_endpoint.starts_with("http://") || ws_endpoint.starts_with("https://") {
      if self.kind == BrowserKind::WebKit {
        return Err(crate::error::FerriError::unsupported(
          "Playwright WebKit cannot connect through WebDriver; use safari().connect() for real Safari",
        ));
      }
      let plan = LaunchPlan {
        backend: if self.kind == BrowserKind::Chromium
          && crate::backend::webdriver::uses_android_chrome(options.capabilities.as_ref())
        {
          BackendKind::CdpWs
        } else if self.kind == BrowserKind::Safari {
          BackendKind::WebDriver
        } else {
          BackendKind::Bidi
        },
        kind: self.kind,
        ws_endpoint: Some(ws_endpoint.to_string()),
        default_viewport: None,
        ..LaunchPlan::default()
      };
      let browser_name = match self.kind {
        BrowserKind::Chromium => "chrome",
        BrowserKind::Firefox => "firefox",
        BrowserKind::WebKit | BrowserKind::Safari => "safari",
      };
      let mode = ConnectMode::WebDriver {
        endpoint: ws_endpoint.to_string(),
        browser_name: browser_name.to_string(),
        protocol: crate::backend::webdriver::WebDriverProtocol::from_backend(self.backend),
        capabilities: options.capabilities,
        headers: options.headers,
        timeout: options.timeout,
      };
      let state = BrowserState::with_plan(mode, plan).start_owned(resources)?.await?;
      return Browser::from_ready_state(state, "default").await;
    }
    if matches!(self.kind, BrowserKind::Firefox | BrowserKind::Safari) {
      let plan = LaunchPlan {
        backend: BackendKind::Bidi,
        kind: self.kind,
        ws_endpoint: Some(ws_endpoint.to_string()),
        connection_headers: options.headers,
        timeout: options.timeout,
        ..LaunchPlan::default()
      };
      let state = BrowserState::with_plan(ConnectMode::ConnectUrl(ws_endpoint.to_string()), plan)
        .start_owned(resources)?
        .await?;
      return Browser::from_ready_state(state, "default").await;
    }
    let cdp_opts = ConnectOverCdpOptions {
      headers: options.headers,
      slow_mo: options.slow_mo,
      timeout: options.timeout,
    };
    self
      .connect_over_cdp_with_resources(ws_endpoint, cdp_opts, resources)
      .await
  }

  /// Playwright: `browserType.connectOverCDP(endpointURL, options?) -> Browser`.
  /// Chromium-only.
  ///
  /// # Errors
  ///
  /// Returns an error if the WebSocket handshake fails or the product
  /// is not Chromium.
  pub async fn connect_over_cdp(self, endpoint_url: &str, options: ConnectOverCdpOptions) -> Result<Browser> {
    self.connect_over_cdp_with_resources(endpoint_url, options, None).await
  }

  pub(crate) async fn connect_over_cdp_with_resources(
    self,
    endpoint_url: &str,
    options: ConnectOverCdpOptions,
    resources: Option<&crate::BrowserResources>,
  ) -> Result<Browser> {
    if self.kind != BrowserKind::Chromium {
      return Err(crate::error::FerriError::Unsupported(format!(
        "connectOverCDP is only supported for Chromium ({} cannot use the Chrome DevTools Protocol)",
        self.kind.name()
      )));
    }
    let plan = LaunchPlan {
      backend: BackendKind::CdpWs,
      kind: BrowserKind::Chromium,
      ws_endpoint: Some(endpoint_url.to_string()),
      connection_headers: options.headers,
      timeout: options.timeout,
      default_viewport: None,
      ..LaunchPlan::default()
    };
    let state = BrowserState::with_plan(ConnectMode::ConnectUrl(endpoint_url.to_string()), plan)
      .start_owned(resources)?
      .await?;
    Browser::from_ready_state(state, "default").await
  }

  /// Playwright: `browserType.launchPersistentContext(userDataDir, options?) -> BrowserContext`.
  /// Launches a browser whose default context shares storage with the
  /// supplied user-data directory and applies the provided
  /// `BrowserContextOptions` to that default context. Returns the
  /// default `ContextRef`; closing the returned context (or the
  /// underlying browser) terminates the launch.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser process fails to start.
  pub async fn launch_persistent_context(
    &self,
    user_data_dir: &Path,
    options: LaunchPersistentContextOptions,
  ) -> Result<ContextRef> {
    self
      .launch_persistent_context_with_resources(user_data_dir, options, None)
      .await
  }

  pub(crate) async fn launch_persistent_context_with_resources(
    &self,
    user_data_dir: &Path,
    options: LaunchPersistentContextOptions,
    resources: Option<&crate::BrowserResources>,
  ) -> Result<ContextRef> {
    let LaunchPersistentContextOptions { launch, context } = options;
    let mut plan = LaunchPlan::from_public(self.kind, self.transport, launch);
    plan.backend =
      crate::options::BrowserSelection::resolve(Some(self.kind), self.backend.or(Some(plan.backend)))?.backend;
    plan.user_data_dir = Some(user_data_dir.to_string_lossy().into_owned());
    let mut state = BrowserState::with_plan(ConnectMode::Launch, plan);
    state.persistent_context = true;
    let state = state.start_owned(resources)?.await?;
    let browser = Browser::from_ready_state(state, "default").await?;
    let default_ctx = browser.default_context();
    // Persist the options bag against the composite key for the
    // default context so subsequent `new_page()` calls in the
    // persistent context honour every field. This mirrors §4.1's
    // `apply_context_options` pathway.
    let composite = default_ctx.key.to_composite();
    if let Some(rv) = context.record_video.clone() {
      browser.state().read().await.set_record_video(&composite, rv);
    }
    browser.state().read().await.set_context_options(&composite, context);
    Ok(default_ctx)
  }
}

/// Playwright top-level `chromium` accessor —
/// `/tmp/playwright/packages/playwright-core/src/client/playwright.ts`.
#[must_use]
pub fn chromium() -> BrowserType {
  BrowserType::chromium()
}

/// Playwright top-level `firefox` accessor.
#[must_use]
pub fn firefox() -> BrowserType {
  BrowserType::firefox()
}

/// Playwright top-level `webkit` accessor. macOS-only; constructing the
/// type on other platforms is allowed but `launch()` returns a typed
/// error.
#[must_use]
pub fn webkit() -> BrowserType {
  BrowserType::webkit()
}

#[must_use]
pub fn safari() -> BrowserType {
  BrowserType::safari()
}
