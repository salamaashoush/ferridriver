//! Phase 2 scaffold smoke test: spawn the host binary, send
//! `Op::ListViews`, confirm empty `Rep::ViewList` comes back, send
//! `Op::Shutdown`, confirm clean exit. Linux-only — non-Linux targets
//! ship a stub `main`.

#![cfg(target_os = "linux")]

use ferridriver_webkit_wire::{Op, Rep, frame_read, frame_write};
use std::io;
use std::os::unix::io::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

fn host_binary_path() -> std::path::PathBuf {
  // `cargo test` builds the binary into the same target dir;
  // `CARGO_BIN_EXE_<name>` is the cargo-supported way to reference it
  // from an integration test.
  env!("CARGO_BIN_EXE_ferridriver-webkit-host").into()
}

fn spawn_host() -> io::Result<(UnixStream, std::process::Child)> {
  let (parent, child) = UnixStream::pair()?;
  let child_fd = child.into_raw_fd();

  // SAFETY: pre_exec runs in the forked child before exec. We clear
  // FD_CLOEXEC on child_fd and dup2 it into place as fd 3, mirroring
  // the parent runtime path in `IpcClient::spawn_host_process`.
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

#[test]
fn list_views_returns_empty_then_shutdown_exits_clean() -> io::Result<()> {
  let (mut sock, mut child) = spawn_host()?;
  let mut writer = sock.try_clone()?;

  // 1. Op::ListViews → expect Rep::ViewList with count = 0
  frame_write(&mut writer, 42, Op::ListViews as u8, &[])?;
  let (rid, rep, payload) = frame_read(&mut sock)?;
  assert_eq!(rid, 42, "echoed req_id");
  assert_eq!(rep, Rep::ViewList as u8, "ListViews reply code");
  assert_eq!(payload.len(), 4, "empty view list payload = u32(0)");
  assert_eq!(u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]), 0);

  // 2. Op::Shutdown — host exits without a reply (fd close is the signal)
  frame_write(&mut writer, 43, Op::Shutdown as u8, &[])?;
  let status = child.wait()?;
  assert!(status.success(), "host exited cleanly: {status:?}");
  Ok(())
}

#[test]
fn unimplemented_op_returns_unsupported_error() -> io::Result<()> {
  let (mut sock, mut child) = spawn_host()?;
  let mut writer = sock.try_clone()?;

  // Unknown op byte 99 — not in the Op enum, must surface as a
  // protocol-level error reply so the parent maps it back to its
  // own `unknown op code` error and doesn't get stuck.
  frame_write(&mut writer, 7, 99, &[])?;
  let (rid, rep, payload) = frame_read(&mut sock)?;
  assert_eq!(rid, 7);
  assert_eq!(rep, Rep::Error as u8, "unknown ops surface as Error");

  let n = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
  let msg =
    std::str::from_utf8(&payload[4..4 + n]).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
  assert!(msg.starts_with("unknown op"), "got {msg:?}");

  frame_write(&mut writer, 8, Op::Shutdown as u8, &[])?;
  let _ = child.wait();
  Ok(())
}
