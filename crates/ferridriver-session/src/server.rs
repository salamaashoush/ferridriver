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

use crate::dispatch::{Dispatcher, EventSink};
use crate::lifecycle::Lifecycle;
use crate::protocol::{CLOSE_VERB, Command, ServerFrame};
use crate::transport::{read_frame, write_frame};
use crate::{Result, SessionError};

/// Where a session server listens.
#[derive(Debug, Clone)]
pub enum Endpoint {
  /// A Unix-domain socket at the given filesystem path.
  #[cfg(unix)]
  Unix(std::path::PathBuf),
  #[cfg(unix)]
  OwnedUnix(Arc<tempfile::TempDir>),
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
  listener: std::sync::Mutex<Option<Listener>>,
  lifecycle: Arc<Lifecycle>,
}

enum Listener {
  #[cfg(unix)]
  Unix {
    listener: UnixListener,
    path: std::path::PathBuf,
    identity: (u64, u64),
    _directory: Option<Arc<tempfile::TempDir>>,
  },
  Tcp(TcpListener),
}

impl SessionServer {
  /// Bind a server to `endpoint`, ready to [`SessionServer::serve`].
  ///
  /// Existing Unix socket paths are never replaced. For a TCP endpoint with
  /// port `0`, the OS-assigned address is captured into
  /// [`SessionServer::endpoint_string`].
  ///
  /// # Errors
  ///
  /// Returns [`SessionError::Io`] if the socket / address cannot be bound.
  pub async fn bind(endpoint: Endpoint, dispatcher: Arc<dyn Dispatcher>) -> Result<Self> {
    match endpoint {
      #[cfg(unix)]
      Endpoint::Unix(path) => Self::bind_unix(&path, None, dispatcher),
      #[cfg(unix)]
      Endpoint::OwnedUnix(directory) => {
        Self::bind_unix(&directory.path().join("ipc.sock"), Some(directory), dispatcher)
      },
      Endpoint::Tcp(addr) => {
        let listener = TcpListener::bind(&addr).await?;
        let local = listener.local_addr()?;
        Ok(Self {
          endpoint_string: format!("ws://{local}"),
          listener: std::sync::Mutex::new(Some(Listener::Tcp(listener))),
          lifecycle: Arc::new(Lifecycle::new(format!("ws://{local}"), dispatcher)?),
        })
      },
    }
  }

  #[cfg(unix)]
  fn bind_unix(
    path: &std::path::Path,
    directory: Option<Arc<tempfile::TempDir>>,
    dispatcher: Arc<dyn Dispatcher>,
  ) -> Result<Self> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent)?;
    }
    let socket = UnixListener::bind(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    let listener = Listener::Unix {
      listener: socket,
      identity: (metadata.dev(), metadata.ino()),
      path: path.to_path_buf(),
      _directory: directory,
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(Self {
      endpoint_string: path.to_string_lossy().into_owned(),
      lifecycle: Arc::new(Lifecycle::new(path.to_string_lossy().into_owned(), dispatcher)?),
      listener: std::sync::Mutex::new(Some(listener)),
    })
  }

  pub(crate) fn generation(&self) -> &str {
    &self.lifecycle.generation
  }

  pub(crate) fn is_stopped(&self) -> bool {
    *self.lifecycle.stopped.borrow()
  }

  pub(crate) async fn stopped(&self) {
    let _ = self.lifecycle.stopped.subscribe().wait_for(|stopped| *stopped).await;
  }

  pub(crate) fn publish(&self, claim: crate::registry::RegistryClaim) {
    self.lifecycle.publish(claim);
  }

  pub(crate) fn unpublish(&self) -> Result<()> {
    self.lifecycle.stopped.send_replace(true);
    self.lifecycle.unpublish()
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
    let listener = self
      .listener
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .take()
      .ok_or_else(|| SessionError::Dispatch("session server already started".to_owned()))?;
    let _serving = Serving(Arc::clone(&self.lifecycle));
    let mut connections = tokio::task::JoinSet::new();
    let mut stopped = self.lifecycle.stopped.subscribe();
    let mut draining = self.lifecycle.draining();
    loop {
      tokio::select! {
        biased;
        _ = stopped.wait_for(|stopped| *stopped) => return Ok(()),
        _ = draining.wait_for(|draining| *draining) => break,
        Some(result) = connections.join_next() => connection_ended(result),
        result = self.accept(&listener) => { connections.spawn(result?); },
      }
    }
    drop(listener);
    loop {
      tokio::select! {
        biased;
        _ = stopped.wait_for(|stopped| *stopped) => return Ok(()),
        result = connections.join_next() => match result {
          Some(result) => connection_ended(result),
          None => return Ok(()),
        },
      }
    }
  }

  /// Accept no more connections or commands, and let each connection answer
  /// the command it is running before [`SessionServer::serve`] returns.
  pub(crate) fn drain(&self) -> Result<()> {
    self.lifecycle.drain();
    self.lifecycle.unpublish()
  }

  async fn accept(
    &self,
    listener: &Listener,
  ) -> Result<std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>> {
    let lifecycle = Arc::clone(&self.lifecycle);
    match listener {
      #[cfg(unix)]
      Listener::Unix { listener, .. } => {
        let (stream, _) = listener.accept().await?;
        Ok(Box::pin(serve_connection(stream, lifecycle)))
      },
      Listener::Tcp(listener) => {
        let (stream, _) = listener.accept().await?;
        Ok(Box::pin(serve_connection(stream, lifecycle)))
      },
    }
  }
}

