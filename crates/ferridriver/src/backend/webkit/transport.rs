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
use std::sync::Mutex;
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

/// Write half of the pipe transport. Send is synchronous because the
/// underlying pipe write is non-blocking in practice (the child reads
/// fd 3 promptly) and we want to avoid yet another worker thread.
pub struct WriterHandle {
  inner: Mutex<Box<dyn Write + Send>>,
}

impl WriterHandle {
  /// Serialize `value` and write it followed by a NUL byte. Errors on
  /// IO failure or JSON encoding failure.
  pub fn send(&self, value: &Value) -> Result<(), TransportError> {
    let payload = serde_json::to_vec(value)?;
    let mut guard = self.inner.lock().map_err(|_| TransportError::Closed)?;
    guard.write_all(&payload)?;
    guard.write_all(b"\0")?;
    guard.flush()?;
    Ok(())
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
  /// `pw_run.sh` child. The reader thread is named so it shows up as
  /// `webkit-reader` in `tokio-console` / `ps`.
  /// # Panics
  ///
  /// Panics if the OS refuses to spawn the reader thread (vanishingly
  /// rare — would also block almost everything else in tokio).
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
    Transport {
      reader: ReaderHandle { rx },
      writer: WriterHandle {
        inner: Mutex::new(Box::new(write)),
      },
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
  use std::sync::Arc;

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
    let buf = buf_handle.lock().unwrap();
    assert_eq!(&buf[..], b"{\"id\":42}\0");
  }
}
