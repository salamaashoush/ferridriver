//! Browser management -- mirrors Playwright's `Browser` interface.
//!
//! `Browser` instances are produced by the [`crate::BrowserType`]
//! factory ([`crate::chromium`] / [`crate::firefox`] /
//! [`crate::webkit`]) — there is no `Browser::launch` /
//! `Browser::connect` shortcut. This matches Playwright's
//! `chromium.launch()` / `firefox.launch()` / `webkit.launch()` entry
//! points.
//!
//! ```ignore
//! use ferridriver::{chromium, options::LaunchOptions};
//!
//! let browser = chromium().launch(LaunchOptions::default()).await?;
//! let page = browser.new_page_with_url("https://example.com").await?;
//! ```

use crate::context::ContextRef;
use crate::error::Result;
use crate::page::Page;
use crate::state::BrowserState;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Browser instance. Manages contexts, pages, and browser lifecycle.
///
/// `Clone` is cheap — all clones share the same underlying browser process
/// and state via `Arc`. This enables exposing `browser` as a test fixture.
#[derive(Clone)]
pub struct Browser {
  state: Arc<RwLock<BrowserState>>,
  /// Product version captured once at launch from CDP
  /// `Browser.getVersion().product`. Cached here so `version()` stays
  /// synchronous and `Arc`-shared across cheap `Browser::clone`s.
  version: Arc<str>,
  /// Backend kind cached at construction (mirrors
  /// [`BrowserState::backend_kind`]) so `supports_isolated_contexts`
  /// stays synchronous. The state's `backend_kind` is set once at
  /// `with_plan` and never mutated, so the cache cannot drift.
  backend_kind: crate::backend::BackendKind,
  /// Headless flag cached at construction so `is_headless()` stays sync
  /// without needing to grab the outer `RwLock`.
  headless: bool,
  /// Direct handle to [`BrowserState::context_options`] so the sync
  /// `new_context` setter can register the options bag without having
  /// to obtain the outer `RwLock` read guard. Cloned at launch from
  /// the state and again in [`Self::from_shared_state`].
  context_options: Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, crate::options::BrowserContextOptions>>>,
  /// Mirror of [`BrowserState::record_video`] for the same reason — so
  /// a caller that only sets `record_video` via the bag still gets the
  /// per-page recording runtime kicked off in
  /// [`crate::context::ContextRef::new_page`]. Kept alongside
  /// `context_options` until the video-only registry is retired.
  record_video: Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, crate::options::RecordVideoOptions>>>,
}

impl Browser {
  /// Construct from already-prepared component handles. Used by
  /// [`crate::browser_type`] after `state.ensure_browser()` has run
  /// and by callers who need to supply pre-resolved version/registry
  /// handles (the test runner). The expected single-source-of-truth
  /// path to construct a `Browser` is the `BrowserType` factory.
  pub(crate) fn from_parts(
    state: Arc<RwLock<BrowserState>>,
    version: Arc<str>,
    backend_kind: crate::backend::BackendKind,
    headless: bool,
    context_options: Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, crate::options::BrowserContextOptions>>>,
    record_video: Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, crate::options::RecordVideoOptions>>>,
  ) -> Self {
    Self {
      state,
      version,
      backend_kind,
      headless,
      context_options,
      record_video,
    }
  }

  /// Infra constructor: wrap a [`BrowserState`] whose
  /// `ensure_browser()` has already completed. `BrowserType::launch`
  /// is the user-facing path; this entry point exists for
  /// ferridriver-internal callers (the test runner / test fixtures /
  /// MCP server) that build a [`crate::options::LaunchPlan`] directly
  /// and need a matching `Browser` handle.
  ///
  /// # Safety contract
  ///
  /// The caller MUST have awaited `state.ensure_browser()` (or an
  /// equivalent `ensure_instance(...)` call) before handing the state
  /// in — otherwise `version()` will return `"Unknown"` until a
  /// subsequent ensure.
  #[must_use]
  pub fn from_state(state: BrowserState) -> Self {
    let version: Arc<str> = state
      .default_browser()
      .map(crate::backend::AnyBrowser::version)
      .map_or_else(|| Arc::from("Unknown"), Arc::from);
    let backend_kind = state.backend_kind();
    let headless = state.headless;
    let context_options = state.context_options.clone();
    let record_video = state.record_video.clone();
    Self::from_parts(
      Arc::new(RwLock::new(state)),
      version,
      backend_kind,
      headless,
      context_options,
      record_video,
    )
  }

