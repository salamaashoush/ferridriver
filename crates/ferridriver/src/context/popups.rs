//! Popup target pump and video recording lifecycle for opened pages.

use std::sync::Arc;

use tokio::sync::RwLock;

use super::{ContextRef, apply_context_options_or_defaults, bind_source};
use crate::error::Result;
use crate::page::Page;
use crate::state::BrowserState;

/// Consume one instance's [`crate::backend::PopupInfo`] stream. Each
/// browser-created popup is registered into its owning context,
/// configured like a `new_page` page, wired to its opener, resumed
/// (CDP parks popups paused), and announced as `ContextEvent::Page` —
/// the state-layer half of Playwright's `crBrowser._onAttachedToTarget`
/// / `bidiBrowser._onBrowsingContextCreated` /
/// `wkBrowser._onPageProxyCreated` handling.
pub(crate) fn spawn_popup_pump(
  state: Arc<RwLock<BrowserState>>,
  instance: Arc<str>,
  browser: &crate::backend::AnyBrowser,
) {
  let mut rx = browser.tap_popups();
  tokio::spawn(async move {
    while let Some(popup) = rx.recv().await {
      let state = Arc::clone(&state);
      let instance = Arc::clone(&instance);
      // Per-popup task: registration is a multi-await sequence and the
      // pump must keep consuming (two rapid popups must not serialize
      // behind each other's context configuration).
      tokio::spawn(async move {
        let backend_page = popup.page.clone();
        if let Err(e) = Box::pin(register_popup(state, &instance, popup)).await {
          tracing::debug!("popup registration failed: {e}");
          // Never leave a CDP popup parked on waitForDebuggerOnStart.
          let _ = backend_page.resume_popup().await;
        }
      });
    }
  });
}

async fn register_popup(
  state: Arc<RwLock<BrowserState>>,
  instance: &Arc<str>,
  popup: crate::backend::PopupInfo,
) -> Result<()> {
  let ctx_name = {
    let s = state.read().await;
    s.context_name_for_backend_id(instance, popup.browser_context_id.as_deref())
  };
  let Some(ctx_name) = ctx_name else {
    // A context this browser does not manage (another client's, or one
    // already closed): let the popup run, and let go of it.
    let _ = popup.page.let_go_of_popup().await;
    return Ok(());
  };
  let composite = if &**instance == "default" && ctx_name == "default" {
    "default".to_string()
  } else {
    format!("{instance}:{ctx_name}")
  };
  let key = crate::state::SessionKey::parse(&composite);
  // Wired now, so nothing its first document logs is missed, and listed
  // only once that document has committed, below. Listed before, it is
  // the active tab while still on the empty document it starts with, and
  // whoever reads the context in that moment finds about:blank.
  state.write().await.wire_opened_page(&key, &popup.page, None)?;
  // The browser handle keeps `popup.context().browser()` non-null,
  // matching Playwright where a popup shares the opener's context.
  // `new_async`, not `new`: the sync constructor degrades to a
  // transient emitter when the state lock is contended, and a popup
  // announced on one is a popup the suite's `waitForEvent('page')`
  // never sees.
  let ctx_ref = ContextRef::new_async(Arc::clone(&state), composite.clone())
    .await
    .with_browser(crate::Browser::from_shared_state(Arc::clone(&state)));
  let page = Page::with_context(popup.page.clone(), ctx_ref.clone());

  // While a CDP/WebKit popup is still paused only protocol-level
  // configuration may run — anything that evaluates into the live
  // document (WebSocket-route install) would block until resume and
  // deadlock the pump. Options, init scripts, route interception, and
  // the paused-safe binding install are all pre-document protocol
  // calls, so the popup's first document sees them from the start.
  let (ctx_opts, proxy) = {
    let s = state.read().await;
    (s.get_context_options(&composite), s.effective_proxy(&composite))
  };
  if proxy.is_some()
    && let Err(e) = Box::pin(page.inner().set_proxy(proxy)).await
  {
    tracing::debug!("popup proxy: {e}");
  }
  if let Err(e) = apply_context_options_or_defaults(&page, ctx_opts.as_ref()).await {
    tracing::debug!("popup context options: {e}");
  }
  if let Err(e) = ctx_ref.apply_context_callbacks_pre_doc(page.inner()).await {
    tracing::debug!("popup init-script callbacks: {e}");
  }
  if let Err(e) = ctx_ref.apply_context_init_scripts(&page).await {
    tracing::debug!("popup init scripts: {e}");
  }
  if let Err(e) = ctx_ref.apply_context_routes(&page).await {
    tracing::debug!("popup routes: {e}");
  }
  if let Err(e) = ctx_ref.apply_context_bindings_pre_doc(page.inner()).await {
    tracing::debug!("popup bindings: {e}");
  }
  let opener = if let Some(opener_id) = popup.opener_target_id.as_deref() {
    let opener = {
      let s = state.read().await;
      s.find_page_by_backend_id(instance, opener_id)
    };
    if let Some(opener) = opener.as_ref() {
      page.set_opener(opener);
    }
    opener
  } else {
    None
  };
  popup.page.resume_popup().await?;

  // BiDi popups are never paused; their first navigation can commit before
  // the context pump installs the frame-cache listener. A Classic WebDriver
  // window is found some time after it opened and reports no navigation.
  // A popup that closed at once still counts: Playwright reports it.
  if matches!(
    &popup.page,
    crate::backend::AnyPage::Bidi(_) | crate::backend::AnyPage::WebDriver(_)
  ) && let Err(e) = page.sync_frames().await
  {
    tracing::debug!("popup frames: {e}");
  }
  // Playwright's `context.pages()` leaves out a page that closed before
  // its first document did, while still announcing it.
  if !popup.page.is_closed() {
    state
      .write()
      .await
      .list_opened_page(&key, popup.page.clone(), crate::state::Listing::Popup)?;
  }

  let record_opts = {
    let s = state.read().await;
    s.get_record_video(&composite)
  };
  if let Some(opts) = record_opts {
    start_video_recording(&page, &opts);
  }
  if let Some(recorder) = crate::trace::recorder_for(&composite) {
    crate::trace::record_page_open(&recorder, &crate::trace::trace_page_id(page.inner()));
    if recorder.screenshots {
      crate::trace::spawn_screencast_pump(&recorder, page.inner()).await;
    }
  }
  ctx_ref.emit_page_created(Arc::clone(&page));
  if let Some(opener) = opener {
    opener.events().emit(crate::events::PageEvent::Popup(Arc::clone(&page)));
  }
  // Listed and announced: whoever waits on the browser's popups in flight
  // may now read the context and find this one in it.
  drop(popup.claim);

  // WebSocket-route install evaluates into the live document, so it runs
  // post-resume (first-document sockets on paused backends stay a
  // documented residual), and off the pump: a document held by a dialog
  // it opened would otherwise stall every popup after it.
  tokio::spawn(async move {
    if let Err(e) = ctx_ref.apply_context_ws_routes(&page).await {
      tracing::debug!("popup ws routes: {e}");
    }
  });
  Ok(())
}

