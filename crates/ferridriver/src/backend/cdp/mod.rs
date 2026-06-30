#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]
//! Unified CDP backend -- Chrome `DevTools` Protocol over pipes or WebSocket.
//!
//! Generic over transport: `CdpBrowser<PipeTransport>` for pipe-based,
//! `CdpBrowser<WsTransport>` for WebSocket-based. Both share identical page,
//! element, and event handling logic.
//!
//! Navigation follows Bun's ChromeBackend.cpp architecture: register a oneshot
//! waiter before sending Page.navigate, then await the waiter which resolves
//! when the reader task sees Page.loadEventFired for that session.

pub mod pipe;
pub mod transport;
pub mod ws;

use base64::Engine as _;

use super::{
  AnyElement, AnyPage, Arc, AxNodeData, AxProperty, ConsoleMessage, CookieData, ImageFormat, MetricData,
  NetworkRequest, RwLock, ScreenshotOpts,
};
use crate::error::{FerriError, Result};
use crate::network::{
  self, BodyFn, HeaderEntry, Headers, RawHeadersFn, RemoteAddr, RequestInit, RequestSizes, RequestTiming, Response,
  ResponseInit, SecurityDetails, WebSocket, WebSocketPayload,
};
use rustc_hash::FxHashMap;
use std::time::Duration;
use transport::CdpTransport;

pub(crate) const LC_COMMIT: u8 = 0b001;
pub(crate) const LC_DOMCONTENTLOADED: u8 = 0b010;
pub(crate) const LC_LOAD: u8 = 0b100;

/// Sealed trait mapping transport types to their `AnyPage`/`AnyElement` enum constructors.
/// This avoids having "abstract" methods or duplicated impls on the generic types.
pub trait CdpWrap: CdpTransport + Sized {
  fn wrap_page(page: CdpPage<Self>) -> AnyPage;
  fn wrap_element(elem: CdpElement<Self>) -> AnyElement;
}

impl CdpWrap for pipe::PipeTransport {
  fn wrap_page(page: CdpPage<Self>) -> AnyPage {
    AnyPage::CdpPipe(page)
  }
  fn wrap_element(elem: CdpElement<Self>) -> AnyElement {
    AnyElement::CdpPipe(elem)
  }
}

impl CdpWrap for ws::WsTransport {
  fn wrap_page(page: CdpPage<Self>) -> AnyPage {
    AnyPage::CdpRaw(page)
  }
  fn wrap_element(elem: CdpElement<Self>) -> AnyElement {
    AnyElement::CdpRaw(elem)
  }
}

// ---- CdpBrowser<T> --------------------------------------------------------

pub struct CdpBrowser<T: CdpTransport> {
  transport: Arc<T>,
  child: Arc<tokio::sync::Mutex<Option<super::process::ChildGroup>>>,
  /// Track targetId -> sessionId for already-attached targets.
  attached_targets: std::sync::Mutex<FxHashMap<String, Option<String>>>,
  /// Product version string captured from CDP `Browser.getVersion().product`
  /// at handshake time. Matches what Playwright surfaces via `browser.version()`
  /// (its initializer stores the same value and returns it synchronously).
  /// Example: `"HeadlessChrome/120.0.6099.109"`.
  version: Arc<str>,
  /// Owned `--user-data-dir` for launched browsers. Held as `Arc<TempDir>` so
  /// cheap `Clone`s share ownership, and the directory is removed from disk
  /// when the last handle drops (after the `Child` is killed via
  /// `kill_on_drop(true)`). `None` for `connect()` — we don't own the dir
  /// of a browser someone else launched.
  user_data_dir: Option<Arc<super::async_tempdir::AsyncTempDir>>,
}

impl<T: CdpTransport> CdpBrowser<T> {
  /// Product version string captured at handshake.
  pub fn version(&self) -> &str {
    &self.version
  }
}

impl<T: CdpTransport> Clone for CdpBrowser<T> {
  fn clone(&self) -> Self {
    Self {
      transport: Arc::clone(&self.transport),
      child: Arc::clone(&self.child),
      attached_targets: std::sync::Mutex::new(
        self
          .attached_targets
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .clone(),
      ),
      version: Arc::clone(&self.version),
      user_data_dir: self.user_data_dir.as_ref().map(Arc::clone),
    }
  }
}

/// Build the `Emulation.setDeviceMetricsOverride` params object for
/// `config`. Shared between the per-page `enable_domains` parallel
/// batch (which seeds `last_metrics_params` with the value it just
/// shipped) and `emulate_viewport` (which compares against that
/// seed to skip redundant RTTs). Mirrors Playwright's
/// `_metricsOverride` shape in `crPage.ts:920`.
fn metrics_params_for(config: &crate::options::ViewportConfig) -> serde_json::Value {
  let is_landscape = config.is_landscape || config.width > config.height;
  let orientation = if config.is_mobile {
    if is_landscape {
      serde_json::json!({"angle": 90, "type": "landscapePrimary"})
    } else {
      serde_json::json!({"angle": 0, "type": "portraitPrimary"})
    }
  } else {
    serde_json::json!({"angle": 0, "type": "landscapePrimary"})
  };
  serde_json::json!({
    "width": config.width,
    "height": config.height,
    "deviceScaleFactor": config.device_scale_factor,
    "mobile": config.is_mobile,
    "screenWidth": config.width,
    "screenHeight": config.height,
    "screenOrientation": orientation,
  })
}

impl<T: CdpWrap> CdpBrowser<T> {
  /// Enable required CDP domains on a session so events and queries work.
  /// If `viewport` is provided, sets viewport in the same parallel batch.
  /// If `unpause` is true, sends `Runtime.runIfWaitingForDebugger` in the same
  /// batch (for targets created with `waitForDebuggerOnStart`).
  ///
  /// Mirrors Playwright's `FrameSession._initialize()`
  /// (`/tmp/playwright/packages/playwright-core/src/server/chromium/crPage.ts:495-549`):
  /// every per-page setup CDP command rides one `Promise.all` /
  /// `tokio::join!` so Chrome amortises ordering over a single
  /// receive-loop pass instead of paying one full RTT per command.
  async fn enable_domains(
    transport: &T,
    session_id: Option<&str>,
    viewport: Option<&crate::options::ViewportConfig>,
    unpause: bool,
    init_script: Option<&str>,
  ) -> Result<Option<Vec<super::FrameInfo>>> {
    let ep = &super::EMPTY_PARAMS;

    let vp_params = viewport.map(metrics_params_for);

    // Fire all CDP commands in parallel — matches Playwright's FrameSession._initialize().
    // Keep default page bootstrap minimal. Domains for logging and explicit focus
    // emulation are feature-specific and can be enabled later if needed.
    let vp_fut = async {
      if let Some(params) = vp_params {
        transport
          .send_command(session_id, "Emulation.setDeviceMetricsOverride", &params)
          .await
          .map(|_| ())
      } else {
        Ok(())
      }
    };

    // Unpause future — included in parallel batch so Chrome processes it after
    // all enables (CDP commands on a session are processed in order).
    let unpause_fut = async {
      if unpause {
        transport
          .send_command(session_id, "Runtime.runIfWaitingForDebugger", &super::EMPTY_PARAMS)
          .await
          .map(|_| ())
      } else {
        Ok(())
      }
    };

    // Pre-register the lazy-inject IIFE in the same batch when supplied.
    // `runImmediately:true` runs the script in the existing
    // about:blank context AND every future document, so the first
    // selector op finds `window.__fd` already injected and skips
    // the lazy `ensure_engine_injected` RTT.
    let inject_fut = async {
      if let Some(src) = init_script {
        transport
          .send_command(
            session_id,
            "Page.addScriptToEvaluateOnNewDocument",
            &serde_json::json!({
              "source": src,
              "runImmediately": true,
            }),
          )
          .await
          .map(|_| ())
      } else {
        Ok(())
      }
    };

    // PERF_AUDIT.md §L.3.4 + §M.4 — drop `Page.getFrameTree` from the
    // parallel bootstrap batch. Chrome processes commands on a single
    // CDP session strictly serially, so each command in the batch
    // adds its own processing time (~7-15ms) to the wall-clock floor.
    //
    // The frame tree is now seeded lazily in
    // [`crate::Page::ensure_frame_cache_seeded`] (called from
    // `Page::goto` after the navigate response carries the
    // top-level frame_id) — no RTT for the navigate-then-query flow
    // that all bench / typical tests follow. `peek_main_frame_id()`
    // exposes the cached id from `Page.navigate`'s response.
    // `DOM.enable` rides the parallel batch so `DOM.requestNode` /
    // `DOM.getBoxModel` / `DOM.scrollIntoViewIfNeeded` (used by the
    // element-handle action path: scrollIntoViewIfNeeded, screenshot,
    // setFileInputFiles) return real nodeIds. Without it, those CDP
    // calls return `nodeId: 0` and every "Element not found" error
    // bubbles back to the user even when the page-side selector lookup
    // succeeded. Mirrors Playwright's `_initialize`
    // (`crPage.ts`) which also fires `DOM.enable` per session.
    let lifecycle_params = serde_json::json!({"enabled": true});
    let autoattach_params = serde_json::json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true});
    let (r1, r2, r3, r4, r5, r6, r7, r8, r9) = tokio::join!(
      transport.send_command(session_id, "Page.enable", ep),
      transport.send_command(session_id, "Runtime.enable", ep),
      transport.send_command(session_id, "Network.enable", ep),
      transport.send_command(session_id, "DOM.enable", ep),
      transport.send_command(session_id, "Page.setLifecycleEventsEnabled", &lifecycle_params),
      transport.send_command(session_id, "Target.setAutoAttach", &autoattach_params),
      vp_fut,
      inject_fut,
      unpause_fut,
    );
    r1?;
    r2?;
    r3?;
    r4?;
    r5?;
    r6?;
    r7?;
    r8?;
    r9?;
    Ok(None)
  }

  /// Internal constructor for after transport + child process setup.
  ///
  /// Matches Playwright's `CRBrowser.connect()` exactly:
  /// 1. `Browser.getVersion` — handshake, ensures pipe is ready
  /// 2. `Target.setAutoAttach` — auto-attach new targets with `waitForDebuggerOnStart`
  ///
  /// No page creation here — pages are created on demand via `new_page()`.
  async fn init(
    transport: Arc<T>,
    child: Option<super::process::ChildGroup>,
    user_data_dir: Option<tempfile::TempDir>,
  ) -> Result<Self> {
    let version_resp = transport
      .send_command(None, "Browser.getVersion", &super::EMPTY_PARAMS)
      .await?;
    // `product` is a string like "HeadlessChrome/120.0.6099.109" — mirrors
    // what Playwright surfaces via `browser.version()`.
    let version: Arc<str> = version_resp
      .get("product")
      .and_then(|v| v.as_str())
      .map_or_else(|| Arc::from("Unknown"), Arc::from);

    transport
      .send_command(
        None,
        "Target.setAutoAttach",
        &serde_json::json!({
          "autoAttach": true,
          "waitForDebuggerOnStart": true,
          "flatten": true,
        }),
      )
      .await?;

    Ok(Self {
      transport,
      child: Arc::new(tokio::sync::Mutex::new(child)),
      attached_targets: std::sync::Mutex::new(FxHashMap::default()),
      version,
      user_data_dir: user_data_dir.map(|td| Arc::new(super::async_tempdir::AsyncTempDir::new(td))),
    })
  }

  /// Retrieve all open page targets, attaching to any not yet tracked.
  pub async fn pages(&self) -> Result<Vec<AnyPage>> {
    let result = self
      .transport
      .send_command(None, "Target.getTargets", &super::EMPTY_PARAMS)
      .await?;

    let targets = result
      .get("targetInfos")
      .and_then(|t| t.as_array())
      .cloned()
      .unwrap_or_default();

    let mut pages = Vec::new();
    for target in targets {
      if target.get("type").and_then(|v| v.as_str()) != Some("page") {
        continue;
      }
      let target_id = target
        .get("targetId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

      // Check if we already have a session for this target
      let existing_sid = {
        self
          .attached_targets
          .lock()
          .map_err(|e| FerriError::Backend(format!("Lock poisoned: {e}")))?
          .get(&target_id)
          .cloned()
      };

      let sid = if let Some(sid) = existing_sid {
        sid
      } else {
        // Target not yet tracked — manually attach. This handles pre-existing targets
        // (connect flow) and targets created before setAutoAttach was enabled.
        let attach = self
          .transport
          .send_command(
            None,
            "Target.attachToTarget",
            &serde_json::json!({"targetId": target_id, "flatten": true}),
          )
          .await?;

        let sid = attach
          .get("sessionId")
          .and_then(|v| v.as_str())
          .map(std::string::ToString::to_string);

        self
          .attached_targets
          .lock()
          .map_err(|e| FerriError::Backend(format!("Lock poisoned: {e}")))?
          .insert(target_id.clone(), sid.clone());

        Self::enable_domains(&self.transport, sid.as_deref(), None, false, None).await?;

        sid
      };

      let lc_state = Arc::new(std::sync::Mutex::new(LifecycleState::new()));
      let lc_notify = Arc::new(tokio::sync::Notify::new());
      pages.push(T::wrap_page(CdpPage {
        transport: self.transport.clone(),
        session_id: sid.map(Arc::from),
        target_id: Arc::from(target_id),
        browser_context_id: None,
        events: crate::events::EventEmitter::new(),
        frame_contexts: Arc::new(tokio::sync::RwLock::new(FxHashMap::default())),
        exposed_fns: Arc::new(tokio::sync::RwLock::new(FxHashMap::default())),
        binding_initialized: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        routes: Arc::new(tokio::sync::RwLock::new(Vec::new())),
        fetch_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        http_credentials: Arc::new(tokio::sync::RwLock::new(None)),
        main_frame_id: Arc::new(tokio::sync::OnceCell::new()),
        last_metrics_params: Arc::new(std::sync::Mutex::new(None)),
        seeded_frame_tree: Arc::new(std::sync::Mutex::new(None)),
        last_cursor_pos: Arc::new(std::sync::Mutex::new(None)),
        lifecycle: lc_state.clone(),
        lifecycle_notify: lc_notify.clone(),
        injected_script: Arc::new(InjectedScriptManager::new()),
        nav_request_slot: crate::network::NavRequestSlot::new(),
        dialog_manager: crate::dialog::DialogManager::new(),
        file_chooser_manager: crate::file_chooser::FileChooserManager::new(),
        file_chooser_intercept_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        download_manager: crate::download::DownloadManager::new(),
        download_behavior_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        downloads_dir: Arc::new(
          tempfile::Builder::new()
            .prefix("ferridriver-downloads-")
            .tempdir()
            .map_err(|e| FerriError::Backend(format!("downloads tempdir: {e}")))?,
        ),
        page_backref: crate::backend::PageBackref::new(),
        frame_cache: Arc::new(std::sync::Mutex::new(crate::frame_cache::FrameCache::default())),
        frame_listener_started: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        observed: Arc::new(std::sync::Mutex::new(crate::observed::ObservedBuffers::default())),
      }));
    }
    Ok(pages)
  }

  /// Create a new browser context (isolated cookies, storage, cache).
  /// Matches Playwright's `browser.newContext()` → `Target.createBrowserContext`.
  /// Per-context `proxy` wires through `proxyServer` + `proxyBypassList`
  /// mirroring `crBrowser.ts::doCreateNewContext` at
  /// `/tmp/playwright/packages/playwright-core/src/server/chromium/crBrowser.ts:121`.
  pub async fn new_context(&self, proxy: Option<&crate::options::ProxyConfig>) -> Result<String> {
    let mut params = serde_json::json!({"disposeOnDetach": true});
    if let Some(p) = proxy {
      params["proxyServer"] = serde_json::json!(p.server);
      if let Some(ref bypass) = p.bypass {
        params["proxyBypassList"] = serde_json::json!(bypass);
      }
    }
    let ctx = self
      .transport
      .send_command(None, "Target.createBrowserContext", &params)
      .await?;

    ctx
      .get("browserContextId")
      .and_then(|v| v.as_str())
      .map(String::from)
      .ok_or_else(|| FerriError::backend("No browserContextId"))
  }

  /// Dispose a browser context. Matches Playwright's `context.close()`.
  pub async fn dispose_context(&self, browser_context_id: &str) -> Result<()> {
    self
      .transport
      .send_command(
        None,
        "Target.disposeBrowserContext",
        &serde_json::json!({"browserContextId": browser_context_id}),
      )
      .await?;
    Ok(())
  }

  /// Create a new page, optionally in a specific browser context.
  ///
  /// Follows Playwright's sequence: `Target.createTarget` → wait for auto-attach
  /// event (target is paused) → enable domains → `Runtime.runIfWaitingForDebugger`.
  pub async fn new_page(
    &self,
    url: &str,
    browser_context_id: Option<&str>,
    viewport: Option<&crate::options::ViewportConfig>,
  ) -> Result<AnyPage> {
    // Subscribe to events BEFORE createTarget so we don't miss the auto-attach.
    let mut event_rx = self.transport.subscribe_event_method("Target.attachedToTarget");

    let create_params = if let Some(ctx_id) = browser_context_id {
      serde_json::json!({"url": "about:blank", "browserContextId": ctx_id})
    } else {
      serde_json::json!({"url": "about:blank"})
    };

    let result = self
      .transport
      .send_command(None, "Target.createTarget", &create_params)
      .await?;

    let target_id = result
      .get("targetId")
      .and_then(|v| v.as_str())
      .ok_or_else(|| FerriError::protocol("Target.createTarget", "response missing targetId"))?
      .to_string();

    // Wait for Target.attachedToTarget event (from setAutoAttach in init).
    // The target is paused (waitForDebuggerOnStart) so we can set up everything.
    let tid = target_id.clone();
    let sid = tokio::time::timeout(Duration::from_secs(30), async move {
      while let Some(event) = crate::events::recv_tolerant(&mut event_rx).await {
        if event.get("method").and_then(|m| m.as_str()) == Some("Target.attachedToTarget") {
          if let Some(params) = event.get("params") {
            let event_tid = params
              .get("targetInfo")
              .and_then(|i| i.get("targetId"))
              .and_then(|v| v.as_str())
              .unwrap_or("");
            if event_tid == tid {
              return Ok(params.get("sessionId").and_then(|v| v.as_str()).map(String::from));
            }
          }
        }
      }
      Err(FerriError::target_closed(Some(
        "CDP event channel closed while waiting for Target.attachedToTarget".into(),
      )))
    })
    .await
    .map_err(|_| FerriError::timeout(format!("auto-attach of target {target_id}"), 30_000))??;

    self
      .attached_targets
      .lock()
      .map_err(|e| FerriError::Backend(format!("Lock poisoned: {e}")))?
      .insert(target_id.clone(), sid.clone());

    // Enable domains + unpause in one parallel batch (saves a round-trip).
    // Eager-inject the lazy `window.__fd` selector engine in the same
    // parallel batch as the per-page enables. Mirrors Playwright's
    // `_evaluateOnNewDocument(initScript, 'main', runImmediately:true)`
    // call inside `FrameSession._initialize` (crPage.ts:545). Saves
    // the sequential RTT that an on-demand `ensure_engine_injected`
    // otherwise pays before the first locator/selector use — the
    // bench's `locator(...).click()` etc. fire immediately so the
    // first selector lookup almost always finds the engine missing.
    //
    // The same parallel batch also pulls the IIFE engine inject, so
    // `ensure_engine_injected` becomes a no-op for first selector
    // use. Page.getFrameTree is intentionally NOT in the batch —
    // see PERF_AUDIT §M.4: the post-`Page.navigate` path seeds the
    // frame cache via `peek_main_frame_id()` for free.
    let inject_src = crate::selectors::build_lazy_inject_js();
    let frame_tree_seed =
      Self::enable_domains(&self.transport, sid.as_deref(), viewport, true, Some(&inject_src)).await?;

    let lc_state = Arc::new(std::sync::Mutex::new(LifecycleState::new()));
    let lc_notify = Arc::new(tokio::sync::Notify::new());
    let injected_script = Arc::new(InjectedScriptManager::new());
    // The init script rode the parallel batch above — flag the
    // manager as already-injected so subsequent
    // `ensure_engine_injected` calls become no-ops.
    injected_script
      .injected
      .store(true, std::sync::atomic::Ordering::Relaxed);
    let page = CdpPage {
      transport: self.transport.clone(),
      session_id: sid.map(Arc::from),
      target_id: Arc::from(target_id),
      browser_context_id: browser_context_id.map(Arc::from),
      events: crate::events::EventEmitter::new(),
      frame_contexts: Arc::new(tokio::sync::RwLock::new(FxHashMap::default())),
      exposed_fns: Arc::new(tokio::sync::RwLock::new(FxHashMap::default())),
      binding_initialized: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      routes: Arc::new(tokio::sync::RwLock::new(Vec::new())),
      fetch_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      http_credentials: Arc::new(tokio::sync::RwLock::new(None)),
      main_frame_id: Arc::new(tokio::sync::OnceCell::new()),
      // Seed the metrics cache with the exact params `enable_domains`
      // just shipped, so the first `apply_context_options` call
      // skips the redundant `setDeviceMetricsOverride` RTT when the
      // bag carries the same default size.
      last_metrics_params: Arc::new(std::sync::Mutex::new(viewport.map(metrics_params_for))),
      seeded_frame_tree: Arc::new(std::sync::Mutex::new(frame_tree_seed)),
      last_cursor_pos: Arc::new(std::sync::Mutex::new(None)),
      lifecycle: lc_state.clone(),
      lifecycle_notify: lc_notify.clone(),
      injected_script,
      nav_request_slot: crate::network::NavRequestSlot::new(),
      dialog_manager: crate::dialog::DialogManager::new(),
      file_chooser_manager: crate::file_chooser::FileChooserManager::new(),
      file_chooser_intercept_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      download_manager: crate::download::DownloadManager::new(),
      download_behavior_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      downloads_dir: Arc::new(
        tempfile::Builder::new()
          .prefix("ferridriver-downloads-")
          .tempdir()
          .map_err(|e| FerriError::Backend(format!("downloads tempdir: {e}")))?,
      ),
      page_backref: crate::backend::PageBackref::new(),
      frame_cache: Arc::new(std::sync::Mutex::new(crate::frame_cache::FrameCache::default())),
      frame_listener_started: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      observed: Arc::new(std::sync::Mutex::new(crate::observed::ObservedBuffers::default())),
    };

    // Register lifecycle tracker in the transport reader (synchronous update, zero overhead)
    page.transport.register_lifecycle_tracker(
      page.session_id.as_deref().unwrap_or(""),
      page.lifecycle.clone(),
      page.lifecycle_notify.clone(),
    );

    // Seed `main_frame_id` from `target_id`. For regular page targets
    // (everything we create here), Chrome's top-level frameId equals
    // the targetId — Playwright relies on the same identity
    // (`crPage.ts` reads `targetInfo.targetId` and treats it as the
    // main frame). Without this, a page that is never `goto`'d (e.g.
    // the default about:blank opened by the MCP server before the
    // user issues any navigation) leaves `main_frame_id` empty, and
    // `Page::main_frame()` panics.
    let _ = page.main_frame_id.set(page.target_id.to_string());

    if url != "about:blank" && !url.is_empty() {
      page.goto(url, crate::backend::NavLifecycle::Load, 30_000, None).await?;
    }

    Ok(T::wrap_page(page))
  }

  /// Close the browser process and release resources.
  ///
  /// SIGKILLs the chrome process directly via the held `ChildGroup`.
  /// The graceful CDP `Browser.close` is intentionally skipped — for
  /// test runs the user-data-dir tempdir is removed regardless, and
  /// the CDP roundtrip cost (~5-10ms RTT) outweighs the value of
  /// letting chrome flush its `IndexedDB` / profile state on exit.
  pub async fn close(&mut self) -> Result<()> {
    if let Some(mut group) = self.child.lock().await.take() {
      // `ChildGroup::drop` also kills every helper subprocess in the
      // group, but calling `kill().await` here reaps the parent
      // (waitpid) so the enclosing runtime doesn't carry a zombie.
      let _ = group.inner_mut().kill().await;
    }
    Ok(())
  }
}

// ── Pipe-specific launch ───────────────���─────────────────────────────────────

impl CdpBrowser<pipe::PipeTransport> {
  /// Launch Chrome with `--remote-debugging-pipe` and communicate over fd 3/4.
  pub async fn launch(chromium_path: &str) -> Result<Self> {
    Self::launch_with_flags(chromium_path, &crate::state::chrome_flags(true, &[])).await
  }

  /// Launch Chrome with custom flags and communicate over fd 3/4.
  pub async fn launch_with_flags(chromium_path: &str, flags: &[String]) -> Result<Self> {
    // Hold the user-data-dir as a `TempDir` so it's removed from disk when the
    // browser handle drops. The Child owned by the CDP browser has
    // `kill_on_drop(true)` set in `pipe::PipeTransport::spawn`, so the Chrome
    // process dies first, releasing any locks on the dir.
    let user_data_dir = tempfile::Builder::new()
      .prefix("ferridriver-pipe-")
      .tempdir()
      .map_err(|e| FerriError::Backend(format!("create user-data-dir: {e}")))?;

    let (transport, child) = pipe::PipeTransport::spawn(chromium_path, user_data_dir.path(), flags)?;
    Self::init(
      Arc::new(transport),
      Some(super::process::ChildGroup::new(child)),
      Some(user_data_dir),
    )
    .await
  }

  /// Launch Chrome with a caller-supplied `--user-data-dir`. The
  /// directory is NOT owned by the browser handle — the caller is
  /// responsible for its lifetime. Used by
  /// [`crate::BrowserType::launch_persistent_context`] to ensure
  /// cookies / `localStorage` / `IndexedDB` persist across re-launches
  /// against the same directory.
  pub async fn launch_with_flags_in_dir(
    chromium_path: &str,
    flags: &[String],
    user_data_dir: &std::path::Path,
  ) -> Result<Self> {
    let (transport, child) = pipe::PipeTransport::spawn(chromium_path, user_data_dir, flags)?;
    Self::init(Arc::new(transport), Some(super::process::ChildGroup::new(child)), None).await
  }
}

// ── WS-specific launch + connect ─────────────────────────────────────────────

impl CdpBrowser<ws::WsTransport> {
  /// Launch Chrome with `--remote-debugging-port` and communicate over WebSocket.
  pub async fn launch(chromium_path: &str) -> Result<Self> {
    Box::pin(Self::launch_with_flags(
      chromium_path,
      &crate::state::chrome_flags(true, &[]),
    ))
    .await
  }

  /// Launch Chrome with custom flags and communicate over WebSocket.
  pub async fn launch_with_flags(chromium_path: &str, flags: &[String]) -> Result<Self> {
    // Held as a `TempDir` so the dir is removed from disk when the browser
    // handle drops. The Child spawned by `WsTransport::spawn` has
    // `kill_on_drop(true)` set, so Chrome dies before the directory vanishes.
    let user_data_dir = tempfile::Builder::new()
      .prefix("ferridriver-raw-")
      .tempdir()
      .map_err(|e| FerriError::Backend(format!("create user-data-dir: {e}")))?;

    let (transport, child) = Box::pin(ws::WsTransport::spawn(chromium_path, user_data_dir.path(), flags)).await?;
    Self::init(
      Arc::new(transport),
      Some(super::process::ChildGroup::new(child)),
      Some(user_data_dir),
    )
    .await
  }

