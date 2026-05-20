//! Wire-level binary IPC protocol shared between the ferridriver `WebKit`
//! host (macOS Obj-C / Linux GTK4) and the parent process.
//!
//! Both hosts and the parent client encode and decode using the helpers in
//! this crate. The wire stays byte-identical across platforms — see the
//! crate-level [README](https://github.com/salamaashoush/ferridriver/tree/main/crates/ferridriver-webkit-wire)
//! for the frame format.

#![cfg_attr(not(test), forbid(unsafe_code))]

use std::io::{self, Read, Write};

/// Fixed-size frame header: `u32 payload_len + u32 req_id + u8 op`.
pub const FRAME_HDR: usize = 9;

// ─── Op codes (parent → host) ───────────────────────────────────────────────

/// Parent-to-host request opcodes. Stable u8 discriminants — every Op
/// matched by name in `host.m` AND `ferridriver-webkit-host` must use the
/// same numeric value.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
  CreateView = 1,
  Navigate = 2,
  Evaluate = 3,
  Screenshot = 4,
  Close = 5,
  GoBack = 7,
  GoForward = 8,
  Reload = 9,
  Click = 10,
  Type = 11,
  PressKey = 12,
  KeyDown = 13,
  KeyUp = 14,
  GetUrl = 20,
  GetTitle = 21,
  ListViews = 22,
  SetUserAgent = 30,
  WaitNav = 40,
  SetFileInput = 50,
  SetViewport = 51,
  GetCookies = 60,
  SetCookie = 61,
  DeleteCookie = 62,
  ClearCookies = 63,
  LoadHtml = 64,
  AddInitScript = 65,
  MouseEvent = 66,
  SetLocale = 67,
  SetTimezone = 68,
  EmulateMedia = 69,
  AccessibilityTree = 70,
  /// Route request: sent FROM the host TO the parent when a JS fetch/XHR
  /// matches a route. Payload: str url + str method + str `headers_json` +
  /// str body. Parent responds with `REP_VALUE` containing the serialized
  /// `RouteAction` JSON.
  RouteRequest = 71,
  /// Query the running `WebKit` (macOS) / `WebKitGTK` (Linux) product
  /// version. No payload. Response: `REP_VALUE` with a product string
  /// like `"WebKit/617.1.2 (17618)"` or `"WebKitGTK/2.46.0"`.
  GetWebKitVersion = 72,
  /// Release a `window.__wr` registry entry — the `WebKit` equivalent of
  /// CDP's `Runtime.releaseObject`. Payload: `u64 ref_id + u64 view_id` LE.
  ReleaseRef = 73,
  Shutdown = 255,
}

impl Op {
  /// Map a raw byte to an `Op`, returning `None` for unknown codes.
  #[must_use]
  pub fn from_u8(b: u8) -> Option<Self> {
    Some(match b {
      1 => Self::CreateView,
      2 => Self::Navigate,
      3 => Self::Evaluate,
      4 => Self::Screenshot,
      5 => Self::Close,
      7 => Self::GoBack,
      8 => Self::GoForward,
      9 => Self::Reload,
      10 => Self::Click,
      11 => Self::Type,
      12 => Self::PressKey,
      13 => Self::KeyDown,
      14 => Self::KeyUp,
      20 => Self::GetUrl,
      21 => Self::GetTitle,
      22 => Self::ListViews,
      30 => Self::SetUserAgent,
      40 => Self::WaitNav,
      50 => Self::SetFileInput,
      51 => Self::SetViewport,
      60 => Self::GetCookies,
      61 => Self::SetCookie,
      62 => Self::DeleteCookie,
      63 => Self::ClearCookies,
      64 => Self::LoadHtml,
      65 => Self::AddInitScript,
      66 => Self::MouseEvent,
      67 => Self::SetLocale,
      68 => Self::SetTimezone,
      69 => Self::EmulateMedia,
      70 => Self::AccessibilityTree,
      71 => Self::RouteRequest,
      72 => Self::GetWebKitVersion,
      73 => Self::ReleaseRef,
      255 => Self::Shutdown,
      _ => return None,
    })
  }
}

// ─── Rep codes (host → parent) ──────────────────────────────────────────────