/// Context-level registry handles captured under the single write lock in
/// [`ContextRef::new_page`] so re-applying them onto a fresh page needs no
/// further `BrowserState` locks.
pub(super) struct PageRegistries {
  pub(super) bindings: Arc<
    tokio::sync::RwLock<rustc_hash::FxHashMap<String, rustc_hash::FxHashMap<String, crate::events::ExposedBinding>>>,
  >,
  pub(super) callbacks: Arc<
    tokio::sync::RwLock<rustc_hash::FxHashMap<String, rustc_hash::FxHashMap<String, crate::events::ExposedBinding>>>,
  >,
  pub(super) ws_routes: Arc<tokio::sync::RwLock<crate::state::ContextWsRoutes>>,
  pub(super) routes: Arc<tokio::sync::RwLock<crate::state::ContextRoutes>>,
  pub(super) init_scripts: Arc<tokio::sync::RwLock<crate::state::ContextInitScripts>>,
}

impl PageRegistries {
  pub(super) async fn apply(self, composite: &str, page: &Arc<Page>) -> Result<()> {
    let bindings = {
      let guard = self.bindings.read().await;
      guard
        .get(composite)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect::<Vec<_>>())
        .unwrap_or_default()
    };
    for (name, binding) in bindings {
      page
        .inner()
        .expose_context_binding(&name, bind_source(binding, composite.to_owned()))
        .await?;
    }
    let callbacks = {
      let guard = self.callbacks.read().await;
      guard
        .get(composite)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect::<Vec<_>>())
        .unwrap_or_default()
    };
    for (name, binding) in callbacks {
      page
        .inner()
        .expose_context_callback(&name, bind_source(binding, composite.to_owned()))
        .await?;
    }
    let ws_routes = {
      let guard = self.ws_routes.read().await;
      guard.get(composite).cloned().unwrap_or_default()
    };
    for (matcher, handler) in ws_routes {
      page
        .route_web_socket_scoped(matcher, handler, crate::web_socket_route::WsRouteScope::Context)
        .await?;
    }
    let routes = {
      let mut guard = self.routes.write().await;
      match guard.get_mut(composite) {
        Some(entries) => {
          entries.retain(crate::route::RegisteredRoute::live);
          entries.clone()
        },
        None => Vec::new(),
      }
    };
    for registration in routes {
      page.inner().route(registration).await?;
    }
    let scripts: Vec<_> = {
      let guard = self.init_scripts.read().await;
      guard
        .get(composite)
        .map(|entries| {
          entries
            .iter()
            .map(|e| (e.id, e.source.clone(), Arc::clone(&e.installed)))
            .collect()
        })
        .unwrap_or_default()
    };
    for (key, source, installed) in scripts {
      let id = page.inner().add_context_init_script(key, &source).await?;
      installed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push((page.inner().clone(), id));
    }
    Ok(())
  }
}

