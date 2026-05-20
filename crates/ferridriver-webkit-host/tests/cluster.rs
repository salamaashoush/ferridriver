//! Phase 2b smoke test: drive the host through the first webkit6 op
//! cluster end-to-end. Spawns a real `webkit6::WebView`, navigates to
//! `about:blank`, evaluates a JS expression, asserts the result, then
//! shuts down cleanly.
//!
//! Requires a display (X / Wayland / xvfb). On CI we wrap the test
//! binary with `xvfb-run -a`. Local dev (Arch on user's box) typically
//! has DISPLAY set, so this runs unwrapped.

#![cfg(target_os = "linux")]

use ferridriver_webkit_wire::{Op, Rep, frame_read, frame_write, str_encode};
use std::io::{self, Write};
use std::os::unix::io::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

fn host_binary_path() -> std::path::PathBuf {
  env!("CARGO_BIN_EXE_ferridriver-webkit-host").into()
}

fn spawn_host() -> io::Result<(UnixStream, Child)> {
  let (parent, child) = UnixStream::pair()?;
  let child_fd = child.into_raw_fd();
  #[allow(unsafe_code)]
  let proc = unsafe {
    let mut cmd = Command::new(host_binary_path());
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::inherit());
    cmd.pre_exec(move || {
      let flags = libc::fcntl(child_fd, libc::F_GETFD);
      if flags != -1 {
        libc::fcntl(child_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
      }
      if child_fd != 3 {
        if libc::dup2(child_fd, 3) == -1 {
          return Err(io::Error::last_os_error());
        }
        libc::close(child_fd);
      }
      Ok(())
    });
    cmd.spawn()?
  };
  Ok((parent, proc))
}

/// Detect whether webkit6/GTK can actually run in this environment.
/// Skips the rest of the test cleanly when neither `DISPLAY` nor
/// `WAYLAND_DISPLAY` is set — that's the headless CI case where the
/// test must run under `xvfb-run -a` or it can't start.
fn has_display() -> bool {
  std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

fn send_str_vid(w: &mut UnixStream, rid: u32, op: Op, s: &str, vid: u64) -> io::Result<()> {
  let mut buf = Vec::with_capacity(4 + s.len() + 8);
  str_encode(&mut buf, s);
  buf.extend_from_slice(&vid.to_le_bytes());
  frame_write(w, rid, op as u8, &buf)
}

fn send_str(w: &mut UnixStream, rid: u32, op: Op, s: &str) -> io::Result<()> {
  let mut buf = Vec::with_capacity(4 + s.len());
  str_encode(&mut buf, s);
  frame_write(w, rid, op as u8, &buf)
}

fn send_vid(w: &mut UnixStream, rid: u32, op: Op, vid: u64) -> io::Result<()> {
  frame_write(w, rid, op as u8, &vid.to_le_bytes())
}

/// Read frames until we get a reply with `rid != 0`. Skips streamed
/// events (`NetRequestEvent`, `ConsoleEvent`, etc.) emitted with
/// `rid=0`. The parent's `IpcClient` reader does the same routing; we
/// inline it here so the raw test can talk to the host without
/// reimplementing the full reader.
fn read_reply(s: &mut UnixStream) -> io::Result<(u32, u8, Vec<u8>)> {
  loop {
    let (rid, rep, payload) = frame_read(s)?;
    if rid != 0 {
      return Ok((rid, rep, payload));
    }
  }
}

#[test]
fn create_view_evaluate_close_round_trip() -> io::Result<()> {
  if !has_display() {
    eprintln!("skipping cluster test: no DISPLAY / WAYLAND_DISPLAY (re-run under xvfb-run)");
    return Ok(());
  }

  let (mut sock, mut child) = spawn_host()?;
  let mut writer = sock.try_clone()?;
  writer.flush()?;

  // 1. CreateView "" — returns Rep::ViewCreated with view_id
  send_str(&mut writer, 1, Op::CreateView, "")?;
  let (rid, rep, payload) = read_reply(&mut sock)?;
  assert_eq!(rid, 1, "echoed req_id");
  assert_eq!(
    rep,
    Rep::ViewCreated as u8,
    "expected ViewCreated, got rep={rep} payload={payload:?}"
  );
  let view_id = u64::from_le_bytes([
    payload[0], payload[1], payload[2], payload[3], payload[4], payload[5], payload[6], payload[7],
  ]);
  assert!(view_id > 0, "view_id assigned");

  // 2. Navigate about:blank
  send_str_vid(&mut writer, 2, Op::Navigate, "about:blank", view_id)?;
  let (rid, rep, _) = read_reply(&mut sock)?;
  assert_eq!(rid, 2);
  assert_eq!(rep, Rep::Ok as u8, "Navigate ok");

  // 3. Evaluate `1 + 2` — the eval-body wrapper JSON.stringifies, so
  //    we get back `"3"` (a JSON-encoded number).
  send_str_vid(&mut writer, 3, Op::Evaluate, "1 + 2", view_id)?;
  let (rid, rep, payload) = read_reply(&mut sock)?;
  assert_eq!(rid, 3);
  assert_eq!(
    rep,
    Rep::Value as u8,
    "Evaluate value, got rep={rep} payload={payload:?}"
  );
  let n = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
  let json = std::str::from_utf8(&payload[4..4 + n]).expect("utf-8 payload");
  assert_eq!(json.trim(), "3", "JSON.stringify(1+2) == \"3\"");

  // 4. GetUrl — should be "about:blank"
  send_vid(&mut writer, 4, Op::GetUrl, view_id)?;
  let (rid, rep, payload) = read_reply(&mut sock)?;
  assert_eq!(rid, 4);
  assert_eq!(rep, Rep::Value as u8);
  let n = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
  let url_json = std::str::from_utf8(&payload[4..4 + n]).expect("utf-8 url");
  assert!(url_json.contains("about:blank"), "GetUrl payload {url_json:?}");

  // 5. Close view
  send_vid(&mut writer, 5, Op::Close, view_id)?;
  let (rid, rep, _) = read_reply(&mut sock)?;
  assert_eq!(rid, 5);
  assert_eq!(rep, Rep::Ok as u8);

  // 6. Shutdown — host exits without a reply
  frame_write(&mut writer, 6, Op::Shutdown as u8, &[])?;
  let status = child.wait()?;
  assert!(status.success(), "host exited cleanly: {status:?}");
  Ok(())
}

#[test]
fn get_webkit_version_returns_webkitgtk_string() -> io::Result<()> {
  if !has_display() {
    eprintln!("skipping cluster test: no DISPLAY / WAYLAND_DISPLAY");
    return Ok(());
  }
  let (mut sock, mut child) = spawn_host()?;
  let mut writer = sock.try_clone()?;

  frame_write(&mut writer, 1, Op::GetWebKitVersion as u8, &[])?;
  let (rid, rep, payload) = read_reply(&mut sock)?;
  assert_eq!(rid, 1);
  assert_eq!(rep, Rep::Value as u8);
  let n = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
  let s = std::str::from_utf8(&payload[4..4 + n]).expect("utf-8");
  assert!(
    s.contains("WebKitGTK/"),
    "version string {s:?} should contain WebKitGTK/"
  );

  frame_write(&mut writer, 2, Op::Shutdown as u8, &[])?;
  let _ = child.wait();
  Ok(())
}