  /// Launch Chrome over WebSocket with a caller-supplied
  /// `--user-data-dir`. See `launch_with_flags_in_dir` on the pipe
  /// flavour for the persistence rationale.
  pub async fn launch_with_flags_in_dir(
    chromium_path: &str,
    flags: &[String],
    user_data_dir: &std::path::Path,
  ) -> Result<Self> {
    let (transport, child) = Box::pin(ws::WsTransport::spawn(chromium_path, user_data_dir, flags)).await?;
    Self::init(Arc::new(transport), Some(super::process::ChildGroup::new(child)), None).await
  }

  /// Connect to a running Chrome instance via WebSocket URL.
  pub async fn connect(ws_url: &str) -> Result<Self> {
    let transport = Arc::new(Box::pin(ws::WsTransport::connect(ws_url)).await?);

    // Capture product version for `browser.version()` — same handshake
    // Playwright's CRBrowser.connect does.
    let version_resp = transport
      .send_command(None, "Browser.getVersion", &super::EMPTY_PARAMS)
      .await?;
    let version: Arc<str> = version_resp
      .get("product")
      .and_then(|v| v.as_str())
      .map_or_else(|| Arc::from("Unknown"), Arc::from);

    transport
      .send_command(
        None,
        "Target.setDiscoverTargets",
        &serde_json::json!({"discover": true}),
      )
      .await?;

    // Find existing page targets
    let result = transport
      .send_command(None, "Target.getTargets", &super::EMPTY_PARAMS)
      .await?;

    let mut attached = FxHashMap::default();
    let mut found_page = false;

    if let Some(targets) = result.get("targetInfos").and_then(|t| t.as_array()) {
      for target in targets {
        if target.get("type").and_then(|v| v.as_str()) == Some("page") {
          let target_id = target
            .get("targetId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
          let attach = transport
            .send_command(
              None,
              "Target.attachToTarget",
              &serde_json::json!({"targetId": target_id, "flatten": true}),
            )
            .await?;
          let sid = attach
            .get("sessionId")
            .and_then(|v| v.as_str())
            .map(std::string::ToString::to_string);
          Box::pin(Self::enable_domains(&transport, sid.as_deref(), None, false, None)).await?;
          attached.insert(target_id, sid);
          found_page = true;
          break; // take first page
        }
      }
    }

    // If no existing page, create one
    if !found_page {
      let create_result = transport
        .send_command(None, "Target.createTarget", &serde_json::json!({"url": "about:blank"}))
        .await?;
      let target_id = create_result
        .get("targetId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
      let attach = transport
        .send_command(
          None,
          "Target.attachToTarget",
          &serde_json::json!({"targetId": target_id, "flatten": true}),
        )
        .await?;
      let sid = attach
        .get("sessionId")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);
      Box::pin(Self::enable_domains(&transport, sid.as_deref(), None, false, None)).await?;
      attached.insert(target_id, sid);
    }

    Ok(Self {
      transport,
      child: Arc::new(tokio::sync::Mutex::new(None)),
      attached_targets: std::sync::Mutex::new(attached),
      version,
      user_data_dir: None,
    })
  }
}

// ---- CdpPage<T> ------------------------------------------------------------

/// Recursively collect frame info from a CDP frame tree node.
/// Convert a CDP `Runtime.RemoteObject` JSON payload into a
/// [`crate::js_handle::JSHandleBacking`]. Remote-backed handles come
/// from objects with `objectId`; value-backed handles come from inline
/// primitives. Mirrors Playwright's
/// `crProtocolHelper.ts::createHandle(context, arg)` behaviour.
fn cdp_remote_object_to_backing(arg: &serde_json::Value) -> crate::js_handle::JSHandleBacking {
  if let Some(obj_id) = arg.get("objectId").and_then(|v| v.as_str()) {
    return crate::js_handle::JSHandleBacking::Remote(crate::js_handle::HandleRemote::Cdp(std::sync::Arc::from(
      obj_id,
    )));
  }
  let value = arg.get("value").cloned().unwrap_or(serde_json::Value::Null);
  let ty = arg.get("type").and_then(|v| v.as_str()).unwrap_or("");
  let serialized = if value.is_null() {
    if ty == "undefined" {
      crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Undefined)
    } else {
      crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Null)
    }
  } else {
    let mut ctx = crate::protocol::SerializationContext::default();
    crate::protocol::SerializedValue::from_json(&value, &mut ctx)
  };
  crate::js_handle::JSHandleBacking::Value(serialized)
}

/// Convert a CDP `Runtime.StackTrace` JSON payload into a
/// [`crate::console_message::ConsoleMessageLocation`] from its first
/// call frame. Mirrors Playwright's
/// `crProtocolHelper.ts::toConsoleMessageLocation` byte-for-byte.
fn cdp_stack_trace_to_location(stack: Option<&serde_json::Value>) -> crate::console_message::ConsoleMessageLocation {
  let Some(stack) = stack else {
    return crate::console_message::ConsoleMessageLocation::default();
  };
  let Some(frames) = stack.get("callFrames").and_then(|v| v.as_array()) else {
    return crate::console_message::ConsoleMessageLocation::default();
  };
  let Some(frame) = frames.first() else {
    return crate::console_message::ConsoleMessageLocation::default();
  };
  crate::console_message::ConsoleMessageLocation {
    url: frame.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    line_number: frame
      .get("lineNumber")
      .and_then(serde_json::Value::as_u64)
      .map_or(0, u64_to_u32_saturating),
    column_number: frame
      .get("columnNumber")
      .and_then(serde_json::Value::as_u64)
      .map_or(0, u64_to_u32_saturating),
  }
}

/// Clamp a JSON-typed `u64` (line / column offsets always well within
/// `u32::MAX` in practice) down to `u32` without the clippy warning
/// for unguarded casts.
fn u64_to_u32_saturating(n: u64) -> u32 {
  u32::try_from(n).unwrap_or(u32::MAX)
}

/// Convert a CDP `Runtime.ExceptionDetails` JSON payload into a
/// [`crate::web_error::ErrorDetails`]. Mirrors Playwright's
/// `server/chromium/crProtocolHelper.ts::{getExceptionMessage, exceptionToError}`
/// byte-for-byte (see `/tmp/playwright/packages/playwright-core/src/server/chromium/crProtocolHelper.ts:28-100`):
///
/// 1. Build the combined message:
///    - If `exception.description` is set, use it (carries the full
///      `Error: message\n    at foo (bar.js:3:5)` form the engine
///      produces).
///    - Else if `exception.value` is set, stringify it.
///    - Else fall back to `exceptionDetails.text` and synthesise stack
///      frames by appending `\n    at <func> (<url>:<line>:<col>)` for
///      each `stackTrace.callFrames` entry.
/// 2. Split at the first line starting with `    at`. Everything before
///    is `{ name, message }` (parsed via `splitErrorMessage`'s `': '`
///    separator); everything from that line onward is `stack`.
/// 3. If the exception's remote-object preview exposes a `name`
///    property, override the parsed name with its value
///    (e.g. custom `Error` subclasses).
fn cdp_exception_to_error_details(exception_details: &serde_json::Value) -> crate::web_error::ErrorDetails {
  let message_with_stack = cdp_get_exception_message(exception_details);
  let lines: Vec<&str> = message_with_stack.split('\n').collect();
  let first_stack_idx = lines.iter().position(|l| l.starts_with("    at"));
  let (message_with_name, stack) = match first_stack_idx {
    Some(idx) => (lines[..idx].join("\n"), message_with_stack.clone()),
    None => (message_with_stack.clone(), String::new()),
  };
  let (parsed_name, parsed_message) = split_error_message(&message_with_name);
  let name_override = exception_details
    .get("exception")
    .and_then(|e| e.get("preview"))
    .and_then(|p| p.get("properties"))
    .and_then(|v| v.as_array())
    .and_then(|props| {
      props
        .iter()
        .find(|p| p.get("name").and_then(|n| n.as_str()) == Some("name"))
    })
    .and_then(|p| {
      p.get("value")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string)
    });
  let name = name_override.unwrap_or(parsed_name);
  crate::web_error::ErrorDetails {
    name,
    message: parsed_message,
    stack,
  }
}

/// Source location for a CDP `Runtime.ExceptionDetails`. Mirrors
/// `crProtocolHelper.ts::stackTraceToLocation`: take the top
/// `stackTrace.callFrames` entry, falling back to `{ "", 0, 0 }`.
fn cdp_exception_to_location(exception_details: &serde_json::Value) -> crate::console_message::ConsoleMessageLocation {
  let frame = exception_details
    .get("stackTrace")
    .and_then(|s| s.get("callFrames"))
    .and_then(|f| f.as_array())
    .and_then(|frames| frames.first());
  match frame {
    Some(frame) => crate::console_message::ConsoleMessageLocation {
      url: frame.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
      line_number: frame.get("lineNumber").and_then(serde_json::Value::as_u64).unwrap_or(0) as u32,
      column_number: frame
        .get("columnNumber")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32,
    },
    None => crate::console_message::ConsoleMessageLocation::default(),
  }
}

/// Build the combined `description + stack` message for a CDP
/// `Runtime.ExceptionDetails` payload. Mirrors
/// `crProtocolHelper.ts::getExceptionMessage` byte-for-byte.
fn cdp_get_exception_message(exception_details: &serde_json::Value) -> String {
  use std::fmt::Write as _;
  if let Some(exception) = exception_details.get("exception") {
    if let Some(description) = exception.get("description").and_then(|v| v.as_str()) {
      return description.to_string();
    }
    if let Some(value) = exception.get("value") {
      return value_to_plain_string(value);
    }
  }
  let mut message = exception_details
    .get("text")
    .and_then(|v| v.as_str())
    .unwrap_or("")
    .to_string();
  if let Some(stack_trace) = exception_details.get("stackTrace") {
    if let Some(frames) = stack_trace.get("callFrames").and_then(|v| v.as_array()) {
      for frame in frames {
        let url = frame.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let line = frame.get("lineNumber").and_then(serde_json::Value::as_u64).unwrap_or(0);
        let column = frame
          .get("columnNumber")
          .and_then(serde_json::Value::as_u64)
          .unwrap_or(0);
        let function_name = frame.get("functionName").and_then(|v| v.as_str()).unwrap_or("");
        let function_name = if function_name.is_empty() {
          "<anonymous>"
        } else {
          function_name
        };
        // Write directly into the buffer to avoid an extra allocation;
        // writes into a String are infallible.
        let _ = write!(message, "\n    at {function_name} ({url}:{line}:{column})");
      }
    }
  }
  message
}

/// JS `String(value)` equivalent for the handful of JSON-representable
/// remote-object `value` shapes that appear in `Runtime.ExceptionDetails.exception.value`.
fn value_to_plain_string(value: &serde_json::Value) -> String {
  match value {
    serde_json::Value::Null => "null".to_string(),
    serde_json::Value::Bool(b) => b.to_string(),
    serde_json::Value::Number(n) => n.to_string(),
    serde_json::Value::String(s) => s.clone(),
    other => other.to_string(),
  }
}

/// Split a combined error message into `{ name, message }`. Mirrors
/// Playwright's `packages/isomorphic/stackTrace.ts::splitErrorMessage`:
/// the separator is the first `': '` occurrence; if absent, `name` is
/// empty and `message` is the full input.
fn split_error_message(message: &str) -> (String, String) {
  if let Some(idx) = message.find(':') {
    let name = message[..idx].to_string();
    // Playwright requires the separator to be `': '` — the `+ 2`
    // offset below mirrors `separationIdx + 2` in
    // `splitErrorMessage`. When the char after ':' is not a space,
    // fall back to the full message (keeps parity with the TS
    // `separationIdx + 2 <= message.length` guard).
    if message.as_bytes().get(idx + 1) == Some(&b' ') && idx + 2 <= message.len() {
      let msg = message[idx + 2..].to_string();
      return (name, msg);
    }
  }
  (String::new(), message.to_string())
}

/// CDP `timestamp` is fractional milliseconds. Non-finite / negative
/// values collapse to `0`; representable positives saturate at
/// `u64::MAX`. Works around clippy's unguarded f64-to-u64 cast lint.
fn f64_to_u64_saturating(n: f64) -> u64 {
  #[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
  )]
  let clamped = if !n.is_finite() || n < 0.0 {
    0_u64
  } else if n >= u64::MAX as f64 {
    u64::MAX
  } else {
    n as u64
  };
  clamped
}

fn collect_frames(node: &serde_json::Value, out: &mut Vec<super::FrameInfo>) {
  if let Some(frame) = node.get("frame") {
    out.push(super::FrameInfo {
      frame_id: frame.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
      parent_frame_id: frame
        .get("parentId")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string),
      name: frame.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
      url: frame.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    });
  }
  if let Some(children) = node.get("childFrames").and_then(|v| v.as_array()) {
    for child in children {
      collect_frames(child, out);
    }
  }
}

/// Lifecycle state for a page's current document.
/// Tracks which lifecycle events have fired and which document they belong to.
/// Updated synchronously by the transport reader. Checked synchronously by `goto()`.
pub struct LifecycleState {
  /// loaderId of the current committed document (from Page.frameNavigated).
  pub current_loader_id: String,
  /// Generation counter bumped on every `Page.frameStartedNavigating`. A
  /// post-input "settle" snapshots this before an action and waits for a
  /// commit only if it advanced — Playwright's `waitForSignalsCreatedBy`
  /// barrier, with zero cost when the action started no navigation.
  pub nav_started_seq: u64,
  /// Generation counter bumped on every `Page.frameNavigated` (commit). The
  /// settle waits for THIS to advance (not a loaderId change) so a reload —
  /// which re-commits the same loaderId — also satisfies the barrier.
  pub nav_committed_seq: u64,
  /// Lifecycle events fired for the current document.
  pub fired: u8,
  /// Set by the transport dispatcher when `Inspector.targetCrashed`
  /// fires for the page's session. Goto/reload/wait-for-navigation
  /// observe this and bail out with a typed error instead of stalling
  /// until the lifecycle timeout. Mirrors Playwright's
  /// `Page.openScope` cancellation in `raceNavigationAction`.
  pub crashed: bool,
}

impl LifecycleState {
  fn new() -> Self {
    Self {
      current_loader_id: String::new(),
      nav_started_seq: 0,
      nav_committed_seq: 0,
      fired: 0,
      crashed: false,
    }
  }
}

/// Wrapper function declaration sent to `Runtime.callFunctionOn` /
/// `script.callFunction` by [`CdpPage::call_utility_evaluate`] /
/// [`super::bidi::page::BidiPage::call_utility_evaluate`]. Shared
/// because the flow is identical on both backends: memoise the
/// `UtilityScript` instance on `window.__fd.__us`, `JSON.parse` the
/// serialized-args array, forward each element as an individual arg
/// into `utilityScript.evaluate`, and `JSON.stringify` the result
/// back so the backend's own serializer only has to ship a flat
/// string.
///
/// `serializedArgs` is a JSON-encoded array of `count` wire values
/// (a single JSON string keeps the protocol path trivial). `count`
/// mirrors Playwright's `argCount` to the utility script — the
/// utility script slices `...argsAndHandles` into the first `count`
/// as arguments and the remainder as handles.
pub(crate) const UTILITY_EVAL_WRAPPER: &str = "function(isFn, retVal, expr, count, serializedArgs, ...handles) {\
    const parsed = count > 0 ? JSON.parse(serializedArgs) : [];\
    const us = (window.__fd && window.__fd.__us) ||\
               (window.__fd.__us = window.__fd.newUtilityScript());\
    const result = us.evaluate(isFn, retVal, expr, count, ...parsed, ...handles);\
    /* Hybrid sync/async path: if the user's expression returns a\
       Promise, chain a .then so CDP's awaitPromise:true picks up the\
       resolved value; otherwise return the value directly. The async\
       wrapper imposed Promise + microtask overhead on every call,\
       which dominates the bench's tight evaluate loop. */\
    if (result && typeof result.then === 'function') {\
      return result.then(r => {\
        if (retVal) {\
          const encoded = JSON.stringify(r);\
          return encoded === undefined ? null : encoded;\
        }\
        return r;\
      });\
    }\
    if (retVal) {\
      const encoded = JSON.stringify(result);\
      return encoded === undefined ? null : encoded;\
    }\
    return result;\
  }";

pub struct CdpPage<T: CdpTransport> {
  transport: Arc<T>,
  session_id: Option<Arc<str>>,
  target_id: Arc<str>,
  /// Browser context ID for isolated contexts (used for `Target.disposeBrowserContext` on close).
  browser_context_id: Option<Arc<str>>,
  /// Event emitter for page events (console, dialog, network, frame lifecycle).
  pub events: crate::events::EventEmitter,
  /// Frame ID -> execution context ID mapping for frame-scoped evaluation.
  frame_contexts: Arc<tokio::sync::RwLock<FxHashMap<String, i64>>>,
  /// Registered exposed function callbacks.
  pub exposed_fns: Arc<tokio::sync::RwLock<FxHashMap<String, crate::events::ExposedFn>>>,
  /// Whether the binding channel has been initialized.
  binding_initialized: Arc<std::sync::atomic::AtomicBool>,
  /// Whether this page has been closed.
  closed: Arc<std::sync::atomic::AtomicBool>,
  /// Registered route handlers for network interception.
  routes: Arc<tokio::sync::RwLock<Vec<crate::route::RegisteredRoute>>>,
  /// Whether Fetch domain is enabled for interception.
  fetch_enabled: Arc<std::sync::atomic::AtomicBool>,
  /// HTTP credentials for Fetch.authRequired handling (digest/NTLM/basic).
  http_credentials: Arc<tokio::sync::RwLock<Option<crate::options::HttpCredentials>>>,
  /// Cached main frame ID to avoid repeated `Page.getFrameTree` calls.
  main_frame_id: Arc<tokio::sync::OnceCell<String>>,
  /// Most recently applied `Emulation.setDeviceMetricsOverride`
  /// params. Used by `emulate_viewport` to skip the redundant
  /// `setDeviceMetricsOverride` RTT that `apply_context_options`
  /// would otherwise pay re-applying the same size+orientation
  /// `enable_domains` already sent at page-init time. Cached as the
  /// exact JSON `Value` we'd ship — mirrors Playwright's
  /// `_metricsOverride` JSON-string equality check
  /// (`/tmp/playwright/packages/playwright-core/src/server/chromium/crPage.ts:932`).
  /// Touch emulation lives in a separate command and runs through its
  /// own idempotent path so changing `has_touch` doesn't get masked
  /// by a metrics-only cache hit.
  last_metrics_params: Arc<std::sync::Mutex<Option<serde_json::Value>>>,
  /// Frame tree captured during the per-page `enable_domains` parallel
  /// batch — `Page::with_context::seed_frame_cache` consumes this
  /// instead of paying a sequential `Page.getFrameTree` RTT once
  /// per new page. `None` for backends/paths that didn't pre-seed
  /// (e.g. attach-to-existing); callers fall back to a fresh fetch.
  seeded_frame_tree: Arc<std::sync::Mutex<Option<Vec<super::FrameInfo>>>>,
  /// Last known cursor position. Updated by every `mousePressed` /
  /// `mouseMoved` we ship; consulted by `click_at_with` to skip the
  /// pre-press `mouseMoved` when the cursor is already at the
  /// target (Playwright tracks this as `_lastPosition`). Saves one
  /// CDP RTT per click on tight loops where the cursor stays put
  /// between clicks (the common bench shape: click → assert → click
  /// the same button again).
  last_cursor_pos: Arc<std::sync::Mutex<Option<(f64, f64)>>>,
  /// Lifecycle state for current document — tracks loaderId + fired events.
  /// Updated synchronously by the transport reader task. Checked synchronously by `goto()`.
  lifecycle: Arc<std::sync::Mutex<LifecycleState>>,
  /// Notification sent when lifecycle state is updated.
  lifecycle_notify: Arc<tokio::sync::Notify>,
  /// Manager for lazy engine injection.
  injected_script: Arc<InjectedScriptManager>,
  /// Most recent main-document `Request` observed by the network
  /// listener. Cleared by `goto`/`reload`/`go_back`/`go_forward` before
  /// issuing the navigation so same-document navigations (no new
  /// request) resolve as "no response" — mirrors Playwright's
  /// `Response | null` contract on `page.goto`.
  nav_request_slot: crate::network::NavRequestSlot,
  /// Per-page dialog handler registry. Backend dialog listener
  /// constructs a [`crate::dialog::Dialog`] on `Page.javascriptDialogOpening`
  /// and synchronously calls [`crate::dialog::DialogManager::did_open`].
  /// If no handler claims the dialog, the manager auto-closes (accept
  /// for `beforeunload`, dismiss otherwise) — mirrors
  /// `/tmp/playwright/packages/playwright-core/src/server/dialog.ts::DialogManager`.
  pub dialog_manager: crate::dialog::DialogManager,
  /// Per-page file-chooser handler registry. Backend file-chooser
  /// listener resolves the triggering `<input>` into an
  /// [`crate::element_handle::ElementHandle`] on
  /// `Page.fileChooserOpened` and synchronously calls
  /// [`crate::file_chooser::FileChooserManager::did_open`]. If no
  /// handler claims, the element handle is disposed — mirrors
  /// `/tmp/playwright/packages/playwright-core/src/server/page.ts::_onFileChooserOpened`.
  pub file_chooser_manager: crate::file_chooser::FileChooserManager,
  /// Idempotency latch for `Page.setInterceptFileChooserDialog`.
  /// Mirrors Playwright's `_updateFileChooserInterception` lazy
  /// enabling: the CDP command fires once, the first time a user
  /// shows interest (`page.on('filechooser', ...)`,
  /// `wait_for_file_chooser`, etc.). With no listener registered the
  /// command is never sent — saves one RTT per page in workloads
  /// that don't use file pickers.
  pub file_chooser_intercept_enabled: Arc<std::sync::atomic::AtomicBool>,
  /// Per-page download handler registry. Backend download listener
  /// builds a live [`crate::download::Download`] on
  /// `Browser.downloadWillBegin` and synchronously calls
  /// [`crate::download::DownloadManager::did_open`]; progress events
  /// (`Browser.downloadProgress`) flip the download's terminal state
  /// watch. Mirrors
  /// `/tmp/playwright/packages/playwright-core/src/server/chromium/crBrowser.ts::_onDownloadWillBegin`.
  pub download_manager: crate::download::DownloadManager,
  /// Idempotency latch for `Browser.setDownloadBehavior`. Lazy
  /// enabling: the CDP command fires once, the first time a user
  /// shows interest in downloads (`page.on('download', ...)`,
  /// `wait_for_download`, etc.). Saves one RTT per page on workloads
  /// that don't trigger downloads. Note: `apply_context_options`
  /// still fires the command when `accept_downloads` is explicitly
  /// set in the bag, so opt-in callers keep working without needing
  /// to register a listener first.
  pub download_behavior_enabled: Arc<std::sync::atomic::AtomicBool>,
  /// Per-page temp directory that Chrome is configured to write
  /// downloads into (via `Browser.setDownloadBehavior({ behavior:
  /// 'allowAndName', downloadPath, eventsEnabled: true })`). Held as
  /// `Arc<TempDir>` so the directory lives as long as any [`crate::download::Download`]
  /// referencing a file under it — drop cleans up orphaned files
  /// (matches Playwright's per-context `_downloadsPath` cleanup on
  /// close).
  pub downloads_dir: Arc<tempfile::TempDir>,
  /// Weak back-reference to the outer [`crate::page::Page`] that wraps
  /// this backend page. Populated by [`crate::page::Page::new`] /
  /// `Page::with_context` every time a new `Arc<Page>` is constructed;
  /// the file-chooser listener reads it to turn a CDP `backendNodeId`
  /// into an [`crate::element_handle::ElementHandle`] (which requires
  /// an `Arc<Page>` for the inner `JSHandle`). The outer `Mutex`
  /// allows successive `Page::new` calls on the same backend page to
  /// overwrite the slot — callers like MCP tool handlers wrap the
  /// same backend page fresh on every invocation, so a one-shot
  /// `OnceLock` would lock in a stale weak whose target dies as soon
  /// as the first tool call returns. Stored as `Weak` so the listener
  /// task never keeps the outer page alive after the user drops it.
  pub page_backref: crate::backend::PageBackref,
  /// Shared frame cache. The cache lives on the backend page so it
  /// outlives the short-lived `Arc<crate::page::Page>` wrappers that
  /// MCP tool handlers spin up per call — each wrapper would
  /// otherwise reset the cache and lose state populated by the
  /// previous tool call. `page.frame(name)` / `page.frames()` /
  /// `frame.childFrames()` all read this cache synchronously, so
  /// every wrapper hands them the same `Arc<Mutex<FrameCache>>`.
  pub(crate) frame_cache: Arc<std::sync::Mutex<crate::frame_cache::FrameCache>>,
  /// Idempotent latch for the frame-event listener task. The first
  /// `Arc<crate::page::Page>` constructed for this backend page
  /// spawns the listener that subscribes to `FrameAttached` /
  /// `FrameDetached` / `FrameNavigated` and updates `frame_cache`;
  /// successive wrappers see the latch set and skip the spawn so we
  /// don't end up with N listeners writing the same cache.
  pub(crate) frame_listener_started: Arc<std::sync::atomic::AtomicBool>,
  /// Console / page-error retention for `page.consoleMessages()` /
  /// `page.pageErrors()`. Lives on the backend page (like
  /// [`Self::frame_cache`]) so successive `crate::Page` wrappers share
  /// one history; filled by the same listener task.
  pub(crate) observed: Arc<std::sync::Mutex<crate::observed::ObservedBuffers>>,
}

pub struct InjectedScriptManager {
  injected: std::sync::atomic::AtomicBool,
}

impl InjectedScriptManager {
  fn new() -> Self {
    Self {
      injected: std::sync::atomic::AtomicBool::new(false),
    }
  }

  // `reset()` was removed — init scripts registered via
  // `Page.addScriptToEvaluateOnNewDocument` persist for the page's
  // lifetime, so we never need to re-register on navigation /
  // context-cleared events.

  async fn ensure<T: CdpWrap>(&self, page: &CdpPage<T>) -> Result<()> {
    if self.injected.load(std::sync::atomic::Ordering::Relaxed) {
      return Ok(());
    }
    // Register the selector engine via `Page.addScriptToEvaluateOnNewDocument`
    // with `runImmediately: true` so CDP injects `window.__fd` into:
    //   1. every future document (page navigations, new iframes), and
    //   2. all currently-loaded documents (main frame + already-attached
    //      same-origin iframes).
    // Without this, `Locator`s bound to an iframe `Frame` would
    // `evaluate_to_element(js, Some(iframe_id))` against an execution
    // context where `window.__fd` is undefined, and every action would
    // fail to resolve. Mirrors Playwright's
    // `/tmp/playwright/packages/playwright-core/src/server/chromium/crPage.ts`
    // injection pattern.
    let full_inject_js = crate::selectors::build_lazy_inject_js();
    let _ = page
      .cmd(
        "Page.addScriptToEvaluateOnNewDocument",
        serde_json::json!({
            "source": full_inject_js,
            "runImmediately": true,
        }),
      )
      .await?;
    self.injected.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
  }
}

