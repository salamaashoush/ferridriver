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

use std::time::Duration;

use rustc_hash::FxHashMap;
use serde_json::json;

use super::super::EMPTY_PARAMS;
use super::{CdpBrowser, CdpWrap};
use crate::error::{FerriError, Result};

/// How long emptying a context may take before it is disposed instead.
const RESET_BUDGET: Duration = Duration::from_secs(2);

/// The page a recycled context keeps, and the session attached to it.
pub(super) struct KeptPage {
  pub(super) target: String,
  pub(super) session: String,
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
  /// The session of every kept page in use, by target: the one to detach
  /// when its context comes back.
  sessions: std::sync::Mutex<FxHashMap<String, String>>,
  /// The engine session of every kept page, by target.
  engines: tokio::sync::Mutex<FxHashMap<String, EngineSession>>,
  /// Contexts created with a proxy. A proxy is fixed for a context's
  /// life, so one of these can never stand in for another context.
  proxied: std::sync::Mutex<rustc_hash::FxHashSet<String>>,
}

fn lock<V>(mutex: &std::sync::Mutex<V>) -> std::sync::MutexGuard<'_, V> {
  mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Spares {
  /// An emptied context for a `new_context` to return instead of creating
  /// one. Its page waits in `handed` for the context's first `new_page`.
  pub(super) fn take_context(&self) -> Option<String> {
    let (context, page) = lock(&self.idle).pop()?;
    lock(&self.handed).insert(context.clone(), page);
    Some(context)
  }

  /// The page kept for `context`, for its first `new_page`.
  pub(super) fn take_page(&self, context: &str) -> Option<KeptPage> {
    let page = lock(&self.handed).remove(context)?;
    lock(&self.sessions).insert(page.target.clone(), page.session.clone());
    Some(page)
  }

  /// Record that `context` was created with a proxy.
  pub(super) fn mark_proxied(&self, context: &str) {
    lock(&self.proxied).insert(context.to_string());
  }

  fn forget(&self, context: &str) {
    lock(&self.proxied).remove(context);
    // The sessions themselves go with the context's targets.
    if let Ok(mut engines) = self.engines.try_lock() {
      engines.retain(|_, engine| engine.context != context);
    }
    lock(&self.idle).retain(|(id, _)| id != context);
    if let Some(page) = lock(&self.handed).remove(context) {
      lock(&self.sessions).remove(&page.target);
    }
  }
}

impl<T: CdpWrap> CdpBrowser<T> {
  /// Close a context its user is done with, keeping it for the next
  /// `new_context` when it can be emptied and disposing it otherwise.
  /// `origins` are the origins it visited.
  ///
  /// # Errors
  ///
  /// Returns the disposal's error when the context had to be disposed.
  pub async fn retire_context(&self, browser_context_id: &str, origins: &[String]) -> Result<()> {
    if lock(&self.spares.proxied).contains(browser_context_id) {
      return self.dispose_context(browser_context_id).await;
    }
    let emptied = tokio::time::timeout(RESET_BUDGET, self.empty_context(browser_context_id, origins)).await;
    match emptied {
      Ok(Ok(Some(page))) => {
        lock(&self.spares.idle).push((browser_context_id.to_string(), page));
        return Ok(());
      },
      Ok(Ok(None)) => {},
      Ok(Err(error)) => {
        tracing::debug!(target: "ferridriver::browser", %error, "context not recycled: emptying it failed");
      },
      Err(_) => {
        tracing::debug!(target: "ferridriver::browser", "context not recycled: emptying it timed out");
      },
    }
    self.dispose_context(browser_context_id).await
  }

  pub(super) fn forget_spare(&self, browser_context_id: &str) {
    self.spares.forget(browser_context_id);
  }

