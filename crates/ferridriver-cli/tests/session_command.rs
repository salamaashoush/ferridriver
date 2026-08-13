#![allow(clippy::expect_used, clippy::unwrap_used)]
//! End-to-end tests for named sessions: open a session (detached host launches
//! + binds a browser), drive it with `ferridriver run --session`, see it in
//!   `list`, then `close` it and confirm the host exits and the registry
//!   clears.
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

/// Run `ferridriver <args…>` with the registry pinned to `session_dir`.
/// Returns (success, stdout, stderr).
fn ferridriver(session_dir: &std::path::Path, args: &[&str]) -> (bool, String, String) {
  let out = Command::new(bin())
    .args(args)
    .env("FERRIDRIVER_SESSION_DIR", session_dir)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .output()
    .expect("spawn ferridriver");
  (
    out.status.success(),
    String::from_utf8_lossy(&out.stdout).into_owned(),
    String::from_utf8_lossy(&out.stderr).into_owned(),
  )
}

/// `ferridriver session <args…>`.
fn session(session_dir: &std::path::Path, args: &[&str]) -> (bool, String, String) {
  let mut all = vec!["session"];
  all.extend_from_slice(args);
  ferridriver(session_dir, &all)
}

/// `ferridriver run --session <id> --eval <code>`.
fn run_on(session_dir: &std::path::Path, id: &str, code: &str) -> (bool, String, String) {
  ferridriver(session_dir, &["run", "--session", id, "--eval", code])
}

