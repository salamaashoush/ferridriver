//! The bridge between commands and live browser state.
//!
//! The session crate owns the wire and the registry but knows nothing about
//! how to drive a browser — that lives in `ferridriver` core and the host
//! that holds the bound [`ferridriver::Browser`]. A host implements
//! [`Dispatcher`] to map an incoming [`Command`] onto its browser, and the
//! [`crate::SessionServer`] calls it for every frame.
//!
//! Keeping this a trait is what lets the `run` verb — which needs the
//! `QuickJS` engine in `ferridriver-script` — be supplied by a higher crate
//! without `ferridriver-session` ever depending on the engine.

use async_trait::async_trait;
use tokio::sync::mpsc::UnboundedSender;

use crate::protocol::{ActionPhase, Command, Event, EventPayload, Response, ScriptRequest};

/// The phase-specific half of an [`EventPayload::Action`], so emitting one
/// stays a single call instead of a six-argument one.
#[derive(Debug, Default)]
pub struct ActionDetail {
  pub params: Option<serde_json::Value>,
  pub duration_ms: Option<u64>,
  pub error: Option<String>,
  pub message: Option<String>,
}

/// The tallies half of an [`EventPayload::Page`].
#[derive(Debug, Default, Clone, Copy)]
pub struct PageCounts {
  pub console_errors: usize,
  pub console_warnings: usize,
  pub page_errors: usize,
}

/// Where a running command sends its out-of-band events.
///
/// Each command gets its own sink with the command's id already baked in, so
/// a dispatcher can never mislabel an event. A sink built with
/// [`EventSink::discard`] drops everything, which is what an in-process caller
/// with no connection to write to wants.
#[derive(Clone, Debug)]
pub struct EventSink {
  id: u64,
  tx: Option<UnboundedSender<Event>>,
}

impl EventSink {
  /// A sink that forwards events for command `id` to `tx`.
  #[must_use]
  pub fn new(id: u64, tx: UnboundedSender<Event>) -> Self {
    Self { id, tx: Some(tx) }
  }

  /// A sink that drops every event.
  #[must_use]
  pub fn discard() -> Self {
    Self { id: 0, tx: None }
  }

  /// Emit one event. Silent when the receiving connection has already gone —
  /// a client that hung up mid-run must not fail the run.
  pub fn send(&self, payload: EventPayload) {
    if let Some(tx) = &self.tx {
      let _ = tx.send(Event { id: self.id, payload });
    }
  }

  /// Emit one console line.
  pub fn console(&self, level: impl Into<String>, message: impl Into<String>, ts_ms: u64) {
    self.send(EventPayload::Console {
      level: level.into(),
      message: message.into(),
      ts_ms,
    });
  }

  /// Emit one line of generated source.
  pub fn code(&self, line: impl Into<String>) {
    self.send(EventPayload::Code { line: line.into() });
  }

  /// Report the page the run finished on.
  pub fn page(&self, url: impl Into<String>, title: impl Into<String>, counts: PageCounts) {
    self.send(EventPayload::Page {
      url: url.into(),
      title: title.into(),
      console_errors: counts.console_errors,
      console_warnings: counts.console_warnings,
      page_errors: counts.page_errors,
    });
  }

  /// Emit one action edge. `params` / `duration_ms` / `error` / `message` are
  /// the fields that phase carries; the rest stay absent on the wire.
  pub fn action(&self, phase: ActionPhase, call_id: &str, title: &str, detail: ActionDetail) {
    self.send(EventPayload::Action {
      phase,
      call_id: call_id.to_string(),
      title: title.to_string(),
      params: detail.params,
      duration_ms: detail.duration_ms,
      error: detail.error,
      message: detail.message,
    });
  }
}

/// Maps session commands onto a live browser.
///
/// Implementations are shared across all client connections to one bound
/// browser, so `&self` methods must be safe under concurrent calls. The
/// server serializes nothing on the dispatcher's behalf; an implementation
/// that needs per-context exclusivity takes its own locks (as the MCP server
/// already does via its context guards).
#[async_trait]
pub trait Dispatcher: Send + Sync + 'static {
  /// Handle one command and produce its response, emitting any progress into
  /// `events` as it goes.
  ///
  /// Implementations should map a domain failure to [`Response::err`] with
  /// the same `id`, not return a transport-level error — a failed command is a
  /// normal response the client renders, not a dropped connection.
  async fn dispatch(&self, command: Command, events: EventSink) -> Response;

  /// The list of verbs this dispatcher understands, for `help` / discovery.
  /// Default empty; hosts override to advertise their surface.
  fn verbs(&self) -> Vec<&'static str> {
    Vec::new()
  }
}

/// Runs scripts against a bound browser, supplied by a higher crate that owns
/// the `QuickJS` engine (`ferridriver-script`).
///
/// The session crate stays below the scripting layer, so it cannot run JS
/// itself. A bound browser without a host has no usable surface at all — the
/// `run` verb is the whole protocol — so every bind path installs one.
#[async_trait]
pub trait ScriptHost: Send + Sync + 'static {
  /// Run `request` against the named `context` of the bound browser,
  /// streaming console output into `events` as it happens.
  ///
  /// `Ok(value)` is the serialized script result — including a script-level
  /// error, which is a *result* the client renders with its console, not a
  /// failed command. `Err(msg)` is reserved for failures to run at all
  /// (a malformed request, a context that cannot be opened).
  async fn run(
    &self,
    context: &str,
    request: ScriptRequest,
    events: EventSink,
  ) -> std::result::Result<serde_json::Value, String>;
}

#[cfg(test)]
pub(crate) mod test_support {
  use super::*;

  /// A dispatcher used by server/client tests: echoes the verb and args back
  /// as text, fails the reserved verb `boom`, and emits console events for
  /// the reserved verb `chatty` before answering.
  pub struct EchoDispatcher;

  #[async_trait]
  impl Dispatcher for EchoDispatcher {
    async fn dispatch(&self, command: Command, events: EventSink) -> Response {
      if command.verb == "boom" {
        return Response::err(command.id, "explosion");
      }
      if command.verb == "chatty" {
        events.console("log", "first", 1);
        events.console("error", "second", 2);
      }
      let ctx = command.context.as_deref().unwrap_or("default");
      Response::ok(command.id, format!("{}@{}:{}", command.verb, ctx, command.args))
    }

    fn verbs(&self) -> Vec<&'static str> {
      vec!["echo", "boom", "chatty"]
    }
  }
}
