//! Test fixture: a minimal sidecar speaking the fd 3/4 NUL-delimited JSON
//! protocol. Reads requests on fd 3, writes responses on fd 4. Answers
//! `ping` with `{ok:true}`, `echo` by reflecting `params`, `emit` by pushing
//! an id-less event frame `{method, params}` before acking, `exit` by acking
//! then quitting, and anything else with an `{error}`. Used only by the
//! sidecar transport/binding tests
//! (`env!("CARGO_BIN_EXE_sidecar_echo")`).

#[cfg(unix)]
#[expect(
  unsafe_code,
  reason = "the sidecar contract hands the sockets over as the raw descriptor numbers 3 and 4"
)]
fn main() {
  use std::os::fd::FromRawFd;
  use std::os::unix::net::UnixStream;

  // SAFETY: the parent (the sidecar transport) dup'd its request socket onto
  // fd 3 before exec, and nothing else in this process owns it.
  let rx = unsafe { UnixStream::from_raw_fd(3) };
  // SAFETY: likewise for the response socket on fd 4.
  let tx = unsafe { UnixStream::from_raw_fd(4) };
  serve(rx, tx);
}

#[cfg(windows)]
#[expect(
  unsafe_code,
  reason = "the sidecar contract hands the pipes over as the C runtime descriptors 3 and 4"
)]
fn main() {
  use std::os::windows::io::{FromRawHandle, RawHandle};

  // SAFETY: the parent handed its request pipe over as C runtime descriptor
  // 3, so the runtime holds a valid handle for it.
  let rx_handle = unsafe { libc::get_osfhandle(3) } as RawHandle;
  // SAFETY: likewise for the response pipe on descriptor 4.
  let tx_handle = unsafe { libc::get_osfhandle(4) } as RawHandle;
  // SAFETY: the handle came from descriptor 3 above, and nothing else in
  // this process takes ownership of it.
  let rx = unsafe { std::fs::File::from_raw_handle(rx_handle) };
  // SAFETY: likewise for the handle behind descriptor 4.
  let tx = unsafe { std::fs::File::from_raw_handle(tx_handle) };
  serve(rx, tx);
}

fn serve(mut rx: impl std::io::Read, mut tx: impl std::io::Write) {
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
        "emit" => {
          // Push an id-less event frame first, then ack. Ordering matters
          // for the tests: the event is on the wire before `send` resolves.
          let params = v.get("params").cloned().unwrap_or(serde_json::Value::Null);
          let event = params.get("event").and_then(|e| e.as_str()).unwrap_or("");
          let payload = params.get("payload").cloned().unwrap_or(serde_json::Value::Null);
          let pushed = serde_json::json!({ "method": event, "params": payload });
          if let Ok(mut ev) = serde_json::to_vec(&pushed) {
            ev.push(0);
            if tx.write_all(&ev).is_err() {
              return;
            }
            let _ = tx.flush();
          }
          serde_json::json!({ "id": id, "result": { "ok": true } })
        },
        "exit" => {
          // Ack, then quit — lets tests observe the parent-side EOF path
          // (read loop exit marking the transport closed).
          let resp = serde_json::json!({ "id": id, "result": { "ok": true } });
          if let Ok(mut out) = serde_json::to_vec(&resp) {
            out.push(0);
            let _ = tx.write_all(&out);
            let _ = tx.flush();
          }
          return;
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
