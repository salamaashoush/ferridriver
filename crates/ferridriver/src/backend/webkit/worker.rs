//! Dedicated workers over the `Worker` domain (Playwright's
//! `wkWorkers.ts`). A worker has no target of its own: commands for it
//! travel inside `Worker.sendMessageToWorker` on the page's target
//! session, and its replies come back as `Worker.dispatchMessageFromWorker`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rustc_hash::FxHashMap;
use serde_json::{Value, json};

use super::connection::Session;
use crate::error::{FerriError, Result};

type Pending = FxHashMap<u64, tokio::sync::oneshot::Sender<Result<Value>>>;

/// The replies a worker owes, and its global object once known.
#[derive(Default)]
pub(crate) struct WorkerInbox {
  next_id: AtomicU64,
  pending: std::sync::Mutex<Pending>,
  global: tokio::sync::Mutex<Option<String>>,
}

impl WorkerInbox {
  /// Hand a reply the worker dispatched to whoever is waiting for it.
  pub(crate) fn deliver(&self, message: &Value) {
    let Some(id) = message.get("id").and_then(Value::as_u64) else {
      return;
    };
    let Some(waiter) = self
      .pending
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .remove(&id)
    else {
      return;
    };
    let reply = match message.get("error") {
      Some(error) => Err(FerriError::protocol(
        "worker",
        error
          .get("message")
          .and_then(Value::as_str)
          .unwrap_or("worker command failed"),
      )),
      None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
    };
    let _ = waiter.send(reply);
  }

  /// Fail every waiting command: the worker is gone.
  pub(crate) fn close(&self) {
    let pending = std::mem::take(&mut *self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    for (_, waiter) in pending {
      let _ = waiter.send(Err(FerriError::target_closed(Some("Worker was closed".into()))));
    }
  }
}

/// One dedicated worker of a `WebKit` page.
#[derive(Clone)]
pub struct WebKitWorker {
  session: Session,
  worker_id: Arc<str>,
  inbox: Arc<WorkerInbox>,
}

impl WebKitWorker {
  pub(crate) fn new(session: Session, worker_id: &str, inbox: Arc<WorkerInbox>) -> Self {
    Self {
      session,
      worker_id: Arc::from(worker_id),
      inbox,
    }
  }

  async fn command(&self, method: &str, params: Value) -> Result<Value> {
    let id = self.inbox.next_id.fetch_add(1, Ordering::Relaxed) + 1;
    let (reply, replied) = tokio::sync::oneshot::channel();
    self
      .inbox
      .pending
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .insert(id, reply);
    let message = json!({ "id": id, "method": method, "params": params }).to_string();
    if let Err(error) = self
      .session
      .send(
        "Worker.sendMessageToWorker",
        json!({ "workerId": &*self.worker_id, "message": message }),
      )
      .await
    {
      self
        .inbox
        .pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&id);
      return Err(FerriError::protocol("Worker.sendMessageToWorker", error.to_string()));
    }
    replied
      .await
      .map_err(|_| FerriError::target_closed(Some("Worker was closed".into())))?
  }

  /// `WebKit`'s `Runtime.evaluate` cannot await a promise, so the
  /// expression runs through `Runtime.callFunctionOn` on the worker's
  /// global object, which can.
  async fn evaluate_expression(&self, expression: &str) -> Result<Value> {
    let global = {
      let mut global = self.inbox.global.lock().await;
      if global.is_none() {
        let reply = self
          .command("Runtime.evaluate", json!({ "expression": "globalThis" }))
          .await?;
        *global = reply
          .pointer("/result/objectId")
          .and_then(Value::as_str)
          .map(str::to_string);
      }
      global
        .clone()
        .ok_or_else(|| FerriError::protocol("Runtime.evaluate", "worker global has no object id"))?
    };
    let reply = self
      .command(
        "Runtime.callFunctionOn",
        json!({
          "objectId": global,
          "functionDeclaration": format!("function() {{ return ({expression}); }}"),
          "awaitPromise": true,
          "returnByValue": true,
        }),
      )
      .await?;
    if reply.get("wasThrown").and_then(Value::as_bool) == Some(true) {
      let message = reply
        .pointer("/result/description")
        .and_then(Value::as_str)
        .unwrap_or("worker evaluation threw");
      return Err(FerriError::evaluation(message));
    }
    Ok(reply.pointer("/result/value").cloned().unwrap_or(Value::Null))
  }
}

