//! One `WebDriver` `BiDi` session shared by every client of a served
//! browser.
//!
//! Firefox holds one `BiDi` session at a time, and the served browser's own
//! connection holds it, so a client cannot open another. Each client is
//! joined to that one instead: its `session.new` is answered with the
//! session's capabilities, its commands reach the browser under ids of the
//! shared connection and come back under its own, and an event reaches it
//! when one of its own subscriptions names the event. What a client ends is
//! its own: `session.end`, `browser.close` or hanging up removes its
//! subscriptions and what it added (user contexts, intercepts, data
//! collectors, preload scripts), and the browser and every other client
//! carry on. The browser's launch proxy wanting credentials is answered
//! here, for every client's pages: no client knows the password, and a
//! client answering the challenge without it would fail the request.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::{SinkExt as _, StreamExt as _};
use rustc_hash::FxHashMap;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use super::session::BidiSession;
use super::transport::{BidiEvent, EventBarrier};

/// Bounds the requests remembered as answered on a long-lived server.
const ANSWERED_REMEMBERED: usize = 10_000;

/// What one client subscribed to: the subscription the browser knows it
/// by, when the browser names one, and what it covers.
#[derive(Debug, Clone)]
struct Subscription {
  id: Option<String>,
  events: Vec<String>,
  /// Top-level contexts; an event of one of them or of a frame inside
  /// one is covered.
  contexts: Option<Vec<String>>,
  user_contexts: Option<Vec<String>>,
}

impl Subscription {
  fn names(&self, method: &str) -> bool {
    self.events.iter().any(|event| {
      event == method
        || method
          .strip_prefix(event.as_str())
          .is_some_and(|rest| rest.starts_with('.'))
    })
  }
}

/// Where each browsing context sits: its parent and its user context, as
/// the browser announced them.
#[derive(Default)]
struct Tree {
  parents: FxHashMap<String, Option<String>>,
  user_contexts: FxHashMap<String, String>,
}

impl Tree {
  fn top(&self, context: &str) -> String {
    let mut current = context.to_string();
    for _ in 0..64 {
      match self.parents.get(&current) {
        Some(Some(parent)) => current.clone_from(parent),
        _ => break,
      }
    }
    current
  }

  fn user_context(&self, context: &str) -> Option<&str> {
    self.user_contexts.get(&self.top(context)).map(String::as_str)
  }

  fn add(&mut self, info: &Value, parent: Option<&str>) {
    let Some(context) = info["context"].as_str() else {
      return;
    };
    let parent = info["parent"].as_str().or(parent).map(str::to_string);
    self.parents.insert(context.to_string(), parent);
    if let Some(user_context) = info["userContext"].as_str() {
      self.user_contexts.insert(context.to_string(), user_context.to_string());
    }
    for child in info["children"].as_array().into_iter().flatten() {
      self.add(child, Some(context));
    }
  }

  fn remove(&mut self, context: &str) {
    self.parents.remove(context);
    self.user_contexts.remove(context);
  }

  fn covers(&self, subscription: &Subscription, params: &Value) -> bool {
    if subscription.contexts.is_none() && subscription.user_contexts.is_none() {
      return true;
    }
    let Some(context) = params["context"]
      .as_str()
      .or_else(|| params["source"]["context"].as_str())
    else {
      return false;
    };
    if let Some(contexts) = &subscription.contexts {
      return contexts.contains(&self.top(context));
    }
    subscription
      .user_contexts
      .as_ref()
      .zip(self.user_context(context))
      .is_some_and(|(wanted, user_context)| wanted.iter().any(|wanted| wanted == user_context))
  }
}