impl<T: CdpTransport> Clone for CdpPage<T> {
  fn clone(&self) -> Self {
    Self {
      transport: self.transport.clone(),
      session_id: self.session_id.clone(),
      target_id: self.target_id.clone(),
      browser_context_id: self.browser_context_id.clone(),
      events: self.events.clone(),
      frame_contexts: self.frame_contexts.clone(),
      exposed_fns: self.exposed_fns.clone(),
      binding_initialized: self.binding_initialized.clone(),
      closed: self.closed.clone(),
      routes: self.routes.clone(),
      fetch_enabled: self.fetch_enabled.clone(),
      http_credentials: self.http_credentials.clone(),
      main_frame_id: self.main_frame_id.clone(),
      last_metrics_params: self.last_metrics_params.clone(),
      seeded_frame_tree: self.seeded_frame_tree.clone(),
      last_cursor_pos: self.last_cursor_pos.clone(),
      lifecycle: self.lifecycle.clone(),
      lifecycle_notify: self.lifecycle_notify.clone(),
      injected_script: self.injected_script.clone(),
      nav_request_slot: self.nav_request_slot.clone(),
      dialog_manager: self.dialog_manager.clone(),
      file_chooser_manager: self.file_chooser_manager.clone(),
      file_chooser_intercept_enabled: self.file_chooser_intercept_enabled.clone(),
      download_manager: self.download_manager.clone(),
      download_behavior_enabled: self.download_behavior_enabled.clone(),
      downloads_dir: self.downloads_dir.clone(),
      page_backref: self.page_backref.clone(),
      frame_cache: self.frame_cache.clone(),
      frame_listener_started: self.frame_listener_started.clone(),
      observed: self.observed.clone(),
    }
  }
}

impl<T: CdpWrap> CdpPage<T> {
  /// Send a CDP command to this page's session.
  async fn cmd(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    self
      .transport
      .send_command(self.session_id.as_deref(), method, &params)
      .await
  }

  // ---- Navigation ----

