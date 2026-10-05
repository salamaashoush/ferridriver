//! NUL-byte-delimited JSON pipe transport — the wire format Playwright's
//! `WebKit` fork uses for its `--inspector-pipe` option.
//!
//! Wire layout: each message is the JSON encoding of an envelope object,
//! terminated by a single NUL (`\x00`) byte. Reader buffers a partial
//! message across `read()` calls until a NUL is seen, then hands the
//! complete payload to the caller. Writer serializes the envelope and
//! appends a NUL.

use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};

#[derive(Debug, Error)]
pub enum TransportError {
  #[error("transport closed")]
  Closed,
  #[error("io: {0}")]
  Io(#[from] std::io::Error),
  #[error("json: {0}")]
  Json(#[from] serde_json::Error),
}

/// Read half of the pipe transport, exposing decoded JSON envelopes.
pub struct ReaderHandle {
  rx: mpsc::UnboundedReceiver<Result<Value, TransportError>>,
}

impl ReaderHandle {
  /// Block the calling task until the next message arrives. Returns
  /// `None` when the pipe is closed (EOF on the underlying reader).
  pub async fn recv(&mut self) -> Option<Result<Value, TransportError>> {
    self.rx.recv().await
  }
}

/// Write half of the pipe transport. `send` only serializes and queues
/// the frame — a retained writer task performs the pipe write.
/// Writing inline on the calling task looked safe ("the child
/// reads fd 3 promptly") but was a latent stall: a child that pauses
/// reading fills the pipe buffer, `write_all` then blocks a tokio
/// worker thread, and every other sender serializes behind the mutex.
/// CDP and `BiDi` route outbound bytes through a writer task for the
/// same reason.
///
/// The queue is deliberately unbounded: every producer is a
/// request/response caller that awaits its reply (bounding in-flight
/// volume by concurrent callers), or an event-bounded fire-and-forget
/// ack — there is no producer that can outrun a stalled child without
/// first blocking on it.
pub struct WriterHandle {
  tx: mpsc::UnboundedSender<Vec<u8>>,
  stopping: watch::Receiver<bool>,
}

impl WriterHandle {
  #[cfg(test)]
  pub(crate) fn test_queue() -> (Self, mpsc::UnboundedReceiver<Vec<u8>>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (
      Self {
        tx,
        stopping: watch::channel(false).1,
      },
      rx,
    )
  }

  /// Serialize `value` and queue it (with its NUL terminator) for the
  /// writer task. Errors on JSON encoding failure or a closed transport.
  pub fn send(&self, value: &Value) -> Result<(), TransportError> {
    self.send_checked(value, || Ok(()))
  }

  pub(crate) fn send_checked<E: From<TransportError>>(
    &self,
    value: &impl serde::Serialize,
    check: impl FnOnce() -> Result<(), E>,
  ) -> Result<(), E> {
    let mut payload = serde_json::to_vec(value).map_err(TransportError::from)?;
    payload.push(0);
    check()?;
    if *self.stopping.borrow() {
      return Err(TransportError::Closed.into());
    }
    self.tx.send(payload).map_err(|_| TransportError::Closed.into())
  }
}

/// Owns both halves and the I/O tasks of a `--inspector-pipe` connection.
pub struct Transport {
  pub reader: ReaderHandle,
  pub writer: WriterHandle,
  pub(crate) shutdown: watch::Sender<bool>,
  pub(crate) tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Transport {
  /// Construct a transport from asynchronous pipe halves.
  pub fn new<R, W>(read: R, write: W) -> Self
  where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
  {
    let (tx, rx) = mpsc::unbounded_channel();
    let (shutdown, stopping) = watch::channel(false);
    let mut reader_stop = stopping.clone();
    let reader_ended = shutdown.clone();
    let reader = tokio::spawn(async move {
      tokio::select! {
        biased;
        _ = reader_stop.wait_for(|stopping| *stopping) => {},
        () = drain_reader(read, &tx) => {},
      }
      reader_ended.send_replace(true);
    });
    let (wtx, wrx) = mpsc::unbounded_channel::<Vec<u8>>();
    let mut writer_stop = stopping.clone();
    let writer_ended = shutdown.clone();
    let writer = tokio::spawn(async move {
      tokio::select! {
        biased;
        _ = writer_stop.wait_for(|stopping| *stopping) => {},
        () = drain_writer(write, wrx) => {},
      }
      writer_ended.send_replace(true);
    });
    Transport {
      reader: ReaderHandle { rx },
      writer: WriterHandle { tx: wtx, stopping },
      shutdown,
      tasks: vec![reader, writer],
    }
  }
}

async fn drain_writer<W: AsyncWrite + Unpin>(mut write: W, mut rx: mpsc::UnboundedReceiver<Vec<u8>>) {
  while let Some(frame) = rx.recv().await {
    if write.write_all(&frame).await.is_err() || write.flush().await.is_err() {
      // Pipe gone — the reader thread sees EOF and fails pending
      // callbacks; nothing to report from here.
      break;
    }
  }
}

async fn drain_reader<R: AsyncRead + Unpin>(read: R, tx: &mpsc::UnboundedSender<Result<Value, TransportError>>) {
  // `BufRead::read_until` on a NUL terminator gives us exactly one
  // envelope per call. The trailing NUL is included in the returned
  // buffer; we strip it before decoding.
  let mut buf = BufReader::new(read);
  loop {
    let mut frame = Vec::with_capacity(1024);
    match buf.read_until(0, &mut frame).await {
      Ok(0) => break, // EOF
      Ok(_) => {
        if frame.last() == Some(&0) {
          frame.pop();
        }
        if frame.is_empty() {
          continue;
        }
        let parsed = serde_json::from_slice::<Value>(&frame).map_err(TransportError::Json);
        if tx.send(parsed).is_err() {
          break;
        }
      },
      Err(e) => {
        let _ = tx.send(Err(TransportError::Io(e)));
        break;
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::io::Cursor;
  #[test]
  fn admission_is_checked_after_encoding_without_queueing_rejected_frames() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Encoding<'a>(&'a AtomicBool);
    impl serde::Serialize for Encoding<'_> {
      fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.store(true, Ordering::SeqCst);
        serializer.serialize_str("encoded")
      }
    }
    let encoded = AtomicBool::new(false);
    let (writer, mut receiver) = WriterHandle::test_queue();
    let result = writer.send_checked(&Encoding(&encoded), || {
      if encoded.load(Ordering::SeqCst) {
        Err(TransportError::Closed)
      } else {
        Ok(())
      }
    });
    assert!(result.is_err());
    assert!(receiver.try_recv().is_err());
    assert!(encoded.load(Ordering::SeqCst));
  }

  #[tokio::test]
  async fn reader_decodes_nul_delimited_frames() {
    let payload = b"{\"id\":1}\0{\"method\":\"Foo\"}\0";
    let mut transport = Transport::new(Cursor::new(payload.to_vec()), Vec::<u8>::new());
    let first = transport.reader.recv().await.unwrap().unwrap();
    assert_eq!(first["id"], 1);
    let second = transport.reader.recv().await.unwrap().unwrap();
    assert_eq!(second["method"], "Foo");
    assert!(transport.reader.recv().await.is_none());
  }

  #[tokio::test]
  async fn writer_appends_nul() {
    let (read, _read_peer) = tokio::io::duplex(64);
    let (write, mut output) = tokio::io::duplex(64);
    let transport = Transport::new(read, write);
    transport.writer.send(&serde_json::json!({"id": 42})).unwrap();
    let mut bytes = [0; 10];
    tokio::time::timeout(
      std::time::Duration::from_secs(1),
      tokio::io::AsyncReadExt::read_exact(&mut output, &mut bytes),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(&bytes, b"{\"id\":42}\0");
  }
}
