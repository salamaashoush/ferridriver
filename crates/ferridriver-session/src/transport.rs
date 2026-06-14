//! NUL-delimited-JSON framing over an async byte stream.
//!
//! One frame = the UTF-8 JSON encoding of a value followed by a single NUL
//! byte. The functions here are generic over any [`tokio::io::AsyncRead`] /
//! [`tokio::io::AsyncWrite`], so the same code drives a Unix-domain socket, a
//! Windows named pipe, or an in-memory duplex used by tests.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{Result, SessionError};

/// Largest single frame accepted on the wire. A snapshot of a large DOM is
/// the biggest legitimate payload; 64 MiB is far above that and bounds a
/// hostile or corrupt peer from growing the read buffer without limit.
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Serialize `value` to JSON, append a NUL, and write the whole frame.
///
/// # Errors
///
/// Returns [`SessionError::Json`] if `value` fails to serialize and
/// [`SessionError::Io`] on a write failure.
pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<()>
where
  W: AsyncWrite + Unpin,
  T: Serialize,
{
  let mut buf = serde_json::to_vec(value)?;
  buf.push(0);
  writer.write_all(&buf).await?;
  writer.flush().await?;
  Ok(())
}

/// Read bytes from `reader` into `pending` until a NUL terminates a frame,
/// then decode the frame as `T`. `pending` carries any bytes that arrived
/// after the NUL into the next call, so a single read syscall may deliver
/// several frames.
///
/// Returns `Ok(None)` on a clean EOF at a frame boundary (peer hung up with
/// no partial frame buffered).
///
/// # Errors
///
/// Returns [`SessionError::ConnectionClosed`] if EOF arrives mid-frame,
/// [`SessionError::Json`] if a complete frame fails to decode as `T`, and
/// [`SessionError::Io`] on a read failure or a frame exceeding the size cap.
pub async fn read_frame<R, T>(reader: &mut R, pending: &mut Vec<u8>) -> Result<Option<T>>
where
  R: AsyncRead + Unpin,
  T: DeserializeOwned,
{
  // Heap-allocate the scratch buffer so it doesn't bloat the returned future
  // (this fn is awaited inside per-connection and per-call futures).
  let mut chunk = vec![0u8; 8192];
  loop {
    if let Some(nul) = pending.iter().position(|&b| b == 0) {
      let frame: Vec<u8> = pending.drain(..=nul).collect();
      // `frame` includes the trailing NUL; decode everything before it.
      let value = serde_json::from_slice::<T>(&frame[..frame.len() - 1])?;
      return Ok(Some(value));
    }
    if pending.len() > MAX_FRAME_BYTES {
      return Err(SessionError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("session frame exceeded {MAX_FRAME_BYTES} bytes without a terminator"),
      )));
    }
    let n = reader.read(&mut chunk).await?;
    if n == 0 {
      if pending.is_empty() {
        return Ok(None);
      }
      return Err(SessionError::ConnectionClosed);
    }
    pending.extend_from_slice(&chunk[..n]);
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::protocol::{Command, Response};

  #[tokio::test]
  async fn single_frame_roundtrips_over_duplex() {
    let (mut a, mut b) = tokio::io::duplex(1024);
    let cmd = Command::new(1, "snapshot", serde_json::json!({}));
    write_frame(&mut a, &cmd).await.unwrap();
    let mut pending = Vec::new();
    let got: Command = read_frame(&mut b, &mut pending).await.unwrap().unwrap();
    assert_eq!(got.verb, "snapshot");
    assert!(pending.is_empty());
  }

  #[tokio::test]
  async fn two_frames_in_one_write_decode_separately() {
    let (mut a, mut b) = tokio::io::duplex(4096);
    write_frame(&mut a, &Response::ok(1, "first")).await.unwrap();
    write_frame(&mut a, &Response::ok(2, "second")).await.unwrap();
    let mut pending = Vec::new();
    let r1: Response = read_frame(&mut b, &mut pending).await.unwrap().unwrap();
    let r2: Response = read_frame(&mut b, &mut pending).await.unwrap().unwrap();
    assert_eq!(r1.text, "first");
    assert_eq!(r2.text, "second");
  }

  #[tokio::test]
  async fn clean_eof_at_boundary_returns_none() {
    let (a, mut b) = tokio::io::duplex(64);
    drop(a);
    let mut pending = Vec::new();
    let got: Option<Command> = read_frame(&mut b, &mut pending).await.unwrap();
    assert!(got.is_none());
  }

  #[tokio::test]
  async fn truncated_frame_is_connection_closed() {
    let (mut a, mut b) = tokio::io::duplex(64);
    a.write_all(b"{\"id\":1,\"verb\":\"x\"").await.unwrap(); // no NUL
    drop(a);
    let mut pending = Vec::new();
    let err = read_frame::<_, Command>(&mut b, &mut pending).await.unwrap_err();
    assert!(matches!(err, SessionError::ConnectionClosed));
  }
}
