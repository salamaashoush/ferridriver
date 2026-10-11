//! What a user context gives the pages the browser opens in it by itself.
//!
//! A popup cannot be paused over `BiDi`: its first document loads while
//! the pump is still configuring the page. So what a context registers
//! for all its pages is registered once against the user context
//! (preload scripts) or the session (the request intercept, which has no
//! user-context form), as Playwright's `BidiBrowserContext` does, and a
//! popup inherits it from the moment the browser creates it.

use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::json;

use super::session::BidiSession;
use crate::error::{FerriError, Result};
use crate::route::RegisteredRoute;

#[derive(Default)]
pub(crate) struct ContextScopes {
  inner: std::sync::Mutex<Inner>,
  /// Serializes the session intercept's install and removal.
  intercept_change: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct Inner {
  contexts: FxHashMap<String, Scope>,
  /// The session-wide intercept behind every context's routes.
  intercept: Option<String>,
}

#[derive(Default)]
struct Scope {
  /// Context init scripts by registry key; `None` while one installs.
  scripts: FxHashMap<u64, Option<String>>,
  /// Preload scripts installed once for the user context, by name.
  preloads: FxHashSet<String>,
  routes: Vec<RegisteredRoute>,
}

impl ContextScopes {
  fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
    self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  /// Install a context init script for every page of `user_context`,
  /// present and future. A second page of the context finds it there.
  pub(crate) async fn add_script(
    &self,
    session: &BidiSession,
    user_context: &str,
    key: u64,
    function: &str,
  ) -> Result<()> {
    {
      let mut inner = self.lock();
      let scripts = &mut inner.contexts.entry(user_context.to_string()).or_default().scripts;
      if scripts.contains_key(&key) {
        return Ok(());
      }
      scripts.insert(key, None);
    }
    let installed = session
      .transport
      .send_command(
        "script.addPreloadScript",
        json!({"functionDeclaration": function, "userContexts": [user_context]}),
      )
      .await
      .and_then(|result| {
        result
          .get("script")
          .and_then(|v| v.as_str())
          .map(str::to_string)
          .ok_or_else(|| FerriError::protocol("script.addPreloadScript", "missing script id"))
      });
    let mut inner = self.lock();
    let scripts = &mut inner.contexts.entry(user_context.to_string()).or_default().scripts;
    match installed {
      Ok(script) => {
        scripts.insert(key, Some(script));
        Ok(())
      },
      Err(error) => {
        scripts.remove(&key);
        Err(error)
      },
    }
  }

  /// Forget a context init script, handing back the preload to remove.
  pub(crate) fn take_script(&self, user_context: &str, key: u64) -> Option<String> {
    self
      .lock()
      .contexts
      .get_mut(user_context)?
      .scripts
      .remove(&key)
      .flatten()
  }

  /// Whether `name` still has to be preloaded for `user_context`; the
  /// caller that is told so installs it.
  pub(crate) fn claim_preload(&self, user_context: &str, name: &str) -> bool {
    self
      .lock()
      .contexts
      .entry(user_context.to_string())
      .or_default()
      .preloads
      .insert(name.to_string())
  }

  pub(crate) fn release_preload(&self, user_context: &str, name: &str) {
    if let Some(scope) = self.lock().contexts.get_mut(user_context) {
      scope.preloads.remove(name);
    }
  }

  /// The routes a page the browser opens in `user_context` starts with.
  pub(crate) fn routes(&self, user_context: &str) -> Vec<RegisteredRoute> {
    self
      .lock()
      .contexts
      .get(user_context)
      .map(|scope| scope.routes.clone())
      .unwrap_or_default()
  }

  /// Whether the session intercept alone blocked a request, so that no
  /// page's own intercept will see it resolved.
  pub(crate) fn blocked_by_session_only(&self, intercepts: &[&str]) -> bool {
    let inner = self.lock();
    inner
      .intercept
      .as_deref()
      .is_some_and(|ours| !intercepts.is_empty() && intercepts.iter().all(|id| *id == ours))
  }

  /// Record a context route and make sure the session intercept that
  /// reaches the context's popups exists.
  pub(crate) async fn add_route(
    &self,
    session: &BidiSession,
    user_context: &str,
    route: &RegisteredRoute,
  ) -> Result<()> {
    {
      let mut inner = self.lock();
      let routes = &mut inner.contexts.entry(user_context.to_string()).or_default().routes;
      if !routes.iter().any(|known| known.handler_id() == route.handler_id()) {
        routes.push(route.clone());
      }
    }
    let _change = self.intercept_change.lock().await;
    if self.lock().intercept.is_some() {
      return Ok(());
    }
    let result = session
      .transport
      .send_command("network.addIntercept", json!({"phases": ["beforeRequestSent"]}))
      .await?;
    let intercept = result
      .get("intercept")
      .and_then(|v| v.as_str())
      .ok_or_else(|| FerriError::protocol("network.addIntercept", "missing intercept id"))?;
    self.lock().intercept = Some(intercept.to_string());
    Ok(())
  }

  /// Drop the context routes `keep` rejects, and the session intercept
  /// once no context has a route left.
  pub(crate) async fn retain_routes(
    &self,
    session: &BidiSession,
    user_context: &str,
    keep: impl Fn(&RegisteredRoute) -> bool,
  ) {
    {
      let mut inner = self.lock();
      if let Some(scope) = inner.contexts.get_mut(user_context) {
        scope.routes.retain(keep);
      }
    }
    let _change = self.intercept_change.lock().await;
    let unused = {
      let mut inner = self.lock();
      if inner.contexts.values().all(|scope| scope.routes.is_empty()) {
        inner.intercept.take()
      } else {
        None
      }
    };
    if let Some(intercept) = unused {
      let _ = session
        .transport
        .send_command("network.removeIntercept", json!({"intercept": intercept}))
        .await;
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_preload_is_claimed_once_per_user_context() {
    let scopes = ContextScopes::default();
    assert!(scopes.claim_preload("a", "controller"));
    assert!(!scopes.claim_preload("a", "controller"));
    assert!(scopes.claim_preload("b", "controller"));
    scopes.release_preload("a", "controller");
    assert!(scopes.claim_preload("a", "controller"));
  }

  #[test]
  fn only_a_request_no_page_intercept_blocked_is_the_sessions_to_let_through() {
    let scopes = ContextScopes::default();
    assert!(!scopes.blocked_by_session_only(&["session"]));
    scopes.lock().intercept = Some("session".to_string());
    assert!(scopes.blocked_by_session_only(&["session"]));
    assert!(!scopes.blocked_by_session_only(&["session", "page"]));
    assert!(!scopes.blocked_by_session_only(&[]));
  }

  #[test]
  fn a_script_is_handed_back_once() {
    let scopes = ContextScopes::default();
    scopes
      .lock()
      .contexts
      .entry("a".to_string())
      .or_default()
      .scripts
      .insert(3, Some("preload".to_string()));
    assert_eq!(scopes.take_script("a", 3).as_deref(), Some("preload"));
    assert_eq!(scopes.take_script("a", 3), None);
    assert_eq!(scopes.take_script("b", 3), None);
  }
}
