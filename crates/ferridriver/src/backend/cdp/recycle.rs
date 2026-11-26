//! Browser contexts handed from one user to the next.
//!
//! A context and its first page cost a renderer process, about 40ms of
//! wall clock and 95ms of CPU on Chromium, and a suite that gives every
//! test its own context pays that per test. A context retired through
//! [`CdpBrowser::retire_context`] is emptied instead of disposed and
//! kept, with one blank page, for the next `new_context`: about 3ms.
//!
//! Only the browser's side is kept. The layers above build a new
//! context and a new page over it, so nothing they hold (routes,
//! listeners, bindings, init scripts) has anything to carry over. What
//! the previous user did to the browser goes in two ways:
//!
//! * everything it set through the page's protocol session (emulation,
//!   init scripts, bindings, interception, headers) dies with that
//!   session, which is detached and replaced by a new one;
//! * the selector engine is the one thing every user registers alike, and
//!   at 185KB it is most of what a new session costs to set up. It is
//!   registered once on a second session that stays attached to the kept
//!   page for good, so each user's own session carries none of it;
//! * what outlives a session is cleared by name: the document, the tab's
//!   history and name, cookies, permissions, the HTTP cache, and the
//!   storage of every origin the context visited.
//!
//! What Chromium offers no way to clear is the HTTP authentication
//! cache: credentials a previous user answered a challenge with are
//! offered again to the same server. The caller therefore does not
//! retire a context that was given HTTP credentials
//! (`BrowserState::remove_context`), and one made with a proxy is
//! disposed here, a proxy being fixed for a context's life. What is
//! left is credentials a document supplied itself, in a URL.
//!
//! A context that cannot be emptied in time (a page that will not
//! navigate away, a crashed renderer) is disposed as before.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::FutureExt;
use rustc_hash::FxHashMap;
use serde_json::json;

use super::super::EMPTY_PARAMS;
use super::{AttachedTargets, CdpBrowser, CdpTransport};
use crate::error::{FerriError, Result};

/// How long emptying a context may take before it is disposed instead.
const RESET_BUDGET: Duration = Duration::from_secs(2);

/// Signals from the outgoing context about which browser-level overrides
/// its pages actually touched, so recycling skips clearing untouched state.
#[derive(Debug, Clone, Default)]
pub struct RetireHints {
  /// True when any page in the context enabled download behaviour or
  /// changed `acceptDownloads` from its default.
  pub download_touched: bool,
  /// Target id of the sole page in the context when `pages.len() == 1`,
  /// enabling the keep-session fast path when no session-mutating commands
  /// or extra targets occurred.
  pub single_page_target: Option<String>,
}

/// The page a recycled context keeps, and the pre-enabled session attached to it.
pub(super) struct KeptPage {
  pub(super) target: String,
  pub(super) session: String,
  /// `Emulation.setDeviceMetricsOverride` params already applied on `session`, if any.
  pub(super) applied_metrics: Option<serde_json::Value>,
  /// `Emulation.setEmulatedMedia` params already applied on `session`.
  pub(super) applied_media: serde_json::Value,
  /// True when `engine.min.js` is registered directly on `session` (from its
  /// initial `enable_domains` bootstrap) rather than on a separate `EngineSession`.
  pub(super) engine_on_session: bool,
  /// The exact `lazy_inject_js()` source currently registered on this page.
  pub(super) engine_source: String,
}

/// Active page currently checked out by a context, tracked so clean recycles
/// can keep the attached CDP session alive without detach/re-attach/domain setup.
struct ActivePage {
  target: String,
  session: String,
  applied_metrics: Option<serde_json::Value>,
  applied_media: serde_json::Value,
  engine_on_session: bool,
  engine_source: String,
  multi_target: bool,
}

/// The session that holds a kept page's engine registration across its
/// users.
struct EngineSession {
  context: String,
  session: String,
  /// The engine source registered, and the identifiers it was registered
  /// under: a selector engine registered since changes the source, and
  /// the registration is then replaced.
  source: String,
  scripts: Vec<String>,
}

