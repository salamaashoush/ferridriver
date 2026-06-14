#![allow(clippy::expect_used, clippy::unwrap_used)]
//! End-to-end tests for the `ferridriver session` subcommand: open a session
//! (detached host launches + binds a browser), drive it via `exec`, see it in
//! `list`, then `close` it and confirm the host exits and the registry clears.
//!
//! Requires a built `ferridriver` binary (`FERRIDRIVER_BIN` or
//! `target/{debug,release}/ferridriver`) plus Chrome, like the `backends`
//! suite. The session registry is redirected to a temp dir via
//! `FERRIDRIVER_SESSION_DIR` so the test never touches the user cache.

use std::process::{Command, Stdio};

fn bin() -> String {
  std::env::var("FERRIDRIVER_BIN").unwrap_or_else(|_| {
    let base = format!("{}/../../target", env!("CARGO_MANIFEST_DIR"));
    let debug = format!("{base}/debug/ferridriver");
    if std::path::Path::new(&debug).exists() {
      debug
    } else {
      format!("{base}/release/ferridriver")
    }
  })
}

/// Run `ferridriver session <args…>` with the registry pinned to `session_dir`.
/// Returns (success, stdout, stderr).
fn session(session_dir: &std::path::Path, args: &[&str]) -> (bool, String, String) {
  let out = Command::new(bin())
    .arg("session")
    .args(args)
    .env("FERRIDRIVER_SESSION_DIR", session_dir)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .output()
    .expect("spawn ferridriver session");
  (
    out.status.success(),
    String::from_utf8_lossy(&out.stdout).into_owned(),
    String::from_utf8_lossy(&out.stderr).into_owned(),
  )
}

#[test]
fn session_open_exec_list_close_lifecycle() {
  let dir = tempfile::tempdir().unwrap();
  let id = "itest";

  // open: launches a headless browser, binds it, returns once live.
  let (ok, out, err) = session(
    dir.path(),
    &[
      "open",
      id,
      "--headless",
      "data:text/html,<h1>cli-itest</h1><button>go</button>",
    ],
  );
  assert!(ok, "open failed: {out}{err}");
  assert!(out.contains(&format!("session '{id}' open")), "{out}");

  // list shows the live session.
  let (ok, out, _e) = session(dir.path(), &["list"]);
  assert!(ok);
  assert!(out.contains(id), "list missing session: {out}");

  // exec snapshot reaches the live page.
  let (ok, out, err) = session(dir.path(), &["exec", id, "snapshot"]);
  assert!(ok, "exec snapshot failed: {err}");
  assert!(out.contains("cli-itest"), "snapshot missing page text: {out}");

  // exec url returns the data url.
  let (ok, out, _e) = session(dir.path(), &["exec", id, "url"]);
  assert!(ok);
  assert!(out.contains("data:text/html"), "url wrong: {out}");

  // exec eval runs JS in the page.
  let (ok, out, err) = session(dir.path(), &["exec", id, "eval", "--expression", "1 + 2"]);
  assert!(ok, "eval failed: {err}");
  assert!(out.contains('3'), "eval result wrong: {out}");

  // close stops the session.
  let (ok, out, err) = session(dir.path(), &["close", id]);
  assert!(ok, "close failed: {err}");
  assert!(out.contains("closed"), "{out}");

  // The registry no longer lists it (the host has exited and pruned it).
  // Poll briefly to let the host's graceful shutdown finish.
  let mut cleared = false;
  for _ in 0..40 {
    let (_ok, out, _e) = session(dir.path(), &["list"]);
    if out.contains("no live sessions") {
      cleared = true;
      break;
    }
    std::thread::sleep(std::time::Duration::from_millis(50));
  }
  assert!(cleared, "session still listed after close");
}

#[test]
fn open_twice_same_id_is_rejected() {
  let dir = tempfile::tempdir().unwrap();
  let id = "dup";
  let (ok, _o, err) = session(dir.path(), &["open", id, "--headless"]);
  assert!(ok, "first open failed: {err}");

  let (ok, _o, err) = session(dir.path(), &["open", id, "--headless"]);
  assert!(!ok, "second open should fail");
  assert!(err.contains("already exists"), "unexpected error: {err}");

  let _ = session(dir.path(), &["close", id]);
}

#[test]
fn exec_against_missing_session_errors() {
  let dir = tempfile::tempdir().unwrap();
  let (ok, _o, err) = session(dir.path(), &["exec", "ghost", "url"]);
  assert!(!ok, "exec on missing session should fail");
  assert!(err.contains("ghost"), "error should name the session: {err}");
}