  /// Map an [`NavLifecycle`] to the [`LifecycleState::fired`] key the
  /// dispatcher writes for it. Centralised so `goto` / `reload` /
  /// `history_go` / `wait_for_navigation` all read the same vocabulary
  /// the transport produces.
  fn lifecycle_key(lifecycle: crate::backend::NavLifecycle) -> &'static str {
    match lifecycle {
      crate::backend::NavLifecycle::Commit => "commit",
      crate::backend::NavLifecycle::DomContentLoaded => "domcontentloaded",
      crate::backend::NavLifecycle::Load => "load",
    }
  }

  fn lifecycle_fired(fired: u8, target_event: &str) -> bool {
    let flag = match target_event {
      "commit" => LC_COMMIT,
      "domcontentloaded" => LC_DOMCONTENTLOADED,
      "load" => LC_LOAD,
      _ => 0,
    };
    flag != 0 && fired & flag != 0
  }

  pub async fn goto(
    &self,
    url: &str,
    lifecycle: crate::backend::NavLifecycle,
    timeout_ms: u64,
    referer: Option<&str>,
  ) -> Result<Option<Response>> {
    // No `injected_script.reset()` here. The init script registered via
    // `Page.addScriptToEvaluateOnNewDocument` persists for the page's
    // lifetime, so every post-navigation document already runs the
    // self-guarded `window.__fd` IIFE. Resetting the manager triggers
    // a redundant `Page.addScriptToEvaluateOnNewDocument` RTT on every
    // `goto()` — this code used to pay it once per nav for nothing.
    let target_event = Self::lifecycle_key(lifecycle);

    // Clear any previous main-doc request slot so a same-document
    // navigation (no new request) resolves as "no response" per
    // Playwright's `Response | null` contract. The network listener
    // refills the slot when it observes the next navigation request.
    self.nav_request_slot.clear();

    // Send navigation command. Response includes loaderId for this navigation.
    // `referrer` mirrors the CDP `Page.navigate` param (note the CDP
    // spelling uses two r's — we keep Playwright's single-r public spelling
    // `referer` and translate here).
    let mut nav_params = serde_json::json!({ "url": url });
    if let Some(r) = referer {
      nav_params["referrer"] = serde_json::Value::String(r.to_string());
    }
    let nav_result = self.cmd("Page.navigate", nav_params).await?;

    if let Some(error_text) = nav_result.get("errorText").and_then(|v| v.as_str()) {
      if !error_text.is_empty() {
        return Err(FerriError::Backend(format!("Navigation failed: {error_text}")));
      }
    }

    // Page.navigate returns the navigated frame's `frameId`. PERF_AUDIT
    // §M.4 — eagerly seed the lazy `main_frame_id` cache from this
    // response so the wrapper-level [`crate::Page`] frame cache can be
    // populated without paying a separate `Page.getFrameTree` RTT
    // (the bootstrap previously paid that RTT inside `enable_domains`;
    // dropping it shifted the cost into per-action paths if not seeded
    // here).
    if let Some(fid) = nav_result.get("frameId").and_then(|v| v.as_str()) {
      let _ = self.main_frame_id.set(fid.to_string());
    }

    let nav_loader_id = nav_result
      .get("loaderId")
      .and_then(|v| v.as_str())
      .unwrap_or("")
      .to_string();

    // Mirrors Playwright's `Frame.gotoImpl`
    // (`/tmp/playwright/packages/playwright-core/src/server/frames.ts:644`):
    // wait for the InternalNavigation whose `documentId === navigateResult.newDocumentId`,
    // then for the AddLifecycle event matching the wait target. Both
    // gates use the same loaderId, so a late `Page.loadEventFired`
    // from a previous document cannot prematurely satisfy a fresh
    // navigation (the failure mode that produced "Not attached to an
    // active page" when `Page.reload` was sent in the middle of a
    // cross-origin RFH swap).
    self
      .await_loader_lifecycle(&nav_loader_id, target_event, timeout_ms)
      .await?;
    Ok(self.await_nav_response().await)
  }

  /// Resolve the main-document `Response` captured by the network
  /// listener for the most recent navigation. Returns `None` for
  /// same-document navigations (no new request was issued) or when
  /// the underlying request ended in failure.
  async fn await_nav_response(&self) -> Option<Response> {
    let req = self.nav_request_slot.get()?;
    req.response().await.ok().flatten()
  }

  pub async fn wait_for_navigation(&self) -> Result<()> {
    let pre_loader_id = {
      let state = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      state.current_loader_id.clone()
    };
    self.await_loader_change(&pre_loader_id, "load", 30_000).await
  }

  /// Snapshot the navigation state before an input action (cheap, no RTT):
  /// `(frameStartedNavigating generation, frameNavigated/commit generation)`.
  #[must_use]
  pub fn nav_snapshot(&self) -> (u64, u64) {
    let state = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    (state.nav_started_seq, state.nav_committed_seq)
  }

  /// Playwright `waitForSignalsCreatedBy`: if the just-completed action
  /// started a navigation (the `frameStartedNavigating` generation advanced
  /// vs `snap`), wait — bounded, best-effort — for the next commit
  /// (`frameNavigated`). Waiting on the commit *generation* (not a loaderId
  /// change) means a reload, which re-commits the same loaderId, also
  /// satisfies the barrier. No-op (zero cost) when the action started no
  /// navigation, which is the common case.
  pub async fn settle_navigation(&self, snap: (u64, u64), timeout_ms: u64) {
    let (pre_started, pre_committed) = snap;
    {
      let state = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      if state.nav_started_seq == pre_started {
        return; // no navigation started — zero cost.
      }
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
      let notified = self.lifecycle_notify.notified();
      tokio::pin!(notified);
      notified.as_mut().enable();
      {
        let state = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // A commit landed after our snapshot, or the target crashed — done.
        if state.crashed || state.nav_committed_seq != pre_committed {
          return;
        }
      }
      // Bounded: a navigation that starts but never commits (cancelled /
      // no-op) costs at most `timeout_ms`, never a hang.
      if tokio::time::timeout_at(deadline, notified).await.is_err() {
        return;
      }
    }
  }

  /// Force a GC pass in the page's JS engine. Mirrors Playwright's
  /// `crPage.requestGC` (`HeapProfiler.collectGarbage`).
  pub async fn request_gc(&self) -> Result<()> {
    self.cmd("HeapProfiler.collectGarbage", serde_json::json!({})).await?;
    Ok(())
  }

  pub async fn reload(&self, lifecycle: crate::backend::NavLifecycle, timeout_ms: u64) -> Result<Option<Response>> {
    // See `goto`: the registered init script persists across reloads;
    // no need to reset and re-fire `Page.addScriptToEvaluateOnNewDocument`.
    self.nav_request_slot.clear();
    let target_event = Self::lifecycle_key(lifecycle);
    // Snapshot the pre-reload `current_loader_id`. The reload command
    // does not return the new loaderId, so we wait for any commit
    // whose loaderId differs from this snapshot (mirrors
    // Playwright's `Page.reload` calling `waitForNavigation(requiresNewDocument: true)`
    // — `/tmp/playwright/packages/playwright-core/src/server/page.ts:447`).
    let pre_loader_id = {
      let state = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      state.current_loader_id.clone()
    };
    self.cmd("Page.reload", super::empty_params()).await?;
    self
      .await_loader_change(&pre_loader_id, target_event, timeout_ms)
      .await?;
    Ok(self.await_nav_response().await)
  }

  pub async fn go_back(&self, lifecycle: crate::backend::NavLifecycle, timeout_ms: u64) -> Result<Option<Response>> {
    self.history_go(-1, lifecycle, timeout_ms).await
  }

  pub async fn go_forward(&self, lifecycle: crate::backend::NavLifecycle, timeout_ms: u64) -> Result<Option<Response>> {
    self.history_go(1, lifecycle, timeout_ms).await
  }

  async fn history_go(
    &self,
    delta: i32,
    lifecycle: crate::backend::NavLifecycle,
    timeout_ms: u64,
  ) -> Result<Option<Response>> {
    let hist = self.cmd("Page.getNavigationHistory", super::empty_params()).await?;
    let current_i64 = hist
      .get("currentIndex")
      .and_then(serde_json::Value::as_i64)
      .unwrap_or(0);
    let current = i32::try_from(current_i64).unwrap_or(i32::MAX);
    let target = current + delta;
    let entries = hist.get("entries").and_then(|v| v.as_array());
    let Some(entries) = entries else {
      return Ok(None);
    };
    let Ok(target_usize) = usize::try_from(target) else {
      return Ok(None);
    };
    if target_usize >= entries.len() {
      return Ok(None);
    }
    let entry_id = entries[target_usize]
      .get("id")
      .and_then(serde_json::Value::as_i64)
      .unwrap_or(0);

    self.nav_request_slot.clear();
    let target_event = Self::lifecycle_key(lifecycle);
    let pre_loader_id = {
      let state = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      state.current_loader_id.clone()
    };
    self
      .cmd("Page.navigateToHistoryEntry", serde_json::json!({"entryId": entry_id}))
      .await?;
    self
      .await_loader_change(&pre_loader_id, target_event, timeout_ms)
      .await?;
    Ok(self.await_nav_response().await)
  }

  /// Wait until the page's main-frame [`LifecycleState`] has both
  /// committed the supplied `expected_loader_id` AND fired
  /// `target_event` for that loader.
  ///
  /// `Page.navigate` returns the navigation's `loaderId` synchronously
  /// (Chrome 90+), so [`Self::goto`] uses this strict gate. Returns
  /// permissively on timeout (`Ok(())`) to preserve the prior
  /// "navigate-returns-with-whatever-we-have" semantics on hostile
  /// pages; the caller's response wait surfaces real failures.
  async fn await_loader_lifecycle(&self, expected_loader_id: &str, target_event: &str, timeout_ms: u64) -> Result<()> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
      let notified = self.lifecycle_notify.notified();
      tokio::pin!(notified);
      // `Notified::enable` arms the future before the state check so
      // a `notify_waiters` racing between check and await is observed.
      // Without this the dispatcher could fire its notify, the check
      // could still see the pre-notify state, and the subsequent await
      // would miss the wake.
      notified.as_mut().enable();
      {
        let state = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.crashed {
          return Err(FerriError::target_closed(Some("target crashed".into())));
        }
        if state.current_loader_id == expected_loader_id && Self::lifecycle_fired(state.fired, target_event) {
          return Ok(());
        }
      }
      if tokio::time::timeout_at(deadline, notified).await.is_err() {
        return Ok(());
      }
    }
  }

  /// Variant of [`Self::await_loader_lifecycle`] for navigations whose
  /// `loaderId` is unknown ahead of time (`Page.reload`,
  /// `Page.navigateToHistoryEntry`, generic `page.waitForNavigation`).
  /// Waits for the main frame's `current_loader_id` to change away
  /// from `pre_loader_id` AND fire `target_event` for the new loader.
  async fn await_loader_change(&self, pre_loader_id: &str, target_event: &str, timeout_ms: u64) -> Result<()> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
      let notified = self.lifecycle_notify.notified();
      tokio::pin!(notified);
      notified.as_mut().enable();
      {
        let state = self.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.crashed {
          return Err(FerriError::target_closed(Some("target crashed".into())));
        }
        if state.current_loader_id != pre_loader_id
          && !state.current_loader_id.is_empty()
          && Self::lifecycle_fired(state.fired, target_event)
        {
          return Ok(());
        }
      }
      if tokio::time::timeout_at(deadline, notified).await.is_err() {
        return Ok(());
      }
    }
  }

  /// Backend-level synchronous accessor for the cached top-level
  /// `frameId`. Populated by `CdpPage::goto` from the
  /// `Page.navigate` response (see `nav_result.frameId`) and by the
  /// lazy `set_content` path. Used by `crate::Page::ensure_frame_cache_seeded`
  /// to seed the wrapper's frame cache without a separate
  /// `Page.getFrameTree` round-trip when the bootstrap batch no
  /// longer fetches it (`PERF_AUDIT` §M.4).
  #[must_use]
  pub fn peek_main_frame_id(&self) -> Option<String> {
    self.main_frame_id.get().cloned()
  }

  pub async fn url(&self) -> Result<Option<String>> {
    let result = self
      .cmd(
        "Runtime.evaluate",
        serde_json::json!({
            "expression": "location.href",
            "returnByValue": true,
        }),
      )
      .await?;
    Ok(
      result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string),
    )
  }

  pub async fn title(&self) -> Result<Option<String>> {
    let result = self
      .cmd(
        "Runtime.evaluate",
        serde_json::json!({
            "expression": "document.title",
            "returnByValue": true,
        }),
      )
      .await?;
    Ok(
      result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string),
    )
  }

  // ---- JavaScript ----

  pub async fn injected_script(&self) -> Result<String> {
    self.ensure_engine_injected().await?;
    Ok("window.__fd".to_string())
  }

  /// Ensures the selector engine is injected into the current execution context.
  /// Idempotent and navigation-aware.
  pub async fn ensure_engine_injected(&self) -> Result<()> {
    self.injected_script.ensure(self).await
  }

  /// Idempotently fire `Page.setInterceptFileChooserDialog({enabled:true})`.
  /// Mirrors Playwright's `_updateFileChooserInterception`
  /// (`/tmp/playwright/packages/playwright-core/src/server/chromium/crPage.ts:1009`):
  /// the CDP command is a no-op until any test path needs file
  /// chooser events, at which point this method is called and the
  /// command fires once. Subsequent calls are no-ops.
  pub async fn enable_file_chooser_intercept(&self) -> Result<()> {
    if self
      .file_chooser_intercept_enabled
      .swap(true, std::sync::atomic::Ordering::Relaxed)
    {
      return Ok(());
    }
    let _ = self
      .cmd(
        "Page.setInterceptFileChooserDialog",
        serde_json::json!({ "enabled": true }),
      )
      .await;
    Ok(())
  }

  /// Idempotently fire `Browser.setDownloadBehavior` so download
  /// events flow. Mirrors Playwright's per-context download config
  /// (`/tmp/playwright/packages/playwright-core/src/server/chromium/crBrowser.ts:354`)
  /// but lazy: not fired until a test path actually needs downloads.
  pub async fn enable_download_behavior(&self) -> Result<()> {
    if self
      .download_behavior_enabled
      .swap(true, std::sync::atomic::Ordering::Relaxed)
    {
      return Ok(());
    }
    let params = if let Some(ref ctx) = self.browser_context_id {
      serde_json::json!({
        "behavior": "allowAndName",
        "browserContextId": &**ctx,
        "downloadPath": self.downloads_dir.path().to_string_lossy(),
        "eventsEnabled": true,
      })
    } else {
      serde_json::json!({
        "behavior": "allowAndName",
        "downloadPath": self.downloads_dir.path().to_string_lossy(),
        "eventsEnabled": true,
      })
    };
    let _ = self.cmd("Browser.setDownloadBehavior", params).await;
    Ok(())
  }

  pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>> {
    let result = self
      .cmd(
        "Runtime.evaluate",
        serde_json::json!({
            "expression": expression,
            "returnByValue": true,
            "awaitPromise": true,
        }),
      )
      .await?;

    if let Some(exception) = result.get("exceptionDetails") {
      let text = exception
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("Evaluation error");
      return Err(FerriError::Backend(text.to_string()));
    }

    Ok(result.get("result").and_then(|r| r.get("value")).cloned())
  }

  /// ferridriver's equivalent of Playwright's
  /// `evaluateExpression(context, expr, { returnByValue, isFunction }, ...args)`
  /// (`/tmp/playwright/packages/playwright-core/src/server/javascript.ts:248`).
  /// `args` are the variadic positional arguments passed to the user
  /// function after isomorphic serialization — for `page.evaluate(fn, arg)`
  /// that's `[arg]`; for `handle.evaluate(fn, arg)` it's `[handle, arg]`
  /// with the handle supplied via `{h: 0}` in `args[0]` and its wire
  /// ref in `handles[0]`. There is no separate receiver/`this`
  /// binding — Playwright doesn't have one either.
  ///
  /// # Errors
  ///
  /// Returns a String error on protocol failure, `exceptionDetails`
  /// from the page, or backend/handle mismatch.
  #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
  pub async fn call_utility_evaluate(
    &self,
    fn_source: &str,
    args: &[crate::protocol::SerializedValue],
    handles: &[crate::protocol::HandleId],
    frame_id: Option<&str>,
    is_function: Option<bool>,
    return_by_value: bool,
  ) -> Result<crate::js_handle::EvaluateResult> {
    self.ensure_engine_injected().await?;

    let context_id = match frame_id {
      Some(fid) => self.frame_contexts.read().await.get(fid).copied(),
      None => None,
    };

    let args_json = serde_json::to_string(args)?;
    let is_fn_json: serde_json::Value = match is_function {
      Some(true) => serde_json::Value::Bool(true),
      Some(false) => serde_json::Value::Bool(false),
      None => serde_json::Value::Null,
    };

    let count = args.len();

    let mut arguments: Vec<serde_json::Value> = vec![
      serde_json::json!({"value": is_fn_json}),
      serde_json::json!({"value": return_by_value}),
      serde_json::json!({"value": fn_source}),
      serde_json::json!({"value": count}),
      serde_json::json!({"value": args_json}),
    ];
    for handle in handles {
      match handle {
        crate::protocol::HandleId::Cdp(obj_id) => {
          arguments.push(serde_json::json!({"objectId": obj_id}));
        },
        _ => {
          return Err(FerriError::invalid_argument(
            "handles",
            "call_utility_evaluate: non-CDP handle in arg.handles on CDP backend",
          ));
        },
      }
    }

    // Anchor strategy:
    //  1. `executionContextId` (frame_contexts cache hit) — best,
    //     unambiguous, no extra RTT.
    //  2. First handle's `objectId` — Chrome runs the function with
    //     `this` bound to that handle, IN that handle's context. The
    //     wrapper ignores `this`, so this just gives us a free
    //     context anchor without an extra RTT.
    //  3. `Runtime.evaluate` IIFE fallback — avoids `callFunctionOn`
    //     entirely (which needs an anchor) by inlining literal args
    //     into the wrapper expression. Chrome picks the default
    //     execution context for an `evaluate` call without
    //     `contextId`, so no `globalThis` lookup needed.
    //
    //  Prior behaviour fired `Runtime.evaluate("globalThis")` to
    //  obtain an anchoring objectId — costs 1 extra RTT per
    //  evaluate. Path 3 below replaces that with a single
    //  `Runtime.evaluate` (the user's wrapper invocation), saving
    //  one RTT per evaluate that has no handles AND no cached
    //  contextId — i.e. the bench's `page.evaluate(string)` shape.
    if let Some(ctx_id) = context_id {
      let params = serde_json::json!({
        "functionDeclaration": UTILITY_EVAL_WRAPPER,
        "arguments": arguments,
        "returnByValue": return_by_value,
        "awaitPromise": true,
        "executionContextId": ctx_id,
      });
      let response = self.cmd("Runtime.callFunctionOn", params).await?;
      return Self::parse_eval_response(&response, return_by_value);
    }

    if !handles.is_empty() {
      // Anchor on the first handle's objectId — gives Chrome the
      // execution context for free, no extra RTT.
      let anchor = match &handles[0] {
        crate::protocol::HandleId::Cdp(obj_id) => obj_id.clone(),
        _ => {
          return Err(FerriError::invalid_argument(
            "handles",
            "call_utility_evaluate: non-CDP handle in arg.handles on CDP backend",
          ));
        },
      };
      let params = serde_json::json!({
        "functionDeclaration": UTILITY_EVAL_WRAPPER,
        "arguments": arguments,
        "returnByValue": return_by_value,
        "awaitPromise": true,
        "objectId": anchor,
      });
      let response = self.cmd("Runtime.callFunctionOn", params).await?;
      return Self::parse_eval_response(&response, return_by_value);
    }

    // No contextId, no handles — use Runtime.evaluate IIFE. Chrome
    // picks the default execution context. Wrap as
    // `(WRAPPER)(literal_args...)`. Args are JS-literal-safe via
    // `serde_json::to_string` (a valid JSON string is a valid JS
    // string literal too).
    let is_fn_lit = match is_function {
      Some(true) => "true",
      Some(false) => "false",
      None => "null",
    };
    let return_by_value_lit = if return_by_value { "true" } else { "false" };
    let fn_source_lit = serde_json::to_string(fn_source)?;
    let args_json_lit = serde_json::to_string(&args_json)?;
    let expression =
      format!("({UTILITY_EVAL_WRAPPER})({is_fn_lit},{return_by_value_lit},{fn_source_lit},{count},{args_json_lit})");
    let response = self
      .cmd(
        "Runtime.evaluate",
        serde_json::json!({
          "expression": expression,
          "returnByValue": return_by_value,
          "awaitPromise": true,
        }),
      )
      .await?;
    Self::parse_eval_response(&response, return_by_value)
  }

  /// Decode a `Runtime.evaluate` / `Runtime.callFunctionOn` response
  /// produced by the `UtilityScript` wrapper (`UTILITY_EVAL_WRAPPER`
  /// callFunctionOn path).
  /// Both shapes return the same `{ result: { value, objectId, type,
  /// subtype, ... } }` envelope, so the decoder is shared.
  fn parse_eval_response(
    response: &serde_json::Value,
    return_by_value: bool,
  ) -> Result<crate::js_handle::EvaluateResult> {
    if let Some(exception) = response.get("exceptionDetails") {
      let text = exception
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("Evaluation error");
      return Err(FerriError::Backend(text.to_string()));
    }

    let result_obj = response
      .get("result")
      .ok_or_else(|| FerriError::protocol("Runtime.callFunctionOn", "call_utility_evaluate: no result"))?;

    if return_by_value {
      // The wrapper JSON.stringified the isomorphic wire shape so
      // CDP just ships a string back. `result.value` is that string;
      // JSON.parse it and hand to the SerializedValue deserializer.
      // `null` (from `undefined` sentinel) maps to
      // `SerializedValue::Special(Undefined)`.
      let wire = result_obj.get("value").cloned().unwrap_or(serde_json::Value::Null);
      let parsed: crate::protocol::SerializedValue = match wire {
        serde_json::Value::Null => crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Undefined),
        serde_json::Value::String(ref s) => {
          let inner: serde_json::Value = serde_json::from_str(s)
            .map_err(|e| FerriError::Backend(format!("call_utility_evaluate: parse inner JSON: {e}")))?;
          serde_json::from_value(inner)
            .map_err(|e| FerriError::Backend(format!("call_utility_evaluate: parse result: {e}")))?
        },
        other => serde_json::from_value(other)
          .map_err(|e| FerriError::Backend(format!("call_utility_evaluate: parse result: {e}")))?,
      };
      Ok(crate::js_handle::EvaluateResult::Value(parsed))
    } else if let Some(obj_id) = result_obj.get("objectId").and_then(|v| v.as_str()) {
      let is_node = result_obj.get("subtype").and_then(|v| v.as_str()) == Some("node");
      Ok(crate::js_handle::EvaluateResult::Handle(
        crate::js_handle::JSHandleBacking::Remote(crate::js_handle::HandleRemote::Cdp(Arc::from(obj_id))),
        is_node,
      ))
    } else {
      // No objectId — the CDP result is a primitive (number, string,
      // boolean, null, undefined). Playwright's `createHandle`
      // (`/tmp/playwright/packages/playwright-core/src/server/chromium/crProtocolHelper.ts`)
      // produces a value-backed JSHandle here. Parse the inline
      // `value` into our wire shape and ride it back through
      // `JSHandleBacking::Value`.
      let value = result_obj.get("value").cloned().unwrap_or(serde_json::Value::Null);
      let mut ctx = crate::protocol::SerializationContext::default();
      let serialized = if value.is_null() {
        // CDP encodes `null` literally but encodes `undefined` as a
        // missing `value` field with `type: "undefined"`. Distinguish
        // the two via `type` so jsonValue round-trips faithfully.
        let ty = result_obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if ty == "undefined" {
          crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Undefined)
        } else {
          crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Null)
        }
      } else {
        crate::protocol::SerializedValue::from_json(&value, &mut ctx)
      };
      Ok(crate::js_handle::EvaluateResult::Handle(
        crate::js_handle::JSHandleBacking::Value(serialized),
        false,
      ))
    }
  }

  // ---- Frames ----

  pub async fn get_frame_tree(&self) -> Result<Vec<super::FrameInfo>> {
    // Consume the seeded tree captured during the per-page parallel
    // `enable_domains` batch. Skips the per-call `Page.getFrameTree`
    // RTT for the first read after `new_page` — every subsequent read
    // (e.g. after a navigation or iframe attach) goes back to the
    // network. Mirrors Playwright's `frameManager._frameTree` which
    // is seeded from the `Page.getFrameTree` it fires inside
    // `FrameSession._initialize` and then maintained via events
    // (`/tmp/playwright/packages/playwright-core/src/server/chromium/crPage.ts:495-549`).
    {
      let mut guard = match self.seeded_frame_tree.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
      };
      if let Some(tree) = guard.take() {
        return Ok(tree);
      }
    }
    let result = self.cmd("Page.getFrameTree", super::empty_params()).await?;

    let mut frames = Vec::new();
    if let Some(tree) = result.get("frameTree") {
      collect_frames(tree, &mut frames);
    }

    // Chrome's `Page.getFrameTree` returns the iframe's HTML `name`
    // attribute on the `frame.name` field, but srcdoc iframes can
    // report an empty `name` until the child document settles — the
    // attribute is populated on the parent's element but not yet on
    // the CDP-side frame metadata. Fill in the missing names by
    // evaluating `window.name` in each empty-named child frame.
    let child_indices: Vec<usize> = frames
      .iter()
      .enumerate()
      .filter(|(_, f)| f.parent_frame_id.is_some() && f.name.is_empty())
      .map(|(i, _)| i)
      .collect();
    if !child_indices.is_empty() {
      let frame_ids: Vec<String> = child_indices.iter().map(|&i| frames[i].frame_id.clone()).collect();
      let futs: Vec<_> = frame_ids
        .iter()
        .map(|fid| self.evaluate_in_frame("window.name", fid))
        .collect();
      let results = futures::future::join_all(futs).await;
      for (idx, result) in child_indices.into_iter().zip(results) {
        if let Ok(Some(val)) = result {
          if let Some(name) = val.as_str() {
            if !name.is_empty() {
              frames[idx].name = name.to_string();
            }
          }
        }
      }
    }
    Ok(frames)
  }

  /// Deterministic iframe-element -> content-frame id via
  /// `DOM.describeNode` on the element's remote object. Replaces the
  /// fragile name/url cache heuristic for unnamed / `srcdoc` iframes.
  pub async fn content_frame_id(&self, object_id: &str) -> Result<Option<String>> {
    let res = self
      .cmd("DOM.describeNode", serde_json::json!({ "objectId": object_id }))
      .await?;
    Ok(
      res
        .get("node")
        .and_then(|n| n.get("frameId"))
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string),
    )
  }

  pub async fn evaluate_in_frame(&self, expression: &str, frame_id: &str) -> Result<Option<serde_json::Value>> {
    let context_id = {
      let contexts = self.frame_contexts.read().await;
      contexts.get(frame_id).copied()
    };

    if let Some(ctx_id) = context_id {
      let result = self
        .cmd(
          "Runtime.evaluate",
          serde_json::json!({
              "expression": expression,
              "contextId": ctx_id,
              "returnByValue": true,
              "awaitPromise": true,
          }),
        )
        .await?;

      if let Some(exception) = result.get("exceptionDetails") {
        let text = exception
          .get("text")
          .and_then(|v| v.as_str())
          .unwrap_or("Evaluation error");
        return Err(FerriError::Backend(text.to_string()));
      }
      Ok(result.get("result").and_then(|r| r.get("value")).cloned())
    } else {
      Err(FerriError::Backend(format!(
        "No execution context found for frame '{frame_id}'. Frame may not be loaded yet."
      )))
    }
  }

  // ---- Elements ----

  pub async fn find_element(&self, selector: &str) -> Result<AnyElement> {
    let doc = self.cmd("DOM.getDocument", serde_json::json!({"depth": 0})).await?;
    let root_id = doc
      .get("root")
      .and_then(|r| r.get("nodeId"))
      .and_then(serde_json::Value::as_i64)
      .ok_or_else(|| FerriError::protocol("DOM.getDocument", "No document root"))?;

    let result = self
      .cmd(
        "DOM.querySelector",
        serde_json::json!({"nodeId": root_id, "selector": selector}),
      )
      .await?;

    let node_id = result
      .get("nodeId")
      .and_then(serde_json::Value::as_i64)
      .ok_or_else(|| FerriError::protocol("DOM.querySelector", format!("'{selector}' not found")))?;

    if node_id == 0 {
      return Err(FerriError::invalid_selector(selector, "not found"));
    }

    Ok(T::wrap_element(CdpElement {
      transport: self.transport.clone(),
      session_id: self.session_id.clone(),
      handles: Arc::new(tokio::sync::Mutex::new(CdpElementHandles {
        node_id: Some(node_id),
        object_id: None,
      })),
    }))
  }

  /// Construct a [`CdpElement`] directly from a `Runtime.RemoteObjectId`
  /// without re-resolving through the DOM. Used by
  /// [`crate::backend::element_from_remote`] when a [`crate::js_handle::JSHandle`]
  /// turns out to wrap a DOM node and needs to be re-packaged as an
  /// [`crate::element_handle::ElementHandle`] — the remote is already
  /// addressable, so we can skip a round-trip by seeding the element's
  /// `object_id` slot directly.
  pub(crate) fn element_from_object_id(&self, object_id: Arc<str>) -> CdpElement<T> {
    CdpElement {
      transport: self.transport.clone(),
      session_id: self.session_id.clone(),
      handles: Arc::new(tokio::sync::Mutex::new(CdpElementHandles {
        node_id: None,
        object_id: Some(object_id),
      })),
    }
  }

  /// Evaluate `js` in the execution context of `frame_id` (or the main
  /// page when `frame_id` is `None`) and return the resulting DOM
  /// element. Used by `Locator` to scope action-method resolution to
  /// the locator's bound `Frame` — Playwright parity.
  pub async fn evaluate_to_element(&self, js: &str, frame_id: Option<&str>) -> Result<AnyElement> {
    // Note: previously fired `DOM.getDocument` here but discarded the
    // result. It was a leftover from when this path used
    // `DOM.querySelector` (which DOES require the agent's DOM tree
    // to be populated). The current `Runtime.evaluate` path returns
    // a `RemoteObjectId` directly from the renderer's V8 context,
    // independent of the DOM agent state — no priming needed.
    // Saves 1 RTT per locator action / element handle resolve.

    // Resolve the frame's execution context id (None → main page).
    let context_id = match frame_id {
      Some(fid) => {
        let contexts = self.frame_contexts.read().await;
        contexts.get(fid).copied()
      },
      None => None,
    };

    let mut params = serde_json::json!({
      "expression": js,
      "returnByValue": false,
    });
    if let Some(ctx_id) = context_id {
      params["contextId"] = serde_json::json!(ctx_id);
    }

    let result = self.cmd("Runtime.evaluate", params).await?;

    if let Some(exception) = result.get("exceptionDetails") {
      // `text` is typically the short prefix Chrome attaches
      // ("Uncaught"); the real message lives on
      // `exception.exception.description` (V8 format) or
      // `exception.exception.value`. Combine so the caller's
      // `err.contains("strict mode violation")` / similar checks
      // actually see the engine-thrown payload.
      let text = exception.get("text").and_then(|v| v.as_str()).unwrap_or("");
      let inner = exception
        .get("exception")
        .and_then(|e| {
          e.get("description")
            .and_then(|v| v.as_str())
            .or_else(|| e.get("value").and_then(|v| v.as_str()))
        })
        .unwrap_or("");
      let combined = match (text.is_empty(), inner.is_empty()) {
        (false, false) => format!("{text}: {inner}"),
        (false, true) => text.to_string(),
        (true, false) => inner.to_string(),
        (true, true) => "Evaluation error".to_string(),
      };
      return Err(FerriError::evaluation(combined));
    }

    let object_id = result
      .get("result")
      .and_then(|r| r.get("objectId"))
      .and_then(|v| v.as_str())
      .ok_or_else(|| FerriError::protocol("Runtime.evaluate", "JS did not return a DOM element"))?;

    Ok(T::wrap_element(CdpElement {
      transport: self.transport.clone(),
      session_id: self.session_id.clone(),
      handles: Arc::new(tokio::sync::Mutex::new(CdpElementHandles {
        node_id: None,
        object_id: Some(Arc::from(object_id)),
      })),
    }))
  }

  // ---- Content ----

  pub async fn content(&self) -> Result<String> {
    let result = self
      .cmd(
        "Runtime.evaluate",
        serde_json::json!({
            "expression": "document.documentElement.outerHTML",
            "returnByValue": true,
        }),
      )
      .await?;
    Ok(
      result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string(),
    )
  }

  pub async fn set_content(&self, html: &str) -> Result<()> {
    let frame_id = self
      .main_frame_id
      .get_or_try_init(|| async {
        let tree = self.cmd("Page.getFrameTree", super::empty_params()).await?;
        tree
          .get("frameTree")
          .and_then(|f| f.get("frame"))
          .and_then(|f| f.get("id"))
          .and_then(|v| v.as_str())
          .map(std::string::ToString::to_string)
          .ok_or_else(|| FerriError::protocol("Page.getFrameTree", "no main frame"))
      })
      .await?;

    // Selector engine is already injected via Page.addScriptToEvaluateOnNewDocument
    // during page setup, so setDocumentContent triggers it automatically.
    self
      .cmd(
        "Page.setDocumentContent",
        serde_json::json!({"frameId": frame_id, "html": html}),
      )
      .await?;
    Ok(())
  }

  // ---- Screenshots ----

  pub async fn screenshot(&self, opts: ScreenshotOpts) -> Result<Vec<u8>> {
    // Pre-capture: set up per-field state and collect teardown tokens.
    let (style_installed, mask_installed) = self.screenshot_install_dom(&opts).await?;
    let bg_installed = self.screenshot_install_transparent_bg(&opts).await?;
    let params = self.screenshot_build_params(&opts).await?;

    let result = self.cmd("Page.captureScreenshot", params).await;

    // Teardown — always runs so user interaction after a failure sees
    // a pristine page state.
    if style_installed {
      let _ = self.evaluate(crate::backend::screenshot_js::uninstall_style_js()).await;
    }
    if mask_installed {
      let _ = self.evaluate(crate::backend::screenshot_js::uninstall_mask_js()).await;
    }
    if bg_installed {
      let _ = self
        .cmd(
          "Emulation.setDefaultBackgroundColorOverride",
          serde_json::Value::Object(serde_json::Map::default()),
        )
        .await;
    }

    let data = result?
      .get("data")
      .and_then(|v| v.as_str().map(String::from))
      .ok_or_else(|| FerriError::backend("No screenshot data"))?;
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
      .map_err(|e| FerriError::Backend(format!("Decode screenshot: {e}")))
  }

  /// Install the DOM-side screenshot overrides (caret hide, user style,
  /// animation pause, mask overlays) via `Runtime.evaluate`. Returns
  /// `(style_installed, mask_installed)` so the caller knows which
  /// teardown calls to make.
  async fn screenshot_install_dom(&self, opts: &ScreenshotOpts) -> Result<(bool, bool)> {
    let css = crate::backend::screenshot_js::build_css(opts);
    let style_installed = if css.is_empty() {
      false
    } else {
      self
        .evaluate(&crate::backend::screenshot_js::install_style_js(&css))
        .await?;
      true
    };
    let mask_installed = if let Some(js) = crate::backend::screenshot_js::install_mask_js(opts) {
      self.evaluate(&js).await?;
      true
    } else {
      false
    };
    Ok((style_installed, mask_installed))
  }

  /// Enable transparent-background capture via CDP
  /// `Emulation.setDefaultBackgroundColorOverride` when
  /// `opts.omit_background` is set. Returns `true` if the override
  /// was installed (caller must reverse it).
  async fn screenshot_install_transparent_bg(&self, opts: &ScreenshotOpts) -> Result<bool> {
    if !opts.omit_background {
      return Ok(false);
    }
    self
      .cmd(
        "Emulation.setDefaultBackgroundColorOverride",
        serde_json::json!({"color": {"r": 0, "g": 0, "b": 0, "a": 0}}),
      )
      .await?;
    Ok(true)
  }

  /// Build the `Page.captureScreenshot` parameter object from
  /// `opts.format`, `opts.quality`, `opts.clip`, `opts.full_page`,
  /// and `opts.scale`. Caller-supplied clips win over full-page
  /// computation; CSS scale translates to `clip.scale = 1 / devicePixelRatio`.
  async fn screenshot_build_params(&self, opts: &ScreenshotOpts) -> Result<serde_json::Value> {
    use crate::backend::ScreenshotScale;
    let format_str = match opts.format {
      ImageFormat::Png => "png",
      ImageFormat::Jpeg => "jpeg",
      ImageFormat::Webp => "webp",
    };
    let mut params = serde_json::json!({"format": format_str});
    if let Some(q) = opts.quality {
      params["quality"] = serde_json::json!(q);
    }
    let css_scale = matches!(opts.scale, Some(ScreenshotScale::Css));
    if let Some(rect) = opts.clip {
      let scale = if css_scale {
        1.0 / self.device_pixel_ratio().await.unwrap_or(1.0)
      } else {
        1.0
      };
      params["clip"] = serde_json::json!({
          "x": rect.x, "y": rect.y, "width": rect.width, "height": rect.height, "scale": scale
      });
      params["captureBeyondViewport"] = serde_json::json!(true);
    } else if opts.full_page {
      let metrics = self.cmd("Page.getLayoutMetrics", super::empty_params()).await?;
      let content_size = metrics.get("contentSize");
      let w = content_size
        .and_then(|c| c.get("width"))
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(800.0);
      let h = content_size
        .and_then(|c| c.get("height"))
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(600.0);
      let mut scale = metrics
        .get("visualViewport")
        .and_then(|v| v.get("scale"))
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(1.0);
      if css_scale {
        scale /= self.device_pixel_ratio().await.unwrap_or(1.0);
      }
      params["clip"] = serde_json::json!({
          "x": 0, "y": 0, "width": w, "height": h, "scale": scale
      });
      params["captureBeyondViewport"] = serde_json::json!(true);
    }
    Ok(params)
  }

  /// Fetch the target's current `window.devicePixelRatio`. Used to
  /// translate the `scale: "css"` option into CDP's per-clip scale
  /// multiplier — `clip.scale = 1/DPR` means "one image pixel per
  /// CSS pixel" even on Retina.
  async fn device_pixel_ratio(&self) -> Result<f64> {
    let v = self.evaluate("window.devicePixelRatio || 1").await?;
    Ok(v.and_then(|v| v.as_f64()).unwrap_or(1.0))
  }

  pub async fn screenshot_element(&self, selector: &str, format: ImageFormat) -> Result<Vec<u8>> {
    let js = format!(
      r"(function(){{
                const el = document.querySelector('{}');
                if (!el) return null;
                const r = el.getBoundingClientRect();
                return JSON.stringify({{x:r.x,y:r.y,width:r.width,height:r.height}});
            }})()",
      selector.replace('\'', "\\'")
    );
    let result = self.evaluate(&js).await?;
    let rect_str = result
      .and_then(|v| v.as_str().map(std::string::ToString::to_string))
      .ok_or_else(|| FerriError::invalid_selector(selector, "not found"))?;
    let rect: serde_json::Value =
      serde_json::from_str(&rect_str).map_err(|e| FerriError::Backend(format!("Parse rect: {e}")))?;

    let format_str = match format {
      ImageFormat::Png => "png",
      ImageFormat::Jpeg => "jpeg",
      ImageFormat::Webp => "webp",
    };

    let result = self
      .cmd(
        "Page.captureScreenshot",
        serde_json::json!({
            "format": format_str,
            "clip": {
                "x": rect["x"], "y": rect["y"],
                "width": rect["width"], "height": rect["height"],
                "scale": 1
            }
        }),
      )
      .await?;
    let data = result
      .get("data")
      .and_then(|v| v.as_str())
      .ok_or_else(|| FerriError::backend("No screenshot data"))?;
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
      .map_err(|e| FerriError::Backend(format!("Decode: {e}")))
  }

  // ---- Screencast (video recording) ----

  /// Start CDP screencast. Chrome will emit `Page.screencastFrame` events with JPEG data.
  ///
  /// Returns `(frame_rx, shutdown_tx)`. The caller drives shutdown by
  /// sending `()` on `shutdown_tx` — the listener drains any in-flight
  /// `screencastFrame` events that arrived between
  /// `Page.stopScreencast` and the shutdown signal, then drops the
  /// frame sender so the consumer's `recv()` returns `None`. This is
  /// deterministic: no `sleep` hack to "give frames time to arrive".
  pub async fn start_screencast(
    &self,
    quality: u8,
    max_width: u32,
    max_height: u32,
  ) -> Result<(
    tokio::sync::mpsc::UnboundedReceiver<(Vec<u8>, f64)>,
    tokio::sync::oneshot::Sender<()>,
  )> {
    // Subscribe to the event stream BEFORE firing `Page.startScreencast`
    // so the very first `Page.screencastFrame` event isn't lost in the
    // gap between the command being sent and the listener subscribing.
    // For short recordings (fast goto+close) that single missed frame
    // is the difference between ffmpeg producing a valid file and
    // exiting with "Output file does not contain any stream".
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    Self::spawn_screencast_listener(self.transport.clone(), self.session_id.clone(), tx, shutdown_rx);

    self
      .cmd(
        "Page.startScreencast",
        serde_json::json!({
          "format": "jpeg",
          "quality": quality,
          "maxWidth": max_width,
          "maxHeight": max_height,
          "everyNthFrame": 1,
        }),
      )
      .await?;

    Ok((rx, shutdown_tx))
  }

  /// Stop CDP screencast.
  pub async fn stop_screencast(&self) -> Result<()> {
    self.cmd("Page.stopScreencast", serde_json::json!({})).await?;
    Ok(())
  }

  /// Background task: listens for `Page.screencastFrame` events, decodes JPEG, acks, forwards.
  ///
  /// Passes raw JPEG frames to the channel. Frame interpolation (gap-filling) is handled
  /// by the video recorder layer, not here. ACK is sent immediately and non-blocking
  /// (matching Playwright's approach) so Chrome sends the next frame ASAP.
  ///
  /// `shutdown_rx` provides cooperative teardown. The select loop is
  /// `biased` so any event already buffered in the broadcast channel
  /// is processed before the shutdown signal is observed — that's
  /// what keeps short recordings from losing the trailing frames
  /// Chrome emitted just before `Page.stopScreencast` landed.
  fn spawn_screencast_listener(
    transport: Arc<T>,
    session_id: Option<Arc<str>>,
    frame_tx: tokio::sync::mpsc::UnboundedSender<(Vec<u8>, f64)>,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
  ) {
    tokio::spawn(async move {
      let mut rx = transport.subscribe_event_method("Page.screencastFrame");
      let mut shutdown_rx = shutdown_rx;
      loop {
        let event = tokio::select! {
          biased;
          ev = rx.recv() => match ev {
            Ok(ev) => ev,
            Err(_) => break,
          },
          _ = &mut shutdown_rx => {
            // Drain any events already buffered in the broadcast
            // subscription -- those are the frames Chrome shipped
            // before `Page.stopScreencast` returned. Once the
            // subscription is empty, drop `frame_tx` so the consumer
            // sees end-of-stream.
            while let Ok(ev) = rx.try_recv() {
              Self::process_screencast_event(&ev, &session_id, &transport, &frame_tx);
            }
            break;
          },
        };
        Self::process_screencast_event(&event, &session_id, &transport, &frame_tx);
      }
    });
  }

  #[allow(
    clippy::ref_option,
    reason = "matches caller signature inside spawn_screencast_listener"
  )]
  fn process_screencast_event(
    event: &serde_json::Value,
    session_id: &Option<Arc<str>>,
    transport: &Arc<T>,
    frame_tx: &tokio::sync::mpsc::UnboundedSender<(Vec<u8>, f64)>,
  ) {
    if let Some(expected_sid) = session_id {
      let event_sid = event.get("sessionId").and_then(|v| v.as_str());
      if event_sid != Some(&**expected_sid) {
        return;
      }
    }
    if event.get("method").and_then(|m| m.as_str()) != Some("Page.screencastFrame") {
      return;
    }
    let Some(params) = event.get("params") else { return };
    let timestamp = params
      .get("metadata")
      .and_then(|m| m.get("timestamp"))
      .and_then(serde_json::Value::as_f64)
      .unwrap_or_else(|| {
        std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .unwrap_or_default()
          .as_secs_f64()
      });
    if let Some(data_str) = params.get("data").and_then(|v| v.as_str()) {
      if let Ok(jpeg_bytes) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data_str) {
        let _ = frame_tx.send((jpeg_bytes, timestamp));
      }
    }
    let ack_id = params.get("sessionId").and_then(serde_json::Value::as_i64).unwrap_or(0);
    let t = transport.clone();
    let sid = session_id.clone();
    tokio::spawn(async move {
      let _ = t
        .send_command(
          sid.as_deref(),
          "Page.screencastFrameAck",
          &serde_json::json!({ "sessionId": ack_id }),
        )
        .await;
    });
  }

  // ---- PDF ----

  /// Generate a PDF of the current page using CDP `Page.printToPDF`.
  ///
  /// Param mapping mirrors
  /// `/tmp/playwright/packages/playwright-core/src/server/chromium/crPdf.ts::CRPDF.generate`
  /// 1:1. When `format` is set, its canonical inch dimensions override
  /// `width`/`height`. Otherwise `width`/`height` (if present) are converted
  /// to inches via [`crate::options::PdfSize::to_inches`]. Margins default
  /// to `0` per side and are converted the same way. Every optional field
  /// falls back to Playwright's default when `None`.
  pub async fn pdf(&self, opts: crate::options::PdfOptions) -> Result<Vec<u8>> {
    let mut paper_width = 8.5_f64;
    let mut paper_height = 11.0_f64;
    if let Some(ref format) = opts.format {
      if let Some((w, h)) = crate::options::pdf_paper_format_size(format) {
        paper_width = w;
        paper_height = h;
      } else {
        return Err(FerriError::invalid_argument(
          "format",
          format!("unknown paper format: {format}"),
        ));
      }
    } else {
      if let Some(ref w) = opts.width {
        paper_width = w.to_inches();
      }
      if let Some(ref h) = opts.height {
        paper_height = h.to_inches();
      }
    }

    let margin = opts.margin.unwrap_or_default();
    let margin_top = margin.top.as_ref().map_or(0.0, crate::options::PdfSize::to_inches);
    let margin_right = margin.right.as_ref().map_or(0.0, crate::options::PdfSize::to_inches);
    let margin_bottom = margin.bottom.as_ref().map_or(0.0, crate::options::PdfSize::to_inches);
    let margin_left = margin.left.as_ref().map_or(0.0, crate::options::PdfSize::to_inches);

    let params = serde_json::json!({
      "landscape": opts.landscape.unwrap_or(false),
      "displayHeaderFooter": opts.display_header_footer.unwrap_or(false),
      "headerTemplate": opts.header_template.unwrap_or_default(),
      "footerTemplate": opts.footer_template.unwrap_or_default(),
      "printBackground": opts.print_background.unwrap_or(false),
      "scale": opts.scale.unwrap_or(1.0),
      "paperWidth": paper_width,
      "paperHeight": paper_height,
      "marginTop": margin_top,
      "marginBottom": margin_bottom,
      "marginLeft": margin_left,
      "marginRight": margin_right,
      "pageRanges": opts.page_ranges.unwrap_or_default(),
      "preferCSSPageSize": opts.prefer_css_page_size.unwrap_or(false),
      "generateTaggedPDF": opts.tagged.unwrap_or(false),
      "generateDocumentOutline": opts.outline.unwrap_or(false),
    });

    let result = self.cmd("Page.printToPDF", params).await?;
    let data = result
      .get("data")
      .and_then(|v| v.as_str())
      .ok_or_else(|| FerriError::backend("No PDF data"))?;
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
      .map_err(|e| FerriError::Backend(format!("Decode PDF: {e}")))
  }

  // ---- File upload ----

  pub async fn set_file_input(&self, selector: &str, paths: &[String]) -> Result<()> {
    // Resolve the selector to a Runtime.RemoteObjectId via Runtime.evaluate
    // — `DOM.nodeId` is invalidated by document lifecycle events (e.g. the
    // page re-issuing `DOM.setChildNodes`), so a getDocument → querySelector
    // → describeNode pipeline races against the renderer and produces
    // `Could not find node with given id` under CI load.
    // Playwright's `setInputFilePaths` (crPage.ts:312) passes
    // `objectId: handle._objectId` for exactly this reason — the
    // RemoteObject handle stays valid for as long as the JS reference is
    // reachable.
    let escaped = selector.replace('\\', "\\\\").replace('"', "\\\"");
    let expression = format!("document.querySelector(\"{escaped}\")");
    let result = self
      .cmd(
        "Runtime.evaluate",
        serde_json::json!({
            "expression": expression,
            "returnByValue": false,
            "awaitPromise": false,
        }),
      )
      .await?;
    let object_id = result
      .get("result")
      .and_then(|r| r.get("objectId"))
      .and_then(serde_json::Value::as_str)
      .ok_or_else(|| FerriError::protocol("Runtime.evaluate", "Element not found"))?
      .to_string();

    let set_result = self
      .cmd(
        "DOM.setFileInputFiles",
        serde_json::json!({
            "files": paths,
            "objectId": object_id,
        }),
      )
      .await;

    // Release the RemoteObject so the renderer can GC it.
    let _ = self
      .cmd("Runtime.releaseObject", serde_json::json!({ "objectId": object_id }))
      .await;

    set_result?;
    Ok(())
  }

  // ---- Accessibility ----

  pub async fn accessibility_tree(&self) -> Result<Vec<AxNodeData>> {
    self.accessibility_tree_with_depth(-1).await
  }

  pub async fn accessibility_tree_with_depth(&self, depth: i32) -> Result<Vec<AxNodeData>> {
    let result = self
      .cmd("Accessibility.getFullAXTree", serde_json::json!({"depth": depth}))
      .await?;

    let nodes = result
      .get("nodes")
      .and_then(|n| n.as_array())
      .ok_or_else(|| FerriError::protocol("Accessibility.getFullAXTree", "No a11y nodes"))?;

    Ok(
      nodes
        .iter()
        .map(|node| {
          let get_ax_value = |field: &str| -> Option<String> {
            node
              .get(field)
              .and_then(|v| v.get("value"))
              .and_then(|v| v.as_str())
              .map(std::string::ToString::to_string)
          };

          let properties = node
            .get("properties")
            .and_then(|p| p.as_array())
            .map(|props| {
              props
                .iter()
                .map(|p| AxProperty {
                  name: p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_lowercase(),
                  value: p.get("value").and_then(|v| v.get("value")).cloned(),
                })
                .collect()
            })
            .unwrap_or_default();

          AxNodeData {
            node_id: node.get("nodeId").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            parent_id: node
              .get("parentId")
              .and_then(|v| v.as_str())
              .map(std::string::ToString::to_string),
            backend_dom_node_id: node.get("backendDOMNodeId").and_then(serde_json::Value::as_i64),
            ignored: node
              .get("ignored")
              .and_then(serde_json::Value::as_bool)
              .unwrap_or(false),
            role: get_ax_value("role"),
            name: get_ax_value("name"),
            description: get_ax_value("description"),
            properties,
          }
        })
        .collect(),
    )
  }

  // ---- Input ----

  pub async fn click_at(&self, x: f64, y: f64) -> Result<()> {
    self.click_at_opts(x, y, "left", 1).await
  }

  pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<()> {
    let press = self.cmd(
      "Input.dispatchMouseEvent",
      serde_json::json!({"type": "mousePressed", "x": x, "y": y, "button": button, "clickCount": click_count}),
    );
    let release = self.cmd(
      "Input.dispatchMouseEvent",
      serde_json::json!({"type": "mouseReleased", "x": x, "y": y, "button": button, "clickCount": click_count}),
    );
    let _ = tokio::try_join!(press, release)?;
    if let Ok(mut guard) = self.last_cursor_pos.lock() {
      *guard = Some((x, y));
    }
    Ok(())
  }

  /// Single-click fast path: `click_count == 1`, no delay, `steps == 1`.
  /// Pipelines the (optional pre-press move +) press + release in one
  /// `try_join!` so the renderer sees them back-to-back at one CDP RTT.
  async fn single_click_fast_path(&self, x: f64, y: f64, button: &str, mods: u32, skip_move: bool) -> Result<()> {
    let press = self.cmd(
      "Input.dispatchMouseEvent",
      serde_json::json!({
        "type": "mousePressed",
        "x": x,
        "y": y,
        "button": button,
        "clickCount": 1,
        "modifiers": mods,
      }),
    );
    let release = self.cmd(
      "Input.dispatchMouseEvent",
      serde_json::json!({
        "type": "mouseReleased",
        "x": x,
        "y": y,
        "button": button,
        "clickCount": 1,
        "modifiers": mods,
      }),
    );
    if skip_move {
      let _ = tokio::try_join!(press, release)?;
    } else {
      let moved = self.cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({
          "type": "mouseMoved",
          "x": x,
          "y": y,
          "modifiers": mods,
        }),
      );
      let _ = tokio::try_join!(moved, press, release)?;
    }
    Ok(())
  }

  /// Dispatch a click at `(x, y)` honoring the full Playwright option
  /// bag: `button`, `click_count`, modifiers bitmask, delay between
  /// press/release, and `steps` interpolated mousemoves from the last
  /// cursor position to the target. Modifier keydown/keyup is done by
  /// the caller via [`Self::press_modifiers`] /
  /// [`Self::release_modifiers`].
  pub async fn click_at_with(&self, x: f64, y: f64, args: &super::BackendClickArgs) -> Result<()> {
    let button = args.button.as_cdp();
    let mods = args.modifiers_bitmask;
    // Steps-1 intermediate mousemoves + one final at (x,y). Playwright
    // default is `steps: 1` → single mousemove at dest. Mirror that by
    // emitting a `mouseMoved` at the target before press so the page
    // sees the move even when we can't track the prior cursor.
    let steps = args.steps.max(1);
    // Read the prior cursor position synchronously. When `steps == 1`
    // and the prior position is already at the click target, the
    // pre-press `mouseMoved` is a no-op event-wise (Chrome treats
    // identical-position consecutive moves as a single hover state),
    // but the RTT itself still costs ~1 CDP RTT. Skip it on the
    // common bench shape (back-to-back clicks at the same button).
    // Mirrors Playwright's `_lastPosition` short-circuit in
    // `/tmp/playwright/packages/playwright-core/src/server/chromium/crInput.ts`
    // (Mouse class). The short-circuit is bypassed when `steps > 1`
    // (caller asked for an animated move) or when no prior position
    // is known (first click — must seed cursor in the renderer).
    let skip_move = steps == 1
      && match self.last_cursor_pos.lock() {
        Ok(g) => matches!(*g, Some((px, py)) if (px - x).abs() < 0.5 && (py - y).abs() < 0.5),
        Err(_) => false,
      };
    if args.click_count == 1 && args.delay_ms == 0 && steps == 1 {
      self.single_click_fast_path(x, y, button, mods, skip_move).await?;
      if let Ok(mut guard) = self.last_cursor_pos.lock() {
        *guard = Some((x, y));
      }
      return Ok(());
    }

    if !skip_move {
      for i in 1..=steps {
        let t = f64::from(i) / f64::from(steps);
        let sx = x * t; // conservative: interpolate from (0,0) when we lack prior-pos state
        let sy = y * t;
        self
          .cmd(
            "Input.dispatchMouseEvent",
            serde_json::json!({
              "type": "mouseMoved",
              "x": if i == steps { x } else { sx },
              "y": if i == steps { y } else { sy },
              "modifiers": mods,
            }),
          )
          .await?;
      }
    }
    for n in 1..=args.click_count {
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({
            "type": "mousePressed",
            "x": x,
            "y": y,
            "button": button,
            "clickCount": n,
            "modifiers": mods,
          }),
        )
        .await?;
      if args.delay_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(args.delay_ms)).await;
      }
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({
            "type": "mouseReleased",
            "x": x,
            "y": y,
            "button": button,
            "clickCount": n,
            "modifiers": mods,
          }),
        )
        .await?;
    }
    // Record final cursor position for the next click's
    // skip-redundant-mouseMoved short-circuit.
    if let Ok(mut guard) = self.last_cursor_pos.lock() {
      *guard = Some((x, y));
    }
    Ok(())
  }

  /// Dispatch a hover at `(x, y)`: `steps` interpolated `mouseMoved`
  /// events with the caller's CDP `modifiers` bitmask on each, ending
  /// at `(x, y)` exactly. No `mousePressed` / `mouseReleased`.
  pub async fn hover_at_with(&self, x: f64, y: f64, args: &super::BackendHoverArgs) -> Result<()> {
    let mods = args.modifiers_bitmask;
    let steps = args.steps.max(1);
    let skip_move = steps == 1
      && match self.last_cursor_pos.lock() {
        Ok(g) => matches!(*g, Some((px, py)) if (px - x).abs() < 0.5 && (py - y).abs() < 0.5),
        Err(_) => false,
      };
    if !skip_move {
      for i in 1..=steps {
        let t = f64::from(i) / f64::from(steps);
        let sx = if i == steps { x } else { x * t };
        let sy = if i == steps { y } else { y * t };
        self
          .cmd(
            "Input.dispatchMouseEvent",
            serde_json::json!({
              "type": "mouseMoved",
              "x": sx,
              "y": sy,
              "modifiers": mods,
            }),
          )
          .await?;
      }
    }
    if let Ok(mut guard) = self.last_cursor_pos.lock() {
      *guard = Some((x, y));
    }
    Ok(())
  }

  /// Dispatch a native tap at `(x, y)` via CDP `Input.dispatchTouchEvent`:
  /// `touchStart` with a single `TouchPoint { x, y }` immediately followed
  /// by `touchEnd` with an empty `touchPoints` array. Matches Playwright's
  /// `server/chromium/crInput.ts::RawTouchscreenImpl::tap` (commit
  /// reference: `/tmp/playwright/packages/playwright-core/src/server/chromium/crInput.ts:174`).
  /// Modifier bitmask rides on each event so the page sees
  /// `event.shiftKey` etc. as expected.
  ///
  /// Before the first dispatch we flip `Emulation.setTouchEmulationEnabled
  /// { enabled: true, maxTouchPoints: 1 }`. Chromium needs the emulator
  /// enabled for `Input.dispatchTouchEvent` to actually route the events
  /// through the renderer's touch hit-tester — without it, the protocol
  /// accepts the call but no DOM `touchstart` / `pointerup(touch)`
  /// listener fires. Playwright's `BrowserContextOptions.hasTouch` wires
  /// this on context creation; we opt in lazily on first tap so callers
  /// who never tap pay nothing.
  pub async fn tap_at_with(&self, x: f64, y: f64, args: &super::BackendTapArgs) -> Result<()> {
    let mods = args.modifiers_bitmask;
    self
      .cmd(
        "Emulation.setTouchEmulationEnabled",
        serde_json::json!({ "enabled": true, "maxTouchPoints": 1 }),
      )
      .await?;
    self
      .cmd(
        "Input.dispatchTouchEvent",
        serde_json::json!({
          "type": "touchStart",
          "modifiers": mods,
          "touchPoints": [{ "x": x, "y": y }],
        }),
      )
      .await?;
    self
      .cmd(
        "Input.dispatchTouchEvent",
        serde_json::json!({
          "type": "touchEnd",
          "modifiers": mods,
          "touchPoints": [],
        }),
      )
      .await?;
    Ok(())
  }

  /// Press each modifier in `mods` via CDP
  /// `Input.dispatchKeyEvent { type: "keyDown" }`. `key` is the
  /// platform-resolved key name (e.g. `"Meta"` on macOS for
  /// `ControlOrMeta`) and `code` is the DOM `KeyboardEvent.code`.
  pub async fn press_modifiers(&self, mods: &[crate::options::Modifier]) -> Result<()> {
    for md in mods {
      self
        .cmd(
          "Input.dispatchKeyEvent",
          serde_json::json!({
            "type": "keyDown",
            "key": md.key_name(),
            "code": md.key_code(),
            "modifiers": u32::from(md.cdp_bit()),
          }),
        )
        .await?;
    }
    Ok(())
  }

  /// Release each modifier in `mods` via CDP
  /// `Input.dispatchKeyEvent { type: "keyUp" }`. Iterates in reverse
  /// order to match Playwright's unwind behavior in
  /// `/tmp/playwright/packages/playwright-core/src/server/input.ts`.
  pub async fn release_modifiers(&self, mods: &[crate::options::Modifier]) -> Result<()> {
    for md in mods.iter().rev() {
      self
        .cmd(
          "Input.dispatchKeyEvent",
          serde_json::json!({
            "type": "keyUp",
            "key": md.key_name(),
            "code": md.key_code(),
          }),
        )
        .await?;
    }
    Ok(())
  }

  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<()> {
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mouseMoved", "x": x, "y": y}),
      )
      .await?;
    if let Ok(mut guard) = self.last_cursor_pos.lock() {
      *guard = Some((x, y));
    }
    Ok(())
  }

  pub async fn move_mouse_smooth(&self, from_x: f64, from_y: f64, to_x: f64, to_y: f64, steps: u32) -> Result<()> {
    let steps = steps.max(1);
    for i in 0..=steps {
      let t = f64::from(i) / f64::from(steps);
      let ease = t * t * (3.0 - 2.0 * t);
      let x = from_x + (to_x - from_x) * ease;
      let y = from_y + (to_y - from_y) * ease;
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mouseMoved", "x": x, "y": y}),
        )
        .await?;
    }
    Ok(())
  }

  pub async fn click_and_drag(&self, from: (f64, f64), to: (f64, f64), steps: u32) -> Result<()> {
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mousePressed", "x": from.0, "y": from.1, "button": "left", "clickCount": 1}),
      )
      .await?;
    // Playwright default is `1` — a single `mousemove` at the destination.
    // For steps > 1, interpolate with a cubic ease between press and release.
    let steps = steps.max(1);
    for i in 1..=steps {
      let (x, y) = if steps == 1 {
        (to.0, to.1)
      } else {
        let t = f64::from(i) / f64::from(steps);
        let ease = t * t * (3.0 - 2.0 * t);
        (from.0 + (to.0 - from.0) * ease, from.1 + (to.1 - from.1) * ease)
      };
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mouseMoved", "x": x, "y": y, "button": "left"}),
        )
        .await?;
    }
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mouseReleased", "x": to.0, "y": to.1, "button": "left", "clickCount": 1}),
      )
      .await?;
    if let Ok(mut guard) = self.last_cursor_pos.lock() {
      *guard = Some(to);
    }
    Ok(())
  }

  pub async fn mouse_wheel(&self, delta_x: f64, delta_y: f64) -> Result<()> {
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mouseWheel", "x": 0, "y": 0, "deltaX": delta_x, "deltaY": delta_y}),
      )
      .await?;
    Ok(())
  }

  pub async fn mouse_down(&self, x: f64, y: f64, button: &str) -> Result<()> {
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mousePressed", "x": x, "y": y, "button": button, "clickCount": 1}),
      )
      .await?;
    if let Ok(mut guard) = self.last_cursor_pos.lock() {
      *guard = Some((x, y));
    }
    Ok(())
  }

  pub async fn mouse_up(&self, x: f64, y: f64, button: &str) -> Result<()> {
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mouseReleased", "x": x, "y": y, "button": button, "clickCount": 1}),
      )
      .await?;
    if let Ok(mut guard) = self.last_cursor_pos.lock() {
      *guard = Some((x, y));
    }
    Ok(())
  }

  pub async fn type_str(&self, text: &str) -> Result<()> {
    self.cmd("Input.insertText", serde_json::json!({"text": text})).await?;
    Ok(())
  }

  /// Resolve a Playwright-style key name to (DOM key, virtual keycode, text).
  fn resolve_key(key: &str) -> (&str, u32, Option<&str>) {
    match key {
      "Enter" => ("Enter", 13, Some("\r")),
      "Tab" => ("Tab", 9, Some("\t")),
      "Space" | " " => (" ", 32, Some(" ")),
      "Backspace" => ("Backspace", 8, None),
      "Delete" => ("Delete", 46, None),
      "Escape" => ("Escape", 27, None),
      "ArrowLeft" => ("ArrowLeft", 37, None),
      "ArrowRight" => ("ArrowRight", 39, None),
      "ArrowUp" => ("ArrowUp", 38, None),
      "ArrowDown" => ("ArrowDown", 40, None),
      "Home" => ("Home", 36, None),
      "End" => ("End", 35, None),
      "PageUp" => ("PageUp", 33, None),
      "PageDown" => ("PageDown", 34, None),
      "Shift" | "ShiftLeft" | "ShiftRight" => ("Shift", 16, None),
      "Control" | "ControlLeft" | "ControlRight" => ("Control", 17, None),
      "Alt" | "AltLeft" | "AltRight" => ("Alt", 18, None),
      "Meta" | "MetaLeft" => ("Meta", 91, None),
      "MetaRight" => ("Meta", 93, None),
      "F1" => ("F1", 112, None),
      "F2" => ("F2", 113, None),
      "F3" => ("F3", 114, None),
      "F4" => ("F4", 115, None),
      "F5" => ("F5", 116, None),
      "F6" => ("F6", 117, None),
      "F7" => ("F7", 118, None),
      "F8" => ("F8", 119, None),
      "F9" => ("F9", 120, None),
      "F10" => ("F10", 121, None),
      "F11" => ("F11", 122, None),
      "F12" => ("F12", 123, None),
      ch => (ch, 0, if ch.len() == 1 { Some(ch) } else { None }),
    }
  }

  /// Dispatch a keyDown event for a single key (does NOT release it).
  pub async fn key_down(&self, key: &str) -> Result<()> {
    self.key_down_with_mods(key, 0).await
  }

  /// Dispatch a keyDown carrying a CDP `modifiers` bitfield (Alt=1,
  /// Ctrl=2, Meta=4, Shift=8). Used by `press_key` to handle
  /// Playwright-style combos like `"Control+a"`.
  pub(crate) async fn key_down_with_mods(&self, key: &str, modifiers: u32) -> Result<()> {
    let (dom_key, vk, text) = Self::resolve_key(key);
    let down_type = if text.is_some() { "keyDown" } else { "rawKeyDown" };
    // Don't emit text characters while a non-Shift modifier is held —
    // otherwise `Control+a` inserts the literal "a" instead of doing a
    // select-all (mirrors Playwright's behaviour).
    let suppress_text = modifiers & !8 != 0;
    let mut params = serde_json::json!({
        "type": down_type, "key": dom_key,
        "windowsVirtualKeyCode": vk,
        "modifiers": modifiers,
    });
    if let Some(code) = Self::resolve_code(key) {
      params["code"] = serde_json::json!(code);
    }
    if let Some(t) = text
      && !suppress_text
    {
      params["text"] = serde_json::json!(t);
    }
    self.cmd("Input.dispatchKeyEvent", params).await?;
    Ok(())
  }

  /// Dispatch a keyUp carrying the current `modifiers` bitfield. The
  /// up event for a modifier itself should still carry the modifier
  /// bit (it's released as part of this event).
  pub(crate) async fn key_up_with_mods(&self, key: &str, modifiers: u32) -> Result<()> {
    let (dom_key, vk, _) = Self::resolve_key(key);
    let mut params = serde_json::json!({
        "type": "keyUp", "key": dom_key,
        "windowsVirtualKeyCode": vk,
        "modifiers": modifiers,
    });
    if let Some(code) = Self::resolve_code(key) {
      params["code"] = serde_json::json!(code);
    }
    self.cmd("Input.dispatchKeyEvent", params).await?;
    Ok(())
  }

  /// Map a Playwright key name to the DOM `code` value (e.g. `"a"` ->
  /// `"KeyA"`, `"1"` -> `"Digit1"`, modifiers/named keys -> their
  /// canonical code). Required by Chrome to interpret `Control+a` as a
  /// real Ctrl+A keystroke (select-all) instead of plain text input.
  fn resolve_code(key: &str) -> Option<&'static str> {
    match key {
      "Control" | "ControlLeft" => Some("ControlLeft"),
      "ControlRight" => Some("ControlRight"),
      "Shift" | "ShiftLeft" => Some("ShiftLeft"),
      "ShiftRight" => Some("ShiftRight"),
      "Alt" | "AltLeft" => Some("AltLeft"),
      "AltRight" => Some("AltRight"),
      "Meta" | "MetaLeft" => Some("MetaLeft"),
      "MetaRight" => Some("MetaRight"),
      "Enter" => Some("Enter"),
      "Tab" => Some("Tab"),
      "Backspace" => Some("Backspace"),
      "Delete" => Some("Delete"),
      "Escape" => Some("Escape"),
      "ArrowUp" => Some("ArrowUp"),
      "ArrowDown" => Some("ArrowDown"),
      "ArrowLeft" => Some("ArrowLeft"),
      "ArrowRight" => Some("ArrowRight"),
      "Home" => Some("Home"),
      "End" => Some("End"),
      "PageUp" => Some("PageUp"),
      "PageDown" => Some("PageDown"),
      "Space" | " " => Some("Space"),
      k if k.len() == 1 => {
        let c = k.chars().next()?;
        // ASCII letters -> Key<UPPER>; digits -> Digit<n>.
        if c.is_ascii_alphabetic() {
          let upper = c.to_ascii_uppercase();
          Some(match upper {
            'A' => "KeyA",
            'B' => "KeyB",
            'C' => "KeyC",
            'D' => "KeyD",
            'E' => "KeyE",
            'F' => "KeyF",
            'G' => "KeyG",
            'H' => "KeyH",
            'I' => "KeyI",
            'J' => "KeyJ",
            'K' => "KeyK",
            'L' => "KeyL",
            'M' => "KeyM",
            'N' => "KeyN",
            'O' => "KeyO",
            'P' => "KeyP",
            'Q' => "KeyQ",
            'R' => "KeyR",
            'S' => "KeyS",
            'T' => "KeyT",
            'U' => "KeyU",
            'V' => "KeyV",
            'W' => "KeyW",
            'X' => "KeyX",
            'Y' => "KeyY",
            'Z' => "KeyZ",
            _ => return None,
          })
        } else if c.is_ascii_digit() {
          Some(match c {
            '0' => "Digit0",
            '1' => "Digit1",
            '2' => "Digit2",
            '3' => "Digit3",
            '4' => "Digit4",
            '5' => "Digit5",
            '6' => "Digit6",
            '7' => "Digit7",
            '8' => "Digit8",
            '9' => "Digit9",
            _ => return None,
          })
        } else {
          None
        }
      },
      _ => None,
    }
  }

  /// Dispatch a keyUp event for a single key.
  pub async fn key_up(&self, key: &str) -> Result<()> {
    let (dom_key, vk, _) = Self::resolve_key(key);
    self
      .cmd(
        "Input.dispatchKeyEvent",
        serde_json::json!({
            "type": "keyUp", "key": dom_key,
            "windowsVirtualKeyCode": vk,
        }),
      )
      .await?;
    Ok(())
  }

  pub async fn press_key(&self, key: &str) -> Result<()> {
    // Playwright-style modifier combos: `"Control+a"`, `"Shift+Alt+T"`.
    // Press each modifier (down), then the primary key with the
    // modifiers bitfield set so the page sees a real combo (e.g.
    // select-all on Ctrl+A), then release in reverse.
    let parts: Vec<&str> = key.split('+').collect();
    if parts.len() <= 1 {
      self.key_down(key).await?;
      self.key_up(key).await?;
      return Ok(());
    }
    let (mods, primary) = parts.split_at(parts.len() - 1);
    let primary = primary[0];
    let mod_bit = |name: &str| -> u32 {
      match name {
        "Alt" => 1,
        "Control" | "ControlOrMeta" => 2,
        "Meta" => 4,
        "Shift" => 8,
        _ => 0,
      }
    };
    let mut bits = 0u32;
    for m in mods {
      let b = mod_bit(m);
      if b != 0 {
        bits |= b;
        // The modifier's own keyDown carries the cumulative bitfield
        // (Chrome expects to see Ctrl set on the Ctrl keyDown itself).
        self.key_down_with_mods(m, bits).await?;
      }
    }
    self.key_down_with_mods(primary, bits).await?;
    self.key_up_with_mods(primary, bits).await?;
    let mut down_bits = bits;
    for m in mods.iter().rev() {
      let b = mod_bit(m);
      if b != 0 {
        self.key_up_with_mods(m, down_bits).await?;
        down_bits &= !b;
      }
    }
    Ok(())
  }

  // ---- Cookies ----

  pub async fn get_cookies(&self) -> Result<Vec<CookieData>> {
    // Playwright's `context.cookies()` returns EVERY cookie in the
    // browser context (`Storage.getCookies`), not just the current
    // page's (`Network.getCookies` is frame-URL scoped — a cookie set
    // for another domain would be invisible).
    //
    // `Storage.getCookies` is a browser-target command: on a page
    // session it errors with "browserContextId is only allowed for
    // Browser target", and without `browserContextId` on the root
    // session it returns the DEFAULT context's cookies (missing
    // everything set on a page in an isolated `Target.createBrowserContext`).
    // Send on the root session with explicit `browserContextId` so
    // test-fixture contexts see their own cookies.
    let (sid, params) = if let Some(ref ctx_id) = self.browser_context_id {
      (None, serde_json::json!({"browserContextId": ctx_id.as_ref()}))
    } else {
      (self.session_id.as_deref(), super::empty_params())
    };
    let result = self.transport.send_command(sid, "Storage.getCookies", &params).await?;
    let cookies = result
      .get("cookies")
      .and_then(|c| c.as_array())
      .cloned()
      .unwrap_or_default();
    Ok(
      cookies
        .iter()
        .map(|c| CookieData {
          name: c.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
          value: c.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string(),
          domain: c.get("domain").and_then(|v| v.as_str()).unwrap_or("").to_string(),
          path: c.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string(),
          secure: c.get("secure").and_then(serde_json::Value::as_bool).unwrap_or(false),
          http_only: c.get("httpOnly").and_then(serde_json::Value::as_bool).unwrap_or(false),
          expires: c.get("expires").and_then(serde_json::Value::as_f64),
          same_site: c
            .get("sameSite")
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse::<super::SameSite>().ok()),
          url: None,
        })
        .collect(),
    )
  }

  pub async fn set_cookie(&self, cookie: CookieData) -> Result<()> {
    let mut params = serde_json::json!({
        "name": cookie.name,
        "value": cookie.value,
    });
    // Playwright `SetNetworkCookieParam.url`: CDP `Network.setCookie`
    // accepts `url` and derives domain/path from it. Forward it so
    // `addCookies([{ name, value, url }])` (the common Playwright form)
    // works instead of erroring "url or domain needs to be specified".
    if let Some(u) = &cookie.url {
      params["url"] = serde_json::json!(u);
    }
    if !cookie.domain.is_empty() {
      params["domain"] = serde_json::json!(cookie.domain);
    }
    if !cookie.path.is_empty() {
      params["path"] = serde_json::json!(cookie.path);
    }
    params["secure"] = serde_json::json!(cookie.secure);
    params["httpOnly"] = serde_json::json!(cookie.http_only);
    if let Some(e) = cookie.expires {
      params["expires"] = serde_json::json!(e);
    }
    if let Some(ss) = cookie.same_site {
      params["sameSite"] = serde_json::json!(ss.as_str());
    }
    self.cmd("Network.setCookie", params).await?;
    Ok(())
  }

  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<()> {
    let mut params = serde_json::json!({"name": name});
    if let Some(d) = domain {
      params["domain"] = serde_json::json!(d);
    } else if let Ok(Some(url)) = self.url().await {
      params["url"] = serde_json::json!(url);
    }
    self.cmd("Network.deleteCookies", params).await?;
    Ok(())
  }

  pub async fn clear_cookies(&self) -> Result<()> {
    // Use Network.getCookies + Network.deleteCookies (session-scoped)
    // instead of Storage.clearCookies (browser-scoped) to correctly
    // clear cookies for this page's context.
    let cookies = self.get_cookies().await?;
    for c in &cookies {
      self
        .cmd(
          "Network.deleteCookies",
          serde_json::json!({
            "name": c.name,
            "domain": c.domain,
            "path": c.path,
          }),
        )
        .await?;
    }
    Ok(())
  }

  // ---- Emulation ----

  /// Apply a [`crate::options::BrowserContextOptions`] bag to this
  /// page. Every protocol command is inlined here — there are no
  /// per-field helper methods. Mirrors Playwright's
  /// `crPage._updateXxx()` family in
  /// `/tmp/playwright/packages/playwright-core/src/server/chromium/crPage.ts:520`
  /// but folded into one call so `ContextRef::new_page` dispatches
  /// once and every command ships in parallel via [`tokio::join!`].
  ///
  /// Any field whose `Option` is `None` is skipped; `MediaOverride`
  /// fields with `Unchanged` are skipped. Errors from individual
  /// commands fold into a single aggregated error listing every
  /// field that failed — matches Playwright's `Promise.all` failure
  /// mode.
  #[allow(clippy::too_many_lines)]
  pub async fn apply_context_options(&self, opts: &crate::options::BrowserContextOptions) -> Result<()> {
    use futures::future::OptionFuture;

    // `viewport` and `media` compose over `EmulateMediaOptions` /
    // `ViewportConfig` helpers reused elsewhere (`emulate_viewport`,
    // `emulate_media`) — those methods are also called from
    // `Page::set_viewport_size` and `Page::emulate_media` (Playwright
    // public API), so they keep dedicated methods.
    let viewport_fut: OptionFuture<_> = opts
      .resolved_viewport()
      .map(|vp| async move { self.emulate_viewport(&vp).await })
      .into();
    let media_fut: OptionFuture<_> = opts
      .any_media_override()
      .then(|| {
        let m = opts.as_emulate_media();
        async move { self.emulate_media(&m).await }
      })
      .into();

    // Every remaining field is a direct CDP command — inlined below
    // so no `set_*` helper lives on the backend.
    let screen_fut: OptionFuture<_> = opts
      .screen
      .map(|s| async move {
        self
          .cmd(
            "Emulation.setDeviceMetricsOverride",
            serde_json::json!({
              "width": s.width, "height": s.height, "deviceScaleFactor": 1, "mobile": false,
              "screenWidth": s.width, "screenHeight": s.height,
              "screenOrientation": {"angle": 0, "type": "landscapePrimary"},
            }),
          )
          .await
          .map(|_| ())
      })
      .into();
    let ua_fut: OptionFuture<_> = opts
      .user_agent
      .as_deref()
      .map(|ua| async move {
        self
          .cmd("Network.setUserAgentOverride", serde_json::json!({"userAgent": ua}))
          .await
          .map(|_| ())
      })
      .into();
    let locale_fut: OptionFuture<_> = opts
      .locale
      .as_deref()
      .map(|l| async move {
        let _ = self
          .cmd("Emulation.setLocaleOverride", serde_json::json!({"locale": l}))
          .await;
        self
          .cmd(
            "Network.setUserAgentOverride",
            serde_json::json!({"userAgent": "", "acceptLanguage": l}),
          )
          .await
          .map(|_| ())
      })
      .into();
    let tz_fut: OptionFuture<_> = opts
      .timezone_id
      .as_deref()
      .map(|tz| async move {
        self
          .cmd("Emulation.setTimezoneOverride", serde_json::json!({"timezoneId": tz}))
          .await
          .map(|_| ())
      })
      .into();
    let js_fut: OptionFuture<_> = opts
      .java_script_enabled
      .map(|v| async move {
        self
          .cmd("Emulation.setScriptExecutionDisabled", serde_json::json!({"value": !v}))
          .await
          .map(|_| ())
      })
      .into();
    let csp_fut: OptionFuture<_> = opts
      .bypass_csp
      .map(|v| async move {
        self
          .cmd("Page.setBypassCSP", serde_json::json!({"enabled": v}))
          .await
          .map(|_| ())
      })
      .into();
    let tls_fut: OptionFuture<_> = opts
      .ignore_https_errors
      .map(|v| async move {
        self
          .cmd("Security.setIgnoreCertificateErrors", serde_json::json!({"ignore": v}))
          .await
          .map(|_| ())
      })
      .into();
    let creds_fut: OptionFuture<_> = opts
      .http_credentials
      .clone()
      .map(|c| async move {
        *self.http_credentials.write().await = Some(c);
        self.ensure_fetch_enabled().await
      })
      .into();
    let sw_fut: OptionFuture<_> = opts
      .service_workers
      .map(|p| async move {
        if matches!(p, crate::options::ServiceWorkerPolicy::Block) {
          self
            .cmd(
              "Page.addScriptToEvaluateOnNewDocument",
              serde_json::json!({
                "source": "if(navigator.serviceWorker){navigator.serviceWorker.register=()=>Promise.reject(new Error('Service workers blocked'))}"
              }),
            )
            .await
            .map(|_| ())
        } else {
          Ok(())
        }
      })
      .into();
    let dl_fut: OptionFuture<_> = opts
      .accept_downloads
      .map(|accept| async move {
        let behavior = if accept { "allow" } else { "deny" };
        self
          .cmd(
            "Browser.setDownloadBehavior",
            serde_json::json!({"behavior": behavior, "downloadPath": "", "eventsEnabled": true}),
          )
          .await
          .map(|_| ())
      })
      .into();
    let headers_fut: OptionFuture<_> = opts
      .extra_http_headers
      .as_ref()
      .map(|h| async move {
        let pairs: serde_json::Map<String, serde_json::Value> = h
          .iter()
          .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
          .collect();
        self
          .cmd("Network.setExtraHTTPHeaders", serde_json::json!({"headers": pairs}))
          .await
          .map(|_| ())
      })
      .into();
    let geo_fut: OptionFuture<_> = opts
      .geolocation
      .map(|g| async move {
        self
          .cmd(
            "Emulation.setGeolocationOverride",
            serde_json::json!({"latitude": g.latitude, "longitude": g.longitude, "accuracy": g.accuracy}),
          )
          .await
          .map(|_| ())
      })
      .into();
    let perms_fut: OptionFuture<_> = opts
      .permissions
      .as_ref()
      .map(|p| async move {
        // `Browser.grantPermissions` must ship with `browserContextId`
        // so the grant applies to THIS context, and must be sent at
        // the browser level (no sessionId) — matches
        // `/tmp/playwright/packages/playwright-core/src/server/chromium/crBrowser.ts::doGrantPermissions`.
        let mut params = serde_json::json!({"permissions": p});
        if let Some(ref ctx_id) = self.browser_context_id {
          params["browserContextId"] = serde_json::json!(ctx_id.as_ref());
        }
        self
          .transport
          .send_command(None, "Browser.grantPermissions", &params)
          .await
          .map(|_| ())
      })
      .into();
    let offline_fut: OptionFuture<_> = opts
      .offline
      .map(|o| async move {
        self
          .cmd(
            "Network.emulateNetworkConditions",
            serde_json::json!({
              "offline": o, "latency": 0, "downloadThroughput": -1, "uploadThroughput": -1,
            }),
          )
          .await
          .map(|_| ())
      })
      .into();

    let (r_vp, r_scr, r_ua, r_loc, r_tz, r_js, r_csp, r_tls, r_cred, r_sw, r_dl, r_hdr, r_med, r_geo, r_perm, r_off) = tokio::join!(
      viewport_fut,
      screen_fut,
      ua_fut,
      locale_fut,
      tz_fut,
      js_fut,
      csp_fut,
      tls_fut,
      creds_fut,
      sw_fut,
      dl_fut,
      headers_fut,
      media_fut,
      geo_fut,
      perms_fut,
      offline_fut,
    );

    // Aggregate errors — one failure per field, each labelled.
    let mut errs: Vec<String> = Vec::new();
    for (label, r) in [
      ("viewport", r_vp),
      ("screen", r_scr),
      ("userAgent", r_ua),
      ("locale", r_loc),
      ("timezoneId", r_tz),
      ("javaScriptEnabled", r_js),
      ("bypassCSP", r_csp),
      ("ignoreHTTPSErrors", r_tls),
      ("httpCredentials", r_cred),
      ("serviceWorkers", r_sw),
      ("acceptDownloads", r_dl),
      ("extraHTTPHeaders", r_hdr),
      ("media (colorScheme/reducedMotion/forcedColors/contrast)", r_med),
      ("geolocation", r_geo),
      ("permissions", r_perm),
      ("offline", r_off),
    ] {
      if let Some(Err(e)) = r {
        errs.push(format!("{label}: {e}"));
      }
    }
    if errs.is_empty() {
      Ok(())
    } else {
      Err(FerriError::Backend(errs.join("; ")))
    }
  }

  pub async fn emulate_viewport(&self, config: &crate::options::ViewportConfig) -> Result<()> {
    let params = metrics_params_for(config);

    // Skip the `setDeviceMetricsOverride` RTT when params match the
    // last shipped value (Playwright `_metricsOverride` JSON-equality
    // pattern, `crPage.ts:932`). Touch emulation is a separate
    // command and runs through its own idempotent path below — a
    // metrics cache hit must not mask a `has_touch` change.
    let metrics_unchanged = {
      let last = match self.last_metrics_params.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
      };
      last.as_ref() == Some(&params)
    };
    if !metrics_unchanged {
      self.cmd("Emulation.setDeviceMetricsOverride", params.clone()).await?;
      let mut last = match self.last_metrics_params.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
      };
      *last = Some(params);
    }

    if config.has_touch {
      // Pass `maxTouchPoints` explicitly so `navigator.maxTouchPoints`
      // reports a non-zero value (Chrome leaves it at 0 when the
      // param is omitted on some channels). Mirrors Playwright's
      // `crEmulationManager._updateTouch` which uses 5.
      let _ = self
        .cmd(
          "Emulation.setTouchEmulationEnabled",
          serde_json::json!({"enabled": true, "maxTouchPoints": 5}),
        )
        .await;
    }
    Ok(())
  }

  pub async fn emulate_media(&self, opts: &crate::options::EmulateMediaOptions) -> Result<()> {
    use crate::options::MediaOverride;
    // `Emulation.setEmulatedMedia` replaces the emulation state per call.
    // Chrome treats a feature listed in `features` with `value: ""` as
    // *still overridden* (the value just happens to be the empty string)
    // — to actually clear an override the feature must be *omitted* from
    // the array. Tested empirically against Chrome 147; the legacy
    // "always include all four features" path Playwright still uses
    // does not reset the override on current Chromium builds and breaks
    // `page.emulateMedia({colorScheme: null})`.
    //
    // Strategy: include features only when they're actively `Set`.
    // `Disabled` and `Unchanged` drop out of the array so the override
    // is cleared. `media` ships an empty string to clear (still
    // honoured by Chrome).
    let mut features: Vec<serde_json::Value> = Vec::with_capacity(4);
    let push_if_set = |features: &mut Vec<serde_json::Value>, name: &str, o: &MediaOverride| {
      if let MediaOverride::Set(v) = o {
        features.push(serde_json::json!({ "name": name, "value": v }));
      }
    };
    push_if_set(&mut features, "prefers-color-scheme", &opts.color_scheme);
    push_if_set(&mut features, "prefers-reduced-motion", &opts.reduced_motion);
    push_if_set(&mut features, "forced-colors", &opts.forced_colors);
    push_if_set(&mut features, "prefers-contrast", &opts.contrast);
    let media = match &opts.media {
      MediaOverride::Set(v) => v.as_str(),
      MediaOverride::Disabled | MediaOverride::Unchanged => "",
    };
    let params = serde_json::json!({"features": features, "media": media});
    self.cmd("Emulation.setEmulatedMedia", params).await?;
    Ok(())
  }

  /// Reset permissions granted via the options-bag / context-level
  /// `grantPermissions` — called from
  /// [`crate::ContextRef::clear_permissions`] (`Playwright
  /// browserContext.clearPermissions`).
  pub async fn reset_permissions(&self) -> Result<()> {
    // Scope to this page's CDP browser context (see `apply_context_options`
    // for why grant must ship `browserContextId`). `Browser.resetPermissions`
    // is browser-level too.
    let mut params = serde_json::json!({});
    if let Some(ref ctx_id) = self.browser_context_id {
      params["browserContextId"] = serde_json::json!(ctx_id.as_ref());
    }
    self
      .transport
      .send_command(None, "Browser.resetPermissions", &params)
      .await?;
    Ok(())
  }

  /// Direct `Network.setExtraHTTPHeaders` command. Backs
  /// [`crate::Page::set_extra_http_headers`] (Playwright's public
  /// `page.setExtraHTTPHeaders(headers)`). The `apply_context_options`
  /// path inlines the same command separately so both entry points
  /// land independently.
  pub async fn set_extra_http_headers(&self, headers: &FxHashMap<String, String>) -> Result<()> {
    let pairs: serde_json::Map<String, serde_json::Value> = headers
      .iter()
      .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
      .collect();
    self
      .cmd("Network.setExtraHTTPHeaders", serde_json::json!({"headers": pairs}))
      .await?;
    Ok(())
  }

  /// Set (or clear) the HTTP credentials used to answer
  /// `Fetch.authRequired` challenges. Backs
  /// [`crate::Page::set_http_credentials`] (Playwright's public
  /// `browserContext.setHTTPCredentials(creds | null)`). Passing `None`
  /// clears stored credentials so future 401 challenges surface as the
  /// browser's native auth dialog (`CancelAuth`).
  pub async fn set_http_credentials(&self, creds: Option<crate::options::HttpCredentials>) -> Result<()> {
    *self.http_credentials.write().await = creds;
    // Re-arm Fetch so `handleAuthRequests` tracks the new state.
    self.ensure_fetch_enabled().await
  }

  // ---- Tracing ----

  pub async fn start_tracing(&self) -> Result<()> {
    self.cmd("Tracing.start", super::empty_params()).await?;
    Ok(())
  }

  pub async fn stop_tracing(&self) -> Result<()> {
    self.cmd("Tracing.end", super::empty_params()).await?;
    Ok(())
  }

  pub async fn metrics(&self) -> Result<Vec<MetricData>> {
    let result = self.cmd("Performance.getMetrics", super::empty_params()).await?;
    let metrics = result
      .get("metrics")
      .and_then(|m| m.as_array())
      .cloned()
      .unwrap_or_default();
    Ok(
      metrics
        .iter()
        .map(|m| MetricData {
          name: m.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
          value: m.get("value").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
        })
        .collect(),
    )
  }

  // ---- Ref resolution ----

  pub async fn resolve_backend_node(&self, backend_node_id: i64, ref_id: &str) -> Result<AnyElement> {
    let resolve_result = self
      .cmd("DOM.resolveNode", serde_json::json!({"backendNodeId": backend_node_id}))
      .await?;

    let object_id = resolve_result
      .get("object")
      .and_then(|o| o.get("objectId"))
      .and_then(|v| v.as_str())
      .ok_or_else(|| FerriError::protocol("DOM.resolveNode", format!("Ref '{ref_id}' no longer valid.")))?;

    // We intentionally do NOT call `DOM.requestNode` here. Without an
    // explicit `DOM.getDocument` first (skipped by the lazy bootstrap),
    // Chrome reports `nodeId: 0` for nodes not in the DOM-agent's
    // current document. Element paths that genuinely need a nodeId
    // (e.g. `DOM.setFileInputFiles`) lazily request one via
    // `CdpElement::node_id()`; everywhere else, the cached
    // `Runtime` objectId is sufficient and bypasses the DOM agent.
    Ok(T::wrap_element(CdpElement {
      transport: self.transport.clone(),
      session_id: self.session_id.clone(),
      handles: Arc::new(tokio::sync::Mutex::new(CdpElementHandles {
        node_id: None,
        object_id: Some(Arc::from(object_id)),
      })),
    }))
  }

  // ---- Event listeners ----

  pub fn attach_listeners(
    &self,
    console_log: Arc<RwLock<Vec<ConsoleMessage>>>,
    network_log: Arc<RwLock<Vec<NetworkRequest>>>,
    dialog_log: Arc<RwLock<Vec<crate::state::DialogEvent>>>,
  ) {
    let transport = self.transport.clone();
    let session_id = self.session_id.clone();
    let emitter1 = self.events.clone();
    let emitter2 = self.events.clone();
    let emitter3 = self.events.clone();

    Self::spawn_console_listener(
      transport.clone(),
      session_id.clone(),
      console_log,
      emitter1,
      self.page_backref.clone(),
    );
    Self::spawn_web_error_listener(
      transport.clone(),
      session_id.clone(),
      self.events.clone(),
      self.page_backref.clone(),
    );
    Self::spawn_lifecycle_event_listener(&transport, session_id.as_ref(), &self.events);
    Self::spawn_network_listener(
      transport.clone(),
      session_id.clone(),
      network_log,
      emitter2,
      self.nav_request_slot.clone(),
    );
    // Register the emitter-bridge: `page.events().on("dialog", cb)`
    // keeps working because the bridge handler — installed here for
    // the page's lifetime — synchronously claims dialogs on behalf
    // of broadcast listeners. Handler id is discarded: we never
    // unregister until the page is dropped.
    let _ = self.dialog_manager.register_emitter_bridge(self.events.clone());
    // Same pattern for file-chooser: broadcast `filechooser`
    // listeners see live handles via the bridge handler; one-shot
    // `page.wait_for_file_chooser` callers register directly.
    let _ = self.file_chooser_manager.register_emitter_bridge(self.events.clone());
    // And one more for downloads — live [`crate::download::Download`]
    // handles delivered to `page.events().on("download", cb)` via the
    // bridge, same claim-on-open pattern as dialog / file-chooser.
    let _ = self.download_manager.register_emitter_bridge(self.events.clone());

    Self::spawn_dialog_listener(
      self.transport.clone(),
      self.session_id.clone(),
      dialog_log,
      emitter3,
      self.dialog_manager.clone(),
      self.page_backref.clone(),
    );
    Self::spawn_file_chooser_listener(
      self.transport.clone(),
      self.session_id.clone(),
      self.file_chooser_manager.clone(),
      self.page_backref.clone(),
    );
    Self::spawn_download_listener(
      self.transport.clone(),
      self.session_id.clone(),
      self.browser_context_id.clone(),
      self.download_manager.clone(),
      self.downloads_dir.clone(),
      self.page_backref.clone(),
    );
    Self::spawn_frame_context_tracker(
      self.transport.clone(),
      self.session_id.clone(),
      self.frame_contexts.clone(),
      self.events.clone(),
    );
  }

  fn spawn_console_listener(
    transport: Arc<T>,
    session_id: Option<Arc<str>>,
    console_log: Arc<RwLock<Vec<ConsoleMessage>>>,
    emitter: crate::events::EventEmitter,
    page_backref: crate::backend::PageBackref,
  ) {
    tokio::spawn(async move {
      let mut rx = transport.subscribe_event_method("Runtime.consoleAPICalled");
      loop {
        // Tolerate broadcast `Lagged` so the console listener stays
        // alive after a busy session — exit-on-Lagged silently dropped
        // every future `console.*` event.
        let event = match rx.recv().await {
          Ok(e) => e,
          Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
          Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        };
        if let Some(ref expected_sid) = session_id {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(&**expected_sid) {
            continue;
          }
        }

        let Some(params) = event.get("params") else {
          continue;
        };
        // Without an owning page we cannot build live `JSHandle`s —
        // drop the event silently (matches Playwright's
        // `createHandle(context, arg)` guard which returns early if the
        // execution context is unknown).
        let Some(page) = page_backref.upgrade() else {
          continue;
        };

        let type_str = params.get("type").and_then(|v| v.as_str()).unwrap_or("log").to_string();
        let args_json = params
          .get("args")
          .and_then(|v| v.as_array())
          .cloned()
          .unwrap_or_default();
        let mut args: Vec<crate::js_handle::JSHandle> = Vec::with_capacity(args_json.len());
        for arg in &args_json {
          let backing = cdp_remote_object_to_backing(arg);
          let is_node = arg.get("subtype").and_then(|v| v.as_str()) == Some("node");
          args.push(crate::js_handle::JSHandle::from_backing(page.clone(), backing, is_node));
        }
        let location = cdp_stack_trace_to_location(params.get("stackTrace"));
        let timestamp = params
          .get("timestamp")
          .and_then(serde_json::Value::as_f64)
          .map_or(0, f64_to_u64_saturating);

        let msg = crate::console_message::ConsoleMessage::new(&page, type_str, None, args, location, timestamp);
        crate::state::push_capped(
          &mut *console_log.write().await,
          msg.clone(),
          crate::state::CONSOLE_LOG_CAP,
        );
        emitter.emit(crate::events::PageEvent::Console(msg));
      }
    });
  }

  /// Listen for `Runtime.exceptionThrown` and emit
  /// [`crate::events::PageEvent::PageError`] carrying a live
  /// [`crate::web_error::WebError`]. Mirrors Playwright's
  /// `crPage.ts:751` — `session.on('Runtime.exceptionThrown', …)` feeding
  /// `exceptionToError(exceptionDetails)` → `page.addPageError(err)`.
  ///
  /// The exception conversion follows
  /// `crProtocolHelper.ts::{getExceptionMessage, exceptionToError}`
  /// byte-for-byte: the combined `description` + stack lines is split at
  /// the first `    at …` line; everything before becomes
  /// `{ name, message }` (parsed via `splitErrorMessage`'s `': '`
  /// separator), everything from that line onward becomes `stack`. When
  /// the exception carries an object `preview` with a `name` property,
  /// that value overrides the parsed name — matches Playwright's
  /// `nameOverride` branch.
  /// Bridge CDP's `Page.loadEventFired` / `Page.domContentEventFired`
  /// onto the page emitter so `page.on('load' | 'domcontentloaded')`
  /// and `waitForEvent` observe them (Playwright parity).
  fn spawn_lifecycle_event_listener(
    transport: &Arc<T>,
    session_id: Option<&Arc<str>>,
    emitter: &crate::events::EventEmitter,
  ) {
    for (method, ev) in [
      ("Page.loadEventFired", crate::events::PageEvent::Load),
      ("Page.domContentEventFired", crate::events::PageEvent::DomContentLoaded),
    ] {
      let transport = transport.clone();
      let session_id = session_id.cloned();
      let emitter = emitter.clone();
      tokio::spawn(async move {
        let mut rx = transport.subscribe_event_method(method);
        while let Some(event) = crate::events::recv_tolerant(&mut rx).await {
          if let Some(ref expected_sid) = session_id {
            if event.get("sessionId").and_then(|v| v.as_str()) != Some(&**expected_sid) {
              continue;
            }
          }
          emitter.emit(ev.clone());
        }
      });
    }
  }

  fn spawn_web_error_listener(
    transport: Arc<T>,
    session_id: Option<Arc<str>>,
    emitter: crate::events::EventEmitter,
    page_backref: crate::backend::PageBackref,
  ) {
    tokio::spawn(async move {
      let mut rx = transport.subscribe_event_method("Runtime.exceptionThrown");
      while let Some(event) = crate::events::recv_tolerant(&mut rx).await {
        if let Some(ref expected_sid) = session_id {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(&**expected_sid) {
            continue;
          }
        }
        let Some(exception_details) = event.get("params").and_then(|p| p.get("exceptionDetails")) else {
          continue;
        };
        let details = cdp_exception_to_error_details(exception_details);
        let location = cdp_exception_to_location(exception_details);
        let web_err = match page_backref.upgrade() {
          Some(page) => crate::web_error::WebError::new(&page, details, location),
          None => crate::web_error::WebError::new_detached(details, location),
        };
        emitter.emit(crate::events::PageEvent::PageError(web_err));
      }
    });
  }

  fn spawn_network_listener(
    transport: Arc<T>,
    session_id: Option<Arc<str>>,
    network_log: Arc<RwLock<Vec<NetworkRequest>>>,
    emitter: crate::events::EventEmitter,
    nav_request_slot: crate::network::NavRequestSlot,
  ) {
    let tracker: Arc<NetworkTracker<T>> = Arc::new(NetworkTracker::new(
      transport.clone(),
      session_id.clone(),
      nav_request_slot,
    ));
    tokio::spawn(async move {
      let mut rx = transport.subscribe_event_domain("Network");
      while let Some(event) = crate::events::recv_tolerant(&mut rx).await {
        if let Some(ref expected_sid) = session_id {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(&**expected_sid) {
            continue;
          }
        }
        let method = event.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = event.get("params");
        match method {
          "Network.requestWillBeSent" => {
            if let Some(p) = params {
              tracker.on_request_will_be_sent(p, &network_log, &emitter).await;
            }
          },
          "Network.requestWillBeSentExtraInfo" => {
            if let Some(p) = params {
              tracker.on_request_extra_info(p).await;
            }
          },
          "Network.responseReceived" => {
            if let Some(p) = params {
              tracker.on_response_received(p, &emitter).await;
            }
          },
          "Network.responseReceivedExtraInfo" => {
            if let Some(p) = params {
              tracker.on_response_extra_info(p).await;
            }
          },
          "Network.loadingFinished" => {
            if let Some(p) = params {
              tracker.on_loading_finished(p, &emitter).await;
            }
          },
          "Network.loadingFailed" => {
            if let Some(p) = params {
              tracker.on_loading_failed(p, &emitter).await;
            }
          },
          "Network.webSocketCreated" => {
            if let Some(p) = params {
              tracker.on_websocket_created(p, &emitter).await;
            }
          },
          "Network.webSocketFrameSent" => {
            if let Some(p) = params {
              tracker.on_websocket_frame_sent(p).await;
            }
          },
          "Network.webSocketFrameReceived" => {
            if let Some(p) = params {
              tracker.on_websocket_frame_received(p).await;
            }
          },
          "Network.webSocketFrameError" => {
            if let Some(p) = params {
              tracker.on_websocket_error(p).await;
            }
          },
          "Network.webSocketClosed" => {
            if let Some(p) = params {
              tracker.on_websocket_closed(p).await;
            }
          },
          _ => {},
        }
      }
    });
  }

  fn spawn_dialog_listener(
    transport: Arc<T>,
    session_id: Option<Arc<str>>,
    dialog_log: Arc<RwLock<Vec<crate::state::DialogEvent>>>,
    _emitter: crate::events::EventEmitter,
    dialog_manager: crate::dialog::DialogManager,
    page_backref: crate::backend::PageBackref,
  ) {
    tokio::spawn(async move {
      let mut rx = transport.subscribe_event_method("Page.javascriptDialogOpening");
      while let Some(event) = crate::events::recv_tolerant(&mut rx).await {
        if let Some(ref expected_sid) = session_id {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(&**expected_sid) {
            continue;
          }
        }
        let Some(params) = event.get("params") else {
          continue;
        };
        let dialog_type_str = params
          .get("type")
          .and_then(|v| v.as_str())
          .unwrap_or("alert")
          .to_string();
        let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let default_value = params
          .get("defaultPrompt")
          .and_then(|v| v.as_str())
          .unwrap_or("")
          .to_string();
        let dialog_type = crate::dialog::DialogType::parse(&dialog_type_str);

        // Build the responder: a closure that translates the user's
        // accept/dismiss into CDP `Page.handleJavaScriptDialog`. The
        // closure captures an `Arc` of the transport + session id so
        // it stays valid across the dialog's lifetime.
        let responder_transport = Arc::clone(&transport);
        let responder_session = session_id.clone();
        let responder: crate::dialog::DialogResponder = Arc::new(move |response| {
          let transport = Arc::clone(&responder_transport);
          let session = responder_session.clone();
          Box::pin(async move {
            let mut cmd_params = serde_json::json!({
              "accept": matches!(response, crate::dialog::DialogResponse::Accept { .. }),
            });
            if let crate::dialog::DialogResponse::Accept {
              prompt_text: Some(text),
            } = response
            {
              cmd_params["promptText"] = serde_json::Value::String(text);
            }
            transport
              .send_command(session.as_deref(), "Page.handleJavaScriptDialog", &cmd_params)
              .await
              .map(|_| ())
          })
        });

        let dialog = crate::dialog::Dialog::new_with_manager(
          dialog_type,
          message.clone(),
          default_value.clone(),
          responder,
          Some(dialog_manager.clone()),
          page_backref.weak(),
        );

        // Synchronous dialog dispatch — mirrors Playwright's
        // `DialogManager.dialogDidOpen`. Each registered handler is
        // called with the live `Dialog` and returns `true` to claim
        // ownership. If no handler claims, the manager auto-closes
        // (accept for `beforeunload`, dismiss otherwise). There is no
        // grace window and no race: `did_open` calls handlers
        // synchronously in the same stack as the CDP event arrival.
        dialog_manager.did_open(dialog);

        crate::state::push_capped(
          &mut *dialog_log.write().await,
          crate::state::DialogEvent {
            dialog_type: dialog_type_str,
            message,
            action: "dispatched".to_string(),
          },
          crate::state::DIALOG_LOG_CAP,
        );
      }
    });
  }

  /// Listen for `Page.fileChooserOpened` and dispatch a live
  /// [`crate::file_chooser::FileChooser`] through the page's
  /// [`crate::file_chooser::FileChooserManager`].
  ///
  /// Enables `Page.setInterceptFileChooserDialog({ enabled: true })`
  /// at listener-spawn time so the native file picker is suppressed —
  /// mirrors Playwright's `_updateFileChooserInterception` flow. We
  /// intercept unconditionally because the synchronous claim path
  /// in [`crate::file_chooser::FileChooserManager::did_open`] makes
  /// toggling interception per-listener-count (the way Playwright does
  /// it) racy with the user's listener registration.
  ///
  /// The event carries `backendNodeId` + `mode:'selectSingle'|'selectMultiple'`.
  /// Resolution to an [`crate::element_handle::ElementHandle`] goes
  /// through `DOM.resolveNode` + [`crate::page::Page`] via the
  /// `page_backref` weak — the listener task runs before the outer
  /// `Arc<Page>` exists, so the backref is checked per-event and
  /// silently drops events that arrive before the backref is
  /// populated (matches the Playwright server's "frame/context may go
  /// away" guard).
  fn spawn_file_chooser_listener(
    transport: Arc<T>,
    session_id: Option<Arc<str>>,
    file_chooser_manager: crate::file_chooser::FileChooserManager,
    page_backref: crate::backend::PageBackref,
  ) {
    tokio::spawn(async move {
      // Subscribe FIRST so we don't race: routed subscriptions only
      // deliver events published AFTER subscription, and a fast
      // test can trigger the picker before our enable-intercept
      // reply lands. The subscription call is synchronous and cheap.
      //
      // We do NOT fire `Page.setInterceptFileChooserDialog` here.
      // Mirrors Playwright's `_updateFileChooserInterception` lazy
      // pattern (`/tmp/playwright/packages/playwright-core/src/server/chromium/crPage.ts:1009`):
      // interception is enabled only when a `filechooser` listener
      // is registered. The CDP command is fired by
      // `update_file_chooser_intercept` from the page's
      // `on('filechooser', ...)` registration path. Saves one RTT per
      // newly-opened page (~5ms) when no test uses file pickers.
      let mut rx = transport.subscribe_event_method("Page.fileChooserOpened");

      while let Some(event) = crate::events::recv_tolerant(&mut rx).await {
        if let Some(ref expected_sid) = session_id {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(&**expected_sid) {
            continue;
          }
        }
        let Some(params) = event.get("params") else {
          continue;
        };
        let Some(backend_node_id) = params.get("backendNodeId").and_then(serde_json::Value::as_i64) else {
          continue;
        };
        let is_multiple = params
          .get("mode")
          .and_then(|v| v.as_str())
          .is_some_and(|m| m == "selectMultiple");

        // Upgrade the Weak back-reference; the outer Page may not
        // exist yet (listener spawned before `Page::new` populated
        // the backref) or may have been dropped. Either way, no
        // valid target — skip.
        let Some(page) = page_backref.upgrade() else {
          continue;
        };

        // Resolve backendNodeId -> AnyElement -> ElementHandle. This
        // is async, so we spawn the per-event work to keep the
        // outer subscription loop draining — rapid file-picker
        // triggers shouldn't queue up behind a slow DOM resolve.
        let page_clone = Arc::clone(&page);
        let manager_clone = file_chooser_manager.clone();
        tokio::spawn(async move {
          let Ok(element) = page_clone
            .inner()
            .resolve_backend_node(backend_node_id, "filechooser")
            .await
          else {
            // Frame/context may have gone away mid-resolve; Playwright
            // has the same guard and silently returns.
            return;
          };
          let Ok(handle) = crate::element_handle::ElementHandle::from_any_element(page_clone.clone(), element).await
          else {
            return;
          };
          let chooser = crate::file_chooser::FileChooser::new(handle, is_multiple);
          manager_clone.did_open(&chooser);
        });
      }
    });
  }

  /// Listen for `Browser.downloadWillBegin` / `Browser.downloadProgress`
  /// CDP events on this page and dispatch them through the page's
  /// [`crate::download::DownloadManager`].
  ///
  /// Sequence matters: subscribe to the event stream **before**
  /// sending `Browser.setDownloadBehavior`. A fast `<a download>` click
  /// can trigger `Browser.downloadWillBegin` before the configuration
  /// reply lands; subscribing after would race the event. Same
  /// rationale as the file-chooser listener's
  /// `setInterceptFileChooserDialog` ordering.
  ///
  /// Behaviour: `allowAndName` so Chrome writes each download to
  /// `<downloadPath>/<guid>` instead of the server's suggested name
  /// (protects tests against filename collisions on parallel downloads
  /// sharing a tempdir and matches Playwright's own behaviour at
  /// `server/chromium/crBrowser.ts:354`). `eventsEnabled: true` so
  /// `downloadProgress` fires terminal-state events — without it we'd
  /// never resolve `path()`.
  ///
  /// Filtering: `Browser.downloadWillBegin` carries `frameId`. We
  /// claim events whose `frameId` matches our page's main frame id
  /// (populated by the navigation path) so a multi-page browser doesn't
  /// cross-wire downloads. When a download fires before we've cached a
  /// main-frame id, we accept it optimistically — matches Playwright's
  /// `_findOwningPage` behaviour, which also falls through when it
  /// cannot resolve the frame.
  #[allow(clippy::too_many_lines)]
  fn spawn_download_listener(
    transport: Arc<T>,
    session_id: Option<Arc<str>>,
    browser_context_id: Option<Arc<str>>,
    download_manager: crate::download::DownloadManager,
    downloads_dir: Arc<tempfile::TempDir>,
    page_backref: crate::backend::PageBackref,
  ) {
    tokio::spawn(async move {
      // Subscribe FIRST to avoid racing any future enable reply.
      // We do NOT fire `Browser.setDownloadBehavior` here. Mirrors
      // Playwright's lazy pattern: download behaviour is configured
      // when a `download` listener registers (`page.on('download', ...)`,
      // `wait_for_download`) — see `enable_download_behavior` below.
      // Saves one RTT per page when no test uses downloads. Note:
      // `apply_context_options` still fires `setDownloadBehavior` when
      // the `BrowserContextOptions.accept_downloads` field is
      // explicitly set, so opt-in callers keep working.
      let _ = browser_context_id;
      let _ = downloads_dir;
      let mut rx = transport.subscribe_event_domain("Browser");

      while let Some(event) = crate::events::recv_tolerant(&mut rx).await {
        // `Browser.downloadWillBegin` / `Browser.downloadProgress` fire
        // on the root browser session (no `sessionId`) when
        // `eventsEnabled: true`. Events with a `sessionId` come from
        // page targets; those are irrelevant to downloads. Accept
        // events whose `sessionId` is absent OR matches ours — CDP's
        // flatten-mode behaviour varies by Chrome version and we want
        // to cover both.
        let event_sid = event.get("sessionId").and_then(|v| v.as_str());
        if let (Some(expected), Some(got)) = (session_id.as_deref(), event_sid) {
          if got != expected {
            continue;
          }
        }
        let method = event.get("method").and_then(|m| m.as_str()).unwrap_or("");
        match method {
          "Browser.downloadWillBegin" => {
            let Some(params) = event.get("params") else {
              continue;
            };
            let guid = params.get("guid").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if guid.is_empty() {
              continue;
            }
            let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let suggested = params
              .get("suggestedFilename")
              .and_then(|v| v.as_str())
              .unwrap_or("")
              .to_string();

            let Some(page) = page_backref.upgrade() else {
              continue;
            };

            // Build the canceler that issues `Browser.cancelDownload`.
            let transport_c = transport.clone();
            let session_c = session_id.clone();
            let ctx_c = browser_context_id.clone();
            let guid_for_cancel = guid.clone();
            let canceler: crate::download::DownloadCanceler = Arc::new(move || {
              let transport = transport_c.clone();
              let session = session_c.clone();
              let ctx = ctx_c.clone();
              let guid = guid_for_cancel.clone();
              Box::pin(async move {
                let mut params = serde_json::json!({ "guid": guid });
                if let Some(c) = ctx.as_deref() {
                  params["browserContextId"] = serde_json::Value::String(c.to_string());
                }
                transport
                  .send_command(session.as_deref(), "Browser.cancelDownload", &params)
                  .await
                  .map(|_| ())
                  .map_err(|e| crate::error::FerriError::protocol("Browser.cancelDownload", e.to_string()))
              })
            });

            let download = crate::download::Download::new(
              &page,
              guid,
              url,
              suggested,
              downloads_dir.path().to_path_buf(),
              canceler,
            );
            download_manager.did_open(&download);
          },
          "Browser.downloadProgress" => {
            let Some(params) = event.get("params") else {
              continue;
            };
            let guid = params.get("guid").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if guid.is_empty() {
              continue;
            }
            let state = params.get("state").and_then(|v| v.as_str()).unwrap_or("inProgress");
            match state {
              "completed" => {
                if let Some(d) = download_manager.take_for_guid(&guid) {
                  d.report_finished(None, None);
                }
              },
              "canceled" => {
                if let Some(d) = download_manager.take_for_guid(&guid) {
                  d.report_finished(None, Some("canceled".to_string()));
                }
              },
              // "inProgress" — no-op; we only surface terminal state.
              _ => {},
            }
          },
          _ => {},
        }
      }
    });
  }

  fn spawn_frame_context_tracker(
    transport: Arc<T>,
    session_id: Option<Arc<str>>,
    frame_contexts: Arc<tokio::sync::RwLock<FxHashMap<String, i64>>>,
    emitter: crate::events::EventEmitter,
  ) {
    tokio::spawn(async move {
      let mut runtime_rx = transport.subscribe_event_domain("Runtime");
      let mut page_rx = transport.subscribe_event_domain("Page");
      loop {
        let event = tokio::select! {
          ev = runtime_rx.recv() => match ev {
            Ok(event) => event,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
          },
          ev = page_rx.recv() => match ev {
            Ok(event) => event,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
          },
        };
        if let Some(ref expected_sid) = session_id {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(&**expected_sid) {
            continue;
          }
        }

        let method = event.get("method").and_then(|m| m.as_str()).unwrap_or("");
        match method {
          "Runtime.executionContextCreated" => {
            if let Some(ctx) = event.get("params").and_then(|p| p.get("context")) {
              let ctx_id = ctx.get("id").and_then(serde_json::Value::as_i64).unwrap_or(0);
              if let Some(aux) = ctx.get("auxData") {
                let frame_id = aux.get("frameId").and_then(|v| v.as_str()).unwrap_or("");
                let is_default = aux
                  .get("isDefault")
                  .and_then(serde_json::Value::as_bool)
                  .unwrap_or(false);
                if is_default && !frame_id.is_empty() {
                  frame_contexts.write().await.insert(frame_id.to_string(), ctx_id);
                }
              }
            }
          },
          "Runtime.executionContextDestroyed" => {
            if let Some(ctx_id) = event
              .get("params")
              .and_then(|p| p.get("executionContextId"))
              .and_then(serde_json::Value::as_i64)
            {
              let mut contexts = frame_contexts.write().await;
              contexts.retain(|_, &mut v| v != ctx_id);
            }
          },
          "Runtime.executionContextsCleared" => {
            frame_contexts.write().await.clear();
            // Init scripts registered via `Page.addScriptToEvaluateOnNewDocument`
            // are page-session-scoped, not context-scoped — they
            // survive context clears (which happen on every navigation
            // in Chrome). Resetting here forced a redundant
            // re-registration RTT on every page navigation.
          },
          "Page.frameAttached" => {
            if let Some(params) = event.get("params") {
              emitter.emit(crate::events::PageEvent::FrameAttached(super::FrameInfo {
                frame_id: params.get("frameId").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                parent_frame_id: params
                  .get("parentFrameId")
                  .and_then(|v| v.as_str())
                  .map(std::string::ToString::to_string),
                name: String::new(),
                url: String::new(),
              }));
            }
          },
          "Page.frameDetached" => {
            if let Some(fid) = event
              .get("params")
              .and_then(|p| p.get("frameId"))
              .and_then(|v| v.as_str())
            {
              frame_contexts.write().await.remove(fid);
              emitter.emit(crate::events::PageEvent::FrameDetached {
                frame_id: fid.to_string(),
              });
            }
          },
          "Page.frameNavigated" => {
            if let Some(frame) = event.get("params").and_then(|p| p.get("frame")) {
              // Note: we previously called `injected_script.reset()`
              // on main-frame navigation. That was wrong:
              // `Page.addScriptToEvaluateOnNewDocument` registers the
              // source for ALL future documents on this target, so
              // every post-navigation document already runs the
              // self-guarded `window.__fd` IIFE on its own. Resetting
              // forced a redundant `addScriptToEvaluateOnNewDocument`
              // RTT on every navigation — the bench's 100×nav workload
              // was paying ~5ms per test for nothing.
              emitter.emit(crate::events::PageEvent::FrameNavigated(super::FrameInfo {
                frame_id: frame.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                parent_frame_id: frame
                  .get("parentId")
                  .and_then(|v| v.as_str())
                  .map(std::string::ToString::to_string),
                name: frame.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                url: frame.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
              }));
            }
          },
          _ => {},
        }
      }
    });
  }

  // ---- Init Scripts ----

  pub async fn add_init_script(&self, source: &str) -> Result<String> {
    let result = self
      .cmd(
        "Page.addScriptToEvaluateOnNewDocument",
        serde_json::json!({"source": source}),
      )
      .await?;
    let id = result
      .get("identifier")
      .and_then(|v| v.as_str())
      .unwrap_or("")
      .to_string();
    Ok(id)
  }

  pub async fn remove_init_script(&self, identifier: &str) -> Result<()> {
    self
      .cmd(
        "Page.removeScriptToEvaluateOnNewDocument",
        serde_json::json!({"identifier": identifier}),
      )
      .await?;
    Ok(())
  }

  // ---- Exposed Functions ----

  /// The JS source for the binding controller injected as an init script.
  pub const BINDING_CONTROLLER_JS: &'static str = r"(function(){