#[derive(Default)]
pub(super) struct Spares {
  /// Emptied contexts nobody has asked for yet, by context id.
  idle: std::sync::Mutex<Vec<(String, KeptPage)>>,
  /// Contexts handed out by `new_context` whose kept page `new_page` has
  /// not taken yet.
  handed: std::sync::Mutex<FxHashMap<String, KeptPage>>,
  /// Active single-page state by `browser_context_id` for keep-session recycles.
  active_pages: std::sync::Mutex<FxHashMap<String, ActivePage>>,
  /// The session of every kept page in use, by target: the one to detach
  /// when its context comes back on the slow path.
  sessions: std::sync::Mutex<FxHashMap<String, String>>,
  /// The engine session of every kept page, by target.
  engines: tokio::sync::Mutex<FxHashMap<String, EngineSession>>,
  /// Contexts created with a proxy. A proxy is fixed for a context's
  /// life, so one of these can never stand in for another context.
  proxied: std::sync::Mutex<rustc_hash::FxHashSet<String>>,
  /// Most recent default viewport metrics params requested by `new_page`,
  /// pre-applied during recycle so adopting the kept page needs 0 RTTs.
  default_metrics: std::sync::Mutex<Option<serde_json::Value>>,
  /// Number of background retirement tasks currently emptying a context.
  in_flight: AtomicUsize,
  /// Wakes callers of [`Self::take_context_or_wait`] when a background
  /// retirement finishes (whether it succeeded or fell back to disposal).
  notify: tokio::sync::Notify,
}

fn lock<V>(mutex: &std::sync::Mutex<V>) -> std::sync::MutexGuard<'_, V> {
  mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct RetireGuard {
  spares: Arc<Spares>,
  finished: bool,
}

impl RetireGuard {
  fn finish(mut self, recycled: Option<(String, KeptPage)>) {
    self.finished = true;
    self.spares.finish_retire(recycled);
  }
}

impl Drop for RetireGuard {
  fn drop(&mut self) {
    if !self.finished {
      self.spares.finish_retire(None);
    }
  }
}

impl Spares {
  /// An emptied context for a `new_context` to return instead of creating
  /// one. Its page waits in `handed` for the context's first `new_page`.
  pub(super) fn take_context(&self) -> Option<String> {
    let (context, page) = lock(&self.idle).pop()?;
    lock(&self.handed).insert(context.clone(), page);
    Some(context)
  }

  /// Like [`Self::take_context`], but if no spare is ready yet while a
  /// background retirement is already in flight, waits for it to finish
  /// rather than spawning an unnecessary fresh renderer.
  pub(super) async fn take_context_or_wait(&self) -> Option<String> {
    loop {
      if let Some(context) = self.take_context() {
        return Some(context);
      }
      let notified = self.notify.notified();
      tokio::pin!(notified);
      notified.as_mut().enable();
      if let Some(context) = self.take_context() {
        return Some(context);
      }
      if self.in_flight.load(Ordering::Acquire) == 0 {
        return None;
      }
      notified.await;
    }
  }

  fn begin_retire(&self) {
    self.in_flight.fetch_add(1, Ordering::AcqRel);
  }

  fn finish_retire(&self, recycled: Option<(String, KeptPage)>) {
    if let Some(entry) = recycled {
      lock(&self.idle).push(entry);
    }
    self.in_flight.fetch_sub(1, Ordering::AcqRel);
    self.notify.notify_waiters();
  }

  /// Remember the viewport metrics used by `new_page` so recycled pages
  /// can pre-apply it during background teardown.
  pub(super) fn record_default_metrics(&self, metrics: Option<&serde_json::Value>) {
    if let Some(m) = metrics {
      *lock(&self.default_metrics) = Some(m.clone());
    }
  }

  fn default_metrics(&self) -> Option<serde_json::Value> {
    lock(&self.default_metrics).clone()
  }

  /// The page kept for `context`, for its first `new_page`.
  pub(super) fn take_page(&self, context: &str) -> Option<KeptPage> {
    let page = lock(&self.handed).remove(context)?;
    lock(&self.sessions).insert(page.target.clone(), page.session.clone());
    Some(page)
  }