/// A client joined to the session: where its messages go, what it
/// subscribed to, and what it added that outlives a command, as the
/// command that removes each.
struct Client {
  outbox: mpsc::UnboundedSender<Message>,
  subscriptions: Vec<Subscription>,
  added: Vec<(&'static str, Value)>,
}

/// What a command adds to the shared session, and how it is removed: the
/// command, the field of the answer naming it, and the removing command
/// with the parameter it takes. An intercept left behind would hold every
/// request of every client.
const ADDITIONS: [(&str, &str, &str, &str); 4] = [
  (
    "browser.createUserContext",
    "userContext",
    "browser.removeUserContext",
    "userContext",
  ),
  (
    "network.addIntercept",
    "intercept",
    "network.removeIntercept",
    "intercept",
  ),
  (
    "network.addDataCollector",
    "collector",
    "network.removeDataCollector",
    "collector",
  ),
  (
    "script.addPreloadScript",
    "script",
    "script.removePreloadScript",
    "script",
  ),
];

/// The addition a successful `method` with `result` made, as its removal.
fn addition(method: &str, result: &Value) -> Option<(&'static str, Value)> {
  let (_, field, removal, parameter) = ADDITIONS.iter().find(|(added, ..)| *added == method)?;
  let name = result[*field].as_str()?;
  Some((*removal, json!({ *parameter: name })))
}

/// The addition a successful removal `method` with `params` took away.
fn removal(method: &str, params: &Value) -> Option<(&'static str, Value)> {
  let (_, _, removal, parameter) = ADDITIONS.iter().find(|(_, _, removal, _)| *removal == method)?;
  let name = params[*parameter].as_str()?;
  Some((*removal, json!({ *parameter: name })))
}

/// The shared session and every client joined to it.
pub(crate) struct Relay {
  session: Arc<BidiSession>,
  /// The launch proxy's credentials, when it wants them.
  proxy_credentials: Option<crate::options::HttpCredentials>,
  /// Requests whose proxy challenge was answered once: a second means the
  /// proxy refused the credentials, and answering again would never end.
  answered: std::sync::Mutex<rustc_hash::FxHashSet<String>>,
  clients: std::sync::Mutex<FxHashMap<u64, Client>>,
  tree: std::sync::Mutex<Tree>,
  /// Passed once every event received before it reached its clients, so a
  /// reply never overtakes the events the browser sent ahead of it.
  barrier: EventBarrier,
  next_client: AtomicU64,
  events: tokio::task::JoinHandle<()>,
}

impl Drop for Relay {
  fn drop(&mut self) {
    self.events.abort();
  }
}

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
  mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn success(id: &Value, result: &Value) -> Message {
  Message::Text(
    json!({ "type": "success", "id": id, "result": result })
      .to_string()
      .into(),
  )
}

fn error(id: &Value, code: &str, message: &str) -> Message {
  Message::Text(
    json!({ "type": "error", "id": id, "error": code, "message": message })
      .to_string()
      .into(),
  )
}

fn strings(value: &Value) -> Option<Vec<String>> {
  value
    .as_array()
    .map(|items| items.iter().filter_map(Value::as_str).map(str::to_string).collect())
}

impl Relay {
  /// Shares `session` with clients joined by [`Self::join`], answering the
  /// launch proxy's challenges with `proxy_credentials`.
  pub(crate) async fn start(
    session: Arc<BidiSession>,
    proxy_credentials: Option<crate::options::HttpCredentials>,
  ) -> crate::error::Result<Arc<Self>> {
    let mut tap = session.transport.tap_events();
    let barrier = tap.barrier();
    let mut tree = Tree::default();
    let known = session
      .transport
      .send_command("browsingContext.getTree", json!({}))
      .await?;
    for context in known["contexts"].as_array().into_iter().flatten() {
      tree.add(context, None);
    }
    Ok(Arc::new_cyclic(|relay: &std::sync::Weak<Self>| {
      let relay = std::sync::Weak::clone(relay);
      let events = tokio::spawn(async move {
        while let Some(event) = tap.recv().await {
          let Some(relay) = relay.upgrade() else { return };
          relay.deliver(&event);
        }
      });
      Self {
        session,
        proxy_credentials,
        answered: std::sync::Mutex::new(rustc_hash::FxHashSet::default()),
        clients: std::sync::Mutex::new(FxHashMap::default()),
        tree: std::sync::Mutex::new(tree),
        barrier,
        next_client: AtomicU64::new(1),
        events,
      }
    }))
  }

