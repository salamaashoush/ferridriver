//! NUL-byte-delimited JSON pipe transport — the wire format Playwright's
//! `WebKit` fork uses for its `--inspector-pipe` option.
//!
//! Wire layout: each message is the JSON encoding of an envelope object,
//! terminated by a single NUL (`\x00`) byte. Reader buffers a partial
//! message across `read()` calls until a NUL is seen, then hands the
//! complete payload to the caller. Writer serializes the envelope and
//! appends a NUL.

use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use thiserror::Error;
use tokio::sync::mpsc;

#[derive(Debug, Error)]
pub enum TransportError {
  #[error("transport closed")]
  Closed,
  #[error("io: {0}")]
  Io(#[from] std::io::Error),
  #[error("json: {0}")]
  Json(#[from] serde_json::Error),
}

/// Read half of the pipe transport. Wraps a blocking byte source
/// (typically the child stdout fd 4) and exposes a stream of decoded
/// JSON envelopes via a non-blocking `mpsc` channel — the I/O thread
/// reads bytes synchronously and forwards parsed frames to the caller's
/// tokio runtime.
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
/// the frame — a dedicated writer thread performs the blocking pipe
/// write. Writing inline on the calling task looked safe ("the child
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
}

impl WriterHandle {
  /// Serialize `value` and queue it (with its NUL terminator) for the
  /// writer thread. Errors on JSON encoding failure or when the
  /// writer thread has exited (pipe closed).
  pub fn send(&self, value: &Value) -> Result<(), TransportError> {
    let mut payload = serde_json::to_vec(value)?;
    payload.push(0);
    self.tx.send(payload).map_err(|_| TransportError::Closed)
  }
}

/// Owns both halves of a `--inspector-pipe` connection. Spawns one
/// background thread to drain the reader, exposing the decoded frames
/// via [`ReaderHandle::recv`]. Writes happen synchronously through
/// [`WriterHandle::send`].
pub struct Transport {
  pub reader: ReaderHandle,
  pub writer: WriterHandle,
}

impl Transport {
  /// Construct a transport from raw blocking read + write halves.
  /// Usually called with the `Stdio::piped()` fds 3/4 of a spawned
  /// `pw_run.sh` child. The reader/writer threads are named so they
  /// show up as `webkit-reader` / `webkit-writer` in `tokio-console` /
  /// `ps`.
  /// # Panics
  ///
  /// Panics if the OS refuses to spawn the reader or writer thread
  /// (vanishingly rare — would also block almost everything else in
  /// tokio).
  pub fn new<R, W>(read: R, write: W) -> Self
  where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
  {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::Builder::new()
      .name("webkit-reader".into())
      .spawn(move || drain_reader(read, &tx))
      .unwrap_or_else(|e| panic!("spawn webkit-reader: {e}"));
    let (wtx, wrx) = mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::Builder::new()
      .name("webkit-writer".into())
      .spawn(move || drain_writer(write, wrx))
      .unwrap_or_else(|e| panic!("spawn webkit-writer: {e}"));
    Transport {
      reader: ReaderHandle { rx },
      writer: WriterHandle { tx: wtx },
    }
  }
}

fn drain_writer<W: Write>(mut write: W, mut rx: mpsc::UnboundedReceiver<Vec<u8>>) {
  while let Some(frame) = rx.blocking_recv() {
    if write.write_all(&frame).is_err() || write.flush().is_err() {
      // Pipe gone — the reader thread sees EOF and fails pending
      // callbacks; nothing to report from here.
      break;
    }
  }
}

fn drain_reader<R: Read>(read: R, tx: &mpsc::UnboundedSender<Result<Value, TransportError>>) {
  // `BufRead::read_until` on a NUL terminator gives us exactly one
  // envelope per call. The trailing NUL is included in the returned
  // buffer; we strip it before decoding.
  let mut buf = BufReader::new(read);
  loop {
    let mut frame = Vec::with_capacity(1024);
    match buf.read_until(0, &mut frame) {
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
  use std::sync::{Arc, Mutex};

  struct WriterRef(Arc<Mutex<Vec<u8>>>);
  impl Write for WriterRef {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
      self.0.lock().unwrap().extend_from_slice(b);
      Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
      Ok(())
    }
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
    let buf_handle = Arc::new(Mutex::new(Vec::<u8>::new()));
    let transport = Transport::new(Cursor::new(Vec::<u8>::new()), WriterRef(buf_handle.clone()));
    transport.writer.send(&serde_json::json!({"id": 42})).unwrap();
    // The write happens on the webkit-writer thread — wait for it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
      {
        let buf = buf_handle.lock().unwrap();
        if !buf.is_empty() {
          assert_eq!(&buf[..], b"{\"id\":42}\0");
          break;
        }
      }
      assert!(std::time::Instant::now() < deadline, "writer thread never flushed");
      tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
  }
}
