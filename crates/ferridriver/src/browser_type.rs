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
}

impl BrowserType {
  /// Construct a Chromium `BrowserType`. Equivalent to the top-level
  /// [`chromium`] function. Defaults to the CDP-pipe transport.
  #[must_use]
  pub fn chromium() -> Self {
    Self {
      kind: BrowserKind::Chromium,
      transport: None,
    }
  }

  /// Construct a Chromium `BrowserType` with explicit
  /// [`BrowserTypeOptions`]. `transport: Some(Ws)` switches to the
  /// CDP-over-WebSocket backend (`CdpRaw`) instead of the pipe default.
  #[must_use]
  pub fn chromium_with(opts: &BrowserTypeOptions) -> Self {
    Self {
      kind: BrowserKind::Chromium,
      transport: opts.transport,
    }
  }

  /// Construct a Firefox `BrowserType`. Equivalent to the top-level
  /// [`firefox`] function.
  #[must_use]
  pub fn firefox() -> Self {
    Self {
      kind: BrowserKind::Firefox,
      transport: None,
    }
  }

  /// Construct a `WebKit` `BrowserType`. Equivalent to the top-level
  /// [`webkit`] function. Only meaningfully usable on macOS — the
  /// `Self::launch` path returns a typed error elsewhere.
  #[must_use]
  pub fn webkit() -> Self {
    Self {
      kind: BrowserKind::WebKit,
      transport: None,
    }
  }

  /// Rust-only escape hatch used by the test runner to pin both the
  /// product and the wire backend explicitly. NOT exposed in the JS
  /// bindings — it's intended for the test scaffolding that needs to
  /// hold `BrowserKind::Chromium + BackendKind::CdpRaw` (etc.) without
  /// going through `chromium_with({ transport: Ws })`.
  #[must_use]
  pub fn with_backend(kind: BrowserKind, backend: BackendKind) -> Self {
    let transport = match (kind, backend) {
      (BrowserKind::Chromium, BackendKind::CdpRaw) => Some(ChromiumTransport::Ws),
      (BrowserKind::Chromium, BackendKind::CdpPipe) => Some(ChromiumTransport::Pipe),
      _ => None,
    };
    Self { kind, transport }
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
      // WebKit on macOS uses the host process bundled with ferridriver
      // — no separate executable path is exposed.
      BrowserKind::WebKit => None,
    }
  }

  /// Playwright: `browserType.launch(options?) -> Browser`.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser process fails to start.
  pub async fn launch(self, options: LaunchOptions) -> Result<Browser> {
    let plan = LaunchPlan::from_public(self.kind, self.transport, options);
    let mut state = BrowserState::with_plan(ConnectMode::Launch, plan);
    Box::pin(state.ensure_browser()).await?;
    Ok(Browser::from_state(state))
  }

  /// Playwright: `browserType.connect(wsEndpoint, options?) -> Browser`.
  ///
  /// ferridriver currently has no Playwright-server protocol of its
  /// own; this accepts a CDP WebSocket endpoint and behaves like
  /// [`Self::connect_over_cdp`] under the hood. Kept as a separate
  /// method for surface parity with Playwright.
  ///
  /// # Errors
  ///
  /// Returns an error if the WebSocket handshake fails.
  pub async fn connect(self, ws_endpoint: &str, options: ConnectOptions) -> Result<Browser> {
    let cdp_opts = ConnectOverCdpOptions {
      headers: options.headers,
      slow_mo: options.slow_mo,
      timeout: options.timeout,
    };
    self.connect_over_cdp(ws_endpoint, cdp_opts).await
  }

  /// Playwright: `browserType.connectOverCDP(endpointURL, options?) -> Browser`.
  /// Chromium-only.
  ///
  /// # Errors
  ///
  /// Returns an error if the WebSocket handshake fails or the product
  /// is not Chromium.
  pub async fn connect_over_cdp(self, endpoint_url: &str, _options: ConnectOverCdpOptions) -> Result<Browser> {
    if self.kind != BrowserKind::Chromium {
      return Err(crate::error::FerriError::Unsupported(format!(
        "connectOverCDP is only supported for Chromium ({} cannot use the Chrome DevTools Protocol)",
        self.kind.name()
      )));
    }
    let plan = LaunchPlan {
      backend: BackendKind::CdpRaw,
      kind: BrowserKind::Chromium,
      ws_endpoint: Some(endpoint_url.to_string()),
      ..LaunchPlan::default()
    };
    let mut state = BrowserState::with_plan(ConnectMode::ConnectUrl(endpoint_url.to_string()), plan);
    Box::pin(state.ensure_browser()).await?;
    Ok(Browser::from_state(state))
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
    let LaunchPersistentContextOptions { launch, context } = options;
    let mut plan = LaunchPlan::from_public(self.kind, self.transport, launch);
    plan.user_data_dir = Some(user_data_dir.to_string_lossy().into_owned());
    let mut state = BrowserState::with_plan(ConnectMode::Launch, plan);
    state.persistent_context = true;
    Box::pin(state.ensure_browser()).await?;
    let browser = Browser::from_state(state);
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