/// Host-to-parent reply codes. The first six are direct replies to a
/// `req_id`. Codes 7..=13 are streamed events: the host fires them
/// unsolicited (no matching pending request); the parent's reader thread
/// routes them to event logs rather than the `pending` oneshot map.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rep {
  Ok = 1,
  Error = 2,
  Value = 3,
  ViewCreated = 4,
  ViewList = 5,
  Binary = 6,
  /// Screenshot delivered via POSIX shared memory. Payload:
  /// `u32 nameLen + name + u32 pngLen`. Parent opens `shm_open(name)`,
  /// reads pngLen bytes, unlinks. Same encoding on macOS and Linux.
  ShmScreenshot = 7,
  ConsoleEvent = 8,
  DialogEvent = 9,
  NetRequestEvent = 10,
  /// Streamed FROM the host TO the parent: a JS fetch/XHR matched a route
  /// pattern. Parent runs the handler and replies via `Op::RouteRequest`
  /// (same numeric code, opposite direction) with the action JSON.
  RouteRequest = 11,
  NetResponseEvent = 12,
  NetFailureEvent = 13,
}

// ─── Frame I/O ──────────────────────────────────────────────────────────────

/// Write a frame: `[len_le, req_id_le, op, payload...]`. Flushes the
/// writer so the frame lands intact even on a buffered transport.
///
/// # Errors
///
/// Returns an error if the payload length exceeds `u32::MAX`, or any of
/// the underlying `write_all`/`flush` calls fail (broken pipe, EIO, etc.).
pub fn frame_write<W: Write>(w: &mut W, req_id: u32, op: u8, payload: &[u8]) -> io::Result<()> {
  // Payload length fits in u32: enforced by `u32::try_from` (returns Err
  // for >= 2^32). Callers building gigantic payloads bubble the error.
  let len =
    u32::try_from(payload.len()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "payload too large"))?;
  let mut h = [0u8; FRAME_HDR];
  h[0..4].copy_from_slice(&len.to_le_bytes());
  h[4..8].copy_from_slice(&req_id.to_le_bytes());
  h[8] = op;
  w.write_all(&h)?;
  if !payload.is_empty() {
    w.write_all(payload)?;
  }
  w.flush()?;
  Ok(())
}

/// Read one complete frame from `r`. Returns `(req_id, op, payload)`.
/// Blocks until the full header + payload arrive. Use this from the host
/// side; the parent uses its own variant that goes through a tokio
/// oneshot dispatcher.
///
/// # Errors
///
/// Returns an error if either `read_exact` call fails — most commonly
/// `UnexpectedEof` when the parent closes the IPC socket.
pub fn frame_read<R: Read>(r: &mut R) -> io::Result<(u32, u8, Vec<u8>)> {
  let mut h = [0u8; FRAME_HDR];
  r.read_exact(&mut h)?;
  let len = u32::from_le_bytes([h[0], h[1], h[2], h[3]]) as usize;
  let req_id = u32::from_le_bytes([h[4], h[5], h[6], h[7]]);
  let op = h[8];
  let mut payload = vec![0u8; len];
  if len > 0 {
    r.read_exact(&mut payload)?;
  }
  Ok((req_id, op, payload))
}

// ─── String encoding ────────────────────────────────────────────────────────

/// Append `[u32 len_le, UTF-8 bytes]` to `buf`.
pub fn str_encode(buf: &mut Vec<u8>, s: &str) {
  // Same panic-on-overflow policy as macOS host.m: strings >= 4GB are not
  // a real input. `u32::try_from` keeps the API total without paying for
  // a Result on every short string.
  let n = u32::try_from(s.len()).unwrap_or(u32::MAX);
  buf.extend_from_slice(&n.to_le_bytes());
  buf.extend_from_slice(s.as_bytes());
}

/// Decode `[u32 len_le, UTF-8 bytes]` from `data` starting at `*off`,
/// advancing `*off`. Returns the empty string on truncated input — same
/// lenient policy as the macOS host's `read_str`.
#[must_use]
pub fn str_decode(data: &[u8], off: &mut usize) -> String {
  if *off + 4 > data.len() {
    return String::new();
  }
  let n = u32::from_le_bytes([data[*off], data[*off + 1], data[*off + 2], data[*off + 3]]) as usize;
  *off += 4;
  if *off + n > data.len() {
    *off = data.len();
    return String::new();
  }
  let s = String::from_utf8_lossy(&data[*off..*off + n]).to_string();
  *off += n;
  s
}