  /// Record the active page currently owned by `context` so a clean recycle
  /// can keep its attached session alive. If `context` already owns a page,
  /// marks it multi-target so recycling falls back to full target cleanup.
  pub(super) fn record_active_page(
    &self,
    context: &str,
    target: &str,
    session: &str,
    applied_metrics: Option<serde_json::Value>,
    applied_media: serde_json::Value,
    engine_on_session: bool,
    engine_source: String,
  ) {
    let mut active = lock(&self.active_pages);
    if let Some(existing) = active.get_mut(context) {
      if existing.target != target {
        existing.multi_target = true;
      }
      return;
    }
    active.insert(
      context.to_string(),
      ActivePage {
        target: target.to_string(),
        session: session.to_string(),
        applied_metrics,
        applied_media,
        engine_on_session,
        engine_source,
        multi_target: false,
      },
    );
  }

  /// Note that a page target attached to `context` (e.g. a popup). If it is
  /// not the context's tracked single page, mark `multi_target` so recycling
  /// enumerates and closes extra targets.
  pub(super) fn note_attached_target(&self, context: &str, target: &str) {
    if let Some(active) = lock(&self.active_pages).get_mut(context)
      && active.target != target
    {
      active.multi_target = true;
    }
  }

  fn take_active_page(&self, context: &str) -> Option<ActivePage> {
    lock(&self.active_pages).remove(context)
  }

  fn record_session_for_detach(&self, target: &str, session: &str) {
    lock(&self.sessions).insert(target.to_string(), session.to_string());
  }

  /// Record that `context` was created with a proxy.
  pub(super) fn mark_proxied(&self, context: &str) {
    lock(&self.proxied).insert(context.to_string());
  }

  fn forget(&self, context: &str) {
    lock(&self.proxied).remove(context);
    let active = lock(&self.active_pages).remove(context);
    if let Some(active) = active {
      lock(&self.sessions).remove(&active.target);
    }
    // The sessions themselves go with the context's targets.
    if let Ok(mut engines) = self.engines.try_lock() {
      engines.retain(|_, engine| engine.context != context);
    }
    lock(&self.idle).retain(|(id, _)| id != context);
    let handed = lock(&self.handed).remove(context);
    if let Some(page) = handed {
      lock(&self.sessions).remove(&page.target);
    }
  }
}

impl CdpBrowser {
  /// Close a context its user is done with, keeping it for the next
  /// `new_context` when it can be emptied and disposing it otherwise.
  /// `origins` are the origins it visited.
  ///
  /// Non-proxied contexts recycle asynchronously in the background so
  /// `context.close()` returns immediately; a subsequent `new_context()`
  /// either pops an already-idle spare or waits on the in-flight recycle.
  ///
  /// # Errors
  ///
  /// Returns the disposal's error when a proxied or connection-routed
  /// context had to be disposed synchronously.
  pub async fn retire_context(&self, browser_context_id: &str, origins: &[String], hints: RetireHints) -> Result<()> {
    // A context on a connection of its own goes with that connection.
    if lock(&self.spares.proxied).contains(browser_context_id) || self.routes_context(browser_context_id) {
      return self.dispose_context(browser_context_id).await;
    }
    self.spares.begin_retire();
    let transport = Arc::clone(&self.transport);
    let spares = Arc::clone(&self.spares);
    let attached_targets = Arc::clone(&self.attached_targets);
    let context_id = browser_context_id.to_string();
    let origins = origins.to_vec();
    tokio::spawn(async move {
      let guard = RetireGuard {
        spares: Arc::clone(&spares),
        finished: false,
      };
      let emptied = tokio::time::timeout(
        RESET_BUDGET,
        empty_context_on(&transport, &spares, &attached_targets, &context_id, &origins, hints).boxed(),
      )
      .await;
      match emptied {
        Ok(Ok(Some(page))) => {
          guard.finish(Some((context_id, page)));
          return;
        },
        Ok(Ok(None)) => {},
        Ok(Err(error)) => {
          tracing::debug!(target: "ferridriver::browser", %error, "context not recycled: emptying it failed");
        },
        Err(_) => {
          tracing::debug!(target: "ferridriver::browser", "context not recycled: emptying it timed out");
        },
      }
      spares.forget(&context_id);
      let _ = transport
        .send_command(
          None,
          "Target.disposeBrowserContext",
          &json!({"browserContextId": context_id}),
        )
        .await;
      guard.finish(None);
    });
    Ok(())
  }