  /// Answers a challenge of the launch proxy with its credentials, once
  /// per request; `false` for any other event.
  fn answer_proxy(&self, event: &BidiEvent) -> bool {
    let Some(credentials) = &self.proxy_credentials else {
      return false;
    };
    if event.method != "network.authRequired" || event.params["response"]["status"] != 407 {
      return false;
    }
    let Some(request) = event.params["request"]["request"].as_str() else {
      return false;
    };
    let first = {
      let mut answered = lock(&self.answered);
      if answered.len() >= ANSWERED_REMEMBERED {
        answered.clear();
      }
      answered.insert(request.to_string())
    };
    let answer = if first {
      json!({ "request": request, "action": "provideCredentials",
              "credentials": { "type": "password", "username": credentials.username, "password": credentials.password } })
    } else {
      json!({ "request": request, "action": "cancel" })
    };
    let session = Arc::clone(&self.session);
    tokio::spawn(async move {
      if let Err(failed) = session.transport.send_command("network.continueWithAuth", answer).await {
        tracing::debug!(target: "ferridriver::browser_server", %failed, "a proxy challenge was answered already");
      }
    });
    true
  }

  /// Hands `event` to every client one of whose subscriptions covers it.
  fn deliver(&self, event: &BidiEvent) {
    if self.answer_proxy(event) {
      return;
    }
    let mut tree = lock(&self.tree);
    match event.method.as_str() {
      "browsingContext.contextCreated" => tree.add(&event.params, None),
      "browsingContext.contextDestroyed" => {
        if let Some(context) = event.params["context"].as_str() {
          tree.remove(context);
        }
      },
      _ => {},
    }
    let mut text = None;
    let clients = lock(&self.clients);
    for client in clients.values() {
      let covered = client
        .subscriptions
        .iter()
        .any(|subscription| subscription.names(&event.method) && tree.covers(subscription, &event.params));
      if covered {
        let message = text.get_or_insert_with(|| {
          json!({ "type": "event", "method": event.method, "params": event.params }).to_string()
        });
        let _ = client.outbox.send(Message::Text(message.clone().into()));
      }
    }
  }

  /// Joins `socket`, an admitted client, to the session until it hangs up,
  /// ends its session, or `stopped` says the server stopped.
  pub(crate) async fn join<S>(
    self: Arc<Self>,
    socket: tokio_tungstenite::WebSocketStream<S>,
    mut stopped: tokio::sync::watch::Receiver<bool>,
  ) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
  {
    let id = self.next_client.fetch_add(1, Ordering::Relaxed);
    let (mut sink, mut stream) = socket.split();
    let (outbox, mut queued) = mpsc::unbounded_channel::<Message>();
    lock(&self.clients).insert(
      id,
      Client {
        outbox: outbox.clone(),
        subscriptions: Vec::new(),
        added: Vec::new(),
      },
    );
    let writer = tokio::spawn(async move {
      while let Some(message) = queued.recv().await {
        let close = matches!(message, Message::Close(_));
        if sink.send(message).await.is_err() || close {
          break;
        }
      }
      let _ = sink.close().await;
    });
    let mut commands = tokio::task::JoinSet::new();
    loop {
      let frame = tokio::select! {
        frame = stream.next() => frame,
        Some(_) = commands.join_next(), if !commands.is_empty() => continue,
        () = crate::browser_server::until_stopped(&mut stopped) => break,
      };
      let text = match frame {
        Some(Ok(Message::Text(text))) => text,
        Some(Ok(Message::Close(_)) | Err(_)) | None => break,
        Some(Ok(_)) => continue,
      };
      let Ok(command) = serde_json::from_str::<Value>(&text) else {
        let _ = outbox.send(error(&Value::Null, "invalid argument", "the command is not JSON"));
        continue;
      };
      let ending = matches!(command["method"].as_str(), Some("session.end" | "browser.close"));
      let relay = Arc::clone(&self);
      let outbox = outbox.clone();
      if ending {
        // Its last command: what it opened goes, then it is answered.
        relay.leave(id).await;
        let _ = outbox.send(success(&command["id"], &json!({})));
        if command["method"] == "browser.close" {
          let _ = outbox.send(Message::Close(None));
        }
        continue;
      }
      commands.spawn(async move {
        let reply = relay.command(id, &command).await;
        // Events the browser sent before this reply reach the client first.
        let _ = relay.barrier.wait().await;
        let _ = outbox.send(reply);
      });
    }
    commands.shutdown().await;
    self.leave(id).await;
    lock(&self.clients).remove(&id);
    drop(outbox);
    let _ = writer.await;
  }

