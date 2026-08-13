//! The session client: connect to a bound browser's endpoint and issue
//! command frames.
//!
//! Used by the `ferridriver` CLI (`run --session`, `session attach`) and by any
//! host that wants to drive another process's bound browser. One client owns
//! one connection; calls are issued sequentially (the CLI runs a single command
//! per invocation, so no pipelining is needed).

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
#[cfg(unix)]
use tokio::net::UnixStream;

use crate::protocol::{Command, Event, Response, ServerFrame};
use crate::registry::Registry;
use crate::server::Endpoint;
use crate::transport::{read_frame, write_frame};
use crate::{Result, SessionError};

/// A connection to one session server.
pub struct SessionClient {
  stream: Stream,
  pending: Vec<u8>,
}

enum Stream {
  #[cfg(unix)]
  Unix(UnixStream),
  Tcp(TcpStream),
}

impl SessionClient {
  /// Connect to a server at the given endpoint string (a Unix socket path or
  /// a `ws://host:port` / `host:port` TCP address).
  ///
  /// # Errors
  ///
  /// Returns [`SessionError::Io`] if the socket / address refuses the
  /// connection.
  pub async fn connect(endpoint: &str) -> Result<Self> {
    let stream = match Endpoint::parse(endpoint) {
      #[cfg(unix)]
      Endpoint::Unix(path) => Stream::Unix(UnixStream::connect(&path).await?),
      Endpoint::Tcp(addr) => Stream::Tcp(TcpStream::connect(&addr).await?),
    };
    Ok(Self {
      stream,
      pending: Vec::new(),
    })
  }

  /// Resolve a session id through the registry and connect to it.
  ///
  /// Returns [`SessionError::NotFound`] when no descriptor exists and
  /// [`SessionError::Unreachable`] when the descriptor's endpoint refuses the
  /// connection (a stale entry whose owner has died) — in which case the
  /// caller may prune the descriptor.
  ///
  /// # Errors
  ///
  /// Returns [`SessionError::NotFound`] if no descriptor exists,
  /// [`SessionError::Unreachable`] if its endpoint refuses the connection,
  /// or [`SessionError::Json`] / [`SessionError::Io`] on a registry read
  /// failure.
  pub async fn attach(registry: &Registry, id: &str) -> Result<Self> {
    let descriptor = registry
      .get(id)?
      .ok_or_else(|| SessionError::NotFound(id.to_string()))?;
    if descriptor.version != crate::WIRE_VERSION {
      return Err(SessionError::VersionMismatch {
        id: id.to_string(),
        theirs: if descriptor.version.is_empty() {
          "an older build".to_string()
        } else {
          descriptor.version.clone()
        },
        ours: crate::WIRE_VERSION.to_string(),
      });
    }
    match Self::connect(&descriptor.endpoint).await {
      Ok(client) => Ok(client),
      Err(SessionError::Io(_)) => Err(SessionError::Unreachable(id.to_string())),
      Err(e) => Err(e),
    }
  }

  /// Send one command and await its response, discarding any events it
  /// streams. Use [`SessionClient::call_with_events`] to observe them.
  ///
  /// # Errors
  ///
  /// Returns [`SessionError::ConnectionClosed`] if the peer hangs up before
  /// answering, or [`SessionError::Io`] / [`SessionError::Json`] on a
  /// transport / decode failure.
  pub async fn call(&mut self, command: Command) -> Result<Response> {
    self.call_with_events(command, |_| {}).await
  }

  /// Send one command, handing each streamed [`Event`] to `on_event` as it
  /// arrives, and await the terminating response.
  ///
  /// `on_event` runs on the read path, so it must not block: the CLI prints
  /// the console line and returns.
  ///
  /// # Errors
  ///
  /// Same as [`SessionClient::call`].
  pub async fn call_with_events<F>(&mut self, command: Command, on_event: F) -> Result<Response>
  where
    F: FnMut(Event),
  {
    match &mut self.stream {
      #[cfg(unix)]
      Stream::Unix(s) => call_on(s, &mut self.pending, command, on_event).await,
      Stream::Tcp(s) => call_on(s, &mut self.pending, command, on_event).await,
    }
  }
}

async fn call_on<S, F>(stream: &mut S, pending: &mut Vec<u8>, command: Command, mut on_event: F) -> Result<Response>
where
  S: AsyncRead + AsyncWrite + Unpin,
  F: FnMut(Event),
{
  let id = command.id;
  write_frame(stream, &command).await?;
  loop {
    let frame: Option<ServerFrame> = read_frame(stream, pending).await?;
    let Some(frame) = frame else {
      return Err(SessionError::ConnectionClosed);
    };
    match frame {
      // The CLI issues one call per connection, but tolerate a stray earlier
      // frame by matching on id rather than assuming strict lockstep.
      ServerFrame::Event(event) if event.id == id => on_event(event),
      ServerFrame::Response(response) if response.id == id => return Ok(response),
      _ => {},
    }
  }
}