if(globalThis.__fd_bc)return;
var bc={seq:0,cbs:{},fns:{}};
globalThis.__fd_bc=bc;
bc.add=function(name){
  bc.fns[name]=true;
  globalThis[name]=function(){
    var s=++bc.seq;
    var args=[];for(var i=0;i<arguments.length;i++)args.push(arguments[i]);
    var p=new Promise(function(r,j){bc.cbs[s]={r:r,j:j}});
    globalThis.__fd_binding__(JSON.stringify({name:name,seq:s,args:args}));
    return p;
  };
};
bc.del=function(name){delete bc.fns[name];delete globalThis[name]};
bc.resolve=function(seq,val){var c=bc.cbs[seq];if(c){delete bc.cbs[seq];c.r(val)}};
bc.reject=function(seq,err){var c=bc.cbs[seq];if(c){delete bc.cbs[seq];c.j(new Error(err))}};
})()";

  async fn ensure_binding_channel(&self) -> Result<()> {
    if self.binding_initialized.swap(true, std::sync::atomic::Ordering::SeqCst) {
      return Ok(());
    }
    self
      .cmd("Runtime.addBinding", serde_json::json!({"name": "__fd_binding__"}))
      .await?;
    self.add_init_script(Self::BINDING_CONTROLLER_JS).await?;
    self.evaluate(Self::BINDING_CONTROLLER_JS).await?;

    let t = self.transport.clone();
    let sid = self.session_id.clone();
    let fns = self.exposed_fns.clone();
    tokio::spawn(async move {
      let mut rx = t.subscribe_event_method("Runtime.bindingCalled");
      while let Some(event) = crate::events::recv_tolerant(&mut rx).await {
        if let Some(ref expected_sid) = sid {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(&**expected_sid) {
            continue;
          }
        }
        if let Some(params) = event.get("params") {
          let binding_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
          if binding_name != "__fd_binding__" {
            continue;
          }

          let payload_str = params.get("payload").and_then(|v| v.as_str()).unwrap_or("{}");
          let payload: serde_json::Value = serde_json::from_str(payload_str).unwrap_or_default();
          let fn_name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
          let seq = payload.get("seq").and_then(serde_json::Value::as_u64).unwrap_or(0);
          let args: Vec<serde_json::Value> = payload
            .get("args")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

          let maybe_fn = fns.read().await.get(&fn_name).cloned();
          if let Some(callback) = maybe_fn {
            let result = callback(args).await;
            let deliver_js = format!(
              "globalThis.__fd_bc.resolve({}, {})",
              seq,
              serde_json::to_string(&result).unwrap_or_else(|_| "null".into())
            );
            let _ = t
              .send_command(
                sid.as_deref(),
                "Runtime.evaluate",
                &serde_json::json!({"expression": deliver_js}),
              )
              .await;
          } else {
            let deliver_js = format!("globalThis.__fd_bc.reject({seq}, 'Function not found: {fn_name}')");
            let _ = t
              .send_command(
                sid.as_deref(),
                "Runtime.evaluate",
                &serde_json::json!({"expression": deliver_js}),
              )
              .await;
          }
        }
      }
    });
    Ok(())
  }

  pub async fn expose_function(&self, name: &str, func: crate::events::ExposedFn) -> Result<()> {
    self.ensure_binding_channel().await?;
    self.exposed_fns.write().await.insert(name.to_string(), func);
    let register_js = format!("globalThis.__fd_bc.add('{}')", crate::steps::js_escape(name));
    self.add_init_script(&register_js).await?;
    self.evaluate(&register_js).await?;
    Ok(())
  }

  pub async fn remove_exposed_function(&self, name: &str) -> Result<()> {
    self.exposed_fns.write().await.remove(name);
    let js = format!(
      "if(globalThis.__fd_bc)globalThis.__fd_bc.del('{}')",
      crate::steps::js_escape(name)
    );
    self.evaluate(&js).await?;
    Ok(())
  }

  // ---- Lifecycle ----

  pub async fn close_page(&self, opts: crate::options::PageCloseOptions) -> Result<()> {
    if self.closed.swap(true, std::sync::atomic::Ordering::SeqCst) {
      return Ok(());
    }
    // Two CDP paths, matching Playwright's crPage.ts:
    // * runBeforeUnload=true  → `Page.close` — fires beforeunload handlers.
    // * runBeforeUnload=false → `Target.closeTarget` — force-close.
    //
    // Context disposal is still handled by context.close() →
    // BrowserState::remove_context() → Target.disposeBrowserContext (one CDP
    // call kills the context + all its pages, matching Playwright's doClose).
    if opts.run_before_unload.unwrap_or(false) {
      let _ = self
        .transport
        .send_command(self.session_id.as_deref(), "Page.close", &super::EMPTY_PARAMS)
        .await;
    } else {
      let _ = self
        .transport
        .send_command(
          None,
          "Target.closeTarget",
          &serde_json::json!({"targetId": &*self.target_id}),
        )
        .await;
    }
    self.events.emit(crate::events::PageEvent::Close);
    Ok(())
  }

  #[must_use]
  pub fn is_closed(&self) -> bool {
    self.closed.load(std::sync::atomic::Ordering::SeqCst)
  }

  // ---- Network Interception ----

  async fn ensure_fetch_enabled(&self) -> Result<()> {
    let has_creds = self.http_credentials.read().await.is_some();
    if self.fetch_enabled.swap(true, std::sync::atomic::Ordering::SeqCst) {
      // Already enabled — but may need to re-enable with auth handling.
      if has_creds {
        let _ = self.cmd("Fetch.disable", serde_json::json!({})).await;
        self
          .cmd(
            "Fetch.enable",
            serde_json::json!({
                "patterns": [{"urlPattern": "*", "requestStage": "Request"}],
                "handleAuthRequests": true,
            }),
          )
          .await?;
      }
      return Ok(());
    }
    self
      .cmd(
        "Fetch.enable",
        serde_json::json!({
            "patterns": [{"urlPattern": "*", "requestStage": "Request"}],
            "handleAuthRequests": has_creds,
        }),
      )
      .await?;

    let t = self.transport.clone();
    let sid = self.session_id.clone();
    let routes = self.routes.clone();
    let creds = self.http_credentials.clone();
    tokio::spawn(async move {
      Self::handle_fetch_events(t, sid, routes, creds).await;
    });
    Ok(())
  }

  #[allow(clippy::too_many_lines)]
  async fn handle_fetch_events(
    transport: Arc<T>,
    session_id: Option<Arc<str>>,
    routes: Arc<tokio::sync::RwLock<Vec<crate::route::RegisteredRoute>>>,
    http_credentials: Arc<tokio::sync::RwLock<Option<crate::options::HttpCredentials>>>,
  ) {
    // Extract `scheme://host[:port]` from a request URL for
    // origin-scoped credential matching. Handles `http(s)://host[:port]/path`;
    // `data:` / `file:` / opaque schemes return `None` (credentials
    // don't apply to them).
    fn origin_of_url(url: &str) -> Option<String> {
      let (scheme, rest) = url.split_once("://")?;
      let host_and_port = rest.split(['/', '?', '#']).next().unwrap_or("");
      if host_and_port.is_empty() {
        return None;
      }
      Some(format!("{scheme}://{host_and_port}"))
    }
    let mut rx = transport.subscribe_event_domain("Fetch");
    loop {
      // Tolerate broadcast `Lagged` (slow-consumer overflow) so the
      // Fetch interceptor stays alive after a busy session — previously
      // a single overflow killed the listener, future requests dropped
      // through to the network and a routed URL hit `ERR_NAME_NOT_RESOLVED`.
      let event = match rx.recv().await {
        Ok(e) => e,
        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
      };
      if let Some(ref expected_sid) = session_id {
        let event_sid = event.get("sessionId").and_then(|v| v.as_str());
        if event_sid != Some(&**expected_sid) {
          continue;
        }
      }
      let method = event.get("method").and_then(|m| m.as_str());

      // ── Handle Fetch.authRequired — respond with stored credentials ──
      // Playwright scopes credentials to `options.httpCredentials.origin`
      // (see `crNetworkManager.ts::_authenticate`): when `origin` is
      // set and the incoming request's origin doesn't match, we
      // answer `Default` so the browser surfaces the native 401
      // instead of silently authenticating on the wrong host.
      if method == Some("Fetch.authRequired") {
        let Some(params) = event.get("params") else { continue };
        let request_id = params.get("requestId").and_then(|v| v.as_str()).unwrap_or("");
        let req_url = params
          .get("request")
          .and_then(|r| r.get("url"))
          .and_then(|v| v.as_str())
          .unwrap_or("");
        let creds = http_credentials.read().await;
        let response = if let Some(ref c) = *creds {
          let origin_matches = match c.origin.as_deref() {
            None => true,
            Some(expected) => origin_of_url(req_url).is_some_and(|o| o.eq_ignore_ascii_case(expected)),
          };
          if origin_matches {
            serde_json::json!({
              "requestId": request_id,
              "authChallengeResponse": {
                "response": "ProvideCredentials",
                "username": c.username,
                "password": c.password,
              }
            })
          } else {
            // Defer to the browser's default (cancel) so the 401
            // surfaces — matches Playwright's origin-mismatch path.
            serde_json::json!({
              "requestId": request_id,
              "authChallengeResponse": { "response": "Default" }
            })
          }
        } else {
          serde_json::json!({
            "requestId": request_id,
            "authChallengeResponse": { "response": "CancelAuth" }
          })
        };
        let _ = transport
          .send_command(session_id.as_deref(), "Fetch.continueWithAuth", &response)
          .await;
        continue;
      }

      if method != Some("Fetch.requestPaused") {
        continue;
      }
      let Some(params) = event.get("params") else { continue };
      let request_id = params.get("requestId").and_then(|v| v.as_str()).unwrap_or("");
      let req_obj = params.get("request");
      // Borrow URL directly from the JSON event — zero allocation for matching.
      let url = req_obj
        .and_then(|r| r.get("url"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

      // Match-and-consume under a single write lock so a `times`-limited
      // route can never fire more than its budget even when the same logical
      // request pauses at multiple Fetch stages. The matched route's counter
      // is decremented and the route removed the moment it reaches zero.
      let matched_handler = {
        let mut guard = routes.write().await;
        crate::route::take_matching_handler(&mut guard, url)
      };

      if let Some(handler) = matched_handler {
        // Only parse the full request when a route actually matched.
        let method = req_obj
          .and_then(|r| r.get("method"))
          .and_then(|v| v.as_str())
          .unwrap_or("GET");
        let resource_type = params.get("resourceType").and_then(|v| v.as_str()).unwrap_or("");
        let post_data = req_obj.and_then(|r| r.get("postData")).and_then(|v| v.as_str());
        let headers: FxHashMap<String, String> = req_obj
          .and_then(|r| r.get("headers"))
          .and_then(|h| h.as_object())
          .map(|obj| {
            obj
              .iter()
              .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
              .collect()
          })
          .unwrap_or_default();

        let intercepted = crate::route::InterceptedRequest {
          request_id: request_id.to_string(),
          url: url.to_string(),
          method: method.to_string(),
          headers,
          post_data: post_data.map(str::to_string),
          resource_type: resource_type.to_string(),
        };

        let (tx, rx) = tokio::sync::oneshot::channel();
        let route = crate::route::Route::new(intercepted, tx);
        handler(route);
        let action = rx.await.unwrap_or(crate::route::RouteAction::Continue(
          crate::route::ContinueOverrides::default(),
        ));
        Self::execute_route_action(&transport, session_id.as_deref(), request_id, Some(action)).await;
      } else {
        // No matching route — continue with zero allocation beyond the CDP command.
        let _ = transport
          .send_command(
            session_id.as_deref(),
            "Fetch.continueRequest",
            &serde_json::json!({"requestId": request_id}),
          )
          .await;
      }
    }
  }

  async fn execute_route_action(
    transport: &T,
    session_id: Option<&str>,
    request_id: &str,
    action: Option<crate::route::RouteAction>,
  ) {
    match action {
      Some(crate::route::RouteAction::Fulfill(resp)) => {
        let body_b64 = base64::engine::general_purpose::STANDARD.encode(&resp.body);
        let mut hdrs: Vec<serde_json::Value> = resp
          .headers
          .iter()
          .map(|(k, v)| serde_json::json!({"name": k, "value": v}))
          .collect();
        if let Some(ct) = &resp.content_type {
          if !hdrs
            .iter()
            .any(|h| h.get("name").and_then(|n| n.as_str()) == Some("content-type"))
          {
            hdrs.push(serde_json::json!({"name": "content-type", "value": ct}));
          }
        }
        let _ = transport
          .send_command(
            session_id,
            "Fetch.fulfillRequest",
            &serde_json::json!({
                "requestId": request_id,
                "responseCode": resp.status,
                "responsePhrase": crate::route::status_text(resp.status),
                "responseHeaders": hdrs,
                "body": body_b64,
            }),
          )
          .await;
      },
      Some(crate::route::RouteAction::Continue(overrides)) => {
        let mut params = serde_json::json!({"requestId": request_id});
        if let Some(url) = &overrides.url {
          params["url"] = serde_json::Value::String(url.clone());
        }
        if let Some(method) = &overrides.method {
          params["method"] = serde_json::Value::String(method.clone());
        }
        if let Some(headers) = &overrides.headers {
          let hdrs: Vec<serde_json::Value> = headers
            .iter()
            .map(|(k, v)| serde_json::json!({"name": k, "value": v}))
            .collect();
          params["headers"] = serde_json::Value::Array(hdrs);
        }
        if let Some(post_data) = &overrides.post_data {
          params["postData"] = serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(post_data));
        }
        let _ = transport
          .send_command(session_id, "Fetch.continueRequest", &params)
          .await;
      },
      Some(crate::route::RouteAction::Abort(reason)) => {
        let error_reason = match reason.to_lowercase().as_str() {
          "aborted" => "Aborted",
          "accessdenied" => "AccessDenied",
          "addressunreachable" => "AddressUnreachable",
          "blockedbyclient" => "BlockedByClient",
          "connectionfailed" => "ConnectionFailed",
          "connectionrefused" => "ConnectionRefused",
          "connectionreset" => "ConnectionReset",
          "internetdisconnected" => "InternetDisconnected",
          "namenotresolved" => "NameNotResolved",
          "timedout" => "TimedOut",
          _ => "Failed",
        };
        let _ = transport
          .send_command(
            session_id,
            "Fetch.failRequest",
            &serde_json::json!({
                "requestId": request_id,
                "errorReason": error_reason,
            }),
          )
          .await;
      },
      None => {
        let _ = transport
          .send_command(
            session_id,
            "Fetch.continueRequest",
            &serde_json::json!({"requestId": request_id}),
          )
          .await;
      },
    }
  }

  pub async fn route(
    &self,
    matcher: crate::url_matcher::UrlMatcher,
    handler: crate::route::RouteHandler,
    times: Option<u32>,
  ) -> Result<()> {
    self
      .routes
      .write()
      .await
      .push(crate::route::RegisteredRoute::new(matcher, handler, times));
    self.ensure_fetch_enabled().await
  }

  pub async fn unroute(&self, matcher: &crate::url_matcher::UrlMatcher) -> Result<()> {
    let mut routes = self.routes.write().await;
    routes.retain(|r| !r.matcher.equivalent(matcher));
    if routes.is_empty() && self.fetch_enabled.load(std::sync::atomic::Ordering::SeqCst) {
      self.fetch_enabled.store(false, std::sync::atomic::Ordering::SeqCst);
      let _ = self.cmd("Fetch.disable", serde_json::json!({})).await;
    }
    Ok(())
  }

  pub async fn unroute_all(&self, _behavior: crate::options::UnrouteBehavior) -> Result<()> {
    let mut routes = self.routes.write().await;
    routes.clear();
    if self.fetch_enabled.load(std::sync::atomic::Ordering::SeqCst) {
      self.fetch_enabled.store(false, std::sync::atomic::Ordering::SeqCst);
      let _ = self.cmd("Fetch.disable", serde_json::json!({})).await;
    }
    Ok(())
  }

  // ── Handle lifecycle ──

  /// Release the CDP `RemoteObject` identified by `object_id` via
  /// `Runtime.releaseObject`. Used by `AnyPage::release_handle` when
  /// disposing a `JSHandle` / `ElementHandle` on a CDP backend.
  ///
  /// # Errors
  ///
  /// Returns the CDP transport error if the call fails. Already-released
  /// objects surface a protocol error containing `No object with id`
  /// which callers (typically `JSHandle::dispose`) may choose to treat
  /// as success — the dispose path here forwards the error as-is so
  /// idempotence is handled client-side by the `disposed` flag, not by
  /// swallowing protocol failures.
  pub async fn release_object(&self, object_id: &str) -> Result<()> {
    self
      .cmd("Runtime.releaseObject", serde_json::json!({"objectId": object_id}))
      .await
      .map(|_| ())
  }
}

