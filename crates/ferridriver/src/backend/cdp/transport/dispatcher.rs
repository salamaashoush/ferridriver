//! `CdpDispatcher` message routing, response correlation, and lifecycle dispatch.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use tokio::sync::{broadcast, oneshot};

use crate::backend::json_scan;
use crate::error::{FerriError, Result};

use super::{
  AtomicU64, CdpDispatcher, CdpResult, EVENT_BROADCAST_CAPACITY, EventTap, IframeTargetEvent, IframeTargetSubscription,
  LifecycleTracker, PendingCommand, PendingEntry, RttStats, SessionDirty, global_rtt_stats, is_clean_session_command,
  lock_or_recover, rtt_stats_enabled, send_to_taps, send_to_taps_read, truncate_for_log,
};

impl CdpDispatcher {
  pub(crate) fn new() -> Self {
    let (event_tx, _) = broadcast::channel(EVENT_BROADCAST_CAPACITY);
    Self {
      disconnected: std::sync::atomic::AtomicBool::new(false),
      next_id: AtomicU64::new(1),
      pending: Arc::new(DashMap::default()),
      lifecycle_trackers: Arc::new(DashMap::default()),
      iframe_targets: std::sync::Mutex::default(),
      context_snapshots: std::sync::Mutex::default(),
      event_tx: arc_swap::ArcSwapOption::from(Some(Arc::new(event_tx))),
      method_event_txs: Arc::new(DashMap::default()),
      domain_event_txs: Arc::new(DashMap::default()),
      method_taps: Arc::new(DashMap::default()),
      domain_taps: Arc::new(DashMap::default()),
      wildcard_taps: Arc::new(std::sync::Mutex::new(Vec::new())),
      has_wildcard_taps: std::sync::atomic::AtomicBool::new(false),
      rtt_stats: Arc::new(std::sync::Mutex::new(RttStats::default())),
      session_dirty: std::sync::Mutex::default(),
      permissions_dirty_contexts: std::sync::Mutex::default(),
      permissions_dirty_global: std::sync::atomic::AtomicBool::new(false),
      cookies_dirty_contexts: std::sync::Mutex::default(),
      cookies_dirty_global: std::sync::atomic::AtomicBool::new(false),
    }
  }

  pub(crate) fn take_session_dirty(&self, session_id: &str) -> SessionDirty {
    lock_or_recover(&self.session_dirty)
      .remove(session_id)
      .unwrap_or_default()
  }

  pub(crate) fn reset_session_dirty(&self, session_id: &str) {
    lock_or_recover(&self.session_dirty).remove(session_id);
  }

  fn mark_session_dirty(&self, session_id: &str, pointer: bool, network: bool, session_state: bool) {
    if !pointer && !network && !session_state {
      return;
    }
    let mut map = lock_or_recover(&self.session_dirty);
    if let Some(flags) = map.get_mut(session_id) {
      flags.pointer |= pointer;
      flags.network |= network;
      flags.session_state |= session_state;
    } else {
      map.insert(
        session_id.to_string(),
        SessionDirty {
          pointer,
          session_state,
          network,
        },
      );
    }
  }

  pub(crate) fn mark_pointer_dirty(&self, session_id: &str) {
    self.mark_session_dirty(session_id, true, false, false);
  }

  pub(crate) fn take_permissions_dirty(&self, browser_context_id: &str) -> bool {
    let ctx_dirty = lock_or_recover(&self.permissions_dirty_contexts).remove(browser_context_id);
    ctx_dirty || self.permissions_dirty_global.load(Ordering::Acquire)
  }

  pub(crate) fn take_cookies_dirty(&self, browser_context_id: &str) -> bool {
    let ctx_dirty = lock_or_recover(&self.cookies_dirty_contexts).remove(browser_context_id);
    ctx_dirty || self.cookies_dirty_global.load(Ordering::Acquire)
  }

  pub(crate) fn tap_iframe_targets(&self) -> IframeTargetSubscription {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut targets = lock_or_recover(&self.iframe_targets);
    if self.is_disconnected() {
      let _ = sender.send(IframeTargetEvent::Closed);
    } else {
      for event in &targets.attached {
        let _ = sender.send(IframeTargetEvent::Target(Arc::clone(event)));
      }
      targets.taps.push(sender.clone());
    }
    IframeTargetSubscription {
      events: receiver,
      checkpoints: sender,
    }
  }

