#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Persistent-session semantics: a `Session` reuses one `QuickJS` VM
//! across many `execute` calls so user `globalThis` state survives
//! REPL-style, while a poisoning timeout marks the VM for rebuild.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ferridriver_script::{
  InMemoryVars, Outcome, PathSandbox, PluginBinding, PluginToolBinding, RunContext, RunOptions, ScriptEngineConfig,
  ScriptErrorKind, Session, compile_and_extract_plugins,
};

/// A one-tool plugin whose handler bumps a `globalThis` counter so a
/// second invocation in the same session observes the first's state.
const DEMO_PLUGIN: &str = "globalThis.exports = { name: 'demo', handler: async ({ args }) => { \
  globalThis.__n = (globalThis.__n || 0) + 1; return { n: globalThis.__n, got: args }; } };";

/// Bundle + compile the demo plugin through the production pipeline
/// (rolldown -> bytecode) and wrap it as a `PluginBinding`.
async fn demo_binding() -> (tempfile::TempDir, PluginBinding) {
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("demo.js");
  std::fs::write(&path, DEMO_PLUGIN).expect("write plugin");
  let (compiled, failures) = compile_and_extract_plugins(&[path]).await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let cp = compiled.into_iter().next().expect("one compiled plugin");
  assert!(!cp.bytecode.is_empty(), "compiled bytecode must be non-empty");
  (
    tmp,
    PluginBinding {
      bytecode: cp.bytecode,
      tools: vec![PluginToolBinding {
        name: "demo".to_string(),
        allowed_commands: HashMap::new(),
      }],
    },
  )
}

async fn run_demo_plugin_twice() {
  let (_plugin_tmp, binding) = demo_binding().await;
  let tmp = tempfile::tempdir().expect("tempdir");
  let sandbox = PathSandbox::new(tmp.path()).expect("sandbox");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(sandbox),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    plugins: vec![binding],
    trusted_modules: false,
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  let r1 = session
    .execute(
      "return await plugins['demo']({ x: 1 });",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match r1.result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!({ "n": 1, "got": { "x": 1 } })),
    Outcome::Error { error } => panic!("plugin call 1 failed: {error:?}"),
  }

  // Second invocation in the SAME session sees the handler's prior
  // `globalThis` state — proves plugin install-once + persistent VM.
  let r2 = session
    .execute(
      "return await plugins['demo']({ x: 2 });",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match r2.result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!({ "n": 2, "got": { "x": 2 } })),
    Outcome::Error { error } => panic!("plugin call 2 failed: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn typescript_plugin_with_local_import_bundles_and_runs() {
  // Headline migration capability: a `.ts` plugin that imports a
  // plugin-local `.ts` helper. rolldown must transpile + inline the
  // import; the compiled bytecode then runs with no resolver in-session.
  let tmp = tempfile::tempdir().expect("tempdir");
  std::fs::write(
    tmp.path().join("helper.ts"),
    "export const tag = (n: number): string => `t${n}`;\n",
  )
  .expect("write helper");
  std::fs::write(
    tmp.path().join("plug.ts"),
    "import { tag } from './helper';\n\
     interface In { n: number }\n\
     globalThis.exports = { name: 'ts', exposeAsTool: true, \
       async handler({ args }: { args: In }) { return { tag: tag(args.n) }; } };\n",
  )
  .expect("write plugin");

  let (compiled, failures) = compile_and_extract_plugins(&[tmp.path().join("plug.ts")]).await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let cp = compiled.into_iter().next().expect("one compiled plugin");

  let sb_tmp = tempfile::tempdir().expect("tempdir");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(sb_tmp.path()).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    plugins: vec![PluginBinding {
      bytecode: cp.bytecode,
      tools: vec![PluginToolBinding {
        name: "ts".to_string(),
        allowed_commands: HashMap::new(),
      }],
    }],
    trusted_modules: false,
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  let r = session
    .execute(
      "return await plugins['ts']({ n: 7 });",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match r.result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!({ "tag": "t7" })),
    Outcome::Error { error } => panic!("ts plugin failed: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn plugin_bytecode_path_installs_and_persists() {
  // Exercises the production path: rolldown-bundled plugin compiled once
  // to bytecode, `Module::load`ed into the session VM, handler state
  // persisting across two invocations in the same session.
  run_demo_plugin_twice().await;
}

fn make_ctx() -> (tempfile::TempDir, RunContext) {
  let tmp = tempfile::tempdir().expect("tempdir");
  let sandbox = PathSandbox::new(tmp.path()).expect("sandbox");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(sandbox),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    plugins: Vec::new(),
    trusted_modules: false,
  };
  (tmp, ctx)
}

