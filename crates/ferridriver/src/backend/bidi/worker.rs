//! Dedicated workers as `BiDi` realms (Playwright's
//! `bidiPage._onRealmCreated`). A worker realm names no browsing context,
//! only the realm that owns it, so a page claims the workers its own
//! window realms, or its workers' realms, own.

use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::{Value, json};

use super::session::BidiSession;
use crate::error::{FerriError, Result};

/// One dedicated worker of a `BiDi` page.
#[derive(Clone)]
pub struct BidiWorker {
  session: Arc<BidiSession>,
  realm: Arc<str>,
}

impl crate::worker::WorkerSession for BidiWorker {
  fn evaluate<'a>(&'a self, expression: &'a str) -> futures::future::BoxFuture<'a, Result<Value>> {
    Box::pin(self.evaluate_expression(expression))
  }
}

impl BidiWorker {
  async fn evaluate_expression(&self, expression: &str) -> Result<Value> {
    let reply = self
      .session
      .transport
      .send_command(
        "script.evaluate",
        json!({
          "expression": expression,
          "target": { "realm": &*self.realm },
          "awaitPromise": true,
          "resultOwnership": "none",
        }),
      )
      .await
      .map_err(|error| match error {
        // Firefox announces a worker's realm and then reports it missing
        // to every script command, while the worker itself keeps running.
        FerriError::TargetClosed { .. } if error.to_string().contains("Realm with id") => FerriError::unsupported(
          "this browser announces a worker's realm over WebDriver BiDi but does not run scripts in it",
        ),
        other => other,
      })?;
    if reply.get("type").and_then(Value::as_str) == Some("exception") {
      let message = reply
        .pointer("/exceptionDetails/text")
        .and_then(Value::as_str)
        .unwrap_or("worker evaluation threw");
      return Err(FerriError::evaluation(message));
    }
    Ok(match reply.pointer("/result/type").and_then(Value::as_str) {
      Some("string") => reply.pointer("/result/value").cloned().unwrap_or(Value::Null),
      _ => Value::Null,
    })
  }
}

/// The realm graph one page tracks from `script.realmCreated` and
/// `script.realmDestroyed`.
pub(crate) struct WorkerRealms {
  session: Arc<BidiSession>,
  /// The page's browsing context and its descendants.
  contexts: FxHashSet<String>,
  /// Window realms of those contexts, and the worker realms already claimed.
  owners: FxHashSet<String>,
  workers: FxHashMap<String, crate::worker::Worker>,
}

impl WorkerRealms {
  pub(crate) fn new(session: Arc<BidiSession>, context: &str) -> Self {
    Self {
      session,
      contexts: std::iter::once(context.to_string()).collect(),
      owners: FxHashSet::default(),
      workers: FxHashMap::default(),
    }
  }

  /// Take the window realms `script.getRealms` reports: the page's
  /// current document was created before anything listened.
  pub(crate) fn seed(&mut self, reply: &Value) {
    for realm in reply.get("realms").and_then(Value::as_array).into_iter().flatten() {
      if realm.get("type").and_then(Value::as_str) == Some("window")
        && realm.get("sandbox").is_none()
        && let Some(id) = realm.get("realm").and_then(Value::as_str)
      {
        self.owners.insert(id.to_string());
      }
    }
  }

  /// Feed one event; a worker this page now owns is returned for the page
  /// to announce.
  pub(crate) fn observe(&mut self, method: &str, params: &Value) -> Option<crate::worker::Worker> {
    match method {
      "browsingContext.contextCreated" => {
        if let (Some(context), Some(parent)) = (
          params.get("context").and_then(Value::as_str),
          params.get("parent").and_then(Value::as_str),
        ) && self.contexts.contains(parent)
        {
          self.contexts.insert(context.to_string());
        }
        None
      },
      "browsingContext.contextDestroyed" => {
        if let Some(context) = params.get("context").and_then(Value::as_str) {
          self.contexts.remove(context);
        }
        None
      },
      "script.realmCreated" => self.realm_created(params),
      "script.realmDestroyed" => {
        let realm = params.get("realm").and_then(Value::as_str)?;
        self.owners.remove(realm);
        if let Some(worker) = self.workers.remove(realm) {
          worker.did_close();
        }
        None
      },
      _ => None,
    }
  }

  fn realm_created(&mut self, params: &Value) -> Option<crate::worker::Worker> {
    let realm = params.get("realm").and_then(Value::as_str)?;
    match params.get("type").and_then(Value::as_str) {
      Some("window") => {
        let context = params.get("context").and_then(Value::as_str)?;
        if self.contexts.contains(context) && params.get("sandbox").is_none() {
          self.owners.insert(realm.to_string());
        }
        None
      },
      Some("dedicated-worker") => {
        let owner = params.pointer("/owners/0").and_then(Value::as_str)?;
        if !self.owners.contains(owner) {
          return None;
        }
        self.owners.insert(realm.to_string());
        let worker = crate::worker::Worker::new(
          realm.to_string(),
          params
            .get("origin")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
          Arc::new(BidiWorker {
            session: Arc::clone(&self.session),
            realm: Arc::from(realm),
          }),
        );
        self.workers.insert(realm.to_string(), worker.clone());
        Some(worker)
      },
      _ => None,
    }
  }

  /// The page closed: every worker it owned is gone.
  pub(crate) fn close_all(&mut self) {
    for (_, worker) in self.workers.drain() {
      worker.did_close();
    }
  }
}
