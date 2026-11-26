//! `CdpBrowser` pipe, WebSocket, Lightpanda launch, and attach/connect helpers.

use super::super::*;
use super::{created_browser_contexts, is_default_context_page};

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
    let group = crate::backend::process::ChildGroup::recorded(child, Some(user_data_dir.path()), true);
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
    let group = crate::backend::process::ChildGroup::recorded(child, Some(user_data_dir), false);
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
    targets: &[ferridriver_cdp_proto::target::TargetInfo],
    attached: &mut FxHashMap<String, Option<String>>,
  ) -> Result<bool> {
    let created = created_browser_contexts(transport).await?;
    for target in targets {
      if !is_default_context_page(target, &created) {
        continue;
      }
      let target_id = target.target_id.clone();
      let attach = transport
        .send(
          None,
          &ferridriver_cdp_proto::target::AttachToTarget {
            target_id: &target_id,
            flatten: Some(true),
          },
        )
        .await?;
      let sid = Some(attach.session_id);
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
    session: Arc<dyn crate::backend::common::OwnedResource>,
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
      .send(
        None,
        &ferridriver_cdp_proto::target::CreateTarget {
          url: "about:blank",
          browser_context_id: None,
          width: None,
          height: None,
        },
      )
      .await;
    let target_id = match &create_result {
      Ok(r) => r.target_id.clone(),
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
      .send(
        sid.as_deref(),
        &ferridriver_cdp_proto::runtime::RunIfWaitingForDebugger {},
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
      .send(None, &ferridriver_cdp_proto::browser::GetVersion {})
      .await?;
    let version: Arc<str> = if version_resp.product.is_empty() {
      Arc::from("Unknown")
    } else {
      Arc::from(version_resp.product.as_str())
    };
    let headful = !version.starts_with("HeadlessChrome");
    let isolated_contexts = !version_resp.user_agent.contains("Android");

    transport
      .send(
        None,
        &ferridriver_cdp_proto::target::SetDiscoverTargets { discover: true },
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
      .send(
        None,
        &ferridriver_cdp_proto::target::SetAutoAttach {
          auto_attach: true,
          wait_for_debugger_on_start: true,
          flatten: Some(true),
        },
      )
      .await?;

    // Find existing page targets
    let result = transport
      .send(None, &ferridriver_cdp_proto::target::GetTargets {})
      .await?;

    let mut attached = FxHashMap::default();
    let found_page = Self::attach_first_existing_page(&transport, &result.target_infos, &mut attached).await?;

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