  pub(super) fn forget_spare(&self, browser_context_id: &str) {
    self.spares.forget(browser_context_id);
  }

  /// Hand a kept page to the `new_page` that takes it. The page already
  /// has the engine in its blank document; a selector engine registered
  /// since changes the source, and the page then opens a new blank
  /// document under the current one.
  pub(super) async fn adopt_kept_page(
    &self,
    browser_context_id: &str,
    mut page: KeptPage,
    engine: &str,
  ) -> Result<KeptPage> {
    if page.engine_on_session {
      if page.engine_source == engine {
        return Ok(page);
      }
      // `selectors.register` / `setTestIdAttribute` changed the engine source
      // while this page sat idle with the engine on its own session. Replace
      // the session and move the engine to the dedicated EngineSession.
      let (new_session, _) = replace_session_on(
        self.transport.as_ref(),
        &self.spares,
        &self.attached_targets,
        &page.target,
      )
      .boxed()
      .await?;
      engine_registered_on(
        self.transport.as_ref(),
        &self.spares,
        browser_context_id,
        &page.target,
        engine,
      )
      .boxed()
      .await?;
      go_blank_on(self.transport.as_ref(), &new_session, true).boxed().await?;
      let applied_metrics = self.spares.default_metrics();
      let applied_media = super::default_emulated_media_params();
      clear_and_prepare_on(
        self.transport.as_ref(),
        browser_context_id,
        &new_session,
        ClearPlan {
          origins: &[],
          download_touched: false,
          network_dirty: false,
          permissions_dirty: false,
        },
        applied_metrics.as_ref(),
      )
      .boxed()
      .await?;
      self.transport.reset_session_dirty(&new_session);
      self.spares.record_session_for_detach(&page.target, &new_session);
      page.session = new_session;
      page.applied_metrics = applied_metrics;
      page.applied_media = applied_media;
      page.engine_on_session = false;
      page.engine_source = engine.to_string();
      return Ok(page);
    }

    if engine_registered_on(
      self.transport.as_ref(),
      &self.spares,
      browser_context_id,
      &page.target,
      engine,
    )
    .boxed()
    .await?
    {
      go_blank_on(self.transport.as_ref(), &page.session, false)
        .boxed()
        .await?;
      self
        .transport
        .send_command_raw(
          Some(&page.session),
          "Page.resetNavigationHistory",
          super::EMPTY_PARAMS_RAW,
        )
        .await?;
      page.engine_source = engine.to_string();
    }
    Ok(page)
  }
}

struct KeepSessionInput<'a> {
  browser_context_id: &'a str,
  origins: &'a [String],
  hints: &'a RetireHints,
  active: ActivePage,
  permissions_dirty: bool,
  cookies_dirty: bool,
}

