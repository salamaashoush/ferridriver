//! `CdpBrowser` launch, connect, context, target, and popup management.

use super::*;

impl CdpBrowser {
  /// Every cookie of a browser context, without a page: Playwright's
  /// `CRBrowserContext.doGetCookies` (`Storage.getCookies` on the
  /// browser session). `None` is the default context.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Storage.getCookies`, the target has closed, or the command outlives
  /// its time budget.
  pub async fn context_cookies(&self, browser_context_id: Option<&str>) -> Result<Vec<CookieData>> {
    let result = self
      .transport
      .send_command(None, "Storage.getCookies", &context_params(browser_context_id))
      .await?;
    Ok(
      result
        .get("cookies")
        .and_then(serde_json::Value::as_array)
        .map(|cookies| cookies.iter().map(cookie_from_cdp).collect())
        .unwrap_or_default(),
    )
  }

  /// Playwright's `CRBrowserContext.addCookies`: `Storage.setCookies`.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Storage.setCookies`, the target has closed, or the command outlives
  /// its time budget.
  pub async fn add_context_cookies(&self, browser_context_id: Option<&str>, cookies: &[CookieData]) -> Result<()> {
    let mut params = context_params(browser_context_id);
    params["cookies"] = cookies.iter().map(cookie_to_cdp).collect();
    self.transport.send_command(None, "Storage.setCookies", &params).await?;
    Ok(())
  }

  /// Playwright's `CRBrowserContext.doClearCookies`: `Storage.clearCookies`.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Storage.clearCookies`, the target has closed, or the command
  /// outlives its time budget.
  pub async fn clear_context_cookies(&self, browser_context_id: Option<&str>) -> Result<()> {
    self
      .transport
      .send_command(None, "Storage.clearCookies", &context_params(browser_context_id))
      .await?;
    Ok(())
  }

  /// Whether this browser's connection is still up.
  ///
  /// False once the transport hits EOF, which is what a crash or an
  /// OOM-kill looks like from here.
  #[must_use]
  pub fn is_alive(&self) -> bool {
    !self.transport.is_disconnected()
  }

  /// Resolves once the connection to the browser has ended.
  pub fn closed(&self) -> futures::future::BoxFuture<'static, ()> {
    self.transport.closed()
  }

  /// Product version string captured at handshake.
  #[must_use]
  pub fn version(&self) -> &str {
    &self.version
  }

  /// The lock a page holds while it is captured, for a browser that
  /// draws only the tab in front: Chrome on an Android device, the one
  /// browser with a single context. A tab behind another there never
  /// produces the frame a capture waits for, so the page is brought to
  /// the front first and nothing else may take its place until the
  /// capture is done. `None` for a browser that draws every tab.
  pub(crate) fn front_tab_turn(&self) -> Option<Arc<tokio::sync::Mutex<()>>> {
    (!self.isolated_contexts).then(|| Arc::clone(&self.front_tab))
  }

  /// Have this browser's downloads written on its own machine and
  /// fetched from there. For the first caller only, and before a page
  /// asks for downloads: the directory is told to the browser once.
  pub(crate) fn set_remote_downloads(&self, remote: crate::download::RemoteDownloads) {
    let _ = self.downloads_dir.remote.set(remote);
  }

  /// Shared transport handle for attaching a public raw
  /// [`crate::cdp_session::CdpSession`] to the browser target.
  pub(crate) fn transport_arc(&self) -> Arc<CdpTransport> {
    Arc::clone(&self.transport)
  }

  /// Transport flavour (`CdpPipe` or `CdpWs`) behind this browser.
  #[must_use]
  pub(crate) fn backend_kind(&self) -> super::super::BackendKind {
    self.transport.kind()
  }
}

impl Clone for CdpBrowser {
  fn clone(&self) -> Self {
    Self {
      transport: Arc::clone(&self.transport),
      child: Arc::clone(&self.child),
      attached_targets: Arc::clone(&self.attached_targets),
      spares: Arc::clone(&self.spares),
      version: Arc::clone(&self.version),
      isolated_contexts: self.isolated_contexts,
      headful: self.headful,
      front_tab: Arc::clone(&self.front_tab),
      popup_taps: Arc::clone(&self.popup_taps),
      create_ledger: Arc::clone(&self.create_ledger),
      popups: self.popups.clone(),
      user_data_dir: self.user_data_dir.as_ref().map(Arc::clone),
      flushes_on_close: self.flushes_on_close,
      connections: self.connections.clone(),
      downloads_dir: Arc::clone(&self.downloads_dir),
      attach_tasks: Arc::clone(&self.attach_tasks),
      webdriver: self.webdriver.clone(),
      android: self.android.clone(),
    }
  }
}

/// Build the `Emulation.setDeviceMetricsOverride` params object for
/// `config`. Shared between the per-page `enable_domains` parallel
/// batch (which seeds `last_metrics_params` with the value it just
/// shipped) and `emulate_viewport` (which compares against that
/// seed to skip redundant RTTs). Mirrors Playwright's
/// `_metricsOverride` shape in `crPage.ts:920`.
pub(super) fn metrics_params_for(
  config: &crate::options::ViewportConfig,
  screen: Option<crate::options::ScreenSize>,
) -> serde_json::Value {
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
  // `window.screen` reports the context's `screen` when it has one and
  // the viewport otherwise — Playwright ships both in ONE
  // `setDeviceMetricsOverride` (`chromium/crPage.ts::_updateViewport`),
  // which is why the screen cannot be a command of its own: a second
  // override would replace the width, height, scale factor and mobile
  // flag this one just set.
  let screen = screen.unwrap_or(crate::options::ScreenSize {
    width: config.width,
    height: config.height,
  });
  serde_json::json!({
    "width": config.width,
    "height": config.height,
    "deviceScaleFactor": config.device_scale_factor,
    "mobile": config.is_mobile,
    "screenWidth": screen.width,
    "screenHeight": screen.height,
    "screenOrientation": orientation,
  })
}

/// Translate a Playwright glob into a CDP `Fetch.RequestPattern.urlPattern`
/// that is guaranteed to be a strict superset of the glob (so Chrome never
/// skips a request that `UrlMatcher::Glob` would accept, while avoiding pausing
/// non-matching assets). Returns `None` when the pattern is effectively `"*"`
/// or cannot be safely narrowed.
pub(super) fn glob_to_cdp_url_pattern(pattern: &str) -> Option<String> {
  if pattern.is_empty() {
    return None;
  }
  let mut out = String::with_capacity(pattern.len() + 2);
  let bytes = pattern.as_bytes();
  let mut i = 0;
  let push_star = |s: &mut String| {
    if !s.ends_with('*') {
      s.push('*');
    }
  };
  while i < bytes.len() {
    match bytes[i] {
      b'\\' if i + 1 < bytes.len() => {
        let next = bytes[i + 1];
        if matches!(next, b'*' | b'?' | b'\\') {
          out.push('\\');
        }
        out.push(next as char);
        i += 2;
      },
      b'*' => {
        if i + 2 < bytes.len() && bytes[i + 1] == b'*' && bytes[i + 2] == b'/' {
          // `**/` matches zero or more path segments in Playwright; `*` is a
          // strict superset in CDP.
          push_star(&mut out);
          i += 3;
        } else {
          push_star(&mut out);
          i += 1;
        }
      },
      b'{' | b'[' => {
        let close = if bytes[i] == b'{' { b'}' } else { b']' };
        let mut depth = 1usize;
        i += 1;
        while i < bytes.len() && depth > 0 {
          if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
          }
          if bytes[i] == b'{' && close == b'}' {
            depth += 1;
          } else if bytes[i] == close {
            depth -= 1;
          }
          i += 1;
        }
        if depth != 0 {
          return None;
        }
        push_star(&mut out);
      },
      _ => {
        let ch = pattern.get(i..)?.chars().next()?;
        out.push(ch);
        i += ch.len_utf8();
      },
    }
  }
  if !out.starts_with('*') && !out.contains("://") {
    out.insert(0, '*');
  }
  if out == "*" {
    return None;
  }
  Some(out)
}

pub(super) const EMPTY_PARAMS_RAW: &str = "{}";
pub(super) const ENABLED_TRUE_RAW: &str = r#"{"enabled":true}"#;
pub(super) const AUTOATTACH_PARAMS_RAW: &str = r#"{"autoAttach":true,"waitForDebuggerOnStart":true,"flatten":true}"#;