// ---- CdpElement<T> ---------------------------------------------------------

pub struct CdpElement<T: CdpTransport> {
  transport: Arc<T>,
  session_id: Option<Arc<str>>,
  handles: Arc<tokio::sync::Mutex<CdpElementHandles>>,
}

struct CdpElementHandles {
  node_id: Option<i64>,
  object_id: Option<Arc<str>>,
}

impl<T: CdpTransport> Clone for CdpElement<T> {
  fn clone(&self) -> Self {
    Self {
      transport: self.transport.clone(),
      session_id: self.session_id.clone(),
      handles: self.handles.clone(),
    }
  }
}

impl<T: CdpTransport> CdpElement<T> {
  async fn cmd(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    self
      .transport
      .send_command(self.session_id.as_deref(), method, &params)
      .await
  }

  async fn resolve_node_id_from_object(&self, object_id: &str) -> Result<i64> {
    let node_result = self
      .cmd("DOM.requestNode", serde_json::json!({"objectId": object_id}))
      .await?;
    let node_id = node_result
      .get("nodeId")
      .and_then(serde_json::Value::as_i64)
      .ok_or_else(|| FerriError::protocol("DOM.requestNode", "Could not resolve element nodeId"))?;
    if node_id == 0 {
      return Err(FerriError::protocol("DOM.requestNode", "Element not found"));
    }
    Ok(node_id)
  }

