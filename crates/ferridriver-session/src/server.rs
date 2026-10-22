//! The session server: accept client connections and route command frames
//! through a [`Dispatcher`].
//!
//! A bound browser starts one server, which listens on a Unix-domain socket
//! (default) or a TCP address (the `host`/`port` bind path). Each accepted
//! connection is handled concurrently; within a connection, commands are
//! answered in order. The server holds an `Arc<dyn Dispatcher>` shared by
//! all connections so they all drive the same live browser.
//!
//! A Unix socket is owner-only, so the filesystem is its access control. A
//! TCP endpoint has none of its own and the commands it takes run arbitrary
//! scripts, so it can require a token: the first command on every
//! connection must then be [`crate::protocol::AUTH_VERB`] carrying it. One
//! listening beyond loopback always requires one, generated when the caller
//! supplies none. The token travels inside the endpoint string
//! (`ws://host:port/?token=...`), the one value every client is handed.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
#[cfg(unix)]
use tokio::net::UnixListener;

use crate::dispatch::{Dispatcher, EventSink};
use crate::lifecycle::Lifecycle;
use crate::protocol::{AUTH_VERB, CLOSE_VERB, Command, ServerFrame};
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
  /// else is treated as a Unix socket path. A `?token=` query is not part of
  /// the address; [`endpoint_token`] reads it.
  #[must_use]
  pub fn parse(s: &str) -> Self {
    if let Some(rest) = s.strip_prefix("ws://") {
      let address = rest.split_once('?').map_or(rest, |(address, _)| address);
      return Endpoint::Tcp(address.trim_end_matches('/').to_string());
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

/// The token an endpoint string carries, if any.
#[must_use]
pub fn endpoint_token(endpoint: &str) -> Option<&str> {
  let (_, query) = endpoint.strip_prefix("ws://")?.split_once('?')?;
  query
    .split('&')
    .find_map(|pair| pair.strip_prefix("token="))
    .filter(|token| !token.is_empty())
}

/// A fresh random token, URL-safe so it can sit in an endpoint string.
fn generate_token() -> Result<String> {
  use base64::Engine as _;
  let mut bytes = [0; 32];
  getrandom::fill(&mut bytes)
    .map_err(|error| SessionError::Dispatch(format!("session token generation failed: {error}")))?;
  Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// The token a TCP endpoint requires: the caller's, else a generated one
/// unless the endpoint only listens on loopback.
fn required_token(given: Option<String>, loopback: bool) -> Result<Option<String>> {
  match given {
    Some(token) => Ok(Some(token)),
    None if loopback => Ok(None),
    None => generate_token().map(Some),
  }
}

/// A caller-supplied token is written into an endpoint string as-is, so it
/// is held to the characters a URL query carries unescaped.
fn check_token(token: &str) -> Result<()> {
  if !token.is_empty()
    && token
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~'))
  {
    return Ok(());
  }
  Err(SessionError::Dispatch(
    "a session token must be non-empty and use only letters, digits, '-', '_', '.' and '~'".to_owned(),
  ))
}

/// Compare digests rather than the tokens themselves, so the time a
/// mismatch takes says nothing about how much of a guess was right.
fn token_matches(expected: &str, offered: Option<&str>) -> bool {
  use sha2::{Digest as _, Sha256};
  offered.is_some_and(|offered| Sha256::digest(expected.as_bytes()) == Sha256::digest(offered.as_bytes()))
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
  /// [`SessionServer::endpoint_string`]. A TCP endpoint beyond loopback gets
  /// a generated token; see [`SessionServer::bind_with_token`].
  ///
  /// # Errors
  ///
  /// Returns [`SessionError::Io`] if the socket / address cannot be bound.
  pub async fn bind(endpoint: Endpoint, dispatcher: Arc<dyn Dispatcher>) -> Result<Self> {
    Self::bind_with_token(endpoint, dispatcher, None).await
  }

  /// [`SessionServer::bind`], requiring `token` on a TCP endpoint. With no
  /// token, a loopback endpoint stays open to local processes and any other
  /// address gets a generated one. Either way the endpoint string carries
  /// the token clients must present. A Unix socket ignores `token`: the
  /// socket file is owner-only already.
  ///
  /// # Errors
  ///
  /// Returns [`SessionError::Io`] if the socket / address cannot be bound,
  /// and [`SessionError::Dispatch`] for a token an endpoint string cannot
  /// carry.
  pub async fn bind_with_token(
    endpoint: Endpoint,
    dispatcher: Arc<dyn Dispatcher>,
    token: Option<String>,
  ) -> Result<Self> {
    match endpoint {
      #[cfg(unix)]
      Endpoint::Unix(path) => Self::bind_unix(&path, None, dispatcher),
      #[cfg(unix)]
      Endpoint::OwnedUnix(directory) => {
        Self::bind_unix(&directory.path().join("ipc.sock"), Some(directory), dispatcher)
      },
      Endpoint::Tcp(addr) => {
        if let Some(token) = &token {
          check_token(token)?;
        }
        let listener = TcpListener::bind(&addr).await?;
        let local = listener.local_addr()?;
        let token = required_token(token, local.ip().is_loopback())?;
        let endpoint_string = match &token {
          Some(token) => format!("ws://{local}/?token={token}"),
          None => format!("ws://{local}"),
        };
        Ok(Self {
          lifecycle: Arc::new(Lifecycle::new(endpoint_string.clone(), dispatcher)?.with_token(token)),
          endpoint_string,
          listener: std::sync::Mutex::new(Some(Listener::Tcp(listener))),
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
  /// reflects the OS-chosen port and carries the token, if one is
  /// required; for Unix it is the socket path.
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
/// always comes last. The connection keeps reading while a command runs, so
/// commands sent ahead queue up in order and a hang-up is noticed at once:
/// the command's event sink closes, which is what ends a stream that runs
/// until its client goes away.
async fn serve_connection<S>(stream: S, lifecycle: Arc<Lifecycle>) -> Result<()>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  let (mut reader, mut writer) = tokio::io::split(stream);
  let mut connection = Connection::default();
  let mut draining = lifecycle.draining();
  if let Some(token) = lifecycle.token.as_deref()
    && !authenticate(&mut reader, &mut writer, &mut connection.pending, token).await?
  {
    return Ok(());
  }
  loop {
    let command = if let Some(command) = connection.queued.pop_front() {
      command
    } else {
      if connection.hung_up {
        break;
      }
      let command: Option<Command> = tokio::select! {
        biased;
        _ = draining.wait_for(|draining| *draining) => break,
        command = read_frame(&mut reader, &mut connection.pending) => match command {
          Ok(c) => c,
          Err(SessionError::ConnectionClosed) => break,
          Err(e) => return Err(e),
        },
      };
      let Some(command) = command else { break };
      command
    };
    if *draining.borrow() {
      break;
    }

    if command.verb == CLOSE_VERB {
      if close(&mut writer, &lifecycle, &command).await? {
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
    // Closing cuts the command off wherever it is, including a write to a
    // client that stopped reading.
    tokio::select! {
      biased;
      _ = closing.wait_for(|closing| *closing) => return Ok(()),
      result = run_command(&mut reader, &mut writer, &mut connection, &lifecycle, command) => result?,
    }
  }
  Ok(())
}

/// What one connection carries between commands.
#[derive(Default)]
struct Connection {
  /// Bytes read past the last complete frame.
  pending: Vec<u8>,
  /// Commands that arrived while an earlier one ran.
  queued: std::collections::VecDeque<Command>,
  /// The client has gone; nothing more is read or written.
  hung_up: bool,
}

/// Run one command, writing its events as they come and its response last,
/// while still reading the connection: commands sent ahead are queued, and a
/// hang-up closes the command's event sink.
async fn run_command<R, W>(
  reader: &mut R,
  writer: &mut W,
  connection: &mut Connection,
  lifecycle: &Lifecycle,
  command: Command,
) -> Result<()>
where
  R: AsyncRead + Unpin,
  W: AsyncWrite + Unpin,
{
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
        Some(event) => write_frame(writer, &ServerFrame::Event(event)).await?,
        None => events_open = false,
      },
      frame = read_frame::<_, Command>(reader, &mut connection.pending), if !connection.hung_up => match frame {
        Ok(Some(next)) => connection.queued.push_back(next),
        Ok(None) | Err(SessionError::ConnectionClosed) => {
          // The command runs on, but nobody is listening: closing the
          // channel tells a streaming command to end and drops the rest.
          connection.hung_up = true;
          connection.queued.clear();
          rx.close();
        },
        Err(e) => return Err(e),
      },
      response = &mut dispatch => break response,
    }
  };
  if connection.hung_up {
    return Ok(());
  }
  // Events emitted in the same poll that completed the dispatch are still
  // queued; the sink is dropped with the future, so this drains and ends.
  while let Ok(event) = rx.try_recv() {
    write_frame(writer, &ServerFrame::Event(event)).await?;
  }
  write_frame(writer, &ServerFrame::Response(response)).await
}

/// Answer a [`CLOSE_VERB`] command. `true` when the session closed.
async fn close<W>(writer: &mut W, lifecycle: &Arc<Lifecycle>, command: &Command) -> Result<bool>
where
  W: AsyncWrite + Unpin,
{
  if !lifecycle.identifies(command) {
    write_frame(
      writer,
      &ServerFrame::Response(crate::Response::err(
        command.id,
        "close request does not identify this session endpoint",
      )),
    )
    .await?;
    return Ok(false);
  }
  let request = lifecycle.close_request();
  let response = lifecycle.close(command).await;
  let closed = response.ok;
  let written = write_frame(writer, &ServerFrame::Response(response)).await;
  drop(request);
  written?;
  Ok(closed)
}

/// Read the connection's first command and accept it only if it is
/// [`AUTH_VERB`] with the right token. `false` when the connection was
/// refused (and told why) or hung up first.
async fn authenticate<R, W>(reader: &mut R, writer: &mut W, pending: &mut Vec<u8>, token: &str) -> Result<bool>
where
  R: AsyncRead + Unpin,
  W: AsyncWrite + Unpin,
{
  let command: Command = match read_frame(reader, pending).await {
    Ok(Some(command)) => command,
    Ok(None) | Err(SessionError::ConnectionClosed) => return Ok(false),
    Err(error) => return Err(error),
  };
  let offered = command.args.get("token").and_then(serde_json::Value::as_str);
  if command.verb == AUTH_VERB && token_matches(token, offered) {
    write_frame(writer, &ServerFrame::Response(crate::Response::ok(command.id, ""))).await?;
    return Ok(true);
  }
  let reason = if command.verb == AUTH_VERB {
    "session token rejected"
  } else {
    "this session requires a token: authenticate with 'session.auth' first"
  };
  write_frame(writer, &ServerFrame::Response(crate::Response::err(command.id, reason))).await?;
  Ok(false)
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

  #[test]
  fn only_a_loopback_endpoint_may_go_without_a_token() {
    assert_eq!(required_token(None, true).unwrap(), None);
    let generated = required_token(None, false).unwrap().expect("generated beyond loopback");
    assert!(generated.len() >= 32 && check_token(&generated).is_ok(), "{generated}");
    assert_ne!(Some(generated), required_token(None, false).unwrap());
    assert_eq!(
      required_token(Some("sashoush-token".into()), true).unwrap().as_deref(),
      Some("sashoush-token")
    );
    assert!(check_token("has space").is_err());
    assert!(check_token("").is_err());
  }

  #[test]
  fn the_token_rides_in_the_endpoint_query_not_the_address() {
    let endpoint = "ws://127.0.0.1:4000/?token=abc-123";
    assert_eq!(endpoint_token(endpoint), Some("abc-123"));
    assert!(matches!(Endpoint::parse(endpoint), Endpoint::Tcp(addr) if addr == "127.0.0.1:4000"));
    assert_eq!(endpoint_token("ws://127.0.0.1:4000"), None);
    assert_eq!(endpoint_token("/tmp/s.sock"), None);
  }

  #[tokio::test]
  async fn a_token_endpoint_refuses_every_connection_that_does_not_present_it() {
    let server = SessionServer::bind_with_token(
      Endpoint::Tcp("127.0.0.1:0".into()),
      Arc::new(EchoDispatcher),
      Some("sashoush-secret".into()),
    )
    .await
    .unwrap();
    let endpoint = server.endpoint_string().to_string();
    assert!(endpoint.ends_with("/?token=sashoush-secret"), "{endpoint}");
    let _serving = tokio::spawn(async move { server.serve().await });

    let mut authorized = SessionClient::connect(&endpoint).await.unwrap();
    assert!(
      authorized
        .call(Command::new(1, "echo", serde_json::json!({})))
        .await
        .unwrap()
        .ok
    );

    let bare = endpoint.split_once('?').unwrap().0;
    let mut anonymous = SessionClient::connect(bare).await.unwrap();
    let refused = anonymous
      .call(Command::new(1, "echo", serde_json::json!({})))
      .await
      .unwrap();
    assert!(!refused.ok);
    assert!(refused.error.unwrap_or_default().contains("requires a token"));
    // Refused means disconnected, not merely answered.
    assert!(
      anonymous
        .call(Command::new(2, "echo", serde_json::json!({})))
        .await
        .is_err()
    );

    let wrong = endpoint.replace("sashoush-secret", "guess");
    assert!(matches!(
      SessionClient::connect(&wrong).await,
      Err(SessionError::Unauthorized(reason)) if reason.contains("rejected")
    ));
  }

  struct UntilHangup(tokio::sync::mpsc::UnboundedSender<&'static str>);

  #[async_trait::async_trait]
  impl Dispatcher for UntilHangup {
    async fn dispatch(&self, command: Command, events: EventSink) -> crate::Response {
      let _ = self.0.send("started");
      events.closed().await;
      let _ = self.0.send("client gone");
      crate::Response::ok(command.id, "")
    }
  }

  #[tokio::test]
  async fn a_streaming_command_learns_its_client_hung_up() {
    let (tx, mut seen) = tokio::sync::mpsc::unbounded_channel();
    let lifecycle = Arc::new(Lifecycle::new("local".into(), Arc::new(UntilHangup(tx))).unwrap());
    let (server, mut client) = tokio::io::duplex(1024);
    let serving = tokio::spawn(serve_connection(server, lifecycle));
    write_frame(&mut client, &Command::new(1, "screencast", serde_json::json!({})))
      .await
      .unwrap();
    assert_eq!(seen.recv().await, Some("started"));
    drop(client);
    let gone = tokio::time::timeout(std::time::Duration::from_secs(5), seen.recv()).await;
    assert_eq!(gone.unwrap(), Some("client gone"));
    serving.await.unwrap().unwrap();
  }

  struct Slow;

  #[async_trait::async_trait]
  impl Dispatcher for Slow {
    async fn dispatch(&self, command: Command, _events: EventSink) -> crate::Response {
      if command.verb == "slow" {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
      }
      crate::Response::ok(command.id, command.verb)
    }
  }

  #[tokio::test]
  async fn commands_sent_ahead_are_answered_in_order() {
    let lifecycle = Arc::new(Lifecycle::new("local".into(), Arc::new(Slow)).unwrap());
    let (server, mut client) = tokio::io::duplex(4096);
    let _serving = tokio::spawn(serve_connection(server, lifecycle));
    for (id, verb) in [(1, "slow"), (2, "fast"), (3, "fast")] {
      write_frame(&mut client, &Command::new(id, verb, serde_json::json!({})))
        .await
        .unwrap();
    }
    let mut pending = Vec::new();
    let mut answered = Vec::new();
    for _ in 0..3 {
      let frame: ServerFrame = read_frame(&mut client, &mut pending).await.unwrap().unwrap();
      if let ServerFrame::Response(response) = frame {
        answered.push((response.id, response.text));
      }
    }
    assert_eq!(
      answered,
      [
        (1, "slow".to_string()),
        (2, "fast".to_string()),
        (3, "fast".to_string())
      ]
    );
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
