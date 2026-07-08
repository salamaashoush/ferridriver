#![allow(clippy::expect_used, clippy::unwrap_used)]
//! End-to-end coverage of the hardened `commands` capability, driven
//! through the real plugin pipeline (rolldown -> bytecode -> session
//! VM) and the `SessionTable` (so persistent processes get their
//! durable registry installed). No browser needed — the handlers only
//! touch `commands`.

use std::sync::Arc;
use std::time::Duration;

use ferridriver_script::{
  ExtensionBinding, InMemoryVars, Outcome, PathSandbox, RunContext, RunOptions, ScriptEngineConfig, ScriptResult,
  SessionTable, compile_and_extract_extensions,
};

async fn binding(src: &str) -> (tempfile::TempDir, ExtensionBinding) {
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("ext.ts");
  std::fs::write(&path, src).expect("write");
  let (compiled, failures) = compile_and_extract_extensions(&[path]).await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let cp = compiled.into_iter().next().expect("one");
  (
    tmp,
    ExtensionBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
    },
  )
}

fn ctx(sandbox_tmp: &std::path::Path, b: ExtensionBinding) -> RunContext {
  RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(sandbox_tmp).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: vec![b],
    host: ferridriver_script::ExtensionHost::Mcp,
    caps: ferridriver_script::ScriptCaps::default(),
  }
}

/// Run `js` once on a fresh `SessionTable` session that has `plugin`
/// installed (so persistent commands get their durable registry).
async fn run_with(plugin: &str, js: &str) -> ScriptResult {
  let (_p, b) = binding(plugin).await;
  let sb = tempfile::tempdir().expect("tempdir");
  let context = ctx(sb.path(), b);
  let table = SessionTable::new(8, None);
  let slot = table.acquire("s");
  let mut bs = slot.lock().await;
  bs.run(
    ScriptEngineConfig::default(),
    js,
    &[],
    RunOptions::default(),
    context,
    None,
  )
  .await
}