  async fn resolve_object_id_from_node(&self, node_id: i64) -> Result<Arc<str>> {
    let resolved = self
      .cmd("DOM.resolveNode", serde_json::json!({"nodeId": node_id}))
      .await?;
    resolved
      .get("object")
      .and_then(|o| o.get("objectId"))
      .and_then(|v| v.as_str())
      .map(Arc::from)
      .ok_or_else(|| FerriError::protocol("DOM.resolveNode", "Cannot resolve element"))
  }

  async fn node_id(&self) -> Result<i64> {
    let object_id = {
      let handles = self.handles.lock().await;
      if let Some(node_id) = handles.node_id {
        return Ok(node_id);
      }
      handles.object_id.clone()
    };

    let Some(object_id) = object_id else {
      return Err(FerriError::backend("Element handle has neither nodeId nor objectId"));
    };
    let node_id = self.resolve_node_id_from_object(&object_id).await?;
    let mut handles = self.handles.lock().await;
    handles.node_id = Some(node_id);
    Ok(node_id)
  }

  async fn object_id(&self) -> Result<Arc<str>> {
    let node_id = {
      let handles = self.handles.lock().await;
      if let Some(object_id) = &handles.object_id {
        return Ok(object_id.clone());
      }
      handles.node_id
    };

    let Some(node_id) = node_id else {
      return Err(FerriError::backend("Element handle has neither nodeId nor objectId"));
    };
    let object_id = self.resolve_object_id_from_node(node_id).await?;
    let mut handles = self.handles.lock().await;
    handles.object_id = Some(object_id.clone());
    Ok(object_id)
  }