/// Default `Emulation.setEmulatedMedia` params matching [`crate::backend::MediaEmulation::effective`]
/// when neither context nor page specifies custom media overrides. Shipped in the
/// initial `enable_domains` / recycle batch so `apply_context_options` skips the
/// sequential `Emulation.setEmulatedMedia` RTT on every default page.
pub(super) fn default_emulated_media_params() -> serde_json::Value {
  serde_json::json!({
    "features": [
      { "name": "prefers-color-scheme", "value": crate::backend::DEFAULT_COLOR_SCHEME },
      { "name": "prefers-reduced-motion", "value": "no-preference" },
      { "name": "forced-colors", "value": "none" },
      { "name": "prefers-contrast", "value": "no-preference" },
    ],
    "media": "",
  })
}

/// Pre-serialized wire JSON for [`default_emulated_media_params`], computed once
/// per process for zero-allocation dispatch in page bootstrap and recycling.
pub(super) fn default_emulated_media_raw() -> &'static str {
  static RAW: std::sync::OnceLock<String> = std::sync::OnceLock::new();
  RAW.get_or_init(|| serde_json::to_string(&default_emulated_media_params()).unwrap_or_else(|_| "{}".to_string()))
}

/// How much bigger than its viewport a browser window has to be for the
/// page to actually get that viewport.
///
/// Window chrome (title bar, tab strip, borders) eats into the content
/// area, so a window sized to the viewport renders a smaller one.
/// Numbers are Playwright's, per platform
/// (`chromium/crPage.ts::_updateViewport`); a browser with no UI window
/// has no chrome to account for.
///
/// Playwright adds another 46px of height for a persistent context, to
/// leave room for the automation infobar. Deliberately not mirrored:
/// ferridriver's switch list asks Chrome to suppress that bar
/// (`--disable-infobars`), and the page gets its exact size from the
/// metrics override either way — the insets only decide how much of the
/// window the page fills.
pub(super) const fn window_insets(headful: bool) -> (i64, i64) {
  if !headful {
    return (0, 0);
  }
  if cfg!(target_os = "windows") {
    (16, 88)
  } else if cfg!(target_os = "linux") {
    (8, 85)
  } else if cfg!(target_os = "macos") {
    (2, 80)
  } else {
    (24, 88)
  }
}

/// How long a tab brought to the front has to answer a capture before
/// it is brought to the front again, and how many times that is tried.
pub(super) const FRONT_TAB_DRAWN: Duration = Duration::from_millis(750);
pub(super) const FRONT_TAB_ATTEMPTS: usize = 12;

/// Shared targetId -> sessionId cache. See [`CdpBrowser::attached_targets`].
pub(super) type AttachedTargets = Arc<std::sync::Mutex<FxHashMap<String, Option<String>>>>;

/// The handles the attach listener needs to turn an announced target
/// into a claimed popup.
pub(super) struct AttachCtx {
  transport: Arc<CdpTransport>,
  taps: crate::backend::PopupTaps,
  downloads_dir: Arc<DownloadsDir>,
  ledger: Arc<CreateLedger>,
  attached: AttachedTargets,
  spares: Arc<recycle::Spares>,
  headful: Arc<std::sync::atomic::AtomicBool>,
  in_flight: crate::backend::PopupsInFlight,
}

/// A paused page target the attach listener could not attribute yet:
/// no `openerId`, and at least one `Target.createTarget` in flight that
/// may own it.
#[derive(Clone)]
pub(super) struct ParkedTarget {
  target: String,
  session: String,
  browser_context: Option<String>,
  opener: Option<String>,
  /// Counts the target as a popup in flight until a create of ours takes
  /// it or the pump is done with it.
  claim: Arc<crate::backend::PopupClaim>,
}

pub(super) type CreateLedger = crate::backend::CreateLedger<ParkedTarget>;

/// The browser contexts some client created, by id. The browser's own
/// default context is never among them.
pub(super) async fn created_browser_contexts(transport: &CdpTransport) -> Result<rustc_hash::FxHashSet<String>> {
  let contexts = transport
    .send_command(None, "Target.getBrowserContexts", &super::super::EMPTY_PARAMS)
    .await?;
  Ok(
    contexts
      .get("browserContextIds")
      .and_then(serde_json::Value::as_array)
      .map(|ids| {
        ids
          .iter()
          .filter_map(serde_json::Value::as_str)
          .map(str::to_string)
          .collect()
      })
      .unwrap_or_default(),
  )
}

/// Whether a `Target.getTargets` entry is a page of the default context:
/// a page target in none of the `created` contexts.
pub(super) fn is_default_context_page(target: &serde_json::Value, created: &rustc_hash::FxHashSet<String>) -> bool {
  target.get("type").and_then(serde_json::Value::as_str) == Some("page")
    && !target
      .get("browserContextId")
      .and_then(serde_json::Value::as_str)
      .is_some_and(|id| created.contains(id))
}

