//! `bind` / `unbind`: publish a live browser under a session id and tear it
//! down.
//!
//! [`bind`] starts a [`crate::SessionServer`] over the bound browser, writes a
//! registry descriptor so other processes can find it, and returns a
//! [`BoundSession`] handle. Dropping the handle (or calling
//! [`BoundSession::unbind`]) stops the server and removes the descriptor.
//!
//! This is ferridriver's mechanism behind the Playwright-parity
//! `Browser.bind(title, options)` / `Browser.unbind()` surface exposed by the
//! `NAPI` and `QuickJS` bindings — they call straight into here.

use std::sync::Arc;

use ferridriver::Browser;

use crate::Result;
use crate::browser_dispatch::{BrowserDispatcher, browser_name_for, dispatcher_for};
use crate::dispatch::ScriptHook;
use crate::registry::{Registry, SessionDescriptor};
use crate::server::{Endpoint, SessionServer};

/// Options for [`bind`], mirroring Playwright's `Browser.bind` option bag.
#[derive(Debug, Clone, Default)]
pub struct BindOptions {
  /// Working directory associated with the session (for dashboards that group
  /// by project). Maps to Playwright's `workspaceDir`.
  pub workspace_dir: Option<String>,
  /// Arbitrary caller metadata echoed back by `list`.
  pub metadata: Option<serde_json::Value>,
  /// Bind over TCP at this host instead of a Unix socket. When either `host`
  /// or `port` is set, the endpoint is a `ws://` TCP address; otherwise a
  /// Unix-domain socket under the registry directory is used.
  pub host: Option<String>,
  /// TCP port for the `host` bind path. `0` (or unset with `host` present)
  /// lets the OS choose a free port.
  pub port: Option<u16>,
}

/// A live binding: the running server, its registry entry, and enough to tear
/// both down.
pub struct BoundSession {
  id: String,
  endpoint: String,
  registry: Registry,
  server: Arc<SessionServer>,
  serve_task: tokio::task::JoinHandle<()>,
}

impl BoundSession {
  /// The session id this binding was published under.
  #[must_use]
  pub fn id(&self) -> &str {
    &self.id
  }

  /// The resolved endpoint clients connect to (socket path or `ws://` URL).
  #[must_use]
  pub fn endpoint(&self) -> &str {
    &self.endpoint
  }

  /// The running server backing this session. Its listener stays open until
  /// the binding is dropped.
  #[must_use]
  pub fn server(&self) -> &Arc<SessionServer> {
    &self.server
  }

  /// Stop serving and remove the registry descriptor. Idempotent — a second
  /// call (or the `Drop` impl) is a no-op.
  ///
  /// # Errors
  ///
  /// Returns [`SessionError::Io`] if the descriptor cannot be removed.
  pub fn unbind(&self) -> Result<()> {
    self.serve_task.abort();
    self.registry.remove(&self.id)
  }
}

impl Drop for BoundSession {
  fn drop(&mut self) {
    self.serve_task.abort();
    let _ = self.registry.remove(&self.id);
  }
}

/// Publish `browser` under session `id`, starting a server and writing a
/// registry descriptor. The returned [`BoundSession`] owns the server task;
/// keep it alive for as long as the session should be reachable.
///
/// `script_hook`, when supplied, enables the `run-script` verb for clients.
///
/// # Errors
///
/// Returns [`SessionError::Io`] if the server cannot bind its endpoint or the
/// descriptor cannot be written.
pub async fn bind(
  browser: &Browser,
  id: &str,
  options: BindOptions,
  script_hook: Option<Arc<dyn ScriptHook>>,
) -> Result<BoundSession> {
  let registry = Registry::open()?;
  bind_in(&registry, browser, id, options, script_hook).await
}

/// Like [`bind`], but publishes into an explicit registry (used by tests and
/// by hosts that resolve the registry themselves).
///
/// # Errors
///
/// See [`bind`].
pub async fn bind_in(
  registry: &Registry,
  browser: &Browser,
  id: &str,
  options: BindOptions,
  script_hook: Option<Arc<dyn ScriptHook>>,
) -> Result<BoundSession> {
  let mut dispatcher: BrowserDispatcher = dispatcher_for(browser);
  if let Some(hook) = script_hook {
    dispatcher = dispatcher.with_script_hook(hook);
  }
  let browser_name = browser_name_for(browser.backend_kind()).to_string();
  bind_dispatcher(registry, id, Arc::new(dispatcher), browser_name, options).await
}