impl crate::worker::WorkerSession for WebKitWorker {
  fn evaluate<'a>(&'a self, expression: &'a str) -> futures::future::BoxFuture<'a, Result<Value>> {
    Box::pin(self.evaluate_expression(expression))
  }
}

/// The workers of one page, by `workerId`.
pub(crate) type Workers = Arc<std::sync::Mutex<FxHashMap<String, (crate::worker::Worker, Arc<WorkerInbox>)>>>;

/// `Worker.workerCreated`: announce the worker, then let it run
/// (`Worker.initialized`), as `wkWorkers.ts` does.
pub(crate) fn created(params: &Value, session: &Session, workers: &Workers, emitter: &crate::events::EventEmitter) {
  let Some(worker_id) = params.get("workerId").and_then(Value::as_str) else {
    return;
  };
  let inbox = Arc::new(WorkerInbox::default());
  let worker = crate::worker::Worker::new(
    worker_id.to_string(),
    params
      .get("url")
      .and_then(Value::as_str)
      .unwrap_or_default()
      .to_string(),
    Arc::new(WebKitWorker::new(session.clone(), worker_id, Arc::clone(&inbox))),
  );
  workers
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .insert(worker_id.to_string(), (worker.clone(), inbox));
  emitter.emit(crate::events::PageEvent::Worker(worker));
  let session = session.clone();
  let worker_id = worker_id.to_string();
  tokio::spawn(async move {
    let _ = session
      .send("Worker.initialized", json!({ "workerId": worker_id }))
      .await;
  });
}

/// `Worker.dispatchMessageFromWorker`: a reply to one of our commands.
pub(crate) fn dispatched(params: &Value, workers: &Workers) {
  let (Some(worker_id), Some(message)) = (
    params.get("workerId").and_then(Value::as_str),
    params.get("message").and_then(Value::as_str),
  ) else {
    return;
  };
  let inbox = workers
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .get(worker_id)
    .map(|(_, inbox)| Arc::clone(inbox));
  if let (Some(inbox), Ok(message)) = (inbox, serde_json::from_str::<Value>(message)) {
    inbox.deliver(&message);
  }
}

/// `Worker.workerTerminated`, or every worker when `worker_id` is `None`:
/// the page's target went away with them.
pub(crate) fn terminated(worker_id: Option<&str>, workers: &Workers) {
  let gone: Vec<_> = {
    let mut workers = workers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match worker_id {
      Some(id) => workers.remove(id).into_iter().collect(),
      None => workers.drain().map(|(_, entry)| entry).collect(),
    }
  };
  for (worker, inbox) in gone {
    inbox.close();
    worker.did_close();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn a_reply_reaches_its_waiter_and_closing_fails_the_rest() {
    let inbox = WorkerInbox::default();
    let (first, first_reply) = tokio::sync::oneshot::channel();
    let (second, second_reply) = tokio::sync::oneshot::channel();
    inbox.pending.lock().unwrap().insert(1, first);
    inbox.pending.lock().unwrap().insert(2, second);
    inbox.deliver(&json!({ "id": 1, "result": { "result": { "value": "\"ok\"" } } }));
    assert_eq!(
      first_reply.await.unwrap().unwrap(),
      json!({ "result": { "value": "\"ok\"" } })
    );
    inbox.close();
    assert!(second_reply.await.unwrap().unwrap_err().is_target_closed_error());
  }
}
