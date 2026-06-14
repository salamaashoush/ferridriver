//! Named browser sessions for ferridriver.
//!
//! A bound browser is published under a human-readable session id so a
//! separate process (the `ferridriver` CLI, another agent) can reattach to
//! the same live browser and drive it. This is ferridriver's native answer
//! to Playwright's `Browser.bind` / `playwright-cli attach` flow — the API
//! surface matches, the mechanism does not.
//!
//! Three pieces compose the feature:
//! - [`registry`] persists a small descriptor per session under the user
//!   cache dir, so any process can discover live sessions by id.
//! - [`protocol`] + [`transport`] define the NUL-delimited-JSON command wire
//!   spoken over a Unix-domain socket (named pipe on Windows).
//! - [`server`] accepts client connections and routes each command frame
//!   through a [`Dispatcher`]; [`client`] is the matching call/response side.
//!
//! The crate depends only on `ferridriver` core, so it sits below the
//! scripting and binding layers without a cycle. The host (`ferridriver-mcp`,
//! the CLI, the NAPI/QuickJS bindings) supplies a [`Dispatcher`] that maps
//! command verbs onto live browser state.

pub mod bind;
pub mod browser_dispatch;
pub mod client;
pub mod dispatch;
pub mod protocol;
pub mod registry;
pub mod server;
pub mod transport;

pub use bind::{BindOptions, BoundSession, bind, bind_dispatcher, bind_in, unbind_id};
pub use browser_dispatch::{BROWSER_VERBS, BrowserDispatcher, browser_name_for, dispatcher_for, parse_session_key};
pub use client::SessionClient;
pub use dispatch::{Dispatcher, ScriptHook};
pub use protocol::{Command, Response};
pub use registry::{Registry, SessionDescriptor};
pub use server::{Endpoint, SessionServer};

use thiserror::Error;

/// Errors surfaced by the session layer.
#[derive(Debug, Error)]
pub enum SessionError {
  /// The session id was not found in the registry.
  #[error("no session named '{0}'")]
  NotFound(String),
  /// The registry entry exists but its endpoint refused a connection.
  #[error("session '{0}' is not reachable (stale or shutting down)")]
  Unreachable(String),
  /// A command verb the dispatcher does not implement.
  #[error("unknown command verb: '{0}'")]
  UnknownVerb(String),
  /// The peer closed the connection before sending a complete response.
  #[error("connection closed by peer")]
  ConnectionClosed,
  /// The dispatcher returned a domain error for a command.
  #[error("{0}")]
  Dispatch(String),
  /// Filesystem / IO failure (registry read/write, socket bind/connect).
  #[error("io: {0}")]
  Io(#[from] std::io::Error),
  /// JSON encode/decode failure on the wire or in the registry file.
  #[error("json: {0}")]
  Json(#[from] serde_json::Error),
}

/// Convenience result alias for the crate.
pub type Result<T> = std::result::Result<T, SessionError>;