#[track_caller]
fn ok(r: &ScriptResult) -> &serde_json::Value {
  match &r.outcome {
    Outcome::Ok { success } => &success.value,
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[track_caller]
fn err_msg(r: &ScriptResult) -> String {
  match &r.outcome {
    Outcome::Error { error } => error.message.clone(),
    Outcome::Ok { success } => panic!("expected error, got ok: {success:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn argv_form_runs_without_a_shell_so_metachars_are_inert() {
  // The arg contains shell metacharacters; argv mode must pass it as a
  // single literal argument (echo prints it verbatim, nothing executes).
  let plugin = r#"
    defineTool({ name: 't', allow: { commands: { e: { run: ["echo", "${m}"] } } },
      handler: async ({ args, commands }) => commands.run('e', { m: args.m }) });
  "#;
  let r = run_with(
    plugin,
    "return await tools['t']({ m: '$(touch /tmp/ferri_pwned); a && b' });",
  )
  .await;
  assert_eq!(ok(&r), &serde_json::json!("$(touch /tmp/ferri_pwned); a && b"));
}

#[tokio::test(flavor = "multi_thread")]
async fn output_modes_text_json_lines() {
  let plugin = r#"
    defineTool({ name: 'j', allow: { commands: { c: { run: "printf '{\"a\":1}'", output: "json" } } },
      handler: async ({ commands }) => commands.run('c') });
    defineTool({ name: 'l', allow: { commands: { c: { run: "printf 'a\nb\n\nc\n'", output: "lines" } } },
      handler: async ({ commands }) => commands.run('c') });
    defineTool({ name: 'x', allow: { commands: { c: "echo hi" } },
      handler: async ({ commands }) => commands.run('c') });
  "#;
  let j = run_with(plugin, "return await tools['j']();").await;
  assert_eq!(ok(&j), &serde_json::json!({ "a": 1 }));
  let l = run_with(plugin, "return await tools['l']();").await;
  assert_eq!(ok(&l), &serde_json::json!(["a", "b", "c"]));
  let x = run_with(plugin, "return await tools['x']();").await;
  assert_eq!(ok(&x), &serde_json::json!("hi"));
}

#[tokio::test(flavor = "multi_thread")]
async fn strict_unknown_placeholder_errors() {
  let plugin = r#"
    defineTool({ name: 't', allow: { commands: { c: "echo ${name}" } },
      handler: async ({ commands }) => commands.run('c', {}) });
  "#;
  let r = run_with(plugin, "return await tools['t']();").await;
  assert!(err_msg(&r).contains("${name}"), "{}", err_msg(&r));
}

#[tokio::test(flavor = "multi_thread")]
async fn undeclared_command_is_denied() {
  let plugin = r#"
    defineTool({ name: 't', allow: { commands: { allowed: "echo ok" } },
      handler: async ({ commands }) => commands.run('other') });
  "#;
  let r = run_with(plugin, "return await tools['t']();").await;
  assert!(
    err_msg(&r).contains("not in the commands allow-list"),
    "{}",
    err_msg(&r)
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn timeout_kills_a_slow_command() {
  let plugin = r#"
    defineTool({ name: 't', allow: { commands: { slow: { run: "sleep 5", timeoutMs: 150 } } },
      handler: async ({ commands }) => commands.run('slow') });
  "#;
  let started = std::time::Instant::now();
  let r = run_with(plugin, "return await tools['t']();").await;
  assert!(err_msg(&r).contains("timed out after 150ms"), "{}", err_msg(&r));
  assert!(
    started.elapsed() < Duration::from_secs(2),
    "should not have waited the full 5s"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn run_rejects_a_persistent_spec_and_vice_versa() {
  let plugin = r#"
    defineTool({ name: 'p', allow: { commands: { srv: { run: "sleep 1", persistent: true } } },
      handler: async ({ commands }) => commands.run('srv') });
    defineTool({ name: 'o', allow: { commands: { one: "echo hi" } },
      handler: async ({ commands }) => commands.start('one') });
  "#;
  let r = run_with(plugin, "return await tools['p']();").await;
  assert!(err_msg(&r).contains("persistent"), "{}", err_msg(&r));
  let r = run_with(plugin, "return await tools['o']();").await;
  assert!(err_msg(&r).contains("not declared `persistent`"), "{}", err_msg(&r));
}

#[tokio::test(flavor = "multi_thread")]
async fn persistent_start_status_stop_lifecycle() {
  // A "server" that prints a line then idles. start -> status shows it
  // running with captured stdout -> stop -> status shows not running.
  let plugin = r#"
    const SPEC = { run: "echo up; sleep 30", persistent: true };
    defineTool({ name: 'srv', allow: { commands: { s: SPEC } }, handler: async ({ args, commands }) => {
      if (args.op === 'start')  return await commands.start('s');
      if (args.op === 'status') return await commands.status('s');
      if (args.op === 'stop')   { await commands.stop('s'); return 'stopped'; }
    }});
  "#;
  let (_p, b) = binding(plugin).await;
  let sb = tempfile::tempdir().expect("tempdir");
  let context = ctx(sb.path(), b);
  let table = SessionTable::new(8, None);
  let slot = table.acquire("s");

  // start
  {
    let mut bs = slot.lock().await;
    let r = bs
      .run(
        ScriptEngineConfig::default(),
        "return await tools['srv']({ op: 'start' });",
        &[],
        RunOptions::default(),
        context.clone(),
        None,
      )
      .await;
    let v = ok(&r);
    assert!(v["pid"].as_i64().unwrap_or(0) > 0, "got pid: {v}");
  }
  // give it a moment to emit "up"
  tokio::time::sleep(Duration::from_millis(250)).await;
  // status: running, stdout captured
  {
    let mut bs = slot.lock().await;
    let r = bs
      .run(
        ScriptEngineConfig::default(),
        "return await tools['srv']({ op: 'status' });",
        &[],
        RunOptions::default(),
        context.clone(),
        None,
      )
      .await;
    let v = ok(&r);
    assert_eq!(v["running"], serde_json::json!(true), "status: {v}");
    assert!(
      v["stdout"].as_str().unwrap_or("").contains("up"),
      "captured stdout: {v}"
    );
  }
  // stop, then status: not running
  {
    let mut bs = slot.lock().await;
    let _ = bs
      .run(
        ScriptEngineConfig::default(),
        "return await tools['srv']({ op: 'stop' });",
        &[],
        RunOptions::default(),
        context.clone(),
        None,
      )
      .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let r = bs
      .run(
        ScriptEngineConfig::default(),
        "return await tools['srv']({ op: 'status' });",
        &[],
        RunOptions::default(),
        context.clone(),
        None,
      )
      .await;
    // After stop the record is gone -> status reports "no persistent process".
    assert!(
      err_msg(&r).contains("no persistent process"),
      "post-stop status: {}",
      err_msg(&r)
    );
  }
}