  /// Carries out one command of client `id`, answered under its own id.
  async fn command(&self, client: u64, command: &Value) -> Message {
    let id = &command["id"];
    let Some(method) = command["method"].as_str() else {
      return error(id, "invalid argument", "the command names no method");
    };
    let params = command.get("params").cloned().unwrap_or_else(|| json!({}));
    match method {
      "session.new" => success(
        id,
        &json!({ "sessionId": self.session.session_id, "capabilities": self.session.capabilities }),
      ),
      "session.subscribe" => self.subscribe(client, id, params).await,
      "session.unsubscribe" => self.unsubscribe(client, id, &params).await,
      _ => match self.session.transport.send_raw(method, params).await {
        Ok(Ok(result)) => {
          if let Some(joined) = lock(&self.clients).get_mut(&client) {
            if let Some(added) = addition(method, &result) {
              joined.added.push(added);
            }
            if let Some(removed) = removal(method, &command["params"]) {
              joined.added.retain(|added| *added != removed);
            }
          }
          success(id, &result)
        },
        Ok(Err(refused)) => error(id, &refused.error, &refused.message),
        Err(failed) => error(id, "unknown error", &failed.to_string()),
      },
    }
  }

  async fn subscribe(&self, client: u64, id: &Value, params: Value) -> Message {
    let Some(events) = strings(&params["events"]) else {
      return error(id, "invalid argument", "session.subscribe names no events");
    };
    let contexts = strings(&params["contexts"]).map(|contexts| {
      let tree = lock(&self.tree);
      contexts.iter().map(|context| tree.top(context)).collect()
    });
    let user_contexts = strings(&params["userContexts"]);
    match self.session.transport.send_raw("session.subscribe", params).await {
      Ok(Ok(result)) => {
        let subscription = Subscription {
          id: result["subscription"].as_str().map(str::to_string),
          events,
          contexts,
          user_contexts,
        };
        if let Some(joined) = lock(&self.clients).get_mut(&client) {
          joined.subscriptions.push(subscription);
        }
        success(id, &result)
      },
      Ok(Err(refused)) => error(id, &refused.error, &refused.message),
      Err(failed) => error(id, "unknown error", &failed.to_string()),
    }
  }

  /// Removes the client's own subscriptions: those it names, or those
  /// covering the events it names. A subscription of another client, or
  /// the served browser's own, is never removed.
  async fn unsubscribe(&self, client: u64, id: &Value, params: &Value) -> Message {
    let removed: Vec<Subscription> = {
      let mut clients = lock(&self.clients);
      let Some(joined) = clients.get_mut(&client) else {
        return error(id, "invalid argument", "the client has left");
      };
      let (gone, kept): (Vec<Subscription>, Vec<Subscription>) =
        match (strings(&params["subscriptions"]), strings(&params["events"])) {
          (Some(named), _) => {
            if named
              .iter()
              .any(|name| !joined.subscriptions.iter().any(|own| own.id.as_deref() == Some(name)))
            {
              return error(id, "invalid argument", "no such subscription");
            }
            std::mem::take(&mut joined.subscriptions)
              .into_iter()
              .partition(|own| own.id.as_ref().is_some_and(|own| named.contains(own)))
          },
          (None, Some(events)) => std::mem::take(&mut joined.subscriptions)
            .into_iter()
            .partition(|own| own.events.iter().any(|event| events.contains(event))),
          (None, None) => return error(id, "invalid argument", "session.unsubscribe names nothing"),
        };
      joined.subscriptions = kept;
      gone
    };
    if removed.is_empty() {
      return error(id, "invalid argument", "no such subscription");
    }
    self.forget(&removed).await;
    success(id, &json!({}))
  }