struct Serving(Arc<Lifecycle>);

impl Drop for Serving {
  fn drop(&mut self) {
    self.0.stopped.send_replace(true);
    if let Err(error) = self.0.unpublish() {
      tracing::warn!(%error, "session publication cleanup failed");
    }
  }
}

#[cfg(unix)]
impl Drop for Listener {
  fn drop(&mut self) {
    use std::os::unix::fs::MetadataExt as _;
    if let Listener::Unix { path, identity, .. } = self
      && let Ok(metadata) = std::fs::symlink_metadata(&path)
      && (metadata.dev(), metadata.ino()) == *identity
      && let Err(error) = std::fs::remove_file(path)
      && error.kind() != std::io::ErrorKind::NotFound
    {
      tracing::warn!(%error, "session socket cleanup failed");
    }
  }
}

fn connection_ended(result: std::result::Result<Result<()>, tokio::task::JoinError>) {
  match result {
    Ok(Ok(())) => {},
    Ok(Err(error)) => tracing::debug!(%error, "session connection ended"),
    Err(error) => tracing::warn!(%error, "session connection task failed"),
  }
}

/// Read commands from one connection and answer each via the dispatcher,
/// until the peer hangs up or the session drains.
///
/// A command's events are written as they are emitted — that is what makes a
/// remote `run` stream its console like a local one — and the response frame
/// always comes last.
async fn serve_connection<S>(stream: S, lifecycle: Arc<Lifecycle>) -> Result<()>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  let (mut reader, mut writer) = tokio::io::split(stream);
  let mut pending = Vec::new();
  let mut draining = lifecycle.draining();
  loop {
    let command: Option<Command> = tokio::select! {
      biased;
      _ = draining.wait_for(|draining| *draining) => break,
      command = read_frame(&mut reader, &mut pending) => match command {
        Ok(c) => c,
        Err(SessionError::ConnectionClosed) => break,
        Err(e) => return Err(e),
      },
    };
    let Some(command) = command else { break };

    if command.verb == CLOSE_VERB {
      if !lifecycle.identifies(&command) {
        write_frame(
          &mut writer,
          &ServerFrame::Response(crate::Response::err(
            command.id,
            "close request does not identify this session endpoint",
          )),
        )
        .await?;
        continue;
      }
      let request = lifecycle.close_request();
      let response = lifecycle.close(&command).await;
      let closed = response.ok;
      let written = write_frame(&mut writer, &ServerFrame::Response(response)).await;
      drop(request);
      written?;
      if closed {
        return Ok(());
      }
      continue;
    }
    let mut closing = lifecycle.closing();
    if *closing.borrow() {
      write_frame(
        &mut writer,
        &ServerFrame::Response(crate::Response::err(
          command.id,
          "session is closing; retry close if cleanup failed",
        )),
      )
      .await?;
      continue;
    }
    tokio::select! {
      biased;
      _ = closing.wait_for(|closing| *closing) => return Ok(()),
      result = async {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let sink = EventSink::new(command.id, tx);
    let mut dispatch = std::pin::pin!(lifecycle.run(command, sink));
    // `events_open` is what keeps this from spinning: a dispatcher that drops
    // its sink early closes the channel, and an always-ready `None` branch
    // would otherwise be re-polled on every loop iteration.
    let mut events_open = true;
    let response = loop {
      tokio::select! {
        biased;
        event = rx.recv(), if events_open => match event {
          Some(event) => write_frame(&mut writer, &ServerFrame::Event(event)).await?,
          None => events_open = false,
        },
        response = &mut dispatch => break response,
      }
    };
    // Events emitted in the same poll that completed the dispatch are still
    // queued; the sink is dropped with the future, so this drains and ends.
    while let Ok(event) = rx.try_recv() {
      write_frame(&mut writer, &ServerFrame::Event(event)).await?;
    }
    write_frame(&mut writer, &ServerFrame::Response(response)).await?;
        Ok::<(), SessionError>(())
      } => result?,
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::client::SessionClient;
  use crate::dispatch::test_support::EchoDispatcher;

  struct StreamingDispatcher {
    started: tokio::sync::Notify,
    closed: std::sync::atomic::AtomicBool,
  }

  #[async_trait::async_trait]
  impl Dispatcher for StreamingDispatcher {
    async fn dispatch(&self, _command: Command, events: EventSink) -> crate::Response {
      events.console("log", "x".repeat(4096), 0);
      self.started.notify_one();
      std::future::pending().await
    }

    async fn close(&self) -> std::result::Result<(), String> {
      self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
      Ok(())
    }
  }

  #[tokio::test]
  async fn non_reading_streaming_client_cannot_block_cleanup() {
    let dispatcher = Arc::new(StreamingDispatcher {
      started: tokio::sync::Notify::new(),
      closed: std::sync::atomic::AtomicBool::new(false),
    });
    let lifecycle = Arc::new(Lifecycle::new("local".into(), dispatcher.clone()).unwrap());
    let (server, mut client) = tokio::io::duplex(64);
    let serving = tokio::spawn(serve_connection(server, lifecycle.clone()));
    write_frame(&mut client, &Command::new(1, "run", serde_json::json!({})))
      .await
      .unwrap();
    dispatcher.started.notified().await;
    let close = Command::new(
      2,
      CLOSE_VERB,
      serde_json::json!({"endpoint":"local", "generation":lifecycle.generation}),
    );
    let reply = tokio::time::timeout(std::time::Duration::from_secs(1), lifecycle.close(&close))
      .await
      .unwrap();
    assert!(reply.ok);
    assert!(dispatcher.closed.load(std::sync::atomic::Ordering::SeqCst));
    serving.await.unwrap().unwrap();
  }

  struct DrainingDispatcher {
    server: std::sync::OnceLock<std::sync::Weak<SessionServer>>,
  }

  #[async_trait::async_trait]
  impl Dispatcher for DrainingDispatcher {
    async fn dispatch(&self, command: Command, _events: EventSink) -> crate::Response {
      let server = self.server.get().and_then(std::sync::Weak::upgrade).unwrap();
      server.drain().unwrap();
      tokio::task::yield_now().await;
      crate::Response::ok(command.id, "drained by sashoush")
    }
  }

  #[tokio::test]
  async fn a_command_that_drains_the_session_still_gets_its_reply() {
    let dispatcher = Arc::new(DrainingDispatcher {
      server: std::sync::OnceLock::new(),
    });
    let server = Arc::new(
      SessionServer::bind(Endpoint::Tcp("127.0.0.1:0".into()), dispatcher.clone())
        .await
        .unwrap(),
    );
    dispatcher.server.set(Arc::downgrade(&server)).unwrap();
    let serving = tokio::spawn({
      let server = server.clone();
      async move { server.serve().await }
    });
    let mut client = SessionClient::connect(server.endpoint_string()).await.unwrap();
    let reply = client
      .call(Command::new(1, "resume", serde_json::json!({})))
      .await
      .unwrap();
    assert!(reply.ok);
    assert_eq!(reply.text, "drained by sashoush");
    tokio::time::timeout(std::time::Duration::from_secs(5), serving)
      .await
      .expect("a drained server stops once its connections have answered")
      .unwrap()
      .unwrap();
    assert!(SessionClient::connect(server.endpoint_string()).await.is_err());
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn binding_an_occupied_socket_path_preserves_the_owner() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("existing.sock");
    std::fs::write(&path, "sashoush sentinel").unwrap();
    assert!(
      SessionServer::bind(Endpoint::Unix(path.clone()), Arc::new(EchoDispatcher))
        .await
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "sashoush sentinel");
    let path = temp.path().join("live.sock");
    let server = Arc::new(
      SessionServer::bind(Endpoint::Unix(path.clone()), Arc::new(EchoDispatcher))
        .await
        .unwrap(),
    );
    let serving = tokio::spawn({
      let server = server.clone();
      async move { server.serve().await }
    });
    assert!(
      SessionServer::bind(Endpoint::Unix(path), Arc::new(EchoDispatcher))
        .await
        .is_err()
    );
    let mut client = SessionClient::connect(server.endpoint_string()).await.unwrap();
    assert!(
      client
        .call(Command::new(1, "echo", serde_json::json!({})))
        .await
        .unwrap()
        .ok
    );
    serving.abort();
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn dropping_a_listener_does_not_remove_a_replacement_file() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("session.sock");
    let server = SessionServer::bind(Endpoint::Unix(path.clone()), Arc::new(EchoDispatcher))
      .await
      .unwrap();
    std::fs::rename(&path, temp.path().join("previous.sock")).unwrap();
    std::fs::write(&path, "sashoush replacement").unwrap();
    drop(server);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "sashoush replacement");
  }

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

  #[tokio::test]
  async fn events_arrive_before_the_response() {
    use crate::protocol::EventPayload;

    let (endpoint, _h) = spawn_echo_server().await;
    let mut client = SessionClient::connect(&endpoint).await.unwrap();
    let mut seen: Vec<(String, String)> = Vec::new();
    let resp = client
      .call_with_events(Command::new(4, "chatty", serde_json::json!({})), |event| {
        assert_eq!(event.id, 4);
        let EventPayload::Console { level, message, .. } = event.payload else {
          panic!("echo dispatcher only emits console events");
        };
        seen.push((level, message));
      })
      .await
      .unwrap();
    assert!(resp.ok);
    assert_eq!(
      seen,
      vec![
        ("log".to_string(), "first".to_string()),
        ("error".to_string(), "second".to_string())
      ]
    );
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn unix_socket_is_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;

    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("s.sock");
    let _server = SessionServer::bind(Endpoint::Unix(sock.clone()), Arc::new(EchoDispatcher))
      .await
      .unwrap();
    let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "session socket must not be reachable by other users");
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