/// Attempt the keep-session fast path on an active single-page context.
/// Returns `Ok(Ok(kept))` when the session stays attached, or `Ok(Err(net_dirty))`
/// when the slow path is required.
async fn try_keep_session_on(
  transport: &Arc<CdpTransport>,
  spares: &Spares,
  input: KeepSessionInput<'_>,
) -> Result<std::result::Result<KeptPage, Option<bool>>> {
  let KeepSessionInput {
    browser_context_id,
    origins,
    hints,
    active,
    permissions_dirty,
    cookies_dirty,
  } = input;
  let dirty = transport.take_session_dirty(&active.session);
  let mut known_network_dirty = None;
  if !active.multi_target && hints.single_page_target.as_deref() == Some(active.target.as_str()) {
    let current_engine = crate::selectors::lazy_inject_js();
    let engine_matches = !active.engine_on_session || active.engine_source == current_engine.as_ref();
    let metrics_match = active.applied_metrics == spares.default_metrics();
    if !dirty.session_state && engine_matches && metrics_match {
      // Keep-session fast path: the session had no sticky overrides, no extra
      // targets, and matching engine + viewport. Just navigate to about:blank
      // and run the minimal pipelined clear batch.
      if !active.engine_on_session {
        engine_registered_on(
          transport.as_ref(),
          spares,
          browser_context_id,
          &active.target,
          &current_engine,
        )
        .boxed()
        .await?;
      }
      go_blank_on(transport.as_ref(), &active.session, false).boxed().await?;
      clear_kept_session_on(
        transport.as_ref(),
        browser_context_id,
        &active.session,
        ClearPlan {
          origins,
          download_touched: hints.download_touched,
          network_dirty: dirty.network || cookies_dirty,
          permissions_dirty,
        },
      )
      .boxed()
      .await?;
      if dirty.pointer {
        spawn_pointer_reset(transport, &active.session);
      }
      transport.reset_session_dirty(&active.session);
      return Ok(Ok(KeptPage {
        target: active.target,
        session: active.session,
        applied_metrics: active.applied_metrics,
        applied_media: active.applied_media,
        engine_on_session: active.engine_on_session,
        engine_source: active.engine_source,
      }));
    }
    known_network_dirty = Some(dirty.network);
  }
  // Slow path: hand the session back so `replace_session_on` detaches it.
  spares.record_session_for_detach(&active.target, &active.session);
  if dirty.pointer {
    transport.mark_pointer_dirty(&active.session);
  }
  Ok(Err(known_network_dirty))
}

/// Leave `browser_context_id` as a new context would be, with one blank
/// pre-enabled page. Clean single-page sessions stay attached (3 CDP commands);
/// dirty or multi-page contexts detach and rebuild a fresh session.
async fn empty_context_on(
  transport: &Arc<CdpTransport>,
  spares: &Spares,
  attached_targets: &AttachedTargets,
  browser_context_id: &str,
  origins: &[String],
  hints: RetireHints,
) -> Result<Option<KeptPage>> {
  let active = spares.take_active_page(browser_context_id);
  let permissions_dirty = transport.take_permissions_dirty(browser_context_id);
  let cookies_dirty = transport.take_cookies_dirty(browser_context_id);
  let mut known_network_dirty: Option<bool> = None;

  if let Some(active) = active {
    match try_keep_session_on(
      transport,
      spares,
      KeepSessionInput {
        browser_context_id,
        origins,
        hints: &hints,
        active,
        permissions_dirty,
        cookies_dirty,
      },
    )
    .boxed()
    .await?
    {
      Ok(kept) => return Ok(Some(kept)),
      Err(net_dirty) => known_network_dirty = net_dirty,
    }
  }

  let Some(target) = keep_one_page_on(transport.as_ref(), spares, browser_context_id)
    .boxed()
    .await?
  else {
    return Ok(None);
  };
  let (session, pointer_dirty) = replace_session_on(transport.as_ref(), spares, attached_targets, &target)
    .boxed()
    .await?;
  let engine_src = crate::selectors::lazy_inject_js();
  // Before the page goes blank, so the blank document takes the engine
  // as a new-document script, which V8 compiles from cache. Left to
  // the next `new_page` it would have to run immediately in a document
  // already loaded, and that path compiles it afresh: 5ms a page.
  engine_registered_on(transport.as_ref(), spares, browser_context_id, &target, &engine_src)
    .boxed()
    .await?;
  go_blank_on(transport.as_ref(), &session, true).boxed().await?;
  let applied_metrics = spares.default_metrics();
  let applied_media = super::default_emulated_media_params();
  let network_dirty = known_network_dirty.unwrap_or(true) || cookies_dirty;
  clear_and_prepare_on(
    transport.as_ref(),
    browser_context_id,
    &session,
    ClearPlan {
      origins,
      download_touched: hints.download_touched,
      network_dirty,
      permissions_dirty,
    },
    applied_metrics.as_ref(),
  )
  .boxed()
  .await?;
  transport.reset_session_dirty(&session);

  if pointer_dirty {
    spawn_pointer_reset(transport, &session);
  }
  Ok(Some(KeptPage {
    target,
    session,
    applied_metrics,
    applied_media,
    engine_on_session: false,
    engine_source: engine_src.into_owned(),
  }))
}