impl CdpBrowser {
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
    transport: &CdpTransport,
    session_id: Option<&str>,
    viewport: Option<&crate::options::ViewportConfig>,
    unpause: bool,
    init_script: Option<&str>,
  ) -> Result<Option<Vec<super::super::FrameInfo>>> {
    // Page bootstrap has no context options yet; a `screen` in the bag
    // is applied by `apply_context_options`, which re-sends this
    // override with it.
    let vp_params = viewport.map(|vp| metrics_params_for(vp, None));

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
          .send_command_raw(session_id, "Runtime.runIfWaitingForDebugger", EMPTY_PARAMS_RAW)
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
        let [main, utility] = engine_registrations_raw(src, true);
        tokio::try_join!(
          transport.send_command_raw(session_id, "Page.addScriptToEvaluateOnNewDocument", &main),
          transport.send_command_raw(session_id, "Page.addScriptToEvaluateOnNewDocument", &utility),
        )
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
    // The frame cache starts from the target id, which is the main
    // frame's id, and frame events fill it in — no RTT for the
    // navigate-then-query flow that all bench / typical tests follow.
    // `DOM.enable` is NOT required, and Playwright does not send it —
    // the only one in its tree is `crCoverage.ts`, when coverage starts.
    // `DOM.getDocument` enables the agent implicitly, which is what
    // `find_element` (`page.$`) reaches it through; the element ACTION
    // path never needs it at all, because `scroll_into_view`,
    // `screenshot` and `set_input_files` address nodes by Runtime
    // `objectId` and bypass the DOM agent entirely. Nothing subscribes
    // to a `DOM.*` event either, so the agent has no standing work to do.
    //
    // It is kept only because dropping it buys nothing: measured at
    // 0.35ms of a ~42ms page, inside the run-to-run spread. The
    // bootstrap batch is dominated by a fixed ~67ms wait for the new
    // target's renderer, after which all eleven responses land inside
    // 15ms — so command count is not what this costs.
    //
    // Focus emulation keeps the page believing it is focused for its
    // whole life (Playwright's crPage._initialize sends the same).
    // Without it a failed main-frame navigation (aborted route, DNS
    // error) leaves the headless target unfocused, and Chromium then
    // suppresses `Page.screencastFrame` delivery entirely — a
    // screencast started afterwards records zero frames.
    //
    // `Inspector.enable` is what makes Chromium report a renderer crash
    // (`Inspector.targetCrashed`, `page.on('crash')`).
    // `DOM.enable` is omitted — `DOM.getDocument` enables it implicitly
    // when needed, and Playwright never sends it during page bootstrap.
    let default_media = default_emulated_media_raw();
    let (r1, r2, r3, r4, r5, r6, r7, r8, r9, r10, r11) = tokio::join!(
      transport.send_command_raw(session_id, "Page.enable", EMPTY_PARAMS_RAW),
      transport.send_command_raw(session_id, "Runtime.enable", EMPTY_PARAMS_RAW),
      transport.send_command_raw(session_id, "Network.enable", EMPTY_PARAMS_RAW),
      transport.send_command_raw(session_id, "Page.setLifecycleEventsEnabled", ENABLED_TRUE_RAW),
      transport.send_command_raw(session_id, "Target.setAutoAttach", AUTOATTACH_PARAMS_RAW),
      transport.send_command_raw(session_id, "Emulation.setFocusEmulationEnabled", ENABLED_TRUE_RAW),
      transport.send_command_raw(session_id, "Inspector.enable", EMPTY_PARAMS_RAW),
      transport.send_command_raw(session_id, "Emulation.setEmulatedMedia", default_media),
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
    r10?;
    r11?;
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
    transport: Arc<CdpTransport>,
    child: Option<super::super::process::ChildGroup>,
    user_data_dir: Option<tempfile::TempDir>,
  ) -> Result<Self> {
    let downloads_dir = new_downloads_dir()?;
    let mut child = child;
    if let Some(ref mut group) = child {
      group.own_dir(downloads_dir.path());
    }
    let attached_targets: AttachedTargets = Arc::new(std::sync::Mutex::new(FxHashMap::default()));
    let spares = Arc::new(recycle::Spares::default());
    let headful_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (popup_taps, create_ledger, attach_tasks, popups) = Self::spawn_attach_listener(
      &transport,
      &downloads_dir,
      &attached_targets,
      &spares,
      Arc::clone(&headful_flag),
    );

    let auto_attach_params = serde_json::json!({
      "autoAttach": true,
      "waitForDebuggerOnStart": true,
      "flatten": true,
    });
    // Pipeline Browser.getVersion and Target.setAutoAttach in one RTT:
    // attach_listener already owns its tap so no startup attach event can be missed.
    let (version_resp, _) = tokio::try_join!(
      transport.send_command(None, "Browser.getVersion", &super::super::EMPTY_PARAMS),
      transport.send_command(None, "Target.setAutoAttach", &auto_attach_params),
    )?;
    // `product` is a string like "HeadlessChrome/120.0.6099.109" — mirrors
    // what Playwright surfaces via `browser.version()`.
    let version: Arc<str> = version_resp
      .get("product")
      .and_then(|v| v.as_str())
      .map_or_else(|| Arc::from("Unknown"), Arc::from);
    // Chrome reports "HeadlessChrome/<v>" only when it has no UI window.
    let headful = !version.starts_with("HeadlessChrome");
    headful_flag.store(headful, std::sync::atomic::Ordering::Release);
    let isolated_contexts = !version_resp
      .get("userAgent")
      .and_then(serde_json::Value::as_str)
      .is_some_and(|agent| agent.contains("Android"));

    Ok(Self {
      transport,
      child: Arc::new(tokio::sync::Mutex::new(child)),
      attached_targets,
      version,
      isolated_contexts,
      headful,
      front_tab: Arc::default(),
      spares,
      flushes_on_close: user_data_dir.is_none(),
      connections: None,
      user_data_dir: user_data_dir.map(|td| Arc::new(super::super::async_tempdir::AsyncTempDir::new(td))),
      downloads_dir,
      popup_taps,
      create_ledger,
      popups,
      attach_tasks: Arc::new(attach_tasks),
      webdriver: None,
      android: None,
    })
  }

  /// Resume every auto-attached target the page-open flow does not
  /// claim. `setAutoAttach` pauses ALL new targets
  /// (`waitForDebuggerOnStart`); `create_page` resumes only the page
  /// target it created, so service workers, shared workers, and other
  /// worker targets would sit suspended forever — observed as
  /// `navigator.serviceWorker.register()` never resolving. Mirrors
  /// Playwright's `crBrowser.ts` attachedToTarget handling, which
  /// resumes every session it does not turn into a page. Lossless tap:
  /// a dropped attach event would wedge that worker for good.
  ///
  /// Page targets NOT owned by an in-flight `Target.createTarget` are
  /// BROWSER-created popups. Targets with an `openerId` are claimed
  /// outright (`createTarget` pages never carry one); no-opener page
  /// targets (`noopener` windows) go through the [`CreateLedger`]: they
  /// are claimed immediately when no create is in flight, otherwise
  /// parked until every pending create has taken its own target and the
  /// leftovers are flushed as popups. Claimed popups get domains
  /// enabled while still paused, then ride the returned popup taps so
  /// the state-level pump can register them and resume (Playwright's
  /// `crBrowser._onAttachedToTarget` → `CRPage` path).
  fn spawn_attach_listener(
    transport: &Arc<CdpTransport>,
    downloads_dir: &Arc<DownloadsDir>,
    attached: &AttachedTargets,
    spares: &Arc<recycle::Spares>,
    headful: Arc<std::sync::atomic::AtomicBool>,
  ) -> (
    crate::backend::PopupTaps,
    Arc<CreateLedger>,
    AttachTasks,
    crate::backend::PopupWatch,
  ) {
    let popup_taps: crate::backend::PopupTaps = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (popups, mut passes) = crate::backend::PopupWatch::with_barrier();
    let ledger = Arc::new(CreateLedger::default());
    let mut attach_rx = transport.tap_event_methods(
      &[
        "Target.attachedToTarget",
        "Target.detachedFromTarget",
        "Target.targetDestroyed",
      ],
      None,
    );
    let resume_transport = Arc::clone(transport);
    let claim_ctx = AttachCtx {
      transport: Arc::clone(transport),
      taps: Arc::clone(&popup_taps),
      downloads_dir: Arc::clone(downloads_dir),
      ledger: Arc::clone(&ledger),
      attached: Arc::clone(attached),
      spares: Arc::clone(spares),
      headful: Arc::clone(&headful),
      in_flight: popups.in_flight.clone(),
    };
    // Flush channel: `end_create` (called from new_page, no transport
    // access) signals the listener side to claim leftovers.
    let mut flush_rx = ledger.take_flush_rx();
    let flush_downloads = Arc::clone(downloads_dir);
    let flush_transport = Arc::clone(transport);
    let flush_taps = Arc::clone(&popup_taps);
    let flush_ledger = Arc::clone(&ledger);
    let flush_headful = headful;
    let flush_task = tokio::spawn(async move {
      while flush_rx.recv().await.is_some() {
        let is_headful = flush_headful.load(std::sync::atomic::Ordering::Acquire);
        for parked in flush_ledger.take_unowned_parked() {
          Self::claim_parked_popup(&flush_transport, &flush_taps, &flush_downloads, parked, is_headful);
        }
      }
    });
    let attach_task = tokio::spawn(async move {
      // Popup targets already claimed — Chromium can announce the
      // same related target through more than one auto-attach scope.
      let mut claimed_popups: rustc_hash::FxHashSet<String> = rustc_hash::FxHashSet::default();
      loop {
        // Events first: a barrier pass is answered only once every event
        // queued ahead of it has been handled, which is what lets a caller
        // know each popup announced so far is counted in flight.
        let event = tokio::select! {
          biased;
          event = attach_rx.recv() => match event {
            Some(event) => event,
            None => break,
          },
          Some(pass) = passes.recv() => {
            if pass.send(()).is_err() {
              tracing::debug!("popup barrier caller went away");
            }
            continue;
          },
        };
        let Some(params) = event.get("params") else { continue };
        Self::handle_attach_event(&claim_ctx, &resume_transport, &mut claimed_popups, &event, params);
      }
    });
    (
      popup_taps,
      ledger,
      AttachTasks(vec![flush_task.abort_handle(), attach_task.abort_handle()]),
      popups,
    )
  }

  fn handle_attach_event(
    claim_ctx: &AttachCtx,
    resume_transport: &Arc<CdpTransport>,
    claimed_popups: &mut rustc_hash::FxHashSet<String>,
    event: &serde_json::Value,
    params: &serde_json::Value,
  ) {
    // A target that went away must leave the session cache, or the
    // map grows by one dead entry for every page ever opened.
    if event.get("method").and_then(|m| m.as_str()) != Some("Target.attachedToTarget") {
      if let Some(target_id) = params.get("targetId").and_then(|v| v.as_str()) {
        claim_ctx
          .attached
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .remove(target_id);
      }
      return;
    }
    let target_type = params
      .pointer("/targetInfo/type")
      .and_then(|v| v.as_str())
      .unwrap_or("");
    let waiting = params
      .get("waitingForDebugger")
      .and_then(serde_json::Value::as_bool)
      .unwrap_or(false);
    let Some(session_id) = params.get("sessionId").and_then(|v| v.as_str()) else {
      return;
    };
    if target_type == "page" {
      if let (Some(ctx_id), Some(tid)) = (
        params.pointer("/targetInfo/browserContextId").and_then(|v| v.as_str()),
        params.pointer("/targetInfo/targetId").and_then(|v| v.as_str()),
      ) {
        claim_ctx.spares.note_attached_target(ctx_id, tid);
      }
      Self::maybe_claim_popup(claim_ctx, claimed_popups, params, waiting, session_id);
      return;
    }
    if target_type == "iframe" || !waiting {
      return;
    }
    // Fire-and-forget (Playwright's `_sendMayFail`): a worker can
    // attach through two sessions (browser-level AND page-level
    // auto-attach both see it), and a session Chrome never
    // answers must not stall the other session's resume behind
    // the 30s command timeout.
    let session_id = session_id.to_string();
    let transport = Arc::clone(resume_transport);
    tokio::spawn(async move {
      let _ = transport
        .send_command(
          Some(&session_id),
          "Runtime.runIfWaitingForDebugger",
          &super::super::EMPTY_PARAMS,
        )
        .await;
    });
  }

  /// One auto-attached page target. Ownership resolution:
  ///
  /// * `openerId` present → browser-created popup, claim now.
  /// * no opener, no `Target.createTarget` in flight → `noopener`
  ///   popup, claim now.
  /// * no opener, create(s) in flight → parked; the flush after the
  ///   last create completes claims whatever no create took.
  ///
  /// Claims run off the listener loop — `enable_domains` is a
  /// multi-command round-trip and the loop must keep consuming attach
  /// events.
  fn maybe_claim_popup(
    ctx: &AttachCtx,
    claimed: &mut rustc_hash::FxHashSet<String>,
    params: &serde_json::Value,
    waiting: bool,
    session_id: &str,
  ) {
    let opener_id = params
      .pointer("/targetInfo/openerId")
      .and_then(|v| v.as_str())
      .map(std::string::ToString::to_string);
    let target_id = params
      .pointer("/targetInfo/targetId")
      .and_then(|v| v.as_str())
      .unwrap_or("")
      .to_string();
    if !waiting || target_id.is_empty() || claimed.contains(&target_id) {
      return;
    }
    let browser_context_id = params
      .pointer("/targetInfo/browserContextId")
      .and_then(|v| v.as_str())
      .map(std::string::ToString::to_string);
    let parked = ParkedTarget {
      target: target_id.clone(),
      session: session_id.to_string(),
      browser_context: browser_context_id,
      opener: opener_id,
      claim: ctx.in_flight.claim(),
    };
    if parked.opener.is_none() && !ctx.ledger.try_claim_ownerless(&parked.target, parked.clone()) {
      // Parked — a create in flight may own it; the flush decides.
      return;
    }
    claimed.insert(target_id);
    Self::claim_parked_popup(
      &ctx.transport,
      &ctx.taps,
      &ctx.downloads_dir,
      parked,
      ctx.headful.load(std::sync::atomic::Ordering::Acquire),
    );
  }

  /// Claim one resolved popup target: build the paused [`CdpPage`] and
  /// announce it, or resume the target if nothing is listening / the
  /// claim failed (never leave it parked on `waitForDebuggerOnStart`).
  fn claim_parked_popup(
    transport: &Arc<CdpTransport>,
    taps: &crate::backend::PopupTaps,
    downloads_dir: &Arc<DownloadsDir>,
    parked: ParkedTarget,
    headful: bool,
  ) {
    let transport = Arc::clone(transport);
    let taps = Arc::clone(taps);
    let downloads_dir = Arc::clone(downloads_dir);
    tokio::spawn(async move {
      let ParkedTarget {
        target: target_id,
        session: session_id,
        browser_context: browser_context_id,
        opener: opener_id,
        claim,
      } = parked;
      let claimed_page = Self::claim_popup(
        &transport,
        &downloads_dir,
        &session_id,
        &target_id,
        browser_context_id.as_deref(),
        headful,
      )
      .await;
      let deliver_failed = match claimed_page {
        Ok(page) => !crate::backend::push_popup(
          &taps,
          crate::backend::PopupInfo {
            page: AnyPage::Cdp(page),
            browser_context_id,
            opener_target_id: opener_id,
            claim,
          },
        ),
        Err(e) => {
          tracing::debug!("popup claim failed for {target_id}: {e}");
          true
        },
      };
      if deliver_failed {
        // No pump listening (core used without the state layer) or a
        // failed claim — resume so the popup at least runs instead of
        // staying parked on waitForDebuggerOnStart.
        let _ = transport
          .send_command(
            Some(&session_id),
            "Runtime.runIfWaitingForDebugger",
            &super::super::EMPTY_PARAMS,
          )
          .await;
      }
    });
  }

  /// Build a fully initialized [`CdpPage`] for a popup target that
  /// auto-attached paused. Mirrors the `new_page` setup (domain
  /// enables, engine inject, lifecycle tracker, main-frame seed) minus
  /// `Target.createTarget` and minus the resume — the popup pump
  /// resumes after context configuration is applied.
  async fn claim_popup(
    transport: &Arc<CdpTransport>,
    downloads_dir: &Arc<DownloadsDir>,
    session_id: &str,
    target_id: &str,
    browser_context_id: Option<&str>,
    headful: bool,
  ) -> Result<CdpPage> {
    let inject_src = crate::selectors::lazy_inject_js();
    let injected_script = Arc::new(InjectedScriptManager::new());
    injected_script
      .injected
      .store(true, std::sync::atomic::Ordering::Relaxed);
    let page = CdpPage(Arc::new(CdpPageInner {
      shared: super::super::SharedPageState::new(target_id),
      transport: Arc::clone(transport),
      session_id: Some(Arc::from(session_id)),
      setup: Arc::new(paused::PausedSetup::paused()),
      target_id: Arc::from(target_id),
      browser_context_id: browser_context_id.map(Arc::from),
      frame_contexts: Arc::new(std::sync::RwLock::new(FxHashMap::default())),
      utility_contexts: Arc::default(),
      frame_contexts_notify: Arc::new(tokio::sync::Notify::new()),
      renderers: Arc::default(),
      exposed_fns: Arc::new(tokio::sync::RwLock::new(FxHashMap::default())),
      binding_initialized: Arc::new(tokio::sync::Mutex::new(false)),
      routes: Arc::new(tokio::sync::RwLock::new(Vec::new())),
      fetch: Arc::default(),
      http_credentials: Arc::new(tokio::sync::RwLock::new(None)),
      proxy_credentials: Arc::new(tokio::sync::RwLock::new(None)),
      main_frame_id: Arc::new(tokio::sync::OnceCell::new()),
      last_metrics_params: Arc::new(std::sync::Mutex::new(None)),
      last_media_params: Arc::new(std::sync::Mutex::new(Some(default_emulated_media_params()))),
      headful,
      window_id: Arc::new(tokio::sync::OnceCell::new()),
      context_screen: Arc::new(std::sync::Mutex::new(None)),
      seeded_frame_tree: Arc::new(std::sync::Mutex::new(None)),
      last_cursor_pos: Arc::new(std::sync::Mutex::new(None)),
      lifecycle: Arc::new(std::sync::Mutex::new(LifecycleState::new())),
      lifecycle_notify: Arc::new(tokio::sync::Notify::new()),
      injected_script,
      nav_request_slot: crate::network::NavRequestSlot::new(),
      file_chooser_intercept_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      screencast_hub: crate::backend::screencast::ScreencastHub::default(),
      screen_recorder: recording::ScreenRecorder::default(),
      download_behavior_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      accept_downloads: Arc::new(std::sync::atomic::AtomicBool::new(true)),
      downloads_dir: Arc::clone(downloads_dir),
      network_idle: Arc::default(),
      listener_tasks: Arc::new(std::sync::Mutex::new(Vec::new())),
      drag_manager: DragManagerState::new(),
      touch_emulation_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }));
    page.transport.register_lifecycle_tracker(
      page.session_id.as_deref().unwrap_or(""),
      Arc::clone(&page.lifecycle),
      Arc::clone(&page.lifecycle_notify),
      page.frame_observer(),
    );
    let _ = page.main_frame_id.set(page.target_id.to_string());
    let enable = {
      let transport = Arc::clone(transport);
      let session = session_id.to_string();
      let seed = Arc::clone(&page.seeded_frame_tree);
      async move {
        let frames = Box::pin(Self::enable_domains(
          &transport,
          Some(&session),
          None,
          false,
          Some(&inject_src),
        ))
        .await?;
        if frames.is_some() {
          *seed.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = frames;
        }
        Ok(())
      }
    };
    page.setup.write(Box::pin(enable)).await?;
    let setup = Arc::clone(&page.setup);
    page
      .write_paused("Page.getFrameTree", super::super::empty_params(), move |tree| {
        let url = tree.pointer("/frameTree/frame/url").and_then(|url| url.as_str());
        setup.main_frame_url_answered(url.unwrap_or_default());
        Ok(())
      })
      .await?;
    Ok(page)
  }

  pub(crate) fn popup_taps(&self) -> crate::backend::PopupTaps {
    Arc::clone(&self.popup_taps)
  }

  /// Whether `browser_context_id` lives on a connection of its own.
  pub(crate) fn routes_context(&self, browser_context_id: &str) -> bool {
    self
      .connections
      .as_ref()
      .is_some_and(|connections| connections.route(browser_context_id).is_some())
  }

  /// Resolves once every popup the browser announced before this call has
  /// been registered. Events and replies share one ordered connection, so
  /// the reply to a browser-level command comes after the attach event of
  /// any tab the browser created before answering it.
  pub(crate) async fn popups_settled(&self) -> Result<()> {
    self
      .transport
      .send_command(None, "Browser.getVersion", &super::super::EMPTY_PARAMS)
      .await?;
    self.popups.settled().await;
    if let Some(connections) = &self.connections {
      connections.popups_settled().await?;
    }
    Ok(())
  }

  /// The open pages of the browser's default context, attaching to any
  /// not yet tracked. A page of a context some other client created is
  /// that client's, and is neither listed nor attached to.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Target.getTargets` or `Target.attachToTarget`, the target has
  /// closed, or the command outlives its time budget. Also fails when enabling a newly attached page's
  /// domains fails, or a lock is poisoned.
  pub async fn pages(&self) -> Result<Vec<AnyPage>> {
    let created = created_browser_contexts(&self.transport).await?;
    let result = self
      .transport
      .send_command(None, "Target.getTargets", &super::super::EMPTY_PARAMS)
      .await?;

    let targets = result
      .get("targetInfos")
      .and_then(|t| t.as_array())
      .cloned()
      .unwrap_or_default();

    let mut pages = renderer::AdoptedPages::new();
    for target in targets {
      if !is_default_context_page(&target, &created) {
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

        Box::pin(Self::enable_domains(&self.transport, sid.as_deref(), None, false, None)).await?;

        sid
      };

      let page = self.adopted_page(target_id, sid);
      pages.track(page.clone());
      page.initialize_adopted_frame().await?;
    }
    Ok(pages.finish())
  }

  pub(super) fn adopted_page(&self, target_id: String, sid: Option<String>) -> CdpPage {
    let lc_state = Arc::new(std::sync::Mutex::new(LifecycleState::new()));
    let lc_notify = Arc::new(tokio::sync::Notify::new());
    let shared = super::super::SharedPageState::new(&target_id);
    CdpPage(Arc::new(CdpPageInner {
      shared,
      transport: Arc::clone(&self.transport),
      session_id: sid.map(Arc::from),
      setup: Arc::default(),
      target_id: Arc::from(target_id),
      browser_context_id: None,
      frame_contexts: Arc::new(std::sync::RwLock::new(FxHashMap::default())),
      utility_contexts: Arc::default(),
      frame_contexts_notify: Arc::new(tokio::sync::Notify::new()),
      renderers: Arc::default(),
      exposed_fns: Arc::new(tokio::sync::RwLock::new(FxHashMap::default())),
      binding_initialized: Arc::new(tokio::sync::Mutex::new(false)),
      routes: Arc::new(tokio::sync::RwLock::new(Vec::new())),
      fetch: Arc::default(),
      http_credentials: Arc::new(tokio::sync::RwLock::new(None)),
      proxy_credentials: Arc::new(tokio::sync::RwLock::new(None)),
      main_frame_id: Arc::new(tokio::sync::OnceCell::new()),
      last_metrics_params: Arc::new(std::sync::Mutex::new(None)),
      last_media_params: Arc::new(std::sync::Mutex::new(None)),
      headful: self.headful,
      window_id: Arc::new(tokio::sync::OnceCell::new()),
      context_screen: Arc::new(std::sync::Mutex::new(None)),
      seeded_frame_tree: Arc::new(std::sync::Mutex::new(None)),
      last_cursor_pos: Arc::new(std::sync::Mutex::new(None)),
      lifecycle: lc_state,
      lifecycle_notify: lc_notify,
      injected_script: Arc::new(InjectedScriptManager::new()),
      nav_request_slot: crate::network::NavRequestSlot::new(),
      file_chooser_intercept_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      screencast_hub: crate::backend::screencast::ScreencastHub::default(),
      screen_recorder: recording::ScreenRecorder::default(),
      download_behavior_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      accept_downloads: Arc::new(std::sync::atomic::AtomicBool::new(true)),
      downloads_dir: Arc::clone(&self.downloads_dir),
      network_idle: Arc::default(),
      listener_tasks: Arc::new(std::sync::Mutex::new(Vec::new())),
      drag_manager: DragManagerState::new(),
      touch_emulation_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }))
  }

  /// Create a new browser context (isolated cookies, storage, cache).
  /// Matches Playwright's `browser.newContext()` → `Target.createBrowserContext`.
  /// Per-context `proxy` wires through `proxyServer` + `proxyBypassList`
  /// mirroring `crBrowser.ts::doCreateNewContext` at
  /// `/tmp/playwright/packages/playwright-core/src/server/chromium/crBrowser.ts:121`.
  ///
  /// # Errors
  ///
  /// Returns [`FerriError::Unsupported`] on a browser without isolated contexts (Android Chrome), and an
  /// error if `Target.createBrowserContext` fails or its answer carries no `browserContextId`.
  pub async fn new_context(&self, proxy: Option<&crate::options::ProxyConfig>) -> Result<String> {
    if !self.isolated_contexts {
      return Err(FerriError::unsupported(
        "Android Chrome does not support isolated browser contexts",
      ));
    }
    if let Some(connections) = &self.connections {
      return connections.new_context(proxy, &self.popup_taps).await;
    }
    // A proxy is fixed when the context is created, so only a context
    // without one can stand in for another.
    if proxy.is_none()
      && let Some(spare) = self.spares.take_context_or_wait().await
    {
      return Ok(spare);
    }
    let mut params = serde_json::json!({"disposeOnDetach": true});
    if let Some(p) = proxy {
      params["proxyServer"] = serde_json::json!(p.chromium_server());
      if let Some(bypass) = p.chromium_bypass_list() {
        params["proxyBypassList"] = serde_json::json!(bypass);
      }
    }
    let ctx = self
      .transport
      .send_command(None, "Target.createBrowserContext", &params)
      .await?;

    let id = ctx
      .get("browserContextId")
      .and_then(|v| v.as_str())
      .map(String::from)
      .ok_or_else(|| FerriError::backend("No browserContextId"))?;
    if proxy.is_some() {
      self.spares.mark_proxied(&id);
    }
    Ok(id)
  }

  /// Dispose a browser context. Matches Playwright's `context.close()`.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Target.disposeBrowserContext`, the target has closed, or the command
  /// outlives its time budget.
  pub async fn dispose_context(&self, browser_context_id: &str) -> Result<()> {
    if let Some(connections) = &self.connections
      && connections.dispose(browser_context_id).await?
    {
      return Ok(());
    }
    self.forget_spare(browser_context_id);
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
  /// `Target.createTarget` wrapped in the [`CreateLedger`] bracket:
  /// while the create is in flight the attach listener parks no-opener
  /// page targets instead of claiming them as `noopener` popups — the
  /// attach event can beat this response.
  async fn create_owned_target(&self, browser_context_id: Option<&str>) -> Result<String> {
    let create_params = if let Some(ctx_id) = browser_context_id {
      serde_json::json!({"url": "about:blank", "browserContextId": ctx_id})
    } else {
      serde_json::json!({"url": "about:blank"})
    };
    self.create_ledger.begin_create();
    let result = self
      .transport
      .send_command(None, "Target.createTarget", &create_params)
      .await;
    let target_id = result
      .and_then(|r| {
        r.get("targetId")
          .and_then(|v| v.as_str())
          .map(std::string::ToString::to_string)
          .ok_or_else(|| FerriError::protocol("Target.createTarget", "response missing targetId"))
      })
      .inspect_err(|_| self.create_ledger.end_create(None))?;
    self.create_ledger.end_create(Some(&target_id));
    Ok(target_id)
  }

  /// Create a page target and wait for its auto-attached session. The
  /// target comes back paused (`waitForDebuggerOnStart`).
  async fn create_attached_target(&self, browser_context_id: Option<&str>) -> Result<(String, Option<String>)> {
    // Tap BEFORE createTarget so we can't miss the auto-attach; a
    // broadcast drop here was a 30s new_page hang.
    let mut event_rx = self.transport.tap_event_methods(&["Target.attachedToTarget"], None);

    let target_id = self.create_owned_target(browser_context_id).await?;

    // Wait for Target.attachedToTarget event (from setAutoAttach in init).
    // The target is paused (waitForDebuggerOnStart) so we can set up everything.
    let tid = target_id.clone();
    let sid = tokio::time::timeout(Duration::from_secs(30), async move {
      while let Some(event) = event_rx.recv().await {
        if event.get("method").and_then(|m| m.as_str()) == Some("Target.attachedToTarget")
          && let Some(params) = event.get("params")
        {
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
      Err(FerriError::target_closed(Some(
        "CDP event channel closed while waiting for Target.attachedToTarget".into(),
      )))
    })
    .await
    .map_err(|_| FerriError::timeout(format!("auto-attach of target {target_id}"), 30_000))??;
    Ok((target_id, sid))
  }

  /// Either adopt a recycled kept page (adjusting viewport only when it
  /// differs from what recycle pre-applied) or create and bootstrap a fresh
  /// target.
  async fn acquire_page_target(
    &self,
    browser_context_id: Option<&str>,
    viewport: Option<&crate::options::ViewportConfig>,
    desired_metrics: Option<&serde_json::Value>,
  ) -> Result<(
    String,
    Option<String>,
    Option<Vec<super::super::FrameInfo>>,
    serde_json::Value,
  )> {
    use futures::FutureExt;
    let kept = browser_context_id.and_then(|context| Some((context, self.spares.take_page(context)?)));
    let inject_src = crate::selectors::lazy_inject_js();
    if let Some((context, page)) = kept {
      let adopted = self.adopt_kept_page(context, page, &inject_src).boxed().await?;
      let sid = Some(adopted.session.clone());
      if desired_metrics != adopted.applied_metrics.as_ref() {
        match desired_metrics {
          Some(metrics) => {
            self
              .transport
              .send_command(sid.as_deref(), "Emulation.setDeviceMetricsOverride", metrics)
              .await?;
          },
          None => {
            self
              .transport
              .send_command(
                sid.as_deref(),
                "Emulation.clearDeviceMetricsOverride",
                &super::super::EMPTY_PARAMS,
              )
              .await?;
          },
        }
      }
      self.spares.record_active_page(
        context,
        &adopted.target,
        &adopted.session,
        adopted.applied_metrics,
        adopted.applied_media.clone(),
        adopted.engine_on_session,
        adopted.engine_source,
      );
      self
        .attached_targets
        .lock()
        .map_err(|e| FerriError::Backend(format!("Lock poisoned: {e}")))?
        .insert(adopted.target.clone(), sid.clone());
      Ok((adopted.target, sid, None, adopted.applied_media))
    } else {
      let (target_id, sid) = self.create_attached_target(browser_context_id).await?;
      self
        .attached_targets
        .lock()
        .map_err(|e| FerriError::Backend(format!("Lock poisoned: {e}")))?
        .insert(target_id.clone(), sid.clone());
      let frame_tree_seed = Self::enable_domains(
        &self.transport,
        sid.as_deref(),
        viewport,
        true,
        Some(inject_src.as_ref()),
      )
      .boxed()
      .await?;
      let default_media = default_emulated_media_params();
      if let Some(session) = sid.as_deref() {
        // Clear bootstrap commands from dirty tracking so a clean test on this
        // fresh page can use the keep-session fast path on retirement.
        self.transport.reset_session_dirty(session);
        if let Some(ctx) = browser_context_id {
          self.spares.record_active_page(
            ctx,
            &target_id,
            session,
            desired_metrics.cloned(),
            default_media.clone(),
            true,
            inject_src.into_owned(),
          );
        }
      }
      Ok((target_id, sid, frame_tree_seed, default_media))
    }
  }

  /// # Errors
  ///
  /// Returns an error if creating or adopting the page's target fails, enabling its protocol domains fails, a
  /// lock is poisoned, or the initial navigation to `url` fails.
  pub async fn new_page(
    &self,
    url: &str,
    browser_context_id: Option<&str>,
    viewport: Option<&crate::options::ViewportConfig>,
  ) -> Result<AnyPage> {
    if let Some(page) = browser_context_id
      .zip(self.connections.as_ref())
      .and_then(|(context, connections)| connections.new_page(context, url, viewport))
    {
      return page.await;
    }
    let desired_metrics = viewport.map(|vp| metrics_params_for(vp, None));
    self.spares.record_default_metrics(desired_metrics.as_ref());

    let (target_id, sid, frame_tree_seed, applied_media) = self
      .acquire_page_target(browser_context_id, viewport, desired_metrics.as_ref())
      .await?;

    let lc_state = Arc::new(std::sync::Mutex::new(LifecycleState::new()));
    let lc_notify = Arc::new(tokio::sync::Notify::new());
    let injected_script = Arc::new(InjectedScriptManager::new());
    // The init script rode the parallel batch above — flag the
    // manager as already-injected so subsequent
    // `ensure_engine_injected` calls become no-ops.
    injected_script
      .injected
      .store(true, std::sync::atomic::Ordering::Relaxed);
    let shared = super::super::SharedPageState::new(&target_id);
    let page = CdpPage(Arc::new(CdpPageInner {
      shared,
      transport: Arc::clone(&self.transport),
      session_id: sid.map(Arc::from),
      setup: Arc::default(),
      target_id: Arc::from(target_id),
      browser_context_id: browser_context_id.map(Arc::from),
      frame_contexts: Arc::new(std::sync::RwLock::new(FxHashMap::default())),
      utility_contexts: Arc::default(),
      frame_contexts_notify: Arc::new(tokio::sync::Notify::new()),
      renderers: Arc::default(),
      exposed_fns: Arc::new(tokio::sync::RwLock::new(FxHashMap::default())),
      binding_initialized: Arc::new(tokio::sync::Mutex::new(false)),
      routes: Arc::new(tokio::sync::RwLock::new(Vec::new())),
      fetch: Arc::default(),
      http_credentials: Arc::new(tokio::sync::RwLock::new(None)),
      proxy_credentials: Arc::new(tokio::sync::RwLock::new(None)),
      main_frame_id: Arc::new(tokio::sync::OnceCell::new()),
      // Seed the metrics and media caches with the exact params just shipped,
      // so `apply_context_options` skips redundant RTTs when defaults match.
      last_metrics_params: Arc::new(std::sync::Mutex::new(desired_metrics)),
      last_media_params: Arc::new(std::sync::Mutex::new(Some(applied_media))),
      headful: self.headful,
      window_id: Arc::new(tokio::sync::OnceCell::new()),
      context_screen: Arc::new(std::sync::Mutex::new(None)),
      seeded_frame_tree: Arc::new(std::sync::Mutex::new(frame_tree_seed)),
      last_cursor_pos: Arc::new(std::sync::Mutex::new(None)),
      lifecycle: Arc::clone(&lc_state),
      lifecycle_notify: Arc::clone(&lc_notify),
      injected_script,
      nav_request_slot: crate::network::NavRequestSlot::new(),
      file_chooser_intercept_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      screencast_hub: crate::backend::screencast::ScreencastHub::default(),
      screen_recorder: recording::ScreenRecorder::default(),
      download_behavior_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
      accept_downloads: Arc::new(std::sync::atomic::AtomicBool::new(true)),
      downloads_dir: Arc::clone(&self.downloads_dir),
      network_idle: Arc::default(),
      listener_tasks: Arc::new(std::sync::Mutex::new(Vec::new())),
      drag_manager: DragManagerState::new(),
      touch_emulation_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }));

    // Register lifecycle tracker in the transport reader (synchronous update, zero overhead)
    page.transport.register_lifecycle_tracker(
      page.session_id.as_deref().unwrap_or(""),
      Arc::clone(&page.lifecycle),
      Arc::clone(&page.lifecycle_notify),
      page.frame_observer(),
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

    Ok(AnyPage::Cdp(page))
  }

  /// Close the browser process and release resources.
  ///
  /// Caller-owned profiles must flush before exit. Throwaway profiles
  /// can be killed directly because their contents are being discarded.
  ///
  /// # Errors
  ///
  /// Returns an error if the profile did not flush before the browser exited, closing the attached
  /// `WebDriver` or Android session fails, or closing the transport fails.
  pub async fn close(&self) -> Result<()> {
    if let Some(connections) = &self.connections {
      let _ = connections.close().await;
    }
    self.attach_tasks.abort();
    let mut child = self.child.lock().await;
    if let Some(group) = child.as_mut() {
      let mut flushed = Ok(());
      if self.flushes_on_close {
        let timeout = crate::backend::process::PROFILE_FLUSH_TIMEOUT;
        let _ = tokio::time::timeout(
          timeout,
          self
            .transport
            .send_command(None, "Browser.close", &super::super::EMPTY_PARAMS),
        )
        .await;
        flushed = group.wait_for_exit(timeout).await;
      }
      // Group kill first (helpers die with the parent), then reap so
      // the enclosing runtime doesn't carry a zombie.
      group.shutdown_for_close().await;
      child.take();
      if let Err(reason) = flushed {
        return Err(FerriError::Backend(format!(
          "Chromium did not close cleanly while flushing its persistent profile: {reason}"
        )));
      }
    }
    drop(child);
    // Reclaim the profile directory here rather than leaving it to the
    // last `Arc` drop: at process teardown the runtime may never run
    // the deferred removal, which leaked one multi-megabyte Chromium
    // profile per session. A file something still holds after the retries
    // (Windows) is logged and left to the drop, as Playwright's
    // `processLauncher` logs and carries on, rather than failing the close.
    if let Some(dir) = self.user_data_dir.as_ref() {
      let _ = dir.remove_now().await;
    }
    if let Some(session) = &self.webdriver {
      session.close().await?;
    }
    if let Some(android) = &self.android {
      android.close().await?;
    }
    self.transport.close().await
  }

  /// Whether the launched browser process is still running. `None` when
  /// this handle did not launch the process (`connect`) or the child
  /// lock is momentarily held, in which case the caller falls back to
  /// the transport's own liveness signal.
  pub(crate) fn child_is_running(&self) -> Option<bool> {
    let mut guard = self.child.try_lock().ok()?;
    guard.as_mut().map(super::super::process::ChildGroup::is_running)
  }
}

// ── Pipe-specific launch ───────────────���─────────────────────────────────────

impl CdpBrowser {
  /// Launch Chrome with `--remote-debugging-pipe` and communicate over fd 3/4.
  ///
  /// # Errors
  ///
  /// Returns an error if the throwaway profile directory cannot be created, Chrome fails to launch, or the
  /// first protocol handshake fails.
  pub async fn launch_pipe(chromium_path: &str) -> Result<Self> {
    Self::launch_pipe_with_flags(
      chromium_path,
      &crate::state::chrome_flags(true, &[]),
      &rustc_hash::FxHashMap::default(),
    )
    .await
  }

  /// Launch Chrome with custom flags and communicate over fd 3/4.
  ///
  /// # Errors
  ///
  /// Returns an error if the throwaway profile directory cannot be created, Chrome fails to launch, or the
  /// first protocol handshake fails.
  pub async fn launch_pipe_with_flags(
    chromium_path: &str,
    flags: &[String],
    env: &rustc_hash::FxHashMap<String, String>,
  ) -> Result<Self> {
    // Hold the user-data-dir as a `TempDir` so it's removed from disk when the
    // browser handle drops. The Child owned by the CDP browser has
    // `kill_on_drop(true)` set in `pipe::PipeTransport::spawn`, so the Chrome
    // process dies first, releasing any locks on the dir.
    let user_data_dir = tempfile::Builder::new()
      .prefix("ferridriver-pipe-")
      .tempdir()
      .map_err(|e| FerriError::Backend(format!("create user-data-dir: {e}")))?;

    let (transport, child) = pipe::PipeTransport::spawn(chromium_path, user_data_dir.path(), flags, true, env)?;
    let group = super::super::process::ChildGroup::recorded(child, Some(user_data_dir.path()), true);
    Self::init(Arc::new(transport), Some(group), Some(user_data_dir)).await
  }

  /// Launch Chrome with a caller-supplied `--user-data-dir`. The
  /// directory is NOT owned by the browser handle — the caller is
  /// responsible for its lifetime. Used by
  /// [`crate::BrowserType::launch_persistent_context`] to ensure
  /// cookies / `localStorage` / `IndexedDB` persist across re-launches
  /// against the same directory.
  ///
  /// # Errors
  ///
  /// Returns an error if `user_data_dir` cannot be prepared, Chrome fails to launch, or the first protocol
  /// handshake fails.
  pub async fn launch_pipe_with_flags_in_dir(
    chromium_path: &str,
    flags: &[String],
    user_data_dir: &std::path::Path,
    env: &rustc_hash::FxHashMap<String, String>,
  ) -> Result<Self> {
    crate::backend::ensure_user_data_dir(Some(&user_data_dir.to_string_lossy()))?;
    let (transport, child) = pipe::PipeTransport::spawn(chromium_path, user_data_dir, flags, false, env)?;
    let group = super::super::process::ChildGroup::recorded(child, Some(user_data_dir), false);
    Self::init(Arc::new(transport), Some(group), None).await
  }
}

// ── WS-specific launch + connect ─────────────────────────────────────────────

impl CdpBrowser {
  /// The browser's own `DevTools` WebSocket endpoint, the one a second CDP
  /// client can dial. `None` on a pipe-launched browser.
  #[must_use]
  pub fn ws_endpoint(&self) -> Option<&str> {
    self.transport.endpoint()
  }

  /// Launch Chrome with `--remote-debugging-port` and communicate over WebSocket.
  ///
  /// # Errors
  ///
  /// Returns an error if the throwaway profile directory cannot be created, Chrome fails to launch or expose
  /// its WebSocket, or the first protocol handshake fails.
  pub async fn launch_ws(chromium_path: &str) -> Result<Self> {
    Box::pin(Self::launch_ws_with_flags(
      chromium_path,
      &crate::state::chrome_flags(true, &[]),
      &rustc_hash::FxHashMap::default(),
    ))
    .await
  }

  /// Launch Chrome with custom flags and communicate over WebSocket.
  ///
  /// # Errors
  ///
  /// Returns an error if the throwaway profile directory cannot be created, Chrome fails to launch or expose
  /// its WebSocket, or the first protocol handshake fails.
  pub async fn launch_ws_with_flags(
    chromium_path: &str,
    flags: &[String],
    env: &rustc_hash::FxHashMap<String, String>,
  ) -> Result<Self> {
    // Held as a `TempDir` so the dir is removed from disk when the browser
    // handle drops. `WsTransport::spawn` hands Chrome over as a process
    // group that is killed first, so Chrome dies before the directory vanishes.
    let user_data_dir = tempfile::Builder::new()
      .prefix("ferridriver-raw-")
      .tempdir()
      .map_err(|e| FerriError::Backend(format!("create user-data-dir: {e}")))?;

    let (transport, group) = Box::pin(ws::WsTransport::spawn(
      chromium_path,
      user_data_dir.path(),
      flags,
      true,
      env,
    ))
    .await?;
    Self::init(Arc::new(transport), Some(group), Some(user_data_dir)).await
  }

  /// Launch Chrome over WebSocket with a caller-supplied
  /// `--user-data-dir`. See `launch_with_flags_in_dir` on the pipe
  /// flavour for the persistence rationale.
  ///
  /// # Errors
  ///
  /// Returns an error if `user_data_dir` cannot be prepared, Chrome fails to launch or expose its WebSocket,
  /// or the first protocol handshake fails.
  pub async fn launch_ws_with_flags_in_dir(
    chromium_path: &str,
    flags: &[String],
    user_data_dir: &std::path::Path,
    env: &rustc_hash::FxHashMap<String, String>,
  ) -> Result<Self> {
    crate::backend::ensure_user_data_dir(Some(&user_data_dir.to_string_lossy()))?;
    let (transport, group) = Box::pin(ws::WsTransport::spawn(chromium_path, user_data_dir, flags, false, env)).await?;
    Self::init(Arc::new(transport), Some(group), None).await
  }

  /// Launch Lightpanda's CDP server and drive it over WebSocket.
  ///
  /// # Errors
  ///
  /// Returns an error if Lightpanda fails to start or listen, or the first protocol handshake fails.
  pub(crate) async fn launch_lightpanda(
    binary: &std::path::Path,
    options: lightpanda::ServeOptions<'_>,
  ) -> Result<Self> {
    let (ws_url, group) = lightpanda::serve(binary, options).await?;
    let transport = Box::pin(ws::WsTransport::connect_with_headers(&ws_url, &FxHashMap::default())).await?;
    let mut browser = Self::init(Arc::new(transport), Some(group), None).await?;
    // `lightpanda serve` outlives a closed connection, and writes its cookie
    // jar as each context closes, not on exit.
    browser.flushes_on_close = false;
    // Lightpanda holds one browser context per connection.
    browser.connections = Some(Arc::new(connections::ContextConnections::new(ws_url)));
    Ok(browser)
  }

  /// Attach to the first existing page target of a browser we just
  /// connected to, recording its session in `attached`. Returns whether
  /// one was found.
  async fn attach_first_existing_page(
    transport: &Arc<CdpTransport>,
    targets: &serde_json::Value,
    attached: &mut FxHashMap<String, Option<String>>,
  ) -> Result<bool> {
    let Some(targets) = targets.get("targetInfos").and_then(|t| t.as_array()) else {
      return Ok(false);
    };
    let created = created_browser_contexts(transport).await?;
    for target in targets {
      if !is_default_context_page(target, &created) {
        continue;
      }
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
      Box::pin(Self::enable_domains(transport, sid.as_deref(), None, false, None)).await?;
      attached.insert(target_id, sid);
      return Ok(true);
    }
    Ok(false)
  }

  /// Connect to a running Chrome instance via WebSocket URL.
  ///
  /// # Errors
  ///
  /// Returns an error if the WebSocket connection fails or the first protocol handshake fails.
  pub async fn connect(ws_url: &str) -> Result<Self> {
    Self::connect_with_headers(ws_url, &FxHashMap::default()).await
  }

  pub(crate) async fn connect_owned_webdriver(
    ws_url: &str,
    headers: &FxHashMap<String, String>,
    session: Arc<super::super::webdriver::session::WebDriverSession>,
  ) -> Result<Self> {
    let mut browser = Self::connect_with_headers(ws_url, headers).await?;
    browser.webdriver = Some(session);
    Ok(browser)
  }

  /// Give a connected browser that has no page a blank one in its default
  /// context, set up and resumed on the session auto-attach gave it.
  async fn open_blank_page(
    transport: &Arc<CdpTransport>,
    create_ledger: &Arc<CreateLedger>,
    attached: &mut FxHashMap<String, Option<String>>,
  ) -> Result<()> {
    // Same ledger bracket as `new_page` — auto-attach is armed, so
    // this created target must not be mistaken for a noopener popup.
    // Tapped before the create: the attach event can beat its response.
    let mut attach_events = transport.tap_event_methods(&["Target.attachedToTarget"], None);
    create_ledger.begin_create();
    let create_result = transport
      .send_command(None, "Target.createTarget", &serde_json::json!({"url": "about:blank"}))
      .await;
    let target_id = match &create_result {
      Ok(r) => r.get("targetId").and_then(|v| v.as_str()).unwrap_or("").to_string(),
      Err(_) => String::new(),
    };
    create_ledger.end_create(if target_id.is_empty() { None } else { Some(&target_id) });
    create_result?;
    // Auto-attach parked the new target on waitForDebuggerOnStart, on a
    // session of its own. That is the session to set up and resume: a
    // second session attached by hand can run commands, but resuming
    // there leaves the page frozen, never painting and never answering
    // a navigation.
    let tid = target_id.clone();
    let sid = tokio::time::timeout(Duration::from_secs(30), async move {
      while let Some(event) = attach_events.recv().await {
        if let Some(params) = event.get("params")
          && params
            .get("targetInfo")
            .and_then(|info| info.get("targetId"))
            .and_then(|v| v.as_str())
            == Some(tid.as_str())
        {
          return Ok(params.get("sessionId").and_then(|v| v.as_str()).map(String::from));
        }
      }
      Err(FerriError::target_closed(Some(
        "CDP event channel closed while waiting for Target.attachedToTarget".into(),
      )))
    })
    .await
    .map_err(|_| FerriError::timeout(format!("auto-attach of target {target_id}"), 30_000))??;
    Box::pin(Self::enable_domains(transport, sid.as_deref(), None, false, None)).await?;
    let _ = transport
      .send_command(
        sid.as_deref(),
        "Runtime.runIfWaitingForDebugger",
        &super::super::EMPTY_PARAMS,
      )
      .await;
    attached.insert(target_id, sid);
    Ok(())
  }

  pub(crate) async fn connect_with_headers(ws_url: &str, headers: &FxHashMap<String, String>) -> Result<Self> {
    Self::connect_opening(ws_url, headers, true).await
  }

  /// Connect, and with `open_default_page` give a browser with no page a
  /// blank one in its default context. A connection that only exists to
  /// hold a context of its own opens none: on a server with one context
  /// per connection, that page would take the context's place.
  pub(crate) async fn connect_opening(
    ws_url: &str,
    headers: &FxHashMap<String, String>,
    open_default_page: bool,
  ) -> Result<Self> {
    let transport = Arc::new(Box::pin(ws::WsTransport::connect_with_headers(ws_url, headers)).await?);

    // Capture product version for `browser.version()` — same handshake
    // Playwright's CRBrowser.connect does.
    let version_resp = transport
      .send_command(None, "Browser.getVersion", &super::super::EMPTY_PARAMS)
      .await?;
    let version: Arc<str> = version_resp
      .get("product")
      .and_then(|v| v.as_str())
      .map_or_else(|| Arc::from("Unknown"), Arc::from);
    let headful = !version.starts_with("HeadlessChrome");
    let isolated_contexts = !version_resp
      .get("userAgent")
      .and_then(serde_json::Value::as_str)
      .is_some_and(|agent| agent.contains("Android"));

    transport
      .send_command(
        None,
        "Target.setDiscoverTargets",
        &serde_json::json!({"discover": true}),
      )
      .await?;

    let downloads_dir = new_downloads_dir()?;
    let attached_targets: AttachedTargets = Arc::new(std::sync::Mutex::new(FxHashMap::default()));
    let spares = Arc::new(recycle::Spares::default());
    let headful_flag = Arc::new(std::sync::atomic::AtomicBool::new(headful));
    let (popup_taps, create_ledger, attach_tasks, popups) =
      Self::spawn_attach_listener(&transport, &downloads_dir, &attached_targets, &spares, headful_flag);

    // CDP can emit attachedToTarget before setAutoAttach replies. The
    // listener must already own its tap or paused startup targets are lost.
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

    // Find existing page targets
    let result = transport
      .send_command(None, "Target.getTargets", &super::super::EMPTY_PARAMS)
      .await?;

    let mut attached = FxHashMap::default();
    let found_page = Self::attach_first_existing_page(&transport, &result, &mut attached).await?;

    // If no existing page, create one
    if !found_page && open_default_page {
      Self::open_blank_page(&transport, &create_ledger, &mut attached).await?;
    }

    // Hand the targets discovered above to the shared cache the attach
    // listener prunes.
    *attached_targets
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = attached;

    Ok(Self {
      transport,
      downloads_dir,
      child: Arc::new(tokio::sync::Mutex::new(None)),
      attached_targets,
      version,
      isolated_contexts,
      headful,
      front_tab: Arc::default(),
      spares,
      attach_tasks: Arc::new(attach_tasks),
      popup_taps,
      create_ledger,
      popups,
      user_data_dir: None,
      flushes_on_close: false,
      connections: None,
      webdriver: None,
      android: None,
    })
  }
}