  /// Leave `browser_context_id` as a new context would be, with one blank
  /// page on a new session. `None` when it has no page left to keep.
  async fn empty_context(&self, browser_context_id: &str, origins: &[String]) -> Result<Option<KeptPage>> {
    let Some(target) = self.keep_one_page(browser_context_id).await? else {
      return Ok(None);
    };
    let session = self.replace_session(&target).await?;
    // Before the page goes blank, so the blank document takes the engine
    // as a new-document script, which V8 compiles from cache. Left to
    // the next `new_page` it would have to run immediately in a document
    // already loaded, and that path compiles it afresh: 5ms a page.
    self
      .engine_registered(browser_context_id, &target, &crate::selectors::build_lazy_inject_js())
      .await?;
    self.go_blank(&session).await?;
    self
      .clear_what_outlives_a_session(browser_context_id, &session, origins)
      .await?;

    // The pointer is left where the previous user had it, and the next
    // document would open hovered there. The browser answers a pointer
    // event with the frame that shows it, about 16ms away, and puts it
    // ahead of any input the next user sends, so nothing waits for it.
    let transport = std::sync::Arc::clone(&self.transport);
    let pointer_session = session.clone();
    tokio::spawn(async move {
      let away = json!({ "type": "mouseMoved", "x": -1, "y": -1 });
      let _ = transport
        .send_command(Some(&pointer_session), "Input.dispatchMouseEvent", &away)
        .await;
    });
    Ok(Some(KeptPage { target, session }))
  }

  /// Close every page of the context but one, and name the one kept.
  async fn keep_one_page(&self, browser_context_id: &str) -> Result<Option<String>> {
    let targets = self
      .transport
      .send_command(None, "Target.getTargets", &EMPTY_PARAMS)
      .await?;
    let mut pages: Vec<String> = targets
      .get("targetInfos")
      .and_then(serde_json::Value::as_array)
      .into_iter()
      .flatten()
      .filter(|info| {
        info.get("browserContextId").and_then(serde_json::Value::as_str) == Some(browser_context_id)
          && info.get("type").and_then(serde_json::Value::as_str) == Some("page")
      })
      .filter_map(|info| info.get("targetId").and_then(serde_json::Value::as_str))
      .map(str::to_string)
      .collect();
    if pages.is_empty() {
      return Ok(None);
    }
    // The page this context was handed, where it still has it: the
    // browser lists targets in no order worth relying on.
    let handed = {
      let sessions = lock(&self.spares.sessions);
      pages.iter().position(|target| sessions.contains_key(target))
    };
    let target = pages.swap_remove(handed.unwrap_or(0));
    for extra in pages {
      self
        .transport
        .send_command(None, "Target.closeTarget", &json!({ "targetId": extra }))
        .await?;
    }
    Ok(Some(target))
  }

  /// Detach the previous user's session from `target` and attach a new
  /// one. Every override the old session installed goes with it, so the
  /// page then navigates under a session that has installed none, and no
  /// init script or interception of theirs runs in the blank document.
  async fn replace_session(&self, target: &str) -> Result<String> {
    let previous = lock(&self.spares.sessions)
      .remove(target)
      .or_else(|| lock(&self.attached_targets).get(target).cloned().flatten());
    if let Some(session) = previous {
      self
        .transport
        .send_command(None, "Target.detachFromTarget", &json!({ "sessionId": session }))
        .await?;
    }
    let attached = self
      .transport
      .send_command(
        None,
        "Target.attachToTarget",
        &json!({ "targetId": target, "flatten": true }),
      )
      .await?;
    attached
      .get("sessionId")
      .and_then(serde_json::Value::as_str)
      .map(str::to_string)
      .ok_or_else(|| FerriError::protocol("Target.attachToTarget", "missing sessionId"))
  }