  /// Wrap an existing shared state as a Browser handle.
  /// Used by MCP server and other contexts that already manage browser state.
  ///
  /// The version string is read once from the state's default instance; if
  /// the instance has not been launched yet, `version()` returns
  /// `"Unknown"` until a subsequent `ensure_browser` fills it in.
  pub fn from_shared_state(state: Arc<RwLock<BrowserState>>) -> Self {
    let (version, backend_kind, headless, context_options, record_video) = state.try_read().ok().map_or_else(
      || {
        (
          Arc::from("Unknown"),
          crate::backend::BackendKind::CdpPipe,
          true,
          Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
          Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
        )
      },
      |s| {
        (
          s.default_browser()
            .map(crate::backend::AnyBrowser::version)
            .map_or_else(|| Arc::<str>::from("Unknown"), Arc::from),
          s.backend_kind(),
          s.headless,
          s.context_options.clone(),
          s.record_video.clone(),
        )
      },
    );
    Self {
      state,
      version,
      backend_kind,
      headless,
      context_options,
      record_video,
    }
  }

  /// Create a new isolated browser context.
  /// Mirrors Playwright's `browser.newContext(options?)` —
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:22229`.
  /// Pass `None` for the no-options case (Playwright's zero-arg form).
  ///
  /// Options are stored on the shared
  /// [`crate::state::BrowserState::context_options`] registry keyed by
  /// composite session key and consumed by
  /// [`ContextRef::new_page`] as each page is opened. The registry
  /// itself is a plain `std::sync::Mutex` clone-handle on `self` so
  /// this setter stays sync regardless of whether an async writer
  /// holds the outer `RwLock<BrowserState>`.
  pub fn new_context(&self, options: Option<crate::options::BrowserContextOptions>) -> ContextRef {
    static CTX_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = CTX_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = format!("context-{id}");
    let ctx = ContextRef::new(self.state.clone(), name);
    if let Some(opts) = options {
      let composite = ctx.key.to_composite();
      // Mirror `record_video` into the legacy per-video registry too,
      // so the recording runtime (which still reads via
      // `BrowserState::get_record_video`) continues to kick in on
      // every new_page without waiting for that registry to be
      // retired.
      if let Some(ref rv) = opts.record_video {
        let mut rv_map = match self.record_video.lock() {
          Ok(g) => g,
          Err(poisoned) => poisoned.into_inner(),
        };
        rv_map.insert(composite.clone(), rv.clone());
      }
      let mut map = match self.context_options.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
      };
      map.insert(composite, opts);
    }
    ctx
  }

  /// Get the default browser context.
  #[must_use]
  pub fn default_context(&self) -> ContextRef {
    ContextRef::new(self.state.clone(), "default".to_string())
  }

  /// Whether this backend exposes isolated browser contexts (i.e.
  /// `new_context()` actually opens a fresh container vs. silently
  /// returning a handle that resolves to the persistent default).
  ///
  /// Mirrors Playwright's behaviour where `chromium`, `firefox`, and
  /// `webkit` all support multiple contexts. Stock `WKWebView`
  /// (ferridriver's `WebKit` backend) only exposes the persistent
  /// default context — there's no public API for additional
  /// containers without a private framework or a custom `WKProcessPool`
  /// fork. Callers that need to share the persistent default in that
  /// case can branch on this method.
  #[must_use]
  pub fn supports_isolated_contexts(&self) -> bool {
    match self.backend_kind {
      crate::backend::BackendKind::CdpPipe
      | crate::backend::BackendKind::CdpRaw
      | crate::backend::BackendKind::Bidi => true,
      #[cfg(target_os = "macos")]
      crate::backend::BackendKind::WebKit => false,
    }
  }

  /// Backend kind cached at construction. The state's `backend_kind`
  /// is set once at `with_plan` and never mutated, so this always
  /// matches the live state.
  #[must_use]
  pub fn backend_kind(&self) -> crate::backend::BackendKind {
    self.backend_kind
  }

  /// Whether the browser was launched in headless mode. Cached at
  /// construction; the launch plan never flips this after the fact.
  #[must_use]
  pub fn is_headless(&self) -> bool {
    self.headless
  }

  /// Shorthand: create a new page in the default context.
  /// Equivalent to `browser.default_context().new_page()`.
  ///
  /// # Errors
  ///
  /// Returns an error if page creation fails.
  pub async fn new_page(&self) -> Result<Arc<Page>> {
    Box::pin(self.default_context().new_page()).await
  }

  /// Shorthand: create a new page and navigate to URL.
  ///
  /// # Errors
  ///
  /// Returns an error if page creation or navigation fails.
  pub async fn new_page_with_url(&self, url: &str) -> Result<Arc<Page>> {
    let page = Box::pin(self.new_page()).await?;
    page.goto(url, None).await?;
    Ok(page)
  }

  /// Shorthand: get the active page in the default context.
  /// Creates a page if none exists.
  ///
  /// # Errors
  ///
  /// Returns an error if page creation or retrieval fails.
  ///
  pub async fn page(&self) -> Result<Arc<Page>> {
    let ctx = self.default_context();
    let mut pages = ctx.pages().await.unwrap_or_default();
    if pages.is_empty() {
      Box::pin(ctx.new_page()).await
    } else {
      Ok(pages.swap_remove(0))
    }
  }

  /// Close the browser.
  ///
  /// Close the browser. Accepts `Option<`[`crate::options::BrowserCloseOptions`]`>`
  /// — mirrors Playwright's `browser.close({ reason })`. The reason, if
  /// set, is surfaced on `TargetClosed` errors emitted to any in-flight
  /// operation on pages/contexts from this browser. Pass `None` for the
  /// common no-options case.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser cannot be closed cleanly.
  pub async fn close(&self, opts: Option<crate::options::BrowserCloseOptions>) -> Result<()> {
    let mut state = self.state.write().await;
    if let Some(reason) = opts.and_then(|o| o.reason) {
      state.set_close_reason(reason);
    }
    state.shutdown().await;
    Ok(())
  }

  /// Access the internal state (for MCP server integration).
  #[must_use]
  pub fn state(&self) -> &Arc<RwLock<BrowserState>> {
    &self.state
  }

  /// List all browser contexts.
  pub async fn contexts(&self) -> Vec<ContextRef> {
    let state = self.state.read().await;
    state
      .list_contexts()
      .await
      .iter()
      .map(|c| ContextRef::new(self.state.clone(), c.name.clone()))
      .collect()
  }

  /// Real product version string for the running browser — mirrors
  /// Playwright's synchronous `browser.version()`.
  ///
  /// Captured once from CDP `Browser.getVersion().product` at handshake
  /// (e.g. `"HeadlessChrome/120.0.6099.109"` or `"Chrome/120.0.6099.109"`).
  /// For `WebKit` returns `"WebKit"` until we plumb `WKWebView`'s version
  /// through the IPC; for `BiDi` returns `"Firefox"`. Returns `"Unknown"`
  /// if the handshake did not complete before the `Browser` handle was
  /// constructed.
  #[must_use]
  pub fn version(&self) -> &str {
    &self.version
  }

  /// Check if the browser is connected and alive.
  pub async fn is_connected(&self) -> bool {
    let state = self.state.read().await;
    state.is_connected()
  }
}
