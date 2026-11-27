//! `CdpBrowser` target-attach listener and popup target claiming.

use crate::backend::{SharedPageState, empty_params};

use super::{
  AnyPage, Arc, AttachCtx, AttachTasks, AttachedTargets, CdpBrowser, CdpFlavor, CdpPage, CdpPageInner, CdpTransport,
  CreateLedger, DownloadsDir, DragManagerState, FxHashMap, InjectedScriptManager, LifecycleState, ParkedTarget, Result,
  default_emulated_media_params, paused, recording, recycle,
};

impl CdpBrowser {
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
  pub(super) fn spawn_attach_listener(
    transport: &Arc<CdpTransport>,
    downloads_dir: &Arc<DownloadsDir>,
    attached: &AttachedTargets,
    spares: &Arc<recycle::Spares>,
    headful: Arc<std::sync::atomic::AtomicBool>,
    flavor: CdpFlavor,
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
      flavor,
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
          Self::claim_parked_popup(
            &flush_transport,
            &flush_taps,
            &flush_downloads,
            parked,
            is_headful,
            flavor,
          );
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
        .send(
          Some(&session_id),
          &ferridriver_cdp_proto::runtime::RunIfWaitingForDebugger {},
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
      ctx.flavor,
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
    flavor: CdpFlavor,
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
        flavor,
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
          .send(
            Some(&session_id),
            &ferridriver_cdp_proto::runtime::RunIfWaitingForDebugger {},
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
    flavor: CdpFlavor,
  ) -> Result<CdpPage> {
    let inject_src = crate::selectors::lazy_inject_js();
    let injected_script = Arc::new(InjectedScriptManager::new());
    injected_script
      .injected
      .store(true, std::sync::atomic::Ordering::Relaxed);
    let page = CdpPage(Arc::new(CdpPageInner {
      shared: SharedPageState::new(target_id),
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
      flavor,
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
          flavor,
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
      .write_paused("Page.getFrameTree", empty_params(), move |tree| {
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
      .send(None, &ferridriver_cdp_proto::browser::GetVersion {})
      .await?;
    self.popups.settled().await;
    if let Some(connections) = &self.connections {
      connections.popups_settled().await?;
    }
    Ok(())
  }
}