  pub(crate) fn tap_event_methods(
    &self,
    methods: &'static [&'static str],
    session_id: Option<&str>,
  ) -> tokio::sync::mpsc::UnboundedReceiver<Arc<serde_json::Value>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    for method in methods {
      self.method_taps.entry(method).or_default().push(EventTap {
        session: session_id.map(str::to_string),
        tx: tx.clone(),
        strict: false,
      });
    }
    if self.is_disconnected() {
      self.method_taps.clear();
    }
    rx
  }

  /// Lossless, wire-ordered tap of EVERY event carried by exactly one
  /// session (strict match — browser-scoped events do not leak into a
  /// page-session stream). Backs the public `CDPSession` event surface.
  pub(crate) fn tap_all_events(
    &self,
    session_id: &str,
  ) -> tokio::sync::mpsc::UnboundedReceiver<Arc<serde_json::Value>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut taps = lock_or_recover(&self.wildcard_taps);
    if !self.is_disconnected() {
      taps.push(EventTap {
        session: Some(session_id.to_string()),
        tx,
        strict: true,
      });
      self.has_wildcard_taps.store(true, Ordering::Release);
    }
    rx
  }

  pub(crate) fn tap_event_domains(
    &self,
    domains: &'static [&'static str],
    session_id: Option<&str>,
  ) -> tokio::sync::mpsc::UnboundedReceiver<Arc<serde_json::Value>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    for domain in domains {
      self.domain_taps.entry(domain).or_default().push(EventTap {
        session: session_id.map(str::to_string),
        tx: tx.clone(),
        strict: false,
      });
    }
    if self.is_disconnected() {
      self.domain_taps.clear();
    }
    rx
  }

  pub(crate) fn register_lifecycle_tracker(
    &self,
    session_id: &str,
    state: Arc<std::sync::Mutex<crate::backend::cdp::LifecycleState>>,
    notify: Arc<tokio::sync::Notify>,
    frame_observer: super::FrameStateObserver,
  ) {
    // Replay and registration share the event lock so live events cannot overtake the snapshot.
    let snapshots = lock_or_recover(&self.context_snapshots);
    if let Some(contexts) = snapshots.0.get(session_id) {
      for context in contexts.values() {
        frame_observer(&context.event, "Runtime.executionContextCreated");
      }
    }
    self.lifecycle_trackers.insert(
      session_id.to_string(),
      LifecycleTracker {
        state,
        notify,
        frame_observer,
      },
    );
    if self.is_disconnected() {
      self.lifecycle_trackers.clear();
    }
  }

  /// Drop the lifecycle tracker and every tap belonging to
  /// `session_id`. See [`CdpTransport::unregister_session`].
  pub(crate) fn unregister_session(&self, session_id: &str) {
    if session_id.is_empty() {
      return;
    }
    self.lifecycle_trackers.remove(session_id);
    lock_or_recover(&self.iframe_targets).forget_session(session_id);
    let drop_session = |taps: &mut Vec<EventTap>| taps.retain(|t| t.session.as_deref() != Some(session_id));
    for mut entry in self.method_taps.iter_mut() {
      drop_session(entry.value_mut());
    }
    for mut entry in self.domain_taps.iter_mut() {
      drop_session(entry.value_mut());
    }
    {
      let mut wildcard = lock_or_recover(&self.wildcard_taps);
      drop_session(&mut wildcard);
      if wildcard.is_empty() {
        self.has_wildcard_taps.store(false, Ordering::Release);
      }
    }
  }

  pub(crate) fn subscribe_events(&self) -> broadcast::Receiver<Arc<serde_json::Value>> {
    self
      .event_tx
      .load()
      .as_ref()
      .map_or_else(|| broadcast::channel(1).1, |sender| sender.subscribe())
  }

  pub(crate) fn subscribe_event_method(&self, method: &'static str) -> broadcast::Receiver<Arc<serde_json::Value>> {
    let receiver = match self.method_event_txs.entry(method) {
      Entry::Occupied(entry) => entry.get().subscribe(),
      Entry::Vacant(entry) => {
        let (tx, rx) = broadcast::channel(EVENT_BROADCAST_CAPACITY);
        entry.insert(tx);
        rx
      },
    };
    if self.is_disconnected() {
      self.method_event_txs.clear();
    }
    receiver
  }

  pub(crate) fn subscribe_event_domain(&self, domain: &'static str) -> broadcast::Receiver<Arc<serde_json::Value>> {
    let receiver = match self.domain_event_txs.entry(domain) {
      Entry::Occupied(entry) => entry.get().subscribe(),
      Entry::Vacant(entry) => {
        let (tx, rx) = broadcast::channel(EVENT_BROADCAST_CAPACITY);
        entry.insert(tx);
        rx
      },
    };
    if self.is_disconnected() {
      self.domain_event_txs.clear();
    }
    receiver
  }

  /// Drain every in-flight `send_command` oneshot and deliver a
  /// `target_closed` error. Called by the reader task on EOF / error
  /// so callers don't block on responses that will never arrive.
  /// Whether the connection to the browser has ended.
  ///
  /// Set once by the reader task on EOF. A browser that crashed or was
  /// OOM-killed leaves its instance entry behind otherwise, and every
  /// later session routed to that name gets `TargetClosed` forever
  /// instead of a fresh browser.
  pub(crate) fn is_disconnected(&self) -> bool {
    self.disconnected.load(std::sync::atomic::Ordering::Acquire)
  }

  pub(crate) fn fail_all_pending(&self, reason: &str) {
    self.disconnected.store(true, std::sync::atomic::Ordering::Release);
    self.event_tx.store(None);
    self.method_event_txs.clear();
    self.domain_event_txs.clear();
    self.method_taps.clear();
    self.domain_taps.clear();
    lock_or_recover(&self.wildcard_taps).clear();
    self.has_wildcard_taps.store(false, Ordering::Release);
    lock_or_recover(&self.context_snapshots).0.clear();
    lock_or_recover(&self.session_dirty).clear();
    lock_or_recover(&self.permissions_dirty_contexts).clear();
    lock_or_recover(&self.cookies_dirty_contexts).clear();
    self.lifecycle_trackers.clear();
    {
      let mut targets = lock_or_recover(&self.iframe_targets);
      targets.attached.clear();
      for tap in &targets.taps {
        let _ = tap.send(IframeTargetEvent::Closed);
      }
      targets.taps.clear();
    }
    // `DashMap::iter_mut` would hold shard locks; collect keys first.
    let keys: Vec<u64> = self.pending.iter().map(|e| *e.key()).collect();
    for id in keys {
      if let Some((_, entry)) = self.pending.remove(&id) {
        let _ = entry.tx.send(Err(FerriError::target_closed(Some(reason.to_string()))));
      }
    }
  }

  pub(crate) fn pending_command(&self, id: u64) -> PendingCommand<'_> {
    PendingCommand {
      dispatcher: self,
      id: Some(id),
    }
  }

  #[cfg(test)]
  pub(crate) fn pending_count(&self) -> usize {
    self.pending.len()
  }

  /// Drop the in-flight entry for `id` after a send failure or response
  /// timeout. Without this the entry lives in the map until transport
  /// teardown — a browser that stops responding but keeps the pipe open
  /// grows the map by one entry per retried command.
  pub(crate) fn forget_pending(&self, id: u64) {
    self.pending.remove(&id);
  }

  fn fail_session_pending(&self, detached_session: &[u8]) {
    if let Ok(session) = std::str::from_utf8(detached_session) {
      lock_or_recover(&self.context_snapshots).0.remove(session);
    }
    let requests: Vec<u64> = self
      .pending
      .iter()
      .filter(|entry| {
        entry
          .session_id
          .as_deref()
          .is_some_and(|id| id.as_bytes() == detached_session)
      })
      .map(|entry| *entry.key())
      .collect();
    for id in requests {
      if let Some((_, entry)) = self.pending.remove(&id) {
        let _ = entry
          .tx
          .send(Err(FerriError::target_closed(Some("CDP session detached".into()))));
      }
    }
  }

  /// Build a strongly typed CDP command directly into the wire buffer without
  /// constructing an intermediate `serde_json::Value` tree.
  pub(crate) fn build_typed_command<P: serde::Serialize>(
    &self,
    session_id: Option<&str>,
    method: &str,
    params: &P,
  ) -> Result<(u64, Vec<u8>, oneshot::Receiver<CdpResult>)> {
    use std::io::Write as _;

    if self.is_disconnected() {
      return Err(FerriError::target_closed(Some(format!(
        "browser connection is closed (sending {method})"
      ))));
    }
    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
    let mut data = Vec::with_capacity(128 + method.len() + session_id.map_or(0, str::len));
    let _ = write!(data, r#"{{"id":{id},"method":"{method}","params":"#);
    let params_start = data.len();
    serde_json::to_writer(&mut data, params).map_err(|e| FerriError::Backend(format!("Serialize: {e}")))?;
    let params_end = data.len();
    let params_bytes = &data[params_start..params_end];

    self.record_dirty_from_raw(session_id, method, params_bytes);

    if let Some(sid) = session_id {
      let _ = write!(data, r#","sessionId":"{sid}"}}"#);
    } else {
      data.push(b'}');
    }
    data.push(0);

    if tracing::enabled!(target: "ferridriver::cdp::send", tracing::Level::DEBUG) {
      let params_str = std::str::from_utf8(&data[params_start..params_end]).unwrap_or("");
      tracing::debug!(
        target: "ferridriver::cdp::send",
        id,
        method,
        params = truncate_for_log(params_str, 200),
        "CDP >>",
      );
    }

    let rx = self.register_command(id, session_id, method)?;
    Ok((id, data, rx))
  }

  fn record_dirty_from_raw(&self, session_id: Option<&str>, method: &str, params_bytes: &[u8]) {
    if method == "Browser.grantPermissions" {
      let ctx = json_scan::json_string(json_scan::json_field(params_bytes, b"browserContextId"));
      if let Ok(ctx_id) = std::str::from_utf8(ctx)
        && !ctx_id.is_empty()
      {
        lock_or_recover(&self.permissions_dirty_contexts).insert(ctx_id.to_string());
      } else {
        self.permissions_dirty_global.store(true, Ordering::Release);
      }
    } else if method == "Storage.setCookies" {
      let ctx = json_scan::json_string(json_scan::json_field(params_bytes, b"browserContextId"));
      if let Ok(ctx_id) = std::str::from_utf8(ctx)
        && !ctx_id.is_empty()
      {
        lock_or_recover(&self.cookies_dirty_contexts).insert(ctx_id.to_string());
      } else {
        self.cookies_dirty_global.store(true, Ordering::Release);
      }
    }
    if let Some(sid) = session_id {
      let pointer = method == "Input.dispatchTouchEvent"
        || (method == "Input.dispatchMouseEvent"
          && !(json_scan::json_string(json_scan::json_field(params_bytes, b"type")) == b"mouseMoved"
            && json_scan::json_field(params_bytes, b"x") == b"-1"
            && json_scan::json_field(params_bytes, b"y") == b"-1"));
      let network = matches!(
        method,
        "Network.setCookie" | "Network.setCookies" | "Storage.setCookies" | "Page.reload"
      ) || (method == "Page.navigate"
        && json_scan::json_string(json_scan::json_field(params_bytes, b"url")) != b"about:blank");
      let session_state = !is_clean_session_command(method);
      self.mark_session_dirty(sid, pointer, network, session_state);
    }
  }

  /// Build a CDP command as NUL-terminated JSON bytes and register a response receiver.
  ///
  /// Fails immediately once the transport is known to be dead. Without
  /// that check a command issued after the browser exited is queued to a
  /// writer task that has already stopped reading, and the caller waits
  /// the full 30s response timeout for an answer that can never come —
  /// which is exactly what every call did after an external browser kill.
  pub(crate) fn build_command(
    &self,
    session_id: Option<&str>,
    method: &str,
    params: &serde_json::Value,
  ) -> Result<(u64, Vec<u8>, oneshot::Receiver<CdpResult>)> {
    use std::io::Write as _;

    if self.is_disconnected() {
      return Err(FerriError::target_closed(Some(format!(
        "browser connection is closed (sending {method})"
      ))));
    }
    if method == "Browser.grantPermissions" {
      if let Some(ctx_id) = params.get("browserContextId").and_then(serde_json::Value::as_str) {
        lock_or_recover(&self.permissions_dirty_contexts).insert(ctx_id.to_string());
      } else {
        self.permissions_dirty_global.store(true, Ordering::Release);
      }
    } else if method == "Storage.setCookies" {
      if let Some(ctx_id) = params.get("browserContextId").and_then(serde_json::Value::as_str) {
        lock_or_recover(&self.cookies_dirty_contexts).insert(ctx_id.to_string());
      } else {
        self.cookies_dirty_global.store(true, Ordering::Release);
      }
    }
    if let Some(sid) = session_id {
      let pointer = method == "Input.dispatchTouchEvent"
        || (method == "Input.dispatchMouseEvent"
          && !(params.get("type").and_then(serde_json::Value::as_str) == Some("mouseMoved")
            && params.get("x").and_then(serde_json::Value::as_i64) == Some(-1)
            && params.get("y").and_then(serde_json::Value::as_i64) == Some(-1)));
      let network = matches!(
        method,
        "Network.setCookie" | "Network.setCookies" | "Storage.setCookies" | "Page.reload"
      ) || (method == "Page.navigate"
        && params.get("url").and_then(serde_json::Value::as_str) != Some("about:blank"));
      let session_state = !is_clean_session_command(method);
      self.mark_session_dirty(sid, pointer, network, session_state);
    }
    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
    let mut data = Vec::with_capacity(128 + method.len() + session_id.map_or(0, str::len));
    let _ = write!(data, r#"{{"id":{id},"method":"{method}","params":"#);
    let params_start = data.len();
    serde_json::to_writer(&mut data, params).map_err(|e| FerriError::Backend(format!("Serialize: {e}")))?;
    let params_end = data.len();
    if let Some(sid) = session_id {
      let _ = write!(data, r#","sessionId":"{sid}"}}"#);
    } else {
      data.push(b'}');
    }
    data.push(0);

    if tracing::enabled!(target: "ferridriver::cdp::send", tracing::Level::DEBUG) {
      let params_str = std::str::from_utf8(&data[params_start..params_end]).unwrap_or("");
      tracing::debug!(
        target: "ferridriver::cdp::send",
        id,
        method,
        params = truncate_for_log(params_str, 200),
        "CDP >>",
      );
    }

    let rx = self.register_command(id, session_id, method)?;
    Ok((id, data, rx))
  }

  /// Build a CDP command from an already-serialized JSON `params` string.
  ///
  /// Allocates the exact wire frame once and copies `raw_params_json` directly
  /// without walking or re-escaping a `serde_json::Value` tree.
  pub(crate) fn build_command_raw(
    &self,
    session_id: Option<&str>,
    method: &str,
    raw_params_json: &str,
  ) -> Result<(u64, Vec<u8>, oneshot::Receiver<CdpResult>)> {
    use std::io::Write as _;

    if self.is_disconnected() {
      return Err(FerriError::target_closed(Some(format!(
        "browser connection is closed (sending {method})"
      ))));
    }
    if let Some(sid) = session_id {
      let pointer = matches!(method, "Input.dispatchTouchEvent" | "Input.dispatchMouseEvent");
      let network = matches!(
        method,
        "Network.setCookie" | "Network.setCookies" | "Storage.setCookies" | "Page.reload" | "Page.navigate"
      );
      let session_state = !is_clean_session_command(method);
      self.mark_session_dirty(sid, pointer, network, session_state);
    }
    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
    let mut data = Vec::with_capacity(64 + method.len() + raw_params_json.len() + session_id.map_or(0, str::len));
    let _ = write!(data, r#"{{"id":{id},"method":"{method}","params":"#);
    data.extend_from_slice(raw_params_json.as_bytes());
    if let Some(sid) = session_id {
      let _ = write!(data, r#","sessionId":"{sid}"}}"#);
    } else {
      data.push(b'}');
    }
    data.push(0);

    if tracing::enabled!(target: "ferridriver::cdp::send", tracing::Level::DEBUG) {
      tracing::debug!(
        target: "ferridriver::cdp::send",
        id,
        method,
        params = truncate_for_log(raw_params_json, 200),
        "CDP >>",
      );
    }

    let rx = self.register_command(id, session_id, method)?;
    Ok((id, data, rx))
  }

  pub(super) fn register_command(
    &self,
    id: u64,
    session_id: Option<&str>,
    method: &str,
  ) -> Result<oneshot::Receiver<CdpResult>> {
    let (tx, rx) = oneshot::channel();
    let stats_enabled = rtt_stats_enabled();
    let entry = PendingEntry {
      tx,
      session_id: session_id.map(Into::into),
      // Allocate the method String only when stats are enabled —
      // saves the per-command alloc when stats are off (the common
      // path).
      method: if stats_enabled {
        method.to_string()
      } else {
        String::new()
      },
      send_at: stats_enabled.then(Instant::now),
    };
    self.pending.insert(id, entry);
    if self.is_disconnected() {
      self.pending.remove(&id);
      return Err(FerriError::target_closed(Some(format!(
        "browser connection closed while registering {method}"
      ))));
    }
    Ok(rx)
  }

  /// Dispatch a raw CDP message (response or event). Called by the reader task.
  pub(crate) fn dispatch_message(&self, raw: &[u8]) {
    if self.is_disconnected() {
      return;
    }
    let env = json_scan::scan_cdp_envelope(raw);
    let id = env.id;

    if id > 0 {
      // Response
      let payload = if env.error.is_empty() {
        if env.result.is_empty() {
          Ok(serde_json::Value::Object(serde_json::Map::new()))
        } else {
          let val: serde_json::Value =
            serde_json::from_slice(env.result).unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
          Ok(val)
        }
      } else {
        let msg_bytes = json_scan::error_message(env.error);
        let msg_str = std::str::from_utf8(msg_bytes).unwrap_or("CDP error");
        if msg_str == "Session with given id not found." {
          Err(FerriError::target_closed(Some(msg_str.to_owned())))
        } else {
          Err(FerriError::protocol("CDP", msg_str))
        }
      };
      if tracing::enabled!(target: "ferridriver::cdp::recv", tracing::Level::DEBUG) {
        tracing::debug!(
          target: "ferridriver::cdp::recv",
          id,
          ok = payload.is_ok(),
          payload = truncate_for_log(&format!("{payload:?}"), 200),
          "CDP << response",
        );
      }
      if let Some((_, entry)) = self.pending.remove(&id) {
        if rtt_stats_enabled()
          && let Some(send_at) = entry.send_at
        {
          let elapsed = send_at.elapsed().as_nanos();
          lock_or_recover(&self.rtt_stats).record(&entry.method, elapsed);
          lock_or_recover(global_rtt_stats()).record(&entry.method, elapsed);
        }
        let _ = entry.tx.send(payload);
      }
    } else {
      self.dispatch_event(raw, &env);
    }
  }

  fn dispatch_event(&self, raw: &[u8], env: &json_scan::CdpEnvelope<'_>) {
    // Event
    let method = env.method;
    let session_id = env.session_id;
    let method_str = std::str::from_utf8(method).unwrap_or("");
    let sid_str = std::str::from_utf8(session_id).unwrap_or("");

    if !sid_str.is_empty() {
      if method_str == "Network.requestWillBeSent" {
        self.mark_session_dirty(sid_str, false, true, false);
      } else if method_str == "Target.attachedToTarget" {
        // Out-of-process iframe or child target attached under this session.
        self.mark_session_dirty(sid_str, false, false, true);
      }
    }

    let detached_session = match method_str {
      "Inspector.detached" => session_id,
      "Target.detachedFromTarget" => json_scan::json_string(json_scan::json_field(env.params, b"sessionId")),
      _ => b"",
    };
    if !detached_session.is_empty() {
      self.fail_session_pending(detached_session);
    }

    if matches!(
      method_str,
      "Runtime.executionContextCreated"
        | "Runtime.executionContextDestroyed"
        | "Runtime.executionContextsCleared"
        | "Runtime.bindingCalled"
        | "Page.frameDetached"
    ) {
      let mut snapshots = lock_or_recover(&self.context_snapshots);
      if method_str != "Runtime.bindingCalled" {
        snapshots.observe(sid_str, method_str, raw);
      }
      self.dispatch_lifecycle(raw, env.params, method_str, sid_str);
    } else {
      self.dispatch_lifecycle(raw, env.params, method_str, sid_str);
    }

    tracing::trace!(
      target: "ferridriver::cdp::recv",
      method = method_str,
      "CDP << event",
    );

    self.fanout_event(raw, method_str, sid_str);
  }

  fn fanout_event(&self, raw: &[u8], method_str: &str, sid_str: &str) {
    let domain = method_str.split_once('.').map(|(domain, _)| domain);
    let method_tx = self.method_event_txs.get(method_str).map(|entry| entry.clone());
    let domain_tx = domain.and_then(|d| self.domain_event_txs.get(d).map(|entry| entry.clone()));
    let global = self.event_tx.load();
    let needs_global = global.as_ref().is_some_and(|sender| sender.receiver_count() > 0);
    let needs_method = method_tx.as_ref().is_some_and(|tx| tx.receiver_count() > 0);
    let needs_domain = domain_tx.as_ref().is_some_and(|tx| tx.receiver_count() > 0);
    let has_method_taps = self
      .method_taps
      .get(method_str)
      .is_some_and(|taps| taps.iter().any(|tap| tap.matches_session(sid_str)));
    let has_domain_taps = domain.is_some_and(|d| {
      self
        .domain_taps
        .get(d)
        .is_some_and(|taps| taps.iter().any(|tap| tap.matches_session(sid_str)))
    });
    let has_wildcard_taps = self.has_wildcard_taps.load(Ordering::Acquire)
      && lock_or_recover(&self.wildcard_taps)
        .iter()
        .any(|tap| tap.matches_session(sid_str));
    let target_event = matches!(method_str, "Target.attachedToTarget" | "Target.detachedFromTarget");

    if (target_event
      || needs_global
      || needs_method
      || needs_domain
      || has_method_taps
      || has_domain_taps
      || has_wildcard_taps)
      && let Ok(msg) = serde_json::from_slice::<serde_json::Value>(raw)
    {
      let msg = Arc::new(msg);
      if target_event {
        lock_or_recover(&self.iframe_targets).dispatch(&msg);
      }
      // Taps first: state trackers must see the event before any
      // best-effort broadcast consumer can react to it. Deliver under a
      // shared shard read lock and only take an exclusive write lock to
      // prune if a receiver was dropped.
      if has_method_taps {
        Self::deliver_to_taps(&self.method_taps, method_str, sid_str, &msg);
      }
      if has_domain_taps && let Some(d) = domain {
        Self::deliver_to_taps(&self.domain_taps, d, sid_str, &msg);
      }
      if has_wildcard_taps {
        let mut wildcard = lock_or_recover(&self.wildcard_taps);
        send_to_taps(&mut wildcard, sid_str, &msg);
        if wildcard.is_empty() {
          self.has_wildcard_taps.store(false, Ordering::Release);
        }
      }
      if needs_global && let Some(sender) = global.as_ref() {
        let _ = sender.send(Arc::clone(&msg));
      }
      if needs_method && let Some(tx) = method_tx {
        let _ = tx.send(Arc::clone(&msg));
      }
      if needs_domain && let Some(tx) = domain_tx {
        let _ = tx.send(msg);
      }
    }
  }

  fn deliver_to_taps(
    taps_map: &DashMap<&'static str, Vec<EventTap>>,
    key: &str,
    sid_str: &str,
    msg: &Arc<serde_json::Value>,
  ) {
    let prune = taps_map
      .get(key)
      .is_some_and(|taps| send_to_taps_read(&taps, sid_str, msg));
    if prune && let Some(mut taps) = taps_map.get_mut(key) {
      taps.retain(|tap| !tap.tx.is_closed());
    }
  }

  /// Lifecycle tracker dispatch -- tracks `loaderId` for document-accurate
  /// lifecycle. Only main-frame events update the tracker: a subframe's
  /// `Page.frameNavigated` carries a `parentId` (mirrors Playwright's
  /// `_eventBelongsToStaleFrame` filter — main-frame and subframe nav
  /// states have independent lifecycle in
  /// `/tmp/playwright/packages/playwright-core/src/server/frames.ts`),
  /// and a subframe's `Page.lifecycleEvent` carries a `loaderId` that
  /// will not match the main frame's `current_loader_id`.
  ///
  /// `Inspector.targetCrashed` sets `crashed` and wakes waiters so
  /// goto/reload return immediately instead of stalling until timeout.
  fn dispatch_lifecycle(&self, raw: &[u8], params: &[u8], method_str: &str, key: &str) {
    if let Some(tracker) = self.lifecycle_trackers.get(key) {
      (tracker.frame_observer)(raw, method_str);
      match method_str {
        "Page.frameNavigated" => {
          let frame = json_scan::json_field(params, b"frame");
          let parent_id = json_scan::json_field(frame, b"parentId");
          if !parent_id.is_empty() {
            // Subframe commit — leave main-frame lifecycle untouched.
            return;
          }
          let loader_id = json_scan::json_string(json_scan::json_field(frame, b"loaderId"));
          // Allocate the owned loaderId string BEFORE taking the lock so the
          // critical section holds no heap work — only the two field stores.
          let loader_id_owned = std::str::from_utf8(loader_id).unwrap_or("").to_string();
          {
            let mut state = lock_or_recover(&tracker.state);
            state.current_loader_id = loader_id_owned;
            state.fired = crate::backend::cdp::LC_COMMIT;
            state.nav_committed_seq = state.nav_committed_seq.wrapping_add(1);
          }
          tracker.notify.notify_waiters();
        },
        "Page.navigatedWithinDocument" => {
          let frame_id = json_scan::json_string(json_scan::json_field(params, b"frameId"));
          let frame_id_owned = std::str::from_utf8(frame_id).unwrap_or("").to_string();
          {
            let mut state = lock_or_recover(&tracker.state);
            state.same_document_seq = state.same_document_seq.wrapping_add(1);
            state.same_document_frame = frame_id_owned;
          }
          tracker.notify.notify_waiters();
        },
        "Page.frameStartedNavigating" => {
          // A navigation began (any frame). Bump the generation counter so a
          // post-input settle knows the action created a navigation and must
          // wait for it to commit. Main-frame filtering is done by the waiter
          // (it waits for the main-frame loader to change, bounded), so a
          // subframe nav here costs at most the bounded settle, never a hang.
          {
            let mut state = lock_or_recover(&tracker.state);
            state.nav_started_seq = state.nav_started_seq.wrapping_add(1);
          }
          tracker.notify.notify_waiters();
        },
        "Page.lifecycleEvent" => {
          let loader_id = json_scan::json_string(json_scan::json_field(params, b"loaderId"));
          let loader_id_str = std::str::from_utf8(loader_id).unwrap_or("");
          let name = json_scan::json_string(json_scan::json_field(params, b"name"));
          let name_str = std::str::from_utf8(name).unwrap_or("");
          let event_name = match name_str {
            "DOMContentLoaded" => Some(crate::backend::cdp::LC_DOMCONTENTLOADED),
            "load" => Some(crate::backend::cdp::LC_LOAD),
            _ => None,
          };
          if let Some(event_flag) = event_name {
            // Strict loaderId match. A subframe lifecycle event carries
            // the subframe's loaderId, which never matches the main
            // frame's `current_loader_id`. The previous "or is_empty()"
            // relaxation was a workaround for the initial-nav case
            // where `current_loader_id` is unset before the first
            // `Page.frameNavigated` lands; we drop it because the
            // wrapper now seeds `current_loader_id` from the
            // `Page.navigate` response (see
            // `crates/ferridriver/src/backend/cdp/mod.rs::CdpPage::goto`)
            // before awaiting any lifecycle event.
            //
            // The critical section holds only the compare-and-set; the
            // `loader_id_str` borrows the incoming buffer (no allocation),
            // and `notify_waiters` runs after the guard drops.
            let matched = {
              let mut state = lock_or_recover(&tracker.state);
              if state.current_loader_id == loader_id_str {
                state.fired |= event_flag;
                true
              } else {
                false
              }
            };
            if matched {
              tracker.notify.notify_waiters();
            }
          }
        },
        "Inspector.targetCrashed" => {
          let mut state = lock_or_recover(&tracker.state);
          state.crashed = true;
          drop(state);
          tracker.notify.notify_waiters();
        },
        _ => {},
      }
    }
  }
}

pub(super) fn navigated_frame(frame: &serde_json::Value) -> crate::backend::FrameInfo {
  let text = |key: &str| frame.get(key).and_then(serde_json::Value::as_str).unwrap_or("");
  let base_url = text("url");
  let fragment = text("urlFragment");
  let url = if fragment.is_empty() {
    base_url.to_string()
  } else {
    format!("{base_url}{fragment}")
  };
  crate::backend::FrameInfo {
    frame_id: text("id").to_string(),
    parent_frame_id: frame
      .get("parentId")
      .and_then(serde_json::Value::as_str)
      .map(str::to_owned),
    name: text("name").to_string(),
    url,
  }
}

pub(super) fn frame_event(raw: &[u8], method: &str) -> Option<crate::events::PageEvent> {
  use crate::events::PageEvent;
  if !matches!(
    method,
    "Page.frameAttached" | "Page.frameDetached" | "Page.frameNavigated" | "Page.navigatedWithinDocument"
  ) {
    return None;
  }
  let params: serde_json::Value = serde_json::from_slice(json_scan::json_field(raw, b"params")).ok()?;
  let text = |key: &str| {
    params
      .get(key)
      .and_then(serde_json::Value::as_str)
      .unwrap_or("")
      .to_string()
  };
  match method {
    "Page.frameAttached" => Some(PageEvent::FrameAttached(crate::backend::FrameInfo {
      frame_id: text("frameId"),
      parent_frame_id: params
        .get("parentFrameId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned),
      name: String::new(),
      url: String::new(),
    })),
    "Page.frameDetached" => Some(PageEvent::FrameDetached {
      frame_id: text("frameId"),
    }),
    "Page.frameNavigated" => Some(PageEvent::FrameNavigated(navigated_frame(params.get("frame")?))),
    "Page.navigatedWithinDocument" => Some(PageEvent::FrameNavigatedWithinDocument(crate::backend::FrameInfo {
      frame_id: text("frameId"),
      parent_frame_id: None,
      name: String::new(),
      url: text("url"),
    })),
    _ => None,
  }
}

impl Drop for CdpDispatcher {
  fn drop(&mut self) {
    // Dump per-method RTT stats on transport teardown when stats
    // collection is enabled. Catches clean shutdowns where the
    // dispatcher Arc reaches zero before process exit. NAPI / cargo
    // test paths typically exit via `process::exit()` while reader /
    // writer tokio tasks still hold dispatcher clones — for those,
    // [`global_rtt_stats`] aggregates per-dispatcher buckets and is
    // dumped via the libc atexit hook registered in
    // [`rtt_stats_enabled`].
    if rtt_stats_enabled() {
      let local = lock_or_recover(&self.rtt_stats);
      local.dump();
    }
  }
}