  /// Public accessor for the element's cached `RemoteObjectId`. Resolves
  /// and caches via `DOM.resolveNode` on first call. Used by
  /// [`crate::backend::element_handle_remote`] to hand an
  /// [`crate::js_handle::HandleRemote::Cdp`] payload back to
  /// [`crate::js_handle::JSHandle`] / [`crate::element_handle::ElementHandle`]
  /// at handle-materialization time.
  ///
  /// # Errors
  ///
  /// Returns an error if the element carries neither a cached `node_id`
  /// nor an `object_id` — should not happen for elements freshly
  /// returned from `find_element` / `evaluate_to_element`.
  pub async fn ensure_object_id(&self) -> Result<Arc<str>> {
    self.object_id().await
  }

  /// Get element center coordinates for clicking.
  async fn get_center(&self) -> Result<(f64, f64)> {
    let node_id = self.node_id().await?;
    let result = self
      .cmd("DOM.getBoxModel", serde_json::json!({"nodeId": node_id}))
      .await?;
    let content = result
      .get("model")
      .and_then(|m| m.get("content"))
      .and_then(|c| c.as_array())
      .ok_or_else(|| FerriError::protocol("DOM.getBoxModel", "No box model"))?;

    if content.len() < 8 {
      return Err(FerriError::protocol("DOM.getBoxModel", "Invalid box model"));
    }
    let x1 = content[0].as_f64().unwrap_or(0.0);
    let y1 = content[1].as_f64().unwrap_or(0.0);
    let x3 = content[4].as_f64().unwrap_or(0.0);
    let y3 = content[5].as_f64().unwrap_or(0.0);

    Ok((f64::midpoint(x1, x3), f64::midpoint(y1, y3)))
  }

  pub async fn call_js_fn_value(&self, function: &str) -> Result<Option<serde_json::Value>> {
    let object_id = self.object_id().await?;
    let result = self
      .cmd(
        "Runtime.callFunctionOn",
        serde_json::json!({
            "objectId": &*object_id,
            "functionDeclaration": function,
            "returnByValue": true,
            // Allow `function` to be an async function or return a
            // Promise — CDP awaits the resolution before returning so
            // callers don't have to wrap the page-side helper in a
            // synthetic synchronous shell. Synchronous returns pay no
            // extra RTT; Chrome short-circuits when the result is not
            // a Promise.
            "awaitPromise": true,
        }),
      )
      .await?;
    Ok(result.get("result").and_then(|r| r.get("value")).cloned())
  }

  pub async fn click(&self) -> Result<()> {
    // `Input.dispatchMouseEvent` uses top-level page coordinates. Walk
    // up the frame chain (`window.frameElement.getBoundingClientRect()`)
    // and accumulate per-iframe offsets so a button inside an iframe
    // lands at the right page-level coords. Playwright achieves this by
    // having a per-frame CDP session — we have a single session, so
    // we do the offset math in JS instead.
    let center = self
      .call_js_fn_value(
        "function() {
          this.scrollIntoViewIfNeeded();
          var r = this.getBoundingClientRect();
          var x = r.x + r.width / 2;
          var y = r.y + r.height / 2;
          var win = this.ownerDocument.defaultView;
          while (win && win !== win.parent && win.frameElement) {
            var fr = win.frameElement.getBoundingClientRect();
            x += fr.x;
            y += fr.y;
            win = win.parent;
          }
          return { x: x, y: y };
        }",
      )
      .await?;

    if let Some(c) = center {
      let x = c.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
      let y = c.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
      if x == 0.0 && y == 0.0 {
        return self.call_js_fn("function() { this.click(); }").await;
      }
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 1}),
        )
        .await?;
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 1}),
        )
        .await?;
      Ok(())
    } else {
      self.call_js_fn("function() { this.click(); }").await
    }
  }

  pub async fn dblclick(&self) -> Result<()> {
    let center = self.call_js_fn_value(
            "function() { this.scrollIntoViewIfNeeded(); var r = this.getBoundingClientRect(); return {x: r.x + r.width/2, y: r.y + r.height/2}; }"
        ).await?;

    if let Some(c) = center {
      let x = c.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
      let y = c.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
      if x == 0.0 && y == 0.0 {
        return self
          .call_js_fn("function() { this.dispatchEvent(new MouseEvent('dblclick', {bubbles:true})); }")
          .await;
      }
      // First click (clickCount=1) fires 'click'
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 1}),
        )
        .await?;
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 1}),
        )
        .await?;
      // Second click (clickCount=2) fires 'dblclick'
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 2}),
        )
        .await?;
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 2}),
        )
        .await?;
      Ok(())
    } else {
      self
        .call_js_fn("function() { this.dispatchEvent(new MouseEvent('dblclick', {bubbles:true})); }")
        .await
    }
  }

  pub async fn hover(&self) -> Result<()> {
    self.scroll_into_view().await?;
    let (x, y) = self.get_center().await?;
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mouseMoved", "x": x, "y": y}),
      )
      .await?;
    Ok(())
  }

  pub async fn type_str(&self, text: &str) -> Result<()> {
    self.click().await?;
    self.cmd("Input.insertText", serde_json::json!({"text": text})).await?;
    Ok(())
  }

  pub async fn call_js_fn(&self, function: &str) -> Result<()> {
    let object_id = self.object_id().await?;
    self
      .cmd(
        "Runtime.callFunctionOn",
        serde_json::json!({
            "objectId": &*object_id,
            "functionDeclaration": function,
        }),
      )
      .await?;
    Ok(())
  }

  pub async fn scroll_into_view(&self) -> Result<()> {
    // Prefer `objectId` over `nodeId`. CDP's `DOM.scrollIntoViewIfNeeded`
    // accepts either, but the DOM agent's `nodeId` map is keyed on the
    // current document version returned by `DOM.getDocument`. Lazy
    // bootstrap (PERF_AUDIT §M.4) skips that call to save an RTT, so
    // `DOM.requestNode` reports `nodeId: 0` on a fresh page and the
    // caller sees a spurious "Element not found". Routing through
    // the Runtime `objectId` (already resolved by the page-side
    // selector engine) bypasses the DOM-agent state entirely.
    let object_id = self.object_id().await?;
    self
      .cmd(
        "DOM.scrollIntoViewIfNeeded",
        serde_json::json!({"objectId": &*object_id}),
      )
      .await?;
    Ok(())
  }

  pub async fn screenshot(&self, format: ImageFormat) -> Result<Vec<u8>> {
    // Prefer `objectId` over `nodeId` for the same reason as
    // [`Self::scroll_into_view`]: the DOM-agent nodeId map relies on
    // a prior `DOM.getDocument`, which the lazy bootstrap skips
    // (PERF_AUDIT §M.4). `DOM.getBoxModel` accepts `objectId`
    // directly.
    let object_id = self.object_id().await?;
    let result = self
      .cmd("DOM.getBoxModel", serde_json::json!({"objectId": &*object_id}))
      .await?;
    let content = result
      .get("model")
      .and_then(|m| m.get("content"))
      .and_then(|c| c.as_array())
      .ok_or_else(|| FerriError::protocol("DOM.getBoxModel", "No box model"))?;

    if content.len() < 8 {
      return Err(FerriError::protocol("DOM.getBoxModel", "Invalid box model"));
    }

    let x = content[0].as_f64().unwrap_or(0.0);
    let y = content[1].as_f64().unwrap_or(0.0);
    let w = content[4].as_f64().unwrap_or(0.0) - x;
    let h = content[5].as_f64().unwrap_or(0.0) - y;

    let fmt = match format {
      ImageFormat::Png => "png",
      ImageFormat::Jpeg => "jpeg",
      ImageFormat::Webp => "webp",
    };

    let result = self
      .cmd(
        "Page.captureScreenshot",
        serde_json::json!({
            "format": fmt,
            "clip": {"x": x, "y": y, "width": w, "height": h, "scale": 1}
        }),
      )
      .await?;
    let data = result
      .get("data")
      .and_then(|v| v.as_str())
      .ok_or_else(|| FerriError::backend("No screenshot data"))?;
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
      .map_err(|e| FerriError::Backend(format!("Decode: {e}")))
  }
}

// ── NetworkTracker ─────────────────────────────────────────────────────────
//
// Per-page bookkeeping for live `Request` / `Response` / `WebSocket`
// objects. CDP delivers network events out of order — `responseReceived`
// can fire before `requestWillBeSentExtraInfo`; `loadingFinished` arrives
// independently. Tracker indexes by request id and threads the same
// `Request` Arc through every event so listeners see live state.

struct NetworkTracker<T: CdpTransport> {
  transport: Arc<T>,
  session_id: Option<Arc<str>>,
  requests: tokio::sync::Mutex<FxHashMap<String, network::Request>>,
  responses: tokio::sync::Mutex<FxHashMap<String, Response>>,
  websockets: tokio::sync::Mutex<FxHashMap<String, WebSocket>>,
  // Buffered extra-info events that may arrive before the main events.
  pending_request_extra: tokio::sync::Mutex<FxHashMap<String, Vec<HeaderEntry>>>,
  pending_response_extra: tokio::sync::Mutex<FxHashMap<String, Vec<HeaderEntry>>>,
  nav_request_slot: crate::network::NavRequestSlot,
}

impl<T: CdpTransport + 'static> NetworkTracker<T> {
  fn new(transport: Arc<T>, session_id: Option<Arc<str>>, nav_request_slot: crate::network::NavRequestSlot) -> Self {
    Self {
      transport,
      session_id,
      requests: tokio::sync::Mutex::new(FxHashMap::default()),
      responses: tokio::sync::Mutex::new(FxHashMap::default()),
      websockets: tokio::sync::Mutex::new(FxHashMap::default()),
      pending_request_extra: tokio::sync::Mutex::new(FxHashMap::default()),
      pending_response_extra: tokio::sync::Mutex::new(FxHashMap::default()),
      nav_request_slot,
    }
  }

  async fn on_request_will_be_sent(
    self: &Arc<Self>,
    params: &serde_json::Value,
    network_log: &Arc<RwLock<Vec<NetworkRequest>>>,
    emitter: &crate::events::EventEmitter,
  ) {
    let request_id = params
      .get("requestId")
      .and_then(|v| v.as_str())
      .unwrap_or("")
      .to_string();
    if request_id.is_empty() {
      return;
    }
    let req = params.get("request");
    let url = req
      .and_then(|r| r.get("url"))
      .and_then(|v| v.as_str())
      .unwrap_or("")
      .to_string();
    let method = req
      .and_then(|r| r.get("method"))
      .and_then(|v| v.as_str())
      .unwrap_or("GET")
      .to_string();
    let resource_type = params.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let headers = req
      .and_then(|r| r.get("headers"))
      .and_then(|h| h.as_object())
      .map(|obj| {
        obj
          .iter()
          .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
          .collect::<Headers>()
      })
      .unwrap_or_default();
    let post_data = req
      .and_then(|r| r.get("postData"))
      .and_then(|v| v.as_str())
      .map(|s| s.as_bytes().to_vec());
    let frame_id = params
      .get("frameId")
      .and_then(|v| v.as_str())
      .map(std::string::ToString::to_string);
    let is_navigation_request = params
      .get("loaderId")
      .and_then(|v| v.as_str())
      .is_some_and(|loader| params.get("requestId").and_then(|v| v.as_str()) == Some(loader));

    // Redirect detection: when redirectResponse is set we synthesise a
    // Response on the prior request, then create a NEW request linked
    // to it via redirected_from. Same CDP requestId is reused for the
    // chain, so we re-key the prior in the tracker map.
    let redirected_from = if let Some(redirect_resp) = params.get("redirectResponse") {
      let mut requests = self.requests.lock().await;
      if let Some(prev) = requests.remove(&request_id) {
        let synthesised = self.build_response_from_value(prev.clone(), redirect_resp, &request_id);
        prev.set_response(&synthesised).await;
        synthesised.finish_success().await;
        emitter.emit(crate::events::PageEvent::Response(synthesised));
        emitter.emit(crate::events::PageEvent::RequestFinished(prev.clone()));
        Some(prev)
      } else {
        None
      }
    } else {
      None
    };

    let raw_headers_fn = self.make_request_raw_headers_fn(&request_id);

    let new_request = network::Request::new(RequestInit {
      id: request_id.clone(),
      url,
      method,
      resource_type,
      is_navigation_request,
      post_data,
      headers,
      frame_id,
      redirected_from,
      timing: None,
      raw_headers_fn: Some(raw_headers_fn),
    });

    if let Some(extras) = self.pending_request_extra.lock().await.remove(&request_id) {
      new_request.set_raw_headers(extras).await;
    }

    self.requests.lock().await.insert(request_id, new_request.clone());

    // Main-document navigations: update the per-page slot so
    // `AnyPage::goto` / `reload` / history traversals can resolve the
    // final `Response` after the lifecycle waiter fires. CDP flags a
    // request as navigation by setting `loaderId == requestId`; the
    // slot therefore tracks each redirect hop (same requestId, reused
    // across the chain) and naturally ends up pointing at the final
    // request in the chain when redirects settle.
    if new_request.is_navigation_request() {
      self.nav_request_slot.set(new_request.clone());
    }

    crate::state::push_capped(
      &mut *network_log.write().await,
      new_request.clone(),
      crate::state::NETWORK_LOG_CAP,
    );
    emitter.emit(crate::events::PageEvent::Request(new_request));
  }

  async fn on_request_extra_info(self: &Arc<Self>, params: &serde_json::Value) {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
      return;
    };
    let raw = parse_raw_headers(params.get("headers"));
    let requests = self.requests.lock().await;
    if let Some(req) = requests.get(request_id) {
      req.set_raw_headers(raw).await;
    } else {
      drop(requests);
      self
        .pending_request_extra
        .lock()
        .await
        .insert(request_id.to_string(), raw);
    }
  }

  async fn on_response_received(self: &Arc<Self>, params: &serde_json::Value, emitter: &crate::events::EventEmitter) {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
      return;
    };
    let request_id = request_id.to_string();
    let Some(req) = self.requests.lock().await.get(&request_id).cloned() else {
      return;
    };
    let Some(resp_value) = params.get("response") else {
      return;
    };
    let response = self.build_response_from_value(req.clone(), resp_value, &request_id);
    if let Some(extras) = self.pending_response_extra.lock().await.remove(&request_id) {
      response.set_raw_headers(extras).await;
    }
    self.responses.lock().await.insert(request_id, response.clone());
    req.set_response(&response).await;
    emitter.emit(crate::events::PageEvent::Response(response));
  }

  async fn on_response_extra_info(self: &Arc<Self>, params: &serde_json::Value) {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
      return;
    };
    let raw = parse_raw_headers(params.get("headers"));
    let responses = self.responses.lock().await;
    if let Some(resp) = responses.get(request_id) {
      resp.set_raw_headers(raw).await;
    } else {
      drop(responses);
      self
        .pending_response_extra
        .lock()
        .await
        .insert(request_id.to_string(), raw);
    }
  }

  async fn on_loading_finished(self: &Arc<Self>, params: &serde_json::Value, emitter: &crate::events::EventEmitter) {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
      return;
    };
    let Some(req) = self.requests.lock().await.get(request_id).cloned() else {
      return;
    };
    let total_encoded = params
      .get("encodedDataLength")
      .and_then(serde_json::Value::as_f64)
      .unwrap_or(0.0);
    if total_encoded > 0.0 {
      // Best-effort allocation of `encodedDataLength` to body vs
      // headers. CDP doesn't split it — Playwright treats the full
      // value as response body size for `responseBodySize` and
      // queries `getResponseBody` for the actual body byte length on
      // demand. We mirror that: store raw transfer length here.
      #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
      let response_body = total_encoded as u64;
      req.update_sizes(RequestSizes {
        response_body,
        ..RequestSizes::default()
      });
    }
    if let Some(resp) = self.responses.lock().await.get(request_id).cloned() {
      resp.finish_success().await;
    }
    emitter.emit(crate::events::PageEvent::RequestFinished(req));
  }

  async fn on_loading_failed(self: &Arc<Self>, params: &serde_json::Value, emitter: &crate::events::EventEmitter) {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
      return;
    };
    let Some(req) = self.requests.lock().await.get(request_id).cloned() else {
      return;
    };
    let error_text = params
      .get("errorText")
      .and_then(|v| v.as_str())
      .unwrap_or("net::ERR_FAILED")
      .to_string();
    req.set_failure(error_text.clone());
    if let Some(resp) = self.responses.lock().await.get(request_id).cloned() {
      resp.finish_failure(error_text).await;
    }
    emitter.emit(crate::events::PageEvent::RequestFailed(req));
  }

  async fn on_websocket_created(self: &Arc<Self>, params: &serde_json::Value, emitter: &crate::events::EventEmitter) {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
      return;
    };
    let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let ws = WebSocket::new(url);
    self.websockets.lock().await.insert(request_id.to_string(), ws.clone());
    emitter.emit(crate::events::PageEvent::WebSocket(ws));
  }

  async fn on_websocket_frame_sent(self: &Arc<Self>, params: &serde_json::Value) {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
      return;
    };
    let payload = parse_websocket_frame(params);
    if let Some(ws) = self.websockets.lock().await.get(request_id) {
      ws.emit_frame_sent(payload);
    }
  }

  async fn on_websocket_frame_received(self: &Arc<Self>, params: &serde_json::Value) {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
      return;
    };
    let payload = parse_websocket_frame(params);
    if let Some(ws) = self.websockets.lock().await.get(request_id) {
      ws.emit_frame_received(payload);
    }
  }

  async fn on_websocket_error(self: &Arc<Self>, params: &serde_json::Value) {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
      return;
    };
    let msg = params
      .get("errorMessage")
      .and_then(|v| v.as_str())
      .unwrap_or("WebSocket error")
      .to_string();
    if let Some(ws) = self.websockets.lock().await.get(request_id) {
      ws.emit_error(msg);
    }
  }

  async fn on_websocket_closed(self: &Arc<Self>, params: &serde_json::Value) {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
      return;
    };
    if let Some(ws) = self.websockets.lock().await.remove(request_id) {
      ws.emit_close();
    }
  }

  fn build_response_from_value(
    self: &Arc<Self>,
    request: network::Request,
    resp: &serde_json::Value,
    request_id: &str,
  ) -> Response {
    let url = resp.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let status = resp.get("status").and_then(serde_json::Value::as_i64).unwrap_or(0);
    let status_text = resp
      .get("statusText")
      .and_then(|v| v.as_str())
      .unwrap_or("")
      .to_string();
    let from_service_worker = resp
      .get("fromServiceWorker")
      .and_then(serde_json::Value::as_bool)
      .unwrap_or(false);
    let http_version = resp
      .get("protocol")
      .and_then(|v| v.as_str())
      .map(std::string::ToString::to_string);
    let headers = resp
      .get("headers")
      .and_then(|h| h.as_object())
      .map(|obj| {
        obj
          .iter()
          .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
          .collect::<Headers>()
      })
      .unwrap_or_default();
    let remote_addr = resp.get("remoteIPAddress").and_then(|v| v.as_str()).map(|ip| {
      let port = resp
        .get("remotePort")
        .and_then(serde_json::Value::as_u64)
        .and_then(|p| u16::try_from(p).ok())
        .unwrap_or(0);
      RemoteAddr {
        ip_address: ip.to_string(),
        port,
      }
    });
    let security = resp
      .get("securityDetails")
      .and_then(|s| s.as_object())
      .map(|obj| SecurityDetails {
        protocol: obj.get("protocol").and_then(|v| v.as_str()).map(String::from),
        subject_name: obj.get("subjectName").and_then(|v| v.as_str()).map(String::from),
        issuer: obj.get("issuer").and_then(|v| v.as_str()).map(String::from),
        valid_from: obj.get("validFrom").and_then(serde_json::Value::as_f64),
        valid_to: obj.get("validTo").and_then(serde_json::Value::as_f64),
      });
    let timing = resp.get("timing").map(parse_timing).unwrap_or_default();
    request.update_timing(timing);

    let body_fn = self.make_response_body_fn(request_id);
    let raw_headers_fn = self.make_response_raw_headers_fn(request_id);

    Response::new(ResponseInit {
      request,
      url,
      status,
      status_text,
      from_service_worker,
      http_version,
      headers,
      remote_addr,
      security_details: security,
      body_fn: Some(body_fn),
      raw_headers_fn: Some(raw_headers_fn),
    })
  }

  fn make_response_body_fn(self: &Arc<Self>, request_id: &str) -> BodyFn {
    let transport = self.transport.clone();
    let session_id = self.session_id.clone();
    let request_id = request_id.to_string();
    Arc::new(move || {
      let transport = transport.clone();
      let session_id = session_id.clone();
      let request_id = request_id.clone();
      Box::pin(async move {
        let resp = transport
          .send_command(
            session_id.as_deref(),
            "Network.getResponseBody",
            &serde_json::json!({"requestId": request_id}),
          )
          .await
          .map_err(|e| crate::error::FerriError::Protocol {
            method: "Network.getResponseBody".into(),
            message: e.to_string(),
          })?;
        let body = resp.get("body").and_then(|v| v.as_str()).unwrap_or("");
        let base64_encoded = resp
          .get("base64Encoded")
          .and_then(serde_json::Value::as_bool)
          .unwrap_or(false);
        if base64_encoded {
          base64::engine::general_purpose::STANDARD
            .decode(body)
            .map_err(|e| crate::error::FerriError::Backend(format!("base64 decode: {e}")))
        } else {
          Ok(body.as_bytes().to_vec())
        }
      })
    })
  }

  fn make_request_raw_headers_fn(self: &Arc<Self>, request_id: &str) -> RawHeadersFn {
    let tracker = self.clone();
    let request_id = request_id.to_string();
    Arc::new(move || {
      let tracker = tracker.clone();
      let request_id = request_id.clone();
      Box::pin(async move {
        // Fall back to whatever headers the request currently has if no
        // extraInfo arrived (matches Playwright when CDP doesn't fire
        // it, e.g. in Service-Worker-served responses).
        if let Some(req) = tracker.requests.lock().await.get(&request_id) {
          let arr = req.headers_array().await;
          return Ok(arr);
        }
        Ok(Vec::new())
      })
    })
  }

  fn make_response_raw_headers_fn(self: &Arc<Self>, request_id: &str) -> RawHeadersFn {
    let tracker = self.clone();
    let request_id = request_id.to_string();
    Arc::new(move || {
      let tracker = tracker.clone();
      let request_id = request_id.clone();
      Box::pin(async move {
        if let Some(resp) = tracker.responses.lock().await.get(&request_id) {
          return Ok(resp.headers_array().await);
        }
        Ok(Vec::new())
      })
    })
  }
}

fn parse_raw_headers(headers: Option<&serde_json::Value>) -> Vec<HeaderEntry> {
  let Some(headers) = headers else {
    return Vec::new();
  };
  let Some(obj) = headers.as_object() else {
    return Vec::new();
  };
  let mut out = Vec::with_capacity(obj.len());
  for (name, value) in obj {
    let raw = value.as_str().unwrap_or("");
    // CDP joins duplicate header values with `\n`; explode them back so
    // Playwright's `headersArray()` shape is preserved.
    for part in raw.split('\n') {
      out.push(HeaderEntry {
        name: name.clone(),
        value: part.to_string(),
      });
    }
  }
  out
}

fn parse_timing(value: &serde_json::Value) -> RequestTiming {
  let f = |key: &str, default: f64| value.get(key).and_then(serde_json::Value::as_f64).unwrap_or(default);
  RequestTiming {
    start_time: f("requestTime", 0.0) * 1000.0,
    domain_lookup_start: f("dnsStart", -1.0),
    domain_lookup_end: f("dnsEnd", -1.0),
    connect_start: f("connectStart", -1.0),
    secure_connection_start: f("sslStart", -1.0),
    connect_end: f("connectEnd", -1.0),
    request_start: f("sendStart", -1.0),
    response_start: f("receiveHeadersStart", -1.0),
    response_end: f("receiveHeadersEnd", -1.0),
  }
}

fn parse_websocket_frame(params: &serde_json::Value) -> WebSocketPayload {
  let response = params.get("response");
  let opcode = response
    .and_then(|r| r.get("opcode"))
    .and_then(serde_json::Value::as_u64)
    .unwrap_or(1);
  let payload = response
    .and_then(|r| r.get("payloadData"))
    .and_then(|v| v.as_str())
    .unwrap_or("");
  if opcode == 2 {
    let bytes = base64::engine::general_purpose::STANDARD
      .decode(payload)
      .unwrap_or_else(|_| payload.as_bytes().to_vec());
    WebSocketPayload::Binary(bytes)
  } else {
    WebSocketPayload::Text(payload.to_string())
  }
}
