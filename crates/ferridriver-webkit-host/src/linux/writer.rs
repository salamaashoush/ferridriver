//! Reply helpers. Frames are produced on the GTK main thread (dispatch
//! handlers + signal callbacks) and shipped to the dedicated writer
//! thread via [`super::WRITER_TX`]. The writer thread owns the socket
//! and performs the synchronous `frame_write` — keeping it off the
//! main thread is critical, see [`super::Frame`] doc.

use ferridriver_webkit_wire::{Rep, encode_str, encode_view_created, encode_view_list};

fn write(req_id: u32, rep: Rep, payload: &[u8]) {
  super::WRITER_TX.with(|tx| {
    if let Some(s) = tx.borrow().as_ref() {
      if s
        .send(super::Frame {
          rid: req_id,
          rep: rep as u8,
          payload: payload.to_vec(),
        })
        .is_err()
      {
        tracing::error!("writer thread is gone; dropped {rep:?} reply (req_id={req_id})");
      }
    }
  });
}

pub(crate) fn ok(req_id: u32) {
  write(req_id, Rep::Ok, &[]);
}

pub(crate) fn error(req_id: u32, msg: &str) {
  write(req_id, Rep::Error, &encode_str(msg));
}

pub(crate) fn unsupported(req_id: u32, what: &str) {
  error(req_id, &format!("unsupported: {what}"));
}

pub(crate) fn value_string(req_id: u32, s: &str) {
  // `IpcResponse::Value` payload is a single string the parent parses
  // as JSON; for plain strings we wrap them in JSON quotes so the
  // parent's `serde_json::from_str` succeeds and yields
  // `Value::String(...)`.
  let json = serde_json::to_string(s).unwrap_or_else(|_| String::from("\"\""));
  write(req_id, Rep::Value, &encode_str(&json));
}

pub(crate) fn value_raw_json(req_id: u32, json: &str) {
  // For values that are ALREADY JSON-encoded (e.g. the eval-body
  // wrapper has done JSON.stringify on the page side). Skip the
  // double-encode.
  write(req_id, Rep::Value, &encode_str(json));
}

pub(crate) fn view_created(req_id: u32, view_id: u64) {
  write(req_id, Rep::ViewCreated, &encode_view_created(view_id));
}

pub(crate) fn view_list(req_id: u32, ids: &[u64]) {
  write(req_id, Rep::ViewList, &encode_view_list(ids));
}

/// `Rep::ShmScreenshot` reply. Payload built by
/// `dispatch::write_shm` (`u32 nameLen + name + u32 pngLen`). The
/// parent's `decode_shm_screenshot` (in
/// `crates/ferridriver/src/backend/webkit/ipc.rs`) opens the named
/// shared memory and reads `pngLen` bytes; we created and filled the
/// segment but did NOT unlink — the parent owns cleanup.
pub(crate) fn write_shm_screenshot(req_id: u32, payload: &[u8]) {
  write(req_id, Rep::ShmScreenshot, payload);
}

/// Streamed unsolicited `Rep::NetRequestEvent`. Payload is
/// `str id + str method + str url + str resource_type` — the parent's
/// reader thread routes these into `network_log` and the
/// `drain_network_events` consumer resets the `InjectedScriptManager`
/// latch when `resource_type == "Document"`. Emitted from the host's
/// `LoadEvent::Started` signal so main-frame navigations land in the
/// network log AND clear the page-world selector engine.
pub(crate) fn network_request_event(id: &str, method: &str, url: &str, resource_type: &str) {
  let mut buf = Vec::with_capacity(16 + id.len() + method.len() + url.len() + resource_type.len());
  ferridriver_webkit_wire::str_encode(&mut buf, id);
  ferridriver_webkit_wire::str_encode(&mut buf, method);
  ferridriver_webkit_wire::str_encode(&mut buf, url);
  ferridriver_webkit_wire::str_encode(&mut buf, resource_type);
  write(0, Rep::NetRequestEvent, &buf);
}

/// Streamed unsolicited `Rep::ConsoleEvent`. Payload: `str level +
/// str text + u64 view_id`. Parent's reader pushes onto `console_log`.
pub(crate) fn console_event(level: &str, text: &str, view_id: u64) {
  let mut buf = Vec::with_capacity(16 + level.len() + text.len());
  ferridriver_webkit_wire::str_encode(&mut buf, level);
  ferridriver_webkit_wire::str_encode(&mut buf, text);
  buf.extend_from_slice(&view_id.to_le_bytes());
  write(0, Rep::ConsoleEvent, &buf);
}

/// Streamed unsolicited `Rep::DialogEvent`. Payload: `str type +
/// str message + str action`. Parent's `DialogManager` consumes.
pub(crate) fn dialog_event(dialog_type: &str, message: &str, action: &str) {
  let mut buf = Vec::with_capacity(16 + dialog_type.len() + message.len() + action.len());
  ferridriver_webkit_wire::str_encode(&mut buf, dialog_type);
  ferridriver_webkit_wire::str_encode(&mut buf, message);
  ferridriver_webkit_wire::str_encode(&mut buf, action);
  write(0, Rep::DialogEvent, &buf);
}

/// Streamed unsolicited `Rep::NetResponseEvent`. Payload: `str
/// json` where json is a single object `{id, status, statusText,
/// url, headers}` — matches the macOS host's encoding so the parent's
/// `decode_network_response_event` parses it identically.
pub(crate) fn network_response_event_json(json: &str) {
  let buf = encode_str(json);
  write(0, Rep::NetResponseEvent, &buf);
}

/// Streamed unsolicited `Rep::NetFailureEvent`. Payload: `str id + str error_text`.
pub(crate) fn network_failure_event(id: &str, error_text: &str) {
  let mut buf = Vec::with_capacity(8 + id.len() + error_text.len());
  ferridriver_webkit_wire::str_encode(&mut buf, id);
  ferridriver_webkit_wire::str_encode(&mut buf, error_text);
  write(0, Rep::NetFailureEvent, &buf);
}
