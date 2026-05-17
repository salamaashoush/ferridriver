#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Smoke tests for the `ferridriver run` subcommand: a standalone
//! script runner where the script launches its own browser via the
//! Playwright-style `chromium()` / `firefox()` / `webkit()` factories.
//!
//! Requires a built `ferridriver` binary (`FERRIDRIVER_BIN` or
//! `target/{debug,release}/ferridriver`) plus Chrome + Firefox,
//! exactly like the `backends` suite.

use std::io::Write as _;
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

/// Run `ferridriver run <extra…>` with `stdin` piped; returns
/// (success, stdout, stderr).
fn run(extra: &[&str], stdin: Option<&str>) -> (bool, String, String) {
  let mut cmd = Command::new(bin());
  cmd.arg("run").args(extra).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
  let mut child = cmd.spawn().expect("spawn ferridriver run");
  if let Some(s) = stdin {
    child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
  } else {
    drop(child.stdin.take());
  }
  let out = child.wait_with_output().expect("wait");
  (
    out.status.success(),
    String::from_utf8_lossy(&out.stdout).into_owned(),
    String::from_utf8_lossy(&out.stderr).into_owned(),
  )
}

#[test]
fn inline_eval_launches_browser_and_returns_value() {
  let (ok, stdout, stderr) = run(
    &[
      "-e",
      "const b = await chromium().launch({ headless: true }); \
       const p = await (await b.newContext()).newPage(); \
       await p.goto('data:text/html,<title>RunCmd</title>'); \
       const t = await p.title(); await b.close(); return t;",
    ],
    None,
  );
  assert!(ok, "exit ok; stderr={stderr}");
  let v: serde_json::Value = serde_json::from_str(&stdout).expect("json stdout");
  assert_eq!(v["status"], "ok", "{v}");
  assert_eq!(v["value"], "RunCmd", "script launched its own browser: {v}");
}

#[test]
fn file_mode_with_positional_args() {
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("s.js");
  std::fs::write(&path, "return { argc: args.length, first: args[0], sum: 1 + 2 };").unwrap();
  let (ok, stdout, _) = run(&[path.to_str().unwrap(), "--", "alpha", "beta"], None);
  assert!(ok);
  let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
  assert_eq!(v["value"]["argc"], 2);
  assert_eq!(v["value"]["first"], "alpha");
  assert_eq!(v["value"]["sum"], 3);
}

#[test]
fn stdin_dash_reads_source() {
  let (ok, stdout, _) = run(&["-"], Some("return 6 * 7;"));
  assert!(ok);
  let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
  assert_eq!(v["value"], 42);
}

#[test]
fn script_error_exits_nonzero() {
  let (ok, stdout, stderr) = run(&["-e", "throw new Error('boom-run')"], None);
  assert!(!ok, "a thrown error must exit nonzero");
  let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
  assert_eq!(v["status"], "error");
  assert!(stderr.contains("boom-run"), "stderr summary: {stderr}");
}

#[test]
fn factories_match_playwright_chromium_is_chromium_firefox_is_firefox() {
  // The Playwright contract: `chromium()` ALWAYS launches Chromium,
  // `firefox()` ALWAYS Firefox. No flag turns one into the other.
  let mk = |factory: &str| {
    format!("const b = await {factory}().launch({{ headless: true }}); const v = await b.version(); await b.close(); return v;")
  };

  let (ok, stdout, stderr) = run(&["-e", &mk("chromium")], None);
  assert!(ok, "chromium exit ok; stderr={stderr}");
  let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
  let got = v["value"].as_str().unwrap_or_default();
  assert!(
    got.starts_with("Chrome/") || got.starts_with("Chromium/") || got.starts_with("HeadlessChrome/"),
    "chromium() must launch Chromium, got version `{got}`"
  );

  let (ok, stdout, stderr) = run(&["-e", &mk("firefox")], None);
  assert!(ok, "firefox exit ok; stderr={stderr}");
  let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
  let got = v["value"].as_str().unwrap_or_default();
  assert!(
    got.to_ascii_lowercase().contains("firefox"),
    "firefox() must launch Firefox, got version `{got}`"
  );
}