// ─── Streamed-event payload helpers ─────────────────────────────────────────

/// Build the payload for [`Rep::ViewCreated`]: a single LE u64.
#[must_use]
pub fn encode_view_created(view_id: u64) -> [u8; 8] {
  view_id.to_le_bytes()
}

/// Build the payload for [`Rep::ViewList`]: `u32 count_le + count × u64 ids`.
#[must_use]
pub fn encode_view_list(ids: &[u64]) -> Vec<u8> {
  let count = u32::try_from(ids.len()).unwrap_or(u32::MAX);
  let mut out = Vec::with_capacity(4 + ids.len() * 8);
  out.extend_from_slice(&count.to_le_bytes());
  for id in ids {
    out.extend_from_slice(&id.to_le_bytes());
  }
  out
}

/// Build the single-string payload used by [`Rep::Value`] / [`Rep::Error`]
/// and many event Reps (`ConsoleEvent` text, etc).
#[must_use]
pub fn encode_str(s: &str) -> Vec<u8> {
  let mut buf = Vec::with_capacity(4 + s.len());
  str_encode(&mut buf, s);
  buf
}

pub mod js;

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn frame_roundtrip() {
    let mut buf = Vec::new();
    frame_write(&mut buf, 0xDEAD_BEEF, Op::Evaluate as u8, b"hello").unwrap();
    let mut cursor = &buf[..];
    let (rid, op, payload) = frame_read(&mut cursor).unwrap();
    assert_eq!(rid, 0xDEAD_BEEF);
    assert_eq!(op, Op::Evaluate as u8);
    assert_eq!(payload, b"hello");
  }

  #[test]
  fn str_roundtrip_handles_utf8() {
    let mut buf = Vec::new();
    str_encode(&mut buf, "café 🎉");
    let mut off = 0;
    let out = str_decode(&buf, &mut off);
    assert_eq!(out, "café 🎉");
    assert_eq!(off, buf.len());
  }

  #[test]
  fn str_decode_truncated_returns_empty() {
    let buf = [0xFFu8, 0xFF, 0xFF, 0xFF, b'x']; // claims 4GB string, has 1 byte
    let mut off = 0;
    let out = str_decode(&buf, &mut off);
    assert!(out.is_empty());
    assert_eq!(off, buf.len()); // off is parked at end
  }

  #[test]
  fn op_round_trip() {
    for code in [1u8, 2, 3, 22, 71, 72, 73, 255] {
      let op = Op::from_u8(code).unwrap();
      assert_eq!(op as u8, code);
    }
    assert!(Op::from_u8(99).is_none());
  }

  #[test]
  fn view_list_encoding() {
    let buf = encode_view_list(&[1, 2, 3]);
    assert_eq!(&buf[0..4], &3u32.to_le_bytes());
    assert_eq!(&buf[4..12], &1u64.to_le_bytes());
    assert_eq!(&buf[12..20], &2u64.to_le_bytes());
    assert_eq!(&buf[20..28], &3u64.to_le_bytes());
  }

  #[test]
  fn rep_discriminants_are_stable() {
    assert_eq!(Rep::Ok as u8, 1);
    assert_eq!(Rep::Error as u8, 2);
    assert_eq!(Rep::Value as u8, 3);
    assert_eq!(Rep::ViewCreated as u8, 4);
    assert_eq!(Rep::ViewList as u8, 5);
    assert_eq!(Rep::Binary as u8, 6);
    assert_eq!(Rep::ShmScreenshot as u8, 7);
    assert_eq!(Rep::ConsoleEvent as u8, 8);
    assert_eq!(Rep::DialogEvent as u8, 9);
    assert_eq!(Rep::NetRequestEvent as u8, 10);
    assert_eq!(Rep::RouteRequest as u8, 11);
    assert_eq!(Rep::NetResponseEvent as u8, 12);
    assert_eq!(Rep::NetFailureEvent as u8, 13);
  }
}
