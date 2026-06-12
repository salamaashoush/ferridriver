#![allow(clippy::expect_used, clippy::unwrap_used)]
//! End-to-end test for declaring `[[sidecars]]` in `ferridriver.toml` and
//! reaching them from a script. Exercises the full config-parse →
//! engine-config wiring (not the programmatic shortcut): a real
//! `ferridriver.toml` with a `[[sidecars]]` entry is dropped in the run's
//! cwd, then `ferridriver run` connects to the declared sidecar and pings it.
//!
//! Requires a built `ferridriver` binary (`FERRIDRIVER_BIN` or
//! `target/{debug,release}/ferridriver`). The `sidecar_echo` fixture
//! (defined by `ferridriver-script`) is built on demand.

use std::path::PathBuf;
use std::process::Command;

fn target_dir() -> String {
  format!("{}/../../target", env!("CARGO_MANIFEST_DIR"))
}

fn ferridriver_bin() -> String {
  std::env::var("FERRIDRIVER_BIN").unwrap_or_else(|_| {
    let base = target_dir();
    let debug = format!("{base}/debug/ferridriver");
    if std::path::Path::new(&debug).exists() {
      debug
    } else {
      format!("{base}/release/ferridriver")
    }
  })
}

/// Build (if needed) and locate the `sidecar_echo` fixture binary.
fn ensure_sidecar_echo() -> PathBuf {
  let status = Command::new(env!("CARGO"))
    .args(["build", "-p", "ferridriver-script", "--bin", "sidecar_echo"])
    .status()
    .expect("spawn cargo build for sidecar_echo");
  assert!(status.success(), "failed to build sidecar_echo fixture");

  let base = target_dir();
  let debug = PathBuf::from(format!("{base}/debug/sidecar_echo"));
  if debug.exists() {
    debug
  } else {
    PathBuf::from(format!("{base}/release/sidecar_echo"))
  }
}

#[test]
fn declared_sidecar_is_reachable_from_a_script() {
  let echo = ensure_sidecar_echo();
  let dir = tempfile::tempdir().unwrap();
  // A real config file in the run's cwd — `ferridriver run` discovers it via
  // the standard search path, so this drives config-parse → SidecarSpec →
  // ScriptEngineConfig end to end.
  let toml = format!(
    "[[sidecars]]\nname = \"echo\"\ncommand = [\"{}\"]\n",
    echo.to_str().unwrap()
  );
  std::fs::write(dir.path().join("ferridriver.toml"), toml).unwrap();

  let out = Command::new(ferridriver_bin())
    .current_dir(dir.path())
    .args([
      "run",
      "-e",
      "const sc = await sidecars.connect('echo'); const r = await sc.send('ping'); await sc.close(); return r;",
    ])
    .output()
    .expect("spawn ferridriver run");

  let stdout = String::from_utf8_lossy(&out.stdout);
  let stderr = String::from_utf8_lossy(&out.stderr);
  assert!(out.status.success(), "exit ok; stdout={stdout} stderr={stderr}");
  let v: serde_json::Value = serde_json::from_str(&stdout).expect("json stdout");
  assert_eq!(v["status"], "ok", "{v}");
  assert_eq!(v["value"]["ok"], true, "declared sidecar ping returned ok: {v}");
}

/// The gateway extension (`fixtures/sidecar_gateway.ts`) is loaded via
/// `--plugin` and drives the declared sidecar through its tools — the real
/// deployed path (extension → `tools['gateway.*']` → `sidecars`). Covers
/// the event path (`on` + pushed frame) that deadlocked under the
/// multi-threaded runtime before the pump moved onto `ctx.spawn`.
fn gateway_fixture() -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ferridriver-script/tests/fixtures/sidecar_gateway.ts")
}

#[test]
fn gateway_plugin_drives_sidecar_over_the_real_binary() {
  let echo = ensure_sidecar_echo();
  let dir = tempfile::tempdir().unwrap();
  let toml = format!(
    "[[sidecars]]\nname = \"gateway\"\ncommand = [\"{}\"]\n",
    echo.to_str().unwrap()
  );
  std::fs::write(dir.path().join("ferridriver.toml"), toml).unwrap();

  let script = "\
    const ping = await tools['gateway.ping']();\n\
    const echoed = await tools['gateway.call']({ method: 'echo', params: { n: 7 } });\n\
    const evt = await tools['gateway.roundtripEvent']({ event: 'tick', params: { event: 'tick', payload: { hits: 3 } } });\n\
    await tools['gateway.close']();\n\
    return { ping, echoed, evt };";

  let out = Command::new(ferridriver_bin())
    .current_dir(dir.path())
    .args(["run", "--plugin", gateway_fixture().to_str().unwrap(), "-e", script])
    .output()
    .expect("spawn ferridriver run");

  let stdout = String::from_utf8_lossy(&out.stdout);
  let stderr = String::from_utf8_lossy(&out.stderr);
  assert!(out.status.success(), "exit ok; stdout={stdout} stderr={stderr}");
  let v: serde_json::Value = serde_json::from_str(&stdout).expect("json stdout");
  assert_eq!(v["status"], "ok", "{v}");
  assert_eq!(v["value"]["ping"]["ok"], true, "ping: {v}");
  assert_eq!(v["value"]["ping"]["name"], "gateway", "{v}");
  assert_eq!(v["value"]["echoed"], serde_json::json!({ "n": 7 }), "{v}");
  assert_eq!(
    v["value"]["evt"],
    serde_json::json!({ "hits": 3 }),
    "pushed event via plugin: {v}"
  );
}

#[test]
fn unknown_sidecar_name_is_rejected_when_not_declared() {
  let dir = tempfile::tempdir().unwrap();
  // No ferridriver.toml: `sidecars.connect` exists but rejects every name.
  let out = Command::new(ferridriver_bin())
    .current_dir(dir.path())
    .args([
      "run",
      "-e",
      "try { await sidecars.connect('echo'); return 'no-throw'; } catch (e) { return String(e.message || e); }",
    ])
    .output()
    .expect("spawn ferridriver run");

  let stdout = String::from_utf8_lossy(&out.stdout);
  let v: serde_json::Value = serde_json::from_str(&stdout).expect("json stdout");
  let msg = v["value"].as_str().unwrap_or_default();
  assert!(msg.contains("unknown sidecar"), "got: {v}");
}