/// Reset the pointer off-screen asynchronously when the previous user
/// actually dispatched mouse or touch input on this page.
fn spawn_pointer_reset(transport: &Arc<CdpTransport>, session: &str) {
  let transport = Arc::clone(transport);
  let pointer_session = session.to_string();
  tokio::spawn(async move {
    let away = json!({ "type": "mouseMoved", "x": -1, "y": -1 });
    let _ = transport
      .send_command(Some(&pointer_session), "Input.dispatchMouseEvent", &away)
      .await;
  });
}

/// Close every page of the context but one, and name the one kept.
async fn keep_one_page_on(
  transport: &CdpTransport,
  spares: &Spares,
  browser_context_id: &str,
) -> Result<Option<String>> {
  let targets = transport
    .send(None, &ferridriver_cdp_proto::target::GetTargets {})
    .await?;
  let mut pages: Vec<String> = targets
    .target_infos
    .into_iter()
    .filter(|info| info.browser_context_id.as_deref() == Some(browser_context_id) && info.target_type == "page")
    .map(|info| info.target_id)
    .collect();
  if pages.is_empty() {
    return Ok(None);
  }
  // The page this context was handed, where it still has it: the
  // browser lists targets in no order worth relying on.
  let handed = {
    let sessions = lock(&spares.sessions);
    pages.iter().position(|target| sessions.contains_key(target))
  };
  let target = pages.swap_remove(handed.unwrap_or(0));
  if !pages.is_empty() {
    futures::future::try_join_all(pages.into_iter().map(|extra| async move {
      transport
        .send(None, &ferridriver_cdp_proto::target::CloseTarget { target_id: &extra })
        .await
        .map(|_| ())
    }))
    .await?;
  }
  Ok(Some(target))
}

/// Detach the previous user's session from `target` and attach a new
/// one in a single pipelined round trip (CDP executes browser-level commands
/// in FIFO order). Returns `(new_session, pointer_dirty)`.
async fn replace_session_on(
  transport: &CdpTransport,
  spares: &Spares,
  attached_targets: &AttachedTargets,
  target: &str,
) -> Result<(String, bool)> {
  let previous = lock(&spares.sessions)
    .remove(target)
    .or_else(|| lock(attached_targets).get(target).cloned().flatten());
  let pointer_dirty = previous
    .as_deref()
    .is_none_or(|session| transport.take_session_dirty(session).pointer);
  let attach_cmd = ferridriver_cdp_proto::target::AttachToTarget {
    target_id: target,
    flatten: Some(true),
  };
  let attached = if let Some(session) = previous {
    let detach_cmd = ferridriver_cdp_proto::target::DetachFromTarget {
      session_id: Some(&session),
      target_id: None,
    };
    let (_, attached) = tokio::try_join!(transport.send(None, &detach_cmd), transport.send(None, &attach_cmd),)?;
    transport.unregister_session(&session);
    attached
  } else {
    transport.send(None, &attach_cmd).await?
  };
  Ok((attached.session_id, pointer_dirty))
}

