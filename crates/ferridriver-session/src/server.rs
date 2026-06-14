//! The session server: accept client connections and route command frames
//! through a [`Dispatcher`].
//!
//! A bound browser starts one server, which listens on a Unix-domain socket
//! (default) or a TCP loopback address (the `host`/`port` bind path). Each
//! accepted connection is handled concurrently; within a connection, commands
//! are answered in order. The server holds an `Arc<dyn Dispatcher>` shared by
//! all connections so they all drive the same live browser.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
#[cfg(unix)]
use tokio::net::UnixListener;

use crate::dispatch::Dispatcher;
use crate::protocol::Command;
use crate::transport::{read_frame, write_frame};
use crate::{Result, SessionError};

/// Where a session server listens.
#[derive(Debug, Clone)]
pub enum Endpoint {
  /// A Unix-domain socket at the given filesystem path.
  #[cfg(unix)]
  Unix(std::path::PathBuf),
  /// A TCP address (`host:port`). Port `0` lets the OS pick a free port; the
  /// chosen port is reported back by [`SessionServer::endpoint_string`].
  Tcp(String),
}

impl Endpoint {
  /// Parse an endpoint string as written in a registry descriptor: a
  /// `ws://host:port` or bare `host:port` becomes [`Endpoint::Tcp`], anything
  /// else is treated as a Unix socket path.
  #[must_use]
  pub fn parse(s: &str) -> Self {
    if let Some(rest) = s.strip_prefix("ws://") {
      return Endpoint::Tcp(rest.trim_end_matches('/').to_string());
    }
    #[cfg(unix)]
    {
      if s.starts_with('/') || s.contains(".sock") {
        return Endpoint::Unix(std::path::PathBuf::from(s));
      }
    }
    Endpoint::Tcp(s.to_string())
  }
}

/// A running session server.
pub struct SessionServer {
  endpoint_string: String,
  listener: Listener,
  dispatcher: Arc<dyn Dispatcher>,
}

enum Listener {
  #[cfg(unix)]
  Unix(UnixListener, std::path::PathBuf),
  Tcp(TcpListener),
}

impl SessionServer {
  /// Bind a server to `endpoint`, ready to [`SessionServer::serve`].
  ///
  /// For a Unix endpoint, any stale socket file at the path is removed first
  /// (a previous owner that exited without cleanup). For a TCP endpoint with
  /// port `0`, the OS-assigned address is captured into
  /// [`SessionServer::endpoint_string`].
  ///
  /// # Errors
  ///
  /// Returns [`SessionError::Io`] if the socket / address cannot be bound.
  pub async fn bind(endpoint: Endpoint, dispatcher: Arc<dyn Dispatcher>) -> Result<Self> {
    match endpoint {
      #[cfg(unix)]
      Endpoint::Unix(path) => {
        if path.exists() {
          // A leftover socket file blocks bind(); remove it. If a live owner
          // still holds it, the subsequent connect by clients would have gone
          // to that owner — but `bind` here means this process is the owner.
          let _ = std::fs::remove_file(&path);
        }
        if let Some(parent) = path.parent() {
          std::fs::create_dir_all(parent)?;
        }
        let listener = UnixListener::bind(&path)?;
        Ok(Self {
          endpoint_string: path.to_string_lossy().into_owned(),
          listener: Listener::Unix(listener, path),
          dispatcher,
        })
      },
      Endpoint::Tcp(addr) => {
        let listener = TcpListener::bind(&addr).await?;
        let local = listener.local_addr()?;
        Ok(Self {
          endpoint_string: format!("ws://{local}"),
          listener: Listener::Tcp(listener),
          dispatcher,
        })
      },
    }
  }

  /// The resolved endpoint string to publish in the registry. For TCP this
  /// reflects the OS-chosen port; for Unix it is the socket path.
  #[must_use]
  pub fn endpoint_string(&self) -> &str {
    &self.endpoint_string
  }

