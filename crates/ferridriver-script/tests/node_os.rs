#![allow(clippy::expect_used, clippy::unwrap_used)]
//! The native `os` / `node:os` module, over both the ESM import and the
//! `CommonJS` `require` path, checked against what the host actually
//! reports.

use std::path::Path;
use std::sync::Arc;

use ferridriver_script::{
  InMemoryVars, Outcome, RunContext, RunOptions, ScriptEngineConfig, Session, bundle_and_compile,
};

fn ctx(dir: &Path) -> RunContext {
  RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: dir.into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: Vec::new(),
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
  }
}

async fn run(source: &str, dir: &Path, session: &Session, context: &RunContext) -> serde_json::Value {
  let entry = dir.join("entry.ts");
  std::fs::write(&entry, source).expect("write entry");
  // A fresh module name per call: the bytecode cache keys on the entry
  // path plus content, and both scripts share this one path.
  let bundle = bundle_and_compile(std::slice::from_ref(&entry), dir)
    .await
    .expect("bundle");
  let out = session
    .execute_module(&bundle, &[], RunOptions::default(), context)
    .await;
  match out.result.outcome {
    Outcome::Ok { success, .. } => success.value,
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[tokio::test]
async fn os_reports_the_host_it_runs_on() {
  let dir = tempfile::tempdir().expect("tempdir");
  let context = ctx(dir.path());
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");

  let value = run(
    r"
      import os from 'node:os';
      export default {
        platform: os.platform(),
        arch: os.arch(),
        type: os.type(),
        release: os.release(),
        version: os.version(),
        machine: os.machine(),
        endianness: os.endianness(),
        eol: os.EOL,
        devNull: os.devNull,
        homedir: os.homedir(),
        tmpdir: os.tmpdir(),
        hostname: os.hostname(),
        parallelism: os.availableParallelism(),
        cpuCount: os.cpus().length,
        firstCpu: os.cpus()[0],
        totalmem: os.totalmem(),
        freemem: os.freemem(),
        uptime: os.uptime(),
        loadavg: os.loadavg(),
        user: os.userInfo(),
        priority: os.getPriority(),
        interfaces: os.networkInterfaces(),
      };
    ",
    dir.path(),
    &session,
    &context,
  )
  .await;

  assert_identity(&value);
  assert_resources(&value);
  assert_user_and_network(&value);
}

/// Who and where the host is: the uname triple, the paths, the constants.
fn assert_identity(value: &serde_json::Value) {
  let expected_platform = if cfg!(target_os = "macos") { "darwin" } else { "linux" };
  let expected_type = if cfg!(target_os = "macos") { "Darwin" } else { "Linux" };
  assert_eq!(value["platform"], expected_platform);
  assert_eq!(value["type"], expected_type);
  assert_eq!(
    value["endianness"],
    if cfg!(target_endian = "little") { "LE" } else { "BE" }
  );
  assert_eq!(value["eol"], "\n");
  assert_eq!(value["devNull"], "/dev/null");
  assert_eq!(
    value["arch"],
    match std::env::consts::ARCH {
      "aarch64" => "arm64",
      "x86_64" => "x64",
      other => other,
    }
  );

  assert!(
    value["release"].as_str().is_some_and(|r| !r.is_empty()),
    "release comes from uname: {value:?}"
  );
  assert!(
    value["version"].as_str().is_some_and(|v| !v.is_empty()),
    "version: {value:?}"
  );
  assert!(
    value["machine"].as_str().is_some_and(|m| !m.is_empty()),
    "machine: {value:?}"
  );
  assert!(
    value["hostname"].as_str().is_some_and(|h| !h.is_empty()),
    "hostname: {value:?}"
  );

  assert_eq!(
    value["homedir"].as_str(),
    std::env::var("HOME").ok().as_deref(),
    "homedir matches the environment"
  );
  assert_eq!(
    value["tmpdir"].as_str(),
    std::env::temp_dir().to_str(),
    "tmpdir matches the environment"
  );
}

/// What the host has: CPUs, memory, uptime, load.
fn assert_resources(value: &serde_json::Value) {
  assert!(value["parallelism"].as_u64().is_some_and(|n| n > 0));
  assert!(value["cpuCount"].as_u64().is_some_and(|n| n > 0));
  assert!(value["totalmem"].as_u64().is_some_and(|n| n > 0), "totalmem: {value:?}");
  let total = value["totalmem"].as_u64().unwrap_or(0);
  assert!(
    value["freemem"].as_u64().is_some_and(|n| n > 0 && n <= total),
    "freemem is within totalmem: {value:?}"
  );
  assert!(value["uptime"].as_u64().is_some_and(|n| n > 0), "uptime: {value:?}");
  assert_eq!(value["loadavg"].as_array().map(Vec::len), Some(3));

  // The vendored upstream reports every CPU time as a hardcoded 0. These
  // come from /proc/stat or host_processor_info, so a machine that has run
  // long enough to reach this assertion has spent time somewhere.
  let times = &value["firstCpu"]["times"];
  let busy: u64 = ["user", "nice", "sys", "idle", "irq"]
    .iter()
    .filter_map(|k| times[*k].as_u64())
    .sum();
  assert!(busy > 0, "cpu times are read from the kernel, not zeroed: {times:?}");
  assert!(
    value["firstCpu"]["model"].as_str().is_some_and(|m| !m.is_empty()),
    "cpu model: {value:?}"
  );
}

/// Who is running it, and what it is attached to.
fn assert_user_and_network(value: &serde_json::Value) {
  let user = &value["user"];
  assert!(user["uid"].as_u64().is_some(), "userInfo uid: {user:?}");
  assert!(user["gid"].as_u64().is_some(), "userInfo gid: {user:?}");
  assert!(
    user["username"].as_str().is_some_and(|u| !u.is_empty()),
    "userInfo username comes from the password database: {user:?}"
  );
  assert!(
    user["shell"].as_str().is_some_and(|s| s.starts_with('/')),
    "userInfo shell: {user:?}"
  );
  assert!(value["priority"].as_i64().is_some(), "getPriority: {value:?}");

  // Every host has a loopback interface, and Node marks it internal.
  let interfaces = value["interfaces"].as_object().expect("networkInterfaces object");
  let loopback = interfaces
    .values()
    .flat_map(|entries| entries.as_array().cloned().unwrap_or_default())
    .find(|entry| entry["address"] == "127.0.0.1");
  let loopback = loopback.unwrap_or_else(|| panic!("no loopback interface in {interfaces:?}"));
  assert_eq!(loopback["family"], "IPv4");
  assert_eq!(loopback["internal"], serde_json::Value::Bool(true));
  assert_eq!(loopback["netmask"], "255.0.0.0");
  assert!(
    loopback["cidr"].as_str().is_some_and(|c| c.starts_with("127.0.0.1/")),
    "loopback cidr: {loopback:?}"
  );
}

#[tokio::test]
async fn os_serves_the_same_surface_through_require() {
  let dir = tempfile::tempdir().expect("tempdir");
  let context = ctx(dir.path());
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");

  let value = run(
    r"
      const os = require('os');
      const named = require('node:os');
      export default {
        platform: os.platform(),
        sameThroughPrefixedSpecifier: named.platform() === os.platform(),
        keys: Object.keys(os).sort(),
      };
    ",
    dir.path(),
    &session,
    &context,
  )
  .await;

  assert_eq!(
    value["platform"],
    if cfg!(target_os = "macos") { "darwin" } else { "linux" }
  );
  assert_eq!(value["sameThroughPrefixedSpecifier"], serde_json::Value::Bool(true));

  // The require namespace must carry the whole documented surface, not a
  // subset of what the ESM path exports.
  let keys: Vec<&str> = value["keys"]
    .as_array()
    .expect("keys")
    .iter()
    .filter_map(serde_json::Value::as_str)
    .collect();
  for expected in [
    "EOL",
    "arch",
    "availableParallelism",
    "cpus",
    "devNull",
    "endianness",
    "freemem",
    "getPriority",
    "homedir",
    "hostname",
    "loadavg",
    "machine",
    "networkInterfaces",
    "platform",
    "release",
    "setPriority",
    "tmpdir",
    "totalmem",
    "type",
    "uptime",
    "userInfo",
    "version",
  ] {
    assert!(
      keys.contains(&expected),
      "require('os') is missing {expected}: {keys:?}"
    );
  }
}
