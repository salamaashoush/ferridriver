//! Test fixture: a minimal sidecar speaking the fd 3/4 NUL-delimited JSON
//! protocol. Reads requests on fd 3, writes responses on fd 4. Answers
//! `ping` with `{ok:true}`, `echo` by reflecting `params`, anything else with
//! an `{error}`. Used only by the sidecar transport/binding tests
//! (`env!("CARGO_BIN_EXE_sidecar_echo")`).

#[cfg(unix)]
fn main() {
  use std::io::{Read, Write};
  use std::os::fd::FromRawFd;
  use std::os::unix::net::UnixStream;

  // SAFETY: the parent (the sidecar transport) dup'd its socket ends onto
  // fd 3 (read) and fd 4 (write) before exec; we adopt them here.
  #[allow(unsafe_code)]
  let (mut rx, mut tx) = unsafe { (UnixStream::from_raw_fd(3), UnixStream::from_raw_fd(4)) };

  let mut buf: Vec<u8> = Vec::new();
  let mut chunk = [0u8; 4096];
  loop {
    let n = match rx.read(&mut chunk) {
      Ok(0) | Err(_) => break,
      Ok(n) => n,
    };
    buf.extend_from_slice(&chunk[..n]);
    while let Some(pos) = buf.iter().position(|&b| b == 0) {
      let frame: Vec<u8> = buf.drain(..=pos).collect();
      let frame = &frame[..frame.len() - 1];
      let Ok(v) = serde_json::from_slice::<serde_json::Value>(frame) else {
        continue;
      };
      let id = v.get("id").cloned().unwrap_or(serde_json::Value::Null);
      let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
      let resp = match method {
        "ping" => serde_json::json!({ "id": id, "result": { "ok": true } }),
        "echo" => {
          serde_json::json!({ "id": id, "result": v.get("params").cloned().unwrap_or(serde_json::Value::Null) })
        },
        other => {
          serde_json::json!({ "id": id, "error": { "code": -1, "message": format!("unknown method: {other}") } })
        },
      };
      let Ok(mut out) = serde_json::to_vec(&resp) else {
        continue;
      };
      out.push(0);
      if tx.write_all(&out).is_err() {
        return;
      }
      let _ = tx.flush();
    }
  }
}

#[cfg(not(unix))]
fn main() {}