/// Have `engine` registered for `target`'s documents on the page's
/// engine session, attaching that session the first time. Answers
/// whether the registration changed: documents already loaded keep the
/// engine they were given.
async fn engine_registered_on(
  transport: &CdpTransport,
  spares: &Spares,
  browser_context_id: &str,
  target: &str,
  engine: &str,
) -> Result<bool> {
  let mut engines = spares.engines.lock().await;
  if engines.get(target).is_some_and(|held| held.source == engine) {
    return Ok(false);
  }
  let session = if let Some(held) = engines.remove(target) {
    futures::future::try_join_all(held.scripts.iter().map(|identifier| {
      let session = held.session.as_str();
      async move {
        transport
          .send_command(
            Some(session),
            "Page.removeScriptToEvaluateOnNewDocument",
            &json!({ "identifier": identifier }),
          )
          .await
          .map(|_| ())
      }
    }))
    .await?;
    held.session
  } else {
    let attached = transport
      .send_command(
        None,
        "Target.attachToTarget",
        &json!({ "targetId": target, "flatten": true }),
      )
      .await?;
    let session = attached
      .get("sessionId")
      .and_then(serde_json::Value::as_str)
      .ok_or_else(|| FerriError::protocol("Target.attachToTarget", "missing sessionId"))?
      .to_string();
    // A registration only takes effect on a session with `Page` enabled.
    transport
      .send_command_raw(Some(&session), "Page.enable", super::EMPTY_PARAMS_RAW)
      .await?;
    session
  };
  let [main, utility] = super::engine_registrations_raw(engine, false);
  let (in_main, in_utility) = tokio::try_join!(
    transport.send_command_raw(Some(&session), "Page.addScriptToEvaluateOnNewDocument", &main),
    transport.send_command_raw(Some(&session), "Page.addScriptToEvaluateOnNewDocument", &utility),
  )?;
  let scripts = [in_main, in_utility]
    .iter()
    .filter_map(|registered| registered.get("identifier").and_then(serde_json::Value::as_str))
    .map(str::to_string)
    .collect();
  engines.insert(
    target.to_string(),
    EngineSession {
      context: browser_context_id.to_string(),
      session,
      source: engine.to_string(),
      scripts,
    },
  );
  Ok(true)
}

/// Send the page of `session` to a blank document, enabling `Page` first when
/// `enable_page` is true (so `beforeunload` dialogs reach this session).
async fn go_blank_on(transport: &CdpTransport, session: &str, enable_page: bool) -> Result<()> {
  let on_page = Some(session);
  // A document with a `beforeunload` handler asks before it lets the
  // page go, and the navigation waits on the answer. The question only
  // reaches a session with `Page` enabled.
  let mut dialogs = transport.tap_event_methods(&["Page.javascriptDialogOpening"], on_page);
  if enable_page {
    transport.send_command(on_page, "Page.enable", &EMPTY_PARAMS).await?;
  }
  let blank = json!({ "url": "about:blank" });
  let go = transport.send_command(on_page, "Page.navigate", &blank);
  tokio::pin!(go);
  let navigated = loop {
    tokio::select! {
      navigated = &mut go => break navigated?,
      Some(_) = dialogs.recv() => {
        transport
          .send_command(on_page, "Page.handleJavaScriptDialog", &json!({ "accept": true }))
          .await?;
      },
    }
  };
  if let Some(error) = navigated
    .get("errorText")
    .and_then(serde_json::Value::as_str)
    .filter(|e| !e.is_empty())
  {
    return Err(FerriError::backend(format!(
      "the page would not leave its document: {error}"
    )));
  }
  Ok(())
}

struct ClearPlan<'a> {
  origins: &'a [String],
  download_touched: bool,
  network_dirty: bool,
  permissions_dirty: bool,
}

fn push_cmd<'a>(
  cmds: &mut Vec<futures::future::BoxFuture<'a, Result<()>>>,
  transport: &'a CdpTransport,
  session: Option<&'a str>,
  method: &'static str,
  params: serde_json::Value,
) {
  cmds.push(Box::pin(async move {
    transport.send_command(session, method, &params).await.map(|_| ())
  }));
}

fn push_raw_cmd<'a>(
  cmds: &mut Vec<futures::future::BoxFuture<'a, Result<()>>>,
  transport: &'a CdpTransport,
  session: Option<&'a str>,
  method: &'static str,
  raw_params: &'static str,
) {
  cmds.push(Box::pin(async move {
    transport
      .send_command_raw(session, method, raw_params)
      .await
      .map(|_| ())
  }));
}

