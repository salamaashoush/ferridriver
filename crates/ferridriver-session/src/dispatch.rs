//! The bridge between command verbs and live browser state.
//!
//! The session crate owns the wire and the registry but knows nothing about
//! how to drive a browser — that lives in `ferridriver` core and the host
//! that holds the bound [`ferridriver::Browser`]. A host implements
//! [`Dispatcher`] to map an incoming [`Command`] onto its browser, and the
//! [`crate::SessionServer`] calls it for every frame.
//!
//! Keeping this a trait is what lets the `run_script` verb — which needs the
//! `QuickJS` engine in `ferridriver-script` — be supplied by a higher crate
//! without `ferridriver-session` ever depending on the engine.

use async_trait::async_trait;

use crate::protocol::{Command, Response};

/// Maps session commands onto a live browser.
///
/// Implementations are shared across all client connections to one bound
/// browser, so `&self` methods must be safe under concurrent calls. The
/// server serializes nothing on the dispatcher's behalf; an implementation
/// that needs per-context exclusivity takes its own locks (as the MCP server
/// already does via its context guards).
#[async_trait]
pub trait Dispatcher: Send + Sync + 'static {
  /// Handle one command and produce its response.
  ///
  /// Implementations should map a domain failure to [`Response::err`] with
  /// the same `id`, not return a transport-level error — a failed verb is a
  /// normal response the client renders, not a dropped connection.
  async fn dispatch(&self, command: Command) -> Response;

  /// The list of verbs this dispatcher understands, for `help` / discovery.
  /// Default empty; hosts override to advertise their surface.
  fn verbs(&self) -> Vec<&'static str> {
    Vec::new()
  }
}

#[cfg(test)]
pub(crate) mod test_support {
  use super::*;

  /// A dispatcher used by server/client tests: echoes the verb and args back
  /// as text, and fails the reserved verb `boom`.
  pub struct EchoDispatcher;

  #[async_trait]
  impl Dispatcher for EchoDispatcher {
    async fn dispatch(&self, command: Command) -> Response {
      if command.verb == "boom" {
        return Response::err(command.id, "explosion");
      }
      let ctx = command.context.as_deref().unwrap_or("default");
      Response::ok(command.id, format!("{}@{}:{}", command.verb, ctx, command.args))
    }

    fn verbs(&self) -> Vec<&'static str> {
      vec!["echo", "boom"]
    }
  }
}