#[tokio::test(flavor = "multi_thread")]
async fn globals_persist_across_executions() {
  let (_tmp, ctx) = make_ctx();
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  let r1 = session
    .execute(
      "globalThis.h = () => 42; return null;",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  assert!(r1.result.is_ok(), "{:?}", r1.result);
  assert!(!r1.poisoned);

  // Second execution sees the function defined by the first (the user's
  // own chosen contract: `globalThis.h = () => 42` then `h()`).
  let r2 = session.execute("return h();", &[], RunOptions::default(), &ctx).await;
  match r2.result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!(42)),
    Outcome::Error { error } => panic!("expected 42, got error: {error:?}"),
  }
  assert!(!r2.poisoned);
}

#[tokio::test(flavor = "multi_thread")]
async fn let_const_inside_call_do_not_persist() {
  let (_tmp, ctx) = make_ctx();
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  let r1 = session
    .execute("let x = 5; return x;", &[], RunOptions::default(), &ctx)
    .await;
  assert!(r1.result.is_ok(), "{:?}", r1.result);

  // `let`/`const` are scoped to the per-call async wrapper, not global.
  let r2 = session
    .execute("return typeof x;", &[], RunOptions::default(), &ctx)
    .await;
  match r2.result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("undefined")),
    Outcome::Error { error } => panic!("expected undefined, got error: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_throw_does_not_poison_and_state_survives() {
  let (_tmp, ctx) = make_ctx();
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  session
    .execute("globalThis.keep = 'alive'; return 1;", &[], RunOptions::default(), &ctx)
    .await;

  let thrown = session
    .execute("throw new Error('boom');", &[], RunOptions::default(), &ctx)
    .await;
  match thrown.result.outcome {
    Outcome::Error { error } => {
      assert_eq!(error.kind, ScriptErrorKind::Runtime);
      assert!(error.message.contains("boom"), "got: {}", error.message);
    },
    Outcome::Ok { .. } => panic!("expected the throw to surface as an error"),
  }
  // A plain JS throw must NOT poison the VM — state is intact.
  assert!(!thrown.poisoned, "plain throw must not poison the session");

  let after = session
    .execute("return globalThis.keep;", &[], RunOptions::default(), &ctx)
    .await;
  match after.result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("alive")),
    Outcome::Error { error } => panic!("state lost after throw: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn timeout_poisons_the_session() {
  let (_tmp, ctx) = make_ctx();
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  let timed = session
    .execute(
      "while (true) { /* spin */ }",
      &[],
      RunOptions {
        timeout: Some(Duration::from_millis(150)),
        ..RunOptions::default()
      },
      &ctx,
    )
    .await;
  match timed.result.outcome {
    Outcome::Error { error } => assert_eq!(error.kind, ScriptErrorKind::Timeout),
    Outcome::Ok { .. } => panic!("expected timeout"),
  }
  // A fired timeout interrupt halts the interpreter mid-run: the VM is
  // poisoned and the caller must discard it.
  assert!(timed.poisoned, "a timeout must poison the session");
}

#[tokio::test(flavor = "multi_thread")]
async fn framework_globals_refresh_each_call() {
  let (_tmp, ctx) = make_ctx();
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  // `vars` is a framework binding refreshed every call; its backing
  // store is shared, so a value set in call 1 is visible in call 2.
  session
    .execute("vars.set('k', 'v'); return null;", &[], RunOptions::default(), &ctx)
    .await;
  let r = session
    .execute("return vars.get('k');", &[], RunOptions::default(), &ctx)
    .await;
  match r.result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("v")),
    Outcome::Error { error } => panic!("expected 'v', got error: {error:?}"),
  }
}

// ── Runtime shims: timers, URL, web polyfills, proper console ─────────────

#[tokio::test(flavor = "multi_thread")]
async fn set_timeout_resolves_inside_execute() {
  let (_tmp, ctx) = make_ctx();
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  let r = session
    .execute(
      "return await new Promise((resolve) => setTimeout(() => resolve(7), 20));",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match r.result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!(7)),
    Outcome::Error { error } => panic!("setTimeout did not resolve: {error:?}"),
  }
  assert!(!r.poisoned);
}