fn push_clears<'a>(
  cmds: &mut Vec<futures::future::BoxFuture<'a, Result<()>>>,
  transport: &'a CdpTransport,
  browser_context_id: &str,
  session: &'a str,
  plan: &ClearPlan<'a>,
) {
  let on_page = Some(session);
  if plan.network_dirty {
    push_cmd(
      cmds,
      transport,
      None,
      "Storage.clearCookies",
      json!({ "browserContextId": browser_context_id }),
    );
    push_raw_cmd(
      cmds,
      transport,
      on_page,
      "Network.clearBrowserCache",
      super::EMPTY_PARAMS_RAW,
    );
  }
  if plan.permissions_dirty {
    push_cmd(
      cmds,
      transport,
      None,
      "Browser.resetPermissions",
      json!({ "browserContextId": browser_context_id }),
    );
  }
  if plan.download_touched {
    push_cmd(
      cmds,
      transport,
      None,
      "Browser.setDownloadBehavior",
      json!({ "behavior": "default", "browserContextId": browser_context_id }),
    );
  }
  push_raw_cmd(
    cmds,
    transport,
    on_page,
    "Page.resetNavigationHistory",
    super::EMPTY_PARAMS_RAW,
  );
  push_raw_cmd(
    cmds,
    transport,
    on_page,
    "Runtime.evaluate",
    r#"{"expression":"window.name = ''; undefined"}"#,
  );
  for origin in plan.origins {
    push_cmd(
      cmds,
      transport,
      on_page,
      "Storage.clearDataForOrigin",
      json!({ "origin": origin, "storageTypes": "all" }),
    );
  }
}

/// Minimal pipelined clear batch for a clean kept session whose domains and
/// emulation are already active: skips domain setup and any untouched clears.
async fn clear_kept_session_on(
  transport: &CdpTransport,
  browser_context_id: &str,
  session: &str,
  plan: ClearPlan<'_>,
) -> Result<()> {
  let mut cmds = Vec::with_capacity(6 + plan.origins.len());
  push_clears(&mut cmds, transport, browser_context_id, session, &plan);
  futures::future::try_join_all(cmds).await?;
  Ok(())
}

/// Clear what outlives a session AND pre-enable the fresh session's domains
/// and default emulation in a single pipelined batch so the next `new_page`
/// starts with zero protocol round trips.
async fn clear_and_prepare_on(
  transport: &CdpTransport,
  browser_context_id: &str,
  session: &str,
  plan: ClearPlan<'_>,
  default_metrics: Option<&serde_json::Value>,
) -> Result<()> {
  let on_page = Some(session);
  let mut cmds = Vec::with_capacity(15 + plan.origins.len());
  // Pre-enable domains on the fresh session in the same batch.
  push_raw_cmd(&mut cmds, transport, on_page, "Runtime.enable", super::EMPTY_PARAMS_RAW);
  push_raw_cmd(&mut cmds, transport, on_page, "Network.enable", super::EMPTY_PARAMS_RAW);
  push_raw_cmd(
    &mut cmds,
    transport,
    on_page,
    "Page.setLifecycleEventsEnabled",
    super::ENABLED_TRUE_RAW,
  );
  push_raw_cmd(
    &mut cmds,
    transport,
    on_page,
    "Target.setAutoAttach",
    super::AUTOATTACH_PARAMS_RAW,
  );
  push_raw_cmd(
    &mut cmds,
    transport,
    on_page,
    "Emulation.setFocusEmulationEnabled",
    super::ENABLED_TRUE_RAW,
  );
  push_raw_cmd(
    &mut cmds,
    transport,
    on_page,
    "Inspector.enable",
    super::EMPTY_PARAMS_RAW,
  );
  push_raw_cmd(
    &mut cmds,
    transport,
    on_page,
    "Emulation.setEmulatedMedia",
    super::default_emulated_media_raw(),
  );
  if let Some(metrics) = default_metrics {
    push_cmd(
      &mut cmds,
      transport,
      on_page,
      "Emulation.setDeviceMetricsOverride",
      metrics.clone(),
    );
  }
  push_clears(&mut cmds, transport, browser_context_id, session, &plan);
  futures::future::try_join_all(cmds).await?;
  Ok(())
}
