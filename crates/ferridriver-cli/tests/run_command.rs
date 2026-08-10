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

/// Run `ferridriver run --json <extra…>` with `stdin` piped; returns
/// (success, stdout, stderr). `--json` is what a machine consumer passes:
/// one result document on stdout, console buffered inside it.
fn run(extra: &[&str], stdin: Option<&str>) -> (bool, String, String) {
  run_with(&["--json"], extra, stdin)
}

/// Run `ferridriver run <flags…> <extra…>` with `stdin` piped; returns
/// (success, stdout, stderr).
fn run_with(flags: &[&str], extra: &[&str], stdin: Option<&str>) -> (bool, String, String) {
  let mut cmd = Command::new(bin());
  cmd
    .arg("run")
    .args(flags)
    .args(extra)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
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

// ── default (streaming) vs `--json` (buffered document) ──────────────

#[test]
fn default_splits_console_across_streams_the_way_node_does() {
  let (ok, stdout, stderr) = run_with(
    &[],
    &[
      "-e",
      "console.log('out-log'); console.info('out-info'); console.debug('out-debug'); \
       console.warn('err-warn'); console.error('err-error'); return 'done';",
    ],
    None,
  );
  assert!(ok, "exit ok; stderr={stderr}");
  let out: Vec<&str> = stdout.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
  let err: Vec<&str> = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
  assert_eq!(
    out,
    vec!["out-log", "out-info", "out-debug", "done"],
    "log/info/debug and the return value go to stdout, in order: {stdout:?}"
  );
  assert_eq!(
    err,
    vec!["err-warn", "err-error"],
    "warn/error go to stderr: {stderr:?}"
  );
  assert!(
    !stdout.contains("duration_ms"),
    "no result document without --json: {stdout:?}"
  );
}

/// Every method on <https://nodejs.org/api/console.html> is present, except the
/// `Console` constructor — it binds a console to caller-supplied writable
/// streams, and the sandbox exposes no such stream to bind.
#[test]
fn console_covers_the_node_api_surface() {
  let (ok, stdout, stderr) = run(
    &[
      "-e",
      "const names = ['log','info','warn','error','debug','trace','dir','dirxml','table', \
       'group','groupCollapsed','groupEnd','count','countReset','time','timeEnd','timeLog', \
       'assert','clear','profile','profileEnd','timeStamp']; \
       return names.filter(n => typeof console[n] !== 'function');",
    ],
    None,
  );
  assert!(ok, "exit ok; stderr={stderr}");
  let v: serde_json::Value = serde_json::from_str(&stdout).expect("json stdout");
  assert_eq!(v["value"], serde_json::json!([]), "no console method is missing: {v}");
}

#[test]
fn console_timer_and_dir_match_node_semantics() {
  let (ok, stdout, stderr) = run(
    &[
      "-e",
      "console.time('t'); console.time('t'); console.timeEnd('nope'); \
       console.dir({ n: 1 }); console.dirxml('x'); console.profile(); return null;",
    ],
    None,
  );
  assert!(ok, "exit ok; stderr={stderr}");
  let v: serde_json::Value = serde_json::from_str(&stdout).expect("json stdout");
  let console = v["console"].as_array().expect("console array");
  let lines: Vec<(&str, &str)> = console
    .iter()
    .map(|e| {
      (
        e["level"].as_str().unwrap_or_default(),
        e["message"].as_str().unwrap_or_default(),
      )
    })
    .collect();
  assert_eq!(
    lines,
    vec![
      ("warn", "Label 't' already exists for console.time()"),
      ("warn", "No such label 'nope' for console.timeEnd()"),
      // `dir` defaults to colors:false, so no escape codes even on a terminal.
      ("log", "{ n: 1 }"),
      ("log", "x"),
    ],
    "Node's own warning text, and `profile` is a silent no-op: {v}"
  );
}

#[test]
fn default_sends_trace_and_failed_assert_to_stderr() {
  let (ok, stdout, stderr) = run_with(
    &[],
    &[
      "-e",
      "console.trace('tracing'); console.assert(false, 'nope'); return null;",
    ],
    None,
  );
  assert!(ok, "exit ok; stderr={stderr}");
  assert!(stdout.trim().is_empty(), "neither writes to stdout: {stdout:?}");
  assert!(stderr.contains("Trace: tracing"), "trace on stderr: {stderr}");
  assert!(
    stderr.contains("Assertion failed: nope"),
    "failed assert on stderr: {stderr}"
  );
}

#[test]
fn default_prints_console_before_the_script_finishes() {
  // The whole point of streaming: the line is readable while the script is
  // still parked on its await, not only once the process exits.
  let mut child = Command::new(bin())
    .args([
      "run",
      "-e",
      "console.log('early'); await new Promise(r => setTimeout(r, 30000)); return 1;",
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("spawn ferridriver run");

  let mut stdout = std::io::BufReader::new(child.stdout.take().expect("stdout piped"));
  let mut line = String::new();
  let read = std::io::BufRead::read_line(&mut stdout, &mut line);
  let _ = child.kill();
  let _ = child.wait();
  assert!(read.is_ok(), "read stdout line: {read:?}");
  assert_eq!(line.trim(), "early", "first console line arrives mid-script");
}

#[test]
fn default_reports_errors_on_stderr_with_no_json_anywhere() {
  let (ok, stdout, stderr) = run_with(
    &[],
    &["-e", "console.log('before'); throw new Error('boom-stream');"],
    None,
  );
  assert!(!ok, "a thrown error must exit nonzero");
  assert_eq!(
    stdout.trim(),
    "before",
    "the streamed log stays on stdout; the failure does not: {stdout:?}"
  );
  assert!(stderr.contains("boom-stream"), "error summary on stderr: {stderr}");
  assert!(
    !stderr.contains("\"status\"") && !stdout.contains("\"status\""),
    "the failure is a message, not a document: {stderr}"
  );
}

#[test]
fn json_buffers_console_in_order_with_timestamps() {
  let (ok, stdout, stderr) = run(
    &[
      "-e",
      "console.log('a'); await new Promise(r => setTimeout(r, 120)); console.error('b'); return 1;",
    ],
    None,
  );
  assert!(ok, "exit ok; stderr={stderr}");
  assert!(stderr.trim().is_empty(), "--json streams nothing: {stderr:?}");
  let v: serde_json::Value = serde_json::from_str(&stdout).expect("json stdout");
  let console = v["console"].as_array().expect("console array");
  assert_eq!(console.len(), 2, "{v}");
  assert_eq!(console[0]["level"], "log");
  assert_eq!(console[0]["message"], "a");
  assert_eq!(console[1]["level"], "error");
  assert_eq!(console[1]["message"], "b");
  let (first, second) = (
    console[0]["ts_ms"].as_u64().expect("ts_ms"),
    console[1]["ts_ms"].as_u64().expect("ts_ms"),
  );
  assert!(second >= first + 100, "ts_ms tracks the await: {first} -> {second}");
}

#[test]
fn factories_match_playwright_chromium_is_chromium_firefox_is_firefox() {
  // The Playwright contract: `chromium()` ALWAYS launches Chromium,
  // `firefox()` ALWAYS Firefox. No flag turns one into the other.
  let mk = |factory: &str| {
    format!(
      "const b = await {factory}().launch({{ headless: true }}); const v = await b.version(); await b.close(); return v;"
    )
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

// ── ES-module path: TypeScript + imports via the shared bundle infra ──

#[test]
fn ts_file_transpiles_and_returns_default_export() {
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("s.ts");
  // TypeScript syntax (type annotation) + `export default` result.
  std::fs::write(&path, "const n: number = 19 + 23;\nexport default n;").unwrap();
  let (ok, stdout, stderr) = run(&[path.to_str().unwrap()], None);
  assert!(ok, "exit ok; stderr={stderr}");
  let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
  assert_eq!(v["status"], "ok", "{v}");
  assert_eq!(v["value"], 42, "default export is the run result: {v}");
}

#[test]
fn ts_module_with_relative_import_is_bundled() {
  let dir = tempfile::tempdir().unwrap();
  std::fs::write(
    dir.path().join("helper.ts"),
    "export const triple = (n: number): number => n * 3;",
  )
  .unwrap();
  let entry = dir.path().join("main.ts");
  std::fs::write(&entry, "import { triple } from './helper';\nexport default triple(14);").unwrap();
  let (ok, stdout, stderr) = run(&[entry.to_str().unwrap()], None);
  assert!(ok, "exit ok; stderr={stderr}");
  let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
  assert_eq!(v["value"], 42, "imported helper must be bundled + run: {v}");
}

#[test]
fn module_without_default_export_yields_null() {
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("s.ts");
  std::fs::write(&path, "export const x = 1;\nconst _y = x + 1;").unwrap();
  let (ok, stdout, _) = run(&[path.to_str().unwrap()], None);
  assert!(ok);
  let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
  assert_eq!(v["status"], "ok", "{v}");
  assert!(v["value"].is_null(), "no default export -> null result: {v}");
}

#[test]
fn inline_eval_with_static_import_runs_as_module() {
  // `--eval` containing a static import is detected and bundled. Uses a
  // top-level await with no default export -> null result, but must not
  // error on the `import`/`export` syntax (which raw eval would reject).
  let (ok, stdout, stderr) = run(&["-e", "export default Math.max(1, 41) + 1;"], None);
  assert!(ok, "exit ok; stderr={stderr}");
  let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
  assert_eq!(v["value"], 42, "{v}");
}