#[test]
fn session_open_run_list_close_lifecycle() {
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

  // attach renders the live page's snapshot.
  let (ok, out, err) = session(dir.path(), &["attach", id]);
  assert!(ok, "attach failed: {err}");
  assert!(out.contains("cli-itest"), "snapshot missing page text: {out}");

  // A script sees the session's live page.
  let (ok, out, err) = run_on(dir.path(), id, "return page.url();");
  assert!(ok, "run failed: {err}");
  assert!(out.contains("data:text/html"), "url wrong: {out}");

  // …and evaluates in it.
  let (ok, out, err) = run_on(dir.path(), id, "return await page.evaluate('1 + 2');");
  assert!(ok, "evaluate failed: {err}");
  assert!(out.contains('3'), "evaluate result wrong: {out}");

  // Console output streams back from the host.
  let (ok, out, err) = run_on(dir.path(), id, "console.log('from-the-host'); return 'done';");
  assert!(ok, "console run failed: {err}");
  assert!(out.contains("from-the-host"), "console line missing: {out}");

  // State persists across runs: the VM and its globals belong to the session.
  let (ok, _o, err) = run_on(dir.path(), id, "globalThis.carried = 41; return null;");
  assert!(ok, "first stateful run failed: {err}");
  let (ok, out, err) = run_on(dir.path(), id, "return globalThis.carried + 1;");
  assert!(ok, "second stateful run failed: {err}");
  assert!(out.contains("42"), "session state did not persist: {out}");

  // A thrown error is a failed run, not a broken session.
  let (ok, _o, err) = run_on(dir.path(), id, "throw new Error('boom');");
  assert!(!ok, "throwing script should exit non-zero");
  assert!(err.contains("boom"), "error not surfaced: {err}");
  let (ok, _o, err) = run_on(dir.path(), id, "return 'still alive';");
  assert!(ok, "session died after a script error: {err}");

  // --json emits one document carrying the console it streamed.
  let (ok, out, err) = ferridriver(
    dir.path(),
    &[
      "run",
      "--session",
      id,
      "--json",
      "--eval",
      "console.log('in-json'); return 7;",
    ],
  );
  assert!(ok, "json run failed: {err}");
  let doc: serde_json::Value = serde_json::from_str(&out).expect("json run must emit one document");
  assert_eq!(doc["status"], "ok", "{out}");
  assert_eq!(doc["value"], 7, "{out}");
  assert!(
    doc["console"]
      .as_array()
      .is_some_and(|c| c.iter().any(|e| e["message"] == "in-json")),
    "streamed console missing from the json document: {out}"
  );

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
fn trace_streams_the_hosts_actions_to_the_attached_client() {
  let dir = tempfile::tempdir().unwrap();
  let id = "traced";

  let (ok, _o, err) = session(
    dir.path(),
    &["open", id, "--headless", "data:text/html,<button>go</button>"],
  );
  assert!(ok, "open failed: {err}");

  // Actions run in the host; --trace asks it to stream them back.
  let traced = "await page.goto('data:text/html,<button>hi</button>'); \
                await page.locator('button').click(); \
                return 'clicked';";
  let (ok, out, err) = ferridriver(dir.path(), &["run", "--session", id, "--trace", "--eval", traced]);
  assert!(ok, "traced run failed: {out}{err}");
  assert!(out.contains("clicked"), "result missing: {out}");
  // Action lines land on stderr — begin, call log, end — exactly as a local
  // `run --trace` renders them.
  assert!(err.contains("› page.goto"), "no page action streamed: {err}");
  assert!(err.contains("› locator.click"), "no locator action streamed: {err}");
  assert!(
    err.contains("waiting for locator('button')"),
    "call-log lines not streamed: {err}"
  );
  assert!(err.contains("✓ locator.click"), "no completed action line: {err}");

  // Without --trace the same run is silent on stderr: an untraced run must
  // not pay for the observer, nor make the client filter noise.
  let (ok, _o, err) = run_on(dir.path(), id, traced);
  assert!(ok, "untraced run failed: {err}");
  assert!(
    !err.contains("page.goto"),
    "actions streamed without --trace being asked for: {err}"
  );

  let _ = session(dir.path(), &["close", id]);
}

#[test]
fn code_echo_turns_a_session_into_a_test_file() {
  let dir = tempfile::tempdir().unwrap();
  let work = tempfile::tempdir().unwrap();
  let id = "recorder";

  let (ok, _o, err) = session(dir.path(), &["open", id, "--headless", "data:text/html,<p>seed</p>"]);
  assert!(ok, "open failed: {err}");

  let script = "await page.goto('data:text/html,<button>go</button>'); \
                await page.locator('button').click(); \
                return 'done';";

  // The host renders each action it ran; the client shows the lines.
  let (ok, out, err) = ferridriver(dir.path(), &["run", "--session", id, "--code", "--eval", script]);
  assert!(ok, "code run failed: {out}{err}");
  assert!(
    err.contains("await page.locator('button').click();"),
    "no code streamed from the host: {err}"
  );

  // …and writes them as a file that runs on its own.
  let generated = work.path().join("from-session.ts");
  let (ok, _o, err) = ferridriver(
    dir.path(),
    &[
      "run",
      "--session",
      id,
      "--code-out",
      generated.to_str().unwrap(),
      "--eval",
      script,
    ],
  );
  assert!(ok, "code-out run failed: {err}");
  let source = std::fs::read_to_string(&generated).unwrap();
  assert!(
    source.contains("await page.locator('button').click();"),
    "action missing from generated file:\n{source}"
  );

  // Replaying it against the same session must work too — the scaffolding
  // reuses the session's live `page` instead of launching a browser.
  let (ok, _o, err) = ferridriver(dir.path(), &["run", "--session", id, generated.to_str().unwrap()]);
  assert!(ok, "generated file failed to replay on the session: {err}");

  let _ = session(dir.path(), &["close", id]);
}

#[test]
fn report_describes_the_page_the_run_left_and_redacts_declared_secrets() {
  const SECRET: &str = "s3cr3t-cli-71b4";
  let dir = tempfile::tempdir().unwrap();
  let work = tempfile::tempdir().unwrap();
  let id = "reported";

  // The config reaches the detached host only because `open` forwards the
  // global `-c`; without that the host would run unconfigured and nothing
  // below would be redacted.
  std::fs::write(work.path().join(".env.secrets"), format!("APP_PASSWORD={SECRET}\n")).unwrap();
  let config = work.path().join("ferridriver.toml");
  std::fs::write(&config, "[secrets]\nfile = \"./.env.secrets\"\n").unwrap();
  let config = config.to_str().unwrap();

  let (ok, _o, err) = ferridriver(
    dir.path(),
    &[
      "--config",
      config,
      "session",
      "open",
      id,
      "--headless",
      "data:text/html,<p>seed</p>",
    ],
  );
  assert!(ok, "open failed: {err}");

  let script = "await page.goto('data:text/html,<title>Reported</title><input id=pw>'); \
                await page.locator('#pw').fill(args[0]); \
                return 'signed in with ' + args[0];";

  let (ok, out, err) = ferridriver(
    dir.path(),
    &[
      "run",
      "--session",
      id,
      "--report",
      "--code",
      "--eval",
      script,
      "--",
      SECRET,
    ],
  );
  assert!(ok, "reported run failed: {out}{err}");

  // Sections, in the order the contract defines them.
  assert!(out.contains("### Result"), "no result section: {out}");
  assert!(out.contains("### Ran ferridriver code"), "no code section: {out}");
  assert!(out.contains("### Page"), "no page section: {out}");
  // The page the run LEFT the session on, read live in the host.
  assert!(
    out.contains("- Page Title: Reported"),
    "page section does not carry the live title: {out}"
  );

  // The secret reaches this process nowhere — the host redacted before the
  // wire, so neither stream can carry it.
  let both = format!("{out}{err}");
  assert!(
    !both.contains(SECRET),
    "the declared secret leaked to the client: {both}"
  );
  assert!(
    out.contains("<secret>APP_PASSWORD</secret>"),
    "nothing was redacted, so the check above proves nothing: {out}"
  );
  assert!(
    out.contains("await page.locator('#pw').fill(process.env['APP_PASSWORD']);"),
    "the echoed fill did not become an environment read: {out}"
  );

  // `--report --json` keeps one document and folds the sections into it.
  let (ok, out, err) = ferridriver(
    dir.path(),
    &[
      "run",
      "--session",
      id,
      "--report",
      "--json",
      "--eval",
      script,
      "--",
      SECRET,
    ],
  );
  assert!(ok, "reported json run failed: {err}");
  let doc: serde_json::Value = serde_json::from_str(&out).expect("one json document");
  assert!(
    doc["report"]["page"]
      .as_str()
      .is_some_and(|p| p.contains("- Page Title: Reported")),
    "page section missing from the json report: {out}"
  );
  assert!(!out.contains(SECRET), "the secret leaked into the json document: {out}");

  // Without --report the same run says nothing about the page: the host must
  // not pay for a title round-trip nobody asked for.
  let (ok, out, err) = run_on(dir.path(), id, script.replace("args[0]", "'plain'").as_str());
  assert!(ok, "plain run failed: {err}");
  assert!(!out.contains("### Page"), "sections rendered without --report: {out}");

  let _ = session(dir.path(), &["close", id]);
}

#[test]
fn session_runs_typescript_modules_bundled_by_the_client() {
  let dir = tempfile::tempdir().unwrap();
  let work = tempfile::tempdir().unwrap();
  let id = "modules";

  let (ok, _o, err) = session(dir.path(), &["open", id, "--headless", "data:text/html,<p>mod</p>"]);
  assert!(ok, "open failed: {err}");

  // A helper the entry imports: only the CLIENT can resolve this path, which
  // is why bundling happens client-side and compiling host-side.
  std::fs::write(
    work.path().join("helper.ts"),
    "export const title = (): string => 'from-module';\n",
  )
  .unwrap();
  std::fs::write(
    work.path().join("entry.ts"),
    "import { title } from './helper';\nexport default `${title()}:${page.url().slice(0, 4)}`;\n",
  )
  .unwrap();

  let out = Command::new(bin())
    .args(["run", "--session", id, "entry.ts"])
    .current_dir(work.path())
    .env("FERRIDRIVER_SESSION_DIR", dir.path())
    .stdin(Stdio::null())
    .output()
    .expect("spawn ferridriver run");
  let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
  let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
  assert!(out.status.success(), "module run failed: {stdout}{stderr}");
  assert!(stdout.contains("from-module:data"), "module result wrong: {stdout}");

  let _ = session(dir.path(), &["close", id]);
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
fn run_against_missing_session_errors() {
  let dir = tempfile::tempdir().unwrap();
  let (ok, _o, err) = run_on(dir.path(), "ghost", "return 1;");
  assert!(!ok, "run against a missing session should fail");
  assert!(err.contains("ghost"), "error should name the session: {err}");
}

#[test]
fn extensions_belong_to_the_session_host() {
  let dir = tempfile::tempdir().unwrap();
  let (ok, _o, err) = ferridriver(
    dir.path(),
    &[
      "run",
      "--session",
      "any",
      "--extension",
      "./x.ts",
      "--eval",
      "return 1;",
    ],
  );
  assert!(!ok, "--extension with --session should be rejected");
  assert!(
    err.contains("session open"),
    "the error should point at where extensions are loaded: {err}"
  );
}