/// The viewport a backend should open a new page with: an explicit
/// `BrowserContextOptions.viewport` wins over the launch plan's default,
/// and `ViewportOption::Null` drops the default entirely (matches
/// Playwright's `viewport: null` opt-out).
pub(super) fn effective_viewport(
  ctx_opts: Option<&crate::options::BrowserContextOptions>,
  plan_viewport: Option<crate::options::ViewportConfig>,
) -> Option<crate::options::ViewportConfig> {
  let Some(opts) = ctx_opts else {
    return plan_viewport;
  };
  match opts.viewport {
    crate::options::ViewportOption::Null => None,
    crate::options::ViewportOption::Size { .. } => opts.resolved_viewport(),
    crate::options::ViewportOption::Default => plan_viewport,
  }
}

/// Kick off a background recording runtime for a freshly-opened page
/// whose context has `recordVideo` enabled. Constructs the Video
/// handle, attaches it to the page, and spawns a task that awaits the
/// page close then drives the encoder to completion.
///
/// The attach-first-then-spawn ordering matters: callers of
/// [`crate::Page::video`] can observe the handle the moment `new_page`
/// returns, even before the first frame has been captured. The handle
/// blocks on the underlying watch channel inside `path()` /
/// `save_as()` / `delete()`, so observing an in-progress recording
/// and calling `await video.path()` resolves cleanly once the page
/// closes.
///
/// Should a backend's `AnyPage::start_screencast` ever surface a typed
/// error, the video sink is populated with the error text so the
/// Playwright contract — `page.video()` returns a handle; the handle's
/// methods reject with a clear reason — is preserved. (Every current
/// backend, including Playwright `WebKit`, supports screencast.)
pub(super) fn start_video_recording(page: &Arc<Page>, opts: &crate::options::RecordVideoOptions) {
  // Compose the output filename: `<dir>/<timestamp>-<page-id>.<ext>`.
  // Playwright derives its name from the page's GUID; ferridriver uses
  // millisecond-since-epoch + an atomic counter — unique across pages
  // within the same context and the same second. The extension is the
  // ffmpeg encoder's; the browser's own recorder writes `.mp4` beside it,
  // and the path `stop` answers is the one the video handle reports.
  static VIDEO_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

  let (video, sink) = crate::video::Video::new();
  page.attach_video(Arc::new(video));

  let size = opts.size.unwrap_or_default();
  let width = size.width & !1;
  let height = size.height & !1;
  let millis = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |d| d.as_millis());
  let id = VIDEO_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
  let filename = format!("{millis}-{id}.{}", crate::video::video_extension());
  let output_path = opts.dir.join(filename);

  // Ensure the directory exists before the encoder opens the file.
  // Errors here funnel into the sink so the user-facing `path()`
  // rejects with a clear reason instead of the test hanging.
  if let Err(e) = std::fs::create_dir_all(&opts.dir) {
    sink.finish_err(crate::FerriError::backend(format!(
      "failed to create recordVideo.dir {}: {e}",
      opts.dir.display()
    )));
    return;
  }

  let page_for_task = Arc::clone(page);
  tokio::spawn(async move {
    // Default CDP screencast quality matches Playwright's
    // `DEFAULT_SCREENCAST_OPTIONS.quality` (90).
    let handle = match crate::video::start_recording(&page_for_task, output_path.clone(), width, height, 90).await {
      Ok(h) => h,
      Err(e) => {
        sink.finish_err(crate::FerriError::backend(format!("start_recording: {e}")));
        return;
      },
    };
    // Wait for the page to close before stopping the recording.
    // Polls at 50ms — matches the cadence of the frame-cache
    // listener task and keeps the observable delay inside the
    // "page.close() has returned" barrier below ~50ms in the
    // common case.
    while !page_for_task.is_closed() {
      tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    match handle.stop().await {
      Ok(path) => sink.finish_ok(path),
      Err(e) => sink.finish_err(crate::FerriError::backend(format!("stop recording: {e}"))),
    }
  });
}