/// The construction core shared by [`bind_in`] and tests: bind a server over
/// any [`Dispatcher`], publish the descriptor, and spawn the serve task.
///
/// # Errors
///
/// Returns [`SessionError::Io`](crate::SessionError::Io) if the server cannot
/// bind its endpoint or the descriptor cannot be written.
pub async fn bind_dispatcher(
  registry: &Registry,
  id: &str,
  dispatcher: Arc<dyn crate::Dispatcher>,
  browser_name: String,
  options: BindOptions,
) -> Result<BoundSession> {
  let endpoint = match (&options.host, options.port) {
    (Some(host), port) => Endpoint::Tcp(format!("{host}:{}", port.unwrap_or(0))),
    (None, Some(port)) => Endpoint::Tcp(format!("127.0.0.1:{port}")),
    (None, None) => default_socket_endpoint(registry, id),
  };

  let server = SessionServer::bind(endpoint, dispatcher).await?;
  let resolved_endpoint = server.endpoint_string().to_string();
  let server = Arc::new(server);

  let descriptor = SessionDescriptor {
    id: id.to_string(),
    endpoint: resolved_endpoint.clone(),
    pid: std::process::id(),
    browser_name,
    workspace_dir: options.workspace_dir,
    metadata: options.metadata,
  };
  registry.put(&descriptor)?;

  let serve_handle = Arc::clone(&server);
  let serve_task = tokio::spawn(async move {
    if let Err(e) = serve_handle.serve().await {
      tracing::debug!(error = %e, "session server stopped");
    }
  });

  Ok(BoundSession {
    id: id.to_string(),
    endpoint: resolved_endpoint,
    registry: registry.clone(),
    server,
    serve_task,
  })
}

/// Remove a session descriptor by id without holding the [`BoundSession`]
/// handle — used by the CLI's `close` for a session owned by another process
/// (best-effort: the owning process's server keeps running until it exits, but
/// the descriptor is pruned so the session no longer lists).
///
/// # Errors
///
/// Returns [`SessionError::Io`] if the descriptor cannot be removed.
pub fn unbind_id(registry: &Registry, id: &str) -> Result<()> {
  registry.remove(id)
}

#[cfg(unix)]
fn default_socket_endpoint(registry: &Registry, id: &str) -> Endpoint {
  Endpoint::Unix(registry.dir().join(format!("{id}.sock")))
}

#[cfg(not(unix))]
fn default_socket_endpoint(_registry: &Registry, _id: &str) -> Endpoint {
  // No Unix sockets: fall back to an OS-assigned loopback TCP port.
  Endpoint::Tcp("127.0.0.1:0".to_string())
}

impl std::fmt::Debug for BoundSession {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("BoundSession")
      .field("id", &self.id)
      .field("endpoint", &self.endpoint)
      .finish_non_exhaustive()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::client::SessionClient;
  use crate::dispatch::test_support::EchoDispatcher;
  use crate::protocol::Command;

  #[tokio::test]
  async fn bind_publishes_descriptor_and_serves_until_unbound() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = Registry::open_at(tmp.path()).unwrap();

    let session = bind_dispatcher(
      &registry,
      "agent-1",
      Arc::new(EchoDispatcher),
      "chromium".into(),
      BindOptions {
        workspace_dir: Some("/work".into()),
        metadata: Some(serde_json::json!({ "k": "v" })),
        ..Default::default()
      },
    )
    .await
    .unwrap();

    // Descriptor is discoverable by a separate registry handle.
    let other = Registry::open_at(tmp.path()).unwrap();
    let descriptor = other.get("agent-1").unwrap().expect("descriptor written");
    assert_eq!(descriptor.endpoint, session.endpoint());
    assert_eq!(descriptor.browser_name, "chromium");
    assert_eq!(descriptor.workspace_dir.as_deref(), Some("/work"));

    // A client resolving by id reaches the live server.
    let mut client = SessionClient::attach(&other, "agent-1").await.unwrap();
    let resp = client
      .call(Command::new(1, "echo", serde_json::json!({})))
      .await
      .unwrap();
    assert!(resp.ok);

    // Unbind removes the descriptor.
    session.unbind().unwrap();
    assert!(other.get("agent-1").unwrap().is_none());
  }

  #[tokio::test]
  async fn drop_unbinds() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = Registry::open_at(tmp.path()).unwrap();
    {
      let _session = bind_dispatcher(
        &registry,
        "ephemeral",
        Arc::new(EchoDispatcher),
        "webkit".into(),
        BindOptions::default(),
      )
      .await
      .unwrap();
      assert!(registry.get("ephemeral").unwrap().is_some());
    }
    // Give the drop's removal a tick.
    assert!(registry.get("ephemeral").unwrap().is_none());
  }

  #[tokio::test]
  async fn attach_to_missing_session_is_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = Registry::open_at(tmp.path()).unwrap();
    let result = SessionClient::attach(&registry, "nope").await;
    assert!(matches!(result, Err(crate::SessionError::NotFound(_))));
  }
}