  /// Accept and serve connections until the listener errors or the future is
  /// dropped. Each connection is spawned onto its own task. Borrows `self` so
  /// the socket file is cleaned up by the listener's `Drop` when the server
  /// value is finally dropped.
  ///
  /// # Errors
  ///
  /// Returns [`SessionError::Io`] if accepting a connection fails.
  pub async fn serve(&self) -> Result<()> {
    match &self.listener {
      #[cfg(unix)]
      Listener::Unix(listener, _path) => loop {
        let (stream, _) = listener.accept().await?;
        let dispatcher = Arc::clone(&self.dispatcher);
        tokio::spawn(async move {
          if let Err(e) = serve_connection(stream, dispatcher).await {
            tracing::debug!(error = %e, "session connection ended");
          }
        });
      },
      Listener::Tcp(listener) => loop {
        let (stream, _) = listener.accept().await?;
        let dispatcher = Arc::clone(&self.dispatcher);
        tokio::spawn(async move {
          if let Err(e) = serve_connection(stream, dispatcher).await {
            tracing::debug!(error = %e, "session connection ended");
          }
        });
      },
    }
  }
}

#[cfg(unix)]
impl Drop for Listener {
  fn drop(&mut self) {
    if let Listener::Unix(_, path) = self {
      let _ = std::fs::remove_file(path);
    }
  }
}

/// Read commands from one connection and answer each via the dispatcher,
/// until the peer hangs up.
pub(crate) async fn serve_connection<S>(stream: S, dispatcher: Arc<dyn Dispatcher>) -> Result<()>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  let (mut reader, mut writer) = tokio::io::split(stream);
  let mut pending = Vec::new();
  loop {
    let command: Option<Command> = match read_frame(&mut reader, &mut pending).await {
      Ok(c) => c,
      Err(SessionError::ConnectionClosed) => break,
      Err(e) => return Err(e),
    };
    let Some(command) = command else { break };
    let response = dispatcher.dispatch(command).await;
    write_frame(&mut writer, &response).await?;
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::client::SessionClient;
  use crate::dispatch::test_support::EchoDispatcher;

  async fn spawn_echo_server() -> (String, tokio::task::JoinHandle<()>) {
    let server = SessionServer::bind(Endpoint::Tcp("127.0.0.1:0".into()), Arc::new(EchoDispatcher))
      .await
      .unwrap();
    let endpoint = server.endpoint_string().to_string();
    let handle = tokio::spawn(async move {
      let _ = server.serve().await;
    });
    (endpoint, handle)
  }

  #[tokio::test]
  async fn client_command_gets_dispatched_response() {
    let (endpoint, _h) = spawn_echo_server().await;
    let mut client = SessionClient::connect(&endpoint).await.unwrap();
    let resp = client
      .call(Command::new(1, "echo", serde_json::json!({ "x": 1 })))
      .await
      .unwrap();
    assert!(resp.ok);
    assert!(resp.text.starts_with("echo@default:"), "{}", resp.text);
  }

  #[tokio::test]
  async fn dispatcher_error_is_a_response_not_a_drop() {
    let (endpoint, _h) = spawn_echo_server().await;
    let mut client = SessionClient::connect(&endpoint).await.unwrap();
    let resp = client
      .call(Command::new(2, "boom", serde_json::json!({})))
      .await
      .unwrap();
    assert!(!resp.ok);
    assert_eq!(resp.error.as_deref(), Some("explosion"));
    // Connection survives a failed verb — a second call still works.
    let again = client
      .call(Command::new(3, "echo", serde_json::json!({})))
      .await
      .unwrap();
    assert!(again.ok);
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn unix_socket_endpoint_roundtrips_and_cleans_up() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("s.sock");
    let server = SessionServer::bind(Endpoint::Unix(sock.clone()), Arc::new(EchoDispatcher))
      .await
      .unwrap();
    assert_eq!(server.endpoint_string(), sock.to_string_lossy());
    let handle = tokio::spawn(async move {
      let _ = server.serve().await;
    });
    let mut client = SessionClient::connect(&sock.to_string_lossy()).await.unwrap();
    let resp = client
      .call(Command::new(1, "echo", serde_json::json!({})))
      .await
      .unwrap();
    assert!(resp.ok);
    handle.abort();
  }
}