#[tokio::test(flavor = "multi_thread")]
async fn timer_handle_persists_and_clears_across_calls() {
  let (_tmp, ctx) = make_ctx();
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  // Call 1: arm a long timeout, stash its handle on globalThis.
  let r1 = session
    .execute(
      "globalThis.__t = setTimeout(() => { globalThis.__fired = true; }, 10000); \
       return typeof globalThis.__t;",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  assert!(r1.result.is_ok(), "{:?}", r1.result);

  // Call 2: the handle survived REPL-style; clearTimeout accepts it.
  let r2 = session
    .execute(
      "clearTimeout(globalThis.__t); return globalThis.__fired === true;",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match r2.result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!(false), "timer must not have fired"),
    Outcome::Error { error } => panic!("clearTimeout across calls failed: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn url_and_search_params_work() {
  let (_tmp, ctx) = make_ctx();
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  let r = session
    .execute(
      "const p = new URLSearchParams('a=1&b=2'); p.append('b', '3'); \
       return [p.get('a'), p.getAll('b').join(',')];",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match r.result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!(["1", "2,3"])),
    Outcome::Error { error } => panic!("URLSearchParams failed: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn web_polyfills_text_codec_base64_microtask() {
  let (_tmp, ctx) = make_ctx();
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  let r = session
    .execute(
      "const enc = new TextEncoder().encode('hi€'); \
       const dec = new TextDecoder().decode(enc); \
       let mt = 0; queueMicrotask(() => { mt = 1; }); \
       await Promise.resolve(); \
       return { len: enc.length, dec, b64: btoa('hi'), round: atob(btoa('xy')), mt };",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match r.result.outcome {
    Outcome::Ok { success } => {
      assert_eq!(
        success.value["len"],
        serde_json::json!(5),
        "hi€ = 5 UTF-8 bytes: {success:?}"
      );
      assert_eq!(success.value["dec"], serde_json::json!("hi€"));
      assert_eq!(success.value["b64"], serde_json::json!("aGk="));
      assert_eq!(success.value["round"], serde_json::json!("xy"));
      assert_eq!(
        success.value["mt"],
        serde_json::json!(1),
        "queueMicrotask must have run"
      );
    },
    Outcome::Error { error } => panic!("web polyfills failed: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn console_uses_node_style_formatter_and_is_captured() {
  let (_tmp, ctx) = make_ctx();
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  let r = session
    .execute(
      "console.log('n =', 42, { a: 1 }); console.warn(['x', 'y']); return null;",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  assert!(r.result.is_ok(), "{:?}", r.result);
  let console = &r.result.console;
  assert_eq!(console.len(), 2, "two console entries: {console:?}");
  // Top-level string + number stay unquoted; object renders Node-style
  // (not JSON.stringify's {"a":1}).
  let line0 = &console[0].message;
  assert!(line0.starts_with("n = 42 "), "got: {line0}");
  assert!(line0.contains("a: 1"), "object Node-style, got: {line0}");
  // Arrays render structurally (`[ x, y ]`) rather than via
  // JSON.stringify (`["x","y"]`) — the Node-ish renderer.
  assert!(
    console[1].message.contains("[ x, y ]"),
    "array rendered structurally, got: {}",
    console[1].message
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn native_url_class_parses_and_exposes_search_params() {
  let (_tmp, ctx) = make_ctx();
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  let r = session
    .execute(
      "const u = new URL('https://ex.com:8443/a/b?x=1&y=2#frag'); \
       return { href: u.href, host: u.host, hostname: u.hostname, port: u.port, \
                proto: u.protocol, path: u.pathname, search: u.search, hash: u.hash, \
                origin: u.origin, sp: u.searchParams.get('y'), str: String(u) };",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match r.result.outcome {
    Outcome::Ok { success } => {
      let v = &success.value;
      assert_eq!(v["host"], serde_json::json!("ex.com:8443"), "{v}");
      assert_eq!(v["hostname"], serde_json::json!("ex.com"));
      assert_eq!(v["port"], serde_json::json!("8443"));
      assert_eq!(v["proto"], serde_json::json!("https:"));
      assert_eq!(v["path"], serde_json::json!("/a/b"));
      assert_eq!(v["search"], serde_json::json!("?x=1&y=2"));
      assert_eq!(v["hash"], serde_json::json!("#frag"));
      assert_eq!(v["origin"], serde_json::json!("https://ex.com:8443"));
      assert_eq!(v["sp"], serde_json::json!("2"), "searchParams via native URL: {v}");
      assert_eq!(v["str"], serde_json::json!("https://ex.com:8443/a/b?x=1&y=2#frag"));
    },
    Outcome::Error { error } => panic!("native URL failed: {error:?}"),
  }
}