  /// Have `engine` registered for `target`'s documents on the page's
  /// engine session, attaching that session the first time. Answers
  /// whether the registration changed: documents already loaded keep the
  /// engine they were given.
  async fn engine_registered(&self, browser_context_id: &str, target: &str, engine: &str) -> Result<bool> {
    let mut engines = self.spares.engines.lock().await;
    if engines.get(target).is_some_and(|held| held.source == engine) {
      return Ok(false);
    }
    let session = if let Some(held) = engines.remove(target) {
      for identifier in &held.scripts {
        self
          .transport
          .send_command(
            Some(&held.session),
            "Page.removeScriptToEvaluateOnNewDocument",
            &json!({ "identifier": identifier }),
          )
          .await?;
      }
      held.session
    } else {
      let attached = self
        .transport
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
      self
        .transport
        .send_command(Some(&session), "Page.enable", &EMPTY_PARAMS)
        .await?;
      session
    };
    let [main, utility] = super::engine_registrations(engine, false);
    let (in_main, in_utility) = tokio::try_join!(
      self
        .transport
        .send_command(Some(&session), "Page.addScriptToEvaluateOnNewDocument", &main),
      self
        .transport
        .send_command(Some(&session), "Page.addScriptToEvaluateOnNewDocument", &utility),
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

  /// Send the page of `session` to a blank document.
  async fn go_blank(&self, session: &str) -> Result<()> {
    let on_page = Some(session);
    // A document with a `beforeunload` handler asks before it lets the
    // page go, and the navigation waits on the answer. The question only
    // reaches a session with `Page` enabled.
    let mut dialogs = self
      .transport
      .tap_event_methods(&["Page.javascriptDialogOpening"], on_page);
    self
      .transport
      .send_command(on_page, "Page.enable", &EMPTY_PARAMS)
      .await?;
    let blank = json!({ "url": "about:blank" });
    let navigation = self.transport.send_command(on_page, "Page.navigate", &blank);
    tokio::pin!(navigation);
    let navigated = loop {
      tokio::select! {
        navigated = &mut navigation => break navigated?,
        Some(_) = dialogs.recv() => {
          self
            .transport
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

  /// Clear what the previous user left that no session owns: cookies,
  /// permissions, the download behaviour, the HTTP cache, the tab's
  /// history and name, and the storage of every origin it visited.
  async fn clear_what_outlives_a_session(
    &self,
    browser_context_id: &str,
    session: &str,
    origins: &[String],
  ) -> Result<()> {
    let on_page = Some(session);
    let in_context = json!({ "browserContextId": browser_context_id });
    let default_downloads = json!({ "behavior": "default", "browserContextId": browser_context_id });
    let unnamed = json!({ "expression": "window.name = ''; undefined" });
    let clear_origins = async {
      for origin in origins {
        self
          .transport
          .send_command(
            on_page,
            "Storage.clearDataForOrigin",
            &json!({ "origin": origin, "storageTypes": "all" }),
          )
          .await?;
      }
      Ok::<(), FerriError>(())
    };
    tokio::try_join!(
      self.transport.send_command(None, "Storage.clearCookies", &in_context),
      self
        .transport
        .send_command(None, "Browser.resetPermissions", &in_context),
      self
        .transport
        .send_command(None, "Browser.setDownloadBehavior", &default_downloads),
      self
        .transport
        .send_command(on_page, "Network.clearBrowserCache", &EMPTY_PARAMS),
      self
        .transport
        .send_command(on_page, "Page.resetNavigationHistory", &EMPTY_PARAMS),
      self.transport.send_command(on_page, "Runtime.evaluate", &unnamed),
      clear_origins,
    )?;
    Ok(())
  }

  /// Hand a kept page to the `new_page` that takes it. The page already
  /// has the engine in its blank document; a selector engine registered
  /// since changes the source, and the page then opens a new blank
  /// document under the current one.
  pub(super) async fn adopt_kept_page(
    &self,
    browser_context_id: &str,
    page: KeptPage,
    engine: &str,
  ) -> Result<(String, Option<String>)> {
    if self.engine_registered(browser_context_id, &page.target, engine).await? {
      self.go_blank(&page.session).await?;
    }
    Ok((page.target, Some(page.session)))
  }
}