  async fn forget(&self, subscriptions: &[Subscription]) {
    let ids: Vec<&str> = subscriptions.iter().filter_map(|own| own.id.as_deref()).collect();
    if ids.is_empty() {
      return;
    }
    if let Err(failed) = self
      .session
      .transport
      .send(&ferridriver_bidi_proto::session::Unsubscribe {
        events: None,
        subscriptions: Some(&ids),
      })
      .await
    {
      tracing::debug!(target: "ferridriver::browser_server", %failed, "a client's subscriptions outlived it");
    }
  }

  /// Ends what client `id` opened: its subscriptions, its user contexts
  /// (their pages with them), its intercepts, data collectors and preload
  /// scripts.
  async fn leave(&self, id: u64) {
    let (subscriptions, added) = {
      let mut clients = lock(&self.clients);
      let Some(joined) = clients.get_mut(&id) else {
        return;
      };
      (
        std::mem::take(&mut joined.subscriptions),
        std::mem::take(&mut joined.added),
      )
    };
    self.forget(&subscriptions).await;
    for (method, params) in added.into_iter().rev() {
      if let Err(failed) = self.session.transport.send_command(method, params).await {
        tracing::debug!(target: "ferridriver::browser_server", %failed, method, "what a client added outlived it");
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::{Subscription, Tree};
  use serde_json::json;

  fn subscription(events: &[&str], contexts: Option<&[&str]>, user_contexts: Option<&[&str]>) -> Subscription {
    let owned = |items: &[&str]| items.iter().map(|item| (*item).to_string()).collect();
    Subscription {
      id: None,
      events: owned(events),
      contexts: contexts.map(owned),
      user_contexts: user_contexts.map(owned),
    }
  }

  #[test]
  fn what_a_client_adds_is_removed_by_its_own_command() {
    assert_eq!(
      super::addition("network.addIntercept", &json!({ "intercept": "i1" })),
      Some(("network.removeIntercept", json!({ "intercept": "i1" })))
    );
    assert_eq!(
      super::removal("network.removeIntercept", &json!({ "intercept": "i1" })),
      Some(("network.removeIntercept", json!({ "intercept": "i1" })))
    );
    assert_eq!(
      super::addition("browser.createUserContext", &json!({ "userContext": "u1" })),
      Some(("browser.removeUserContext", json!({ "userContext": "u1" })))
    );
    assert_eq!(
      super::addition("browsingContext.create", &json!({ "context": "c1" })),
      None
    );
  }

  #[test]
  fn a_module_names_its_events_and_an_event_itself() {
    let module = subscription(&["browsingContext"], None, None);
    assert!(module.names("browsingContext.load"));
    assert!(!module.names("browsingContextual.load"));
    assert!(!module.names("network.beforeRequestSent"));
    assert!(subscription(&["log.entryAdded"], None, None).names("log.entryAdded"));
  }

  #[test]
  fn a_context_covers_the_frames_inside_it_and_a_user_context_its_pages() {
    let mut tree = Tree::default();
    tree.add(
      &json!({ "context": "tab", "userContext": "acme", "children": [{ "context": "frame", "children": [] }] }),
      None,
    );
    tree.add(&json!({ "context": "other", "userContext": "default" }), None);
    let tab = subscription(&["log"], Some(&["tab"]), None);
    assert!(tree.covers(&tab, &json!({ "source": { "context": "frame" } })));
    assert!(!tree.covers(&tab, &json!({ "context": "other" })));
    assert!(!tree.covers(&tab, &json!({})));
    let acme = subscription(&["network"], None, Some(&["acme"]));
    assert!(tree.covers(&acme, &json!({ "context": "frame" })));
    assert!(!tree.covers(&acme, &json!({ "context": "other" })));
    tree.remove("frame");
    assert!(
      !tree.covers(&acme, &json!({ "context": "frame" })),
      "a frame gone is no one's"
    );
    assert!(tree.covers(&subscription(&["log"], None, None), &json!({})));
  }
}
