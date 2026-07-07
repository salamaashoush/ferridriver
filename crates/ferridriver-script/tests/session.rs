#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Persistent-session semantics: a `Session` reuses one `QuickJS` VM
//! across many `execute` calls so user `globalThis` state survives
//! REPL-style, while a poisoning timeout marks the VM for rebuild.

use std::sync::Arc;
use std::time::Duration;

use ferridriver_script::{
  InMemoryVars, Outcome, PathSandbox, PluginBinding, RunContext, RunOptions, ScriptEngineConfig, ScriptErrorKind,
  Session, compile_and_extract_plugins,
};

/// A one-tool plugin whose handler bumps a `globalThis` counter so a
/// second invocation in the same session observes the first's state.
const DEMO_PLUGIN: &str = "defineTool({ name: 'demo', handler: async ({ args }) => { \
  globalThis.__n = (globalThis.__n || 0) + 1; return { n: globalThis.__n, got: args }; } });";

const BOX_PLUGIN: &str =
  "defineTool({ name: 'box.login', handler: async ({ args }) => ({ ok: true, user: args.user }) });";

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
      name: cp.path.display().to_string(),
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
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  let r1 = session
    .execute(
      "return await tools['demo']({ x: 1 });",
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
      "return await tools['demo']({ x: 2 });",
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
async fn dotted_tool_names_are_projected_as_namespaces() {
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("box.js");
  std::fs::write(&path, BOX_PLUGIN).expect("write plugin");
  let (compiled, failures) = compile_and_extract_plugins(&[path]).await;
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
      name: cp.path.display().to_string(),
    }],
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  let r = session
    .execute(
      "return { \
        flat: await tools['box.login']({ user: 'a' }), \
        nested: await tools.box.login({ user: 'b' }), \
        tools: await tools.box.login({ user: 'c' }), \
        ferridriver: await ferridriver.tools.box.login({ user: 'd' }), \
        global: await box.login({ user: 'e' }) \
      };",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match r.result.outcome {
    Outcome::Ok { success } => assert_eq!(
      success.value,
      serde_json::json!({
        "flat": { "ok": true, "user": "a" },
        "nested": { "ok": true, "user": "b" },
        "tools": { "ok": true, "user": "c" },
        "ferridriver": { "ok": true, "user": "d" },
        "global": { "ok": true, "user": "e" }
      })
    ),
    Outcome::Error { error } => panic!("namespaced plugin failed: {error:?}"),
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
     defineTool({ name: 'ts', exposeAsMcpTool: true, \
       async handler({ args }: { args: In }) { return { tag: tag(args.n) }; } });\n",
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
      name: cp.path.display().to_string(),
    }],
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  let r = session
    .execute("return await tools['ts']({ n: 7 });", &[], RunOptions::default(), &ctx)
    .await;
  match r.result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!({ "tag": "t7" })),
    Outcome::Error { error } => panic!("ts plugin failed: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn allow_net_capability_is_enforced_on_the_request_binding() {
  // The `net` capability must default-deny once declared: a host not in
  // the list is rejected BEFORE the call, and an allowed host passes the
  // guard through to the real client (where it fails for an unrelated,
  // non-allow.net reason — proving the guard let it through).
  const NET_PLUGIN: &str = "defineTool({ name: 'net', \
    allow: { net: ['127.0.0.1'] }, \
    handler: async ({ args, request }) => { await request.get(args.url); return 'ok'; } });";
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("net.js");
  std::fs::write(&path, NET_PLUGIN).expect("write plugin");
  let (compiled, failures) = compile_and_extract_plugins(&[path]).await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let cp = compiled.into_iter().next().expect("one compiled plugin");

  let sb_tmp = tempfile::tempdir().expect("tempdir");
  let request = Arc::new(ferridriver::http_client::HttpClient::new(
    ferridriver::http_client::HttpClientOptions::default(),
  ));
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(sb_tmp.path()).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: Some(request),
    browser: None,
    plugins: vec![PluginBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
    }],
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  // Disallowed host: rejected by the capability guard, no call made.
  let blocked = session
    .execute(
      "return await tools['net']({ url: 'http://blocked.test/' });",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match blocked.result.outcome {
    Outcome::Error { error } => {
      assert!(
        error.message.contains("not in allow.net") && error.message.contains("blocked.test"),
        "expected an allow.net denial naming the host, got: {}",
        error.message
      );
    },
    Outcome::Ok { .. } => panic!("disallowed host must be rejected by the net capability"),
  }

  // Allowed host: guard passes; the real client is reached and fails
  // for a non-capability reason (connection refused on port 1).
  let allowed = session
    .execute(
      "return await tools['net']({ url: 'http://127.0.0.1:1/' });",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match allowed.result.outcome {
    Outcome::Error { error } => assert!(
      !error.message.contains("allow.net"),
      "allowed host must pass the guard; got an allow.net error instead: {}",
      error.message
    ),
    Outcome::Ok { .. } => {},
  }
}

/// Rule 9: the `allow.net` allow-list must bind the global `fetch` too,
/// not only the plugin's `request` arg. Before this fix a net-restricted
/// tool could reach any host via `fetch` (the global was wired straight
/// to the raw session context). Proven page-visible: a disallowed host
/// is rejected with the allow.net denial BEFORE any I/O; an allowed host
/// passes the guard and fails only for an unrelated connection reason.
#[tokio::test(flavor = "multi_thread")]
async fn allow_net_capability_is_enforced_on_the_global_fetch() {
  const NET_PLUGIN: &str = "defineTool({ name: 'netf', \
    allow: { net: ['127.0.0.1'] }, \
    handler: async ({ args }) => { const r = await fetch(args.url); return r.status; } });";
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("netf.js");
  std::fs::write(&path, NET_PLUGIN).expect("write plugin");
  let (compiled, failures) = compile_and_extract_plugins(&[path]).await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let cp = compiled.into_iter().next().expect("one compiled plugin");

  let sb_tmp = tempfile::tempdir().expect("tempdir");
  let request = Arc::new(ferridriver::http_client::HttpClient::new(
    ferridriver::http_client::HttpClientOptions::default(),
  ));
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(sb_tmp.path()).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: Some(request),
    browser: None,
    plugins: vec![PluginBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
    }],
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  // Disallowed host: rejected by the capability guard before any I/O.
  let blocked = session
    .execute(
      "return await tools['netf']({ url: 'http://blocked.test/' });",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match blocked.result.outcome {
    Outcome::Error { error } => assert!(
      error.message.contains("not in allow.net") && error.message.contains("blocked.test"),
      "fetch to a disallowed host must be rejected by allow.net, got: {}",
      error.message
    ),
    Outcome::Ok { .. } => panic!("disallowed fetch host must be rejected by the net capability"),
  }

  // Allowed host: guard passes; the real client is reached and fails for
  // a non-capability reason (connection refused on port 1).
  let allowed = session
    .execute(
      "return await tools['netf']({ url: 'http://127.0.0.1:1/' });",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match allowed.result.outcome {
    Outcome::Error { error } => assert!(
      !error.message.contains("allow.net"),
      "allowed fetch host must pass the guard; got an allow.net error: {}",
      error.message
    ),
    Outcome::Ok { .. } => {},
  }
}

/// The per-poll policy bracket must not leak across tools. Two tools in
/// one VM: `restricted` (allow.net = [127.0.0.1]) and `open` (no net
/// capability). Run concurrently via `Promise.all` so their handler
/// futures interleave at awaits. `restricted`'s fetch to a disallowed
/// host must still be denied, while `open`'s fetch is unrestricted —
/// proving the active policy follows whichever continuation is running,
/// not whichever ran last.
#[tokio::test(flavor = "multi_thread")]
async fn fetch_net_policy_does_not_leak_between_concurrent_tools() {
  const PLUGIN: &str = "defineTool({ name: 'restricted', allow: { net: ['127.0.0.1'] }, \
      handler: async ({ args }) => { try { await fetch(args.url); return 'reached'; } \
        catch (e) { return 'denied:' + String(e.message || e); } } }); \
    defineTool({ name: 'open', \
      handler: async ({ args }) => { try { await fetch(args.url); return 'reached'; } \
        catch (e) { return 'err:' + String(e.message || e); } } });";
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("leak.js");
  std::fs::write(&path, PLUGIN).expect("write plugin");
  let (compiled, failures) = compile_and_extract_plugins(&[path]).await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let cp = compiled.into_iter().next().expect("one compiled plugin");

  let sb_tmp = tempfile::tempdir().expect("tempdir");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(sb_tmp.path()).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: Some(Arc::new(ferridriver::http_client::HttpClient::new(
      ferridriver::http_client::HttpClientOptions::default(),
    ))),
    browser: None,
    plugins: vec![PluginBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
    }],
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  let r = session
    .execute(
      "const [a, b] = await Promise.all([ \
         tools['restricted']({ url: 'http://blocked.test/' }), \
         tools['open']({ url: 'http://127.0.0.1:1/' }) ]); \
       return { restricted: a, open: b };",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match r.result.outcome {
    Outcome::Ok { success } => {
      let restricted = success.value["restricted"].as_str().unwrap_or_default();
      let open = success.value["open"].as_str().unwrap_or_default();
      assert!(
        restricted.contains("denied:") && restricted.contains("not in allow.net"),
        "restricted tool's fetch must be denied by allow.net even under concurrency, got: {restricted}"
      );
      assert!(
        !open.contains("not in allow.net"),
        "the unrestricted tool's fetch must not inherit another tool's allow.net, got: {open}"
      );
    },
    Outcome::Error { error } => panic!("concurrent tool run failed: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn extension_branches_on_ferridriver_host_flag() {
  // One extension file, two contributions gated on the native
  // `ferridriver.host` flag: a tool only under MCP, a step only under
  // BDD. Under host=Mcp the tool registers (callable); under host=Bdd
  // it does NOT (the `tools.<name>` binding is absent).
  const EXT: &str = "if (ferridriver.host === 'mcp') { \
      defineTool({ name: 'mcpOnly', handler: async () => 'tool-ran' }); \
    } \
    if (ferridriver.host === 'bdd') { Given('a step', () => {}); }";
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("ext.js");
  std::fs::write(&path, EXT).expect("write ext");
  let (compiled, failures) = compile_and_extract_plugins(&[path]).await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let cp = compiled.into_iter().next().expect("one compiled");

  let mk = |host| {
    let sb = tempfile::tempdir().expect("tempdir");
    let ctx = RunContext {
      vars: Arc::new(InMemoryVars::new()),
      sandbox: Arc::new(PathSandbox::new(sb.path()).expect("sandbox")),
      artifacts: None,
      page: None,
      browser_context: None,
      request: None,
      browser: None,
      plugins: vec![PluginBinding {
        bytecode: cp.bytecode.clone(),
        name: cp.path.display().to_string(),
      }],
      host,
      caps: ferridriver_script::ScriptCaps::default(),
    };
    (sb, ctx)
  };

  // host = Mcp -> the tool registered and is callable.
  let (_sb1, ctx) = mk(ferridriver_script::ExtensionHost::Mcp);
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  let r = session
    .execute("return await tools['mcpOnly']({});", &[], RunOptions::default(), &ctx)
    .await;
  match r.result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("tool-ran")),
    Outcome::Error { error } => panic!("mcp host should expose the tool: {error:?}"),
  }

  // host = Bdd -> the tool was NOT registered; the binding is absent.
  let (_sb2, ctx) = mk(ferridriver_script::ExtensionHost::Bdd);
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  let r = session
    .execute("return typeof tools['mcpOnly'];", &[], RunOptions::default(), &ctx)
    .await;
  match r.result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("undefined")),
    Outcome::Error { error } => panic!("bdd host lookup should be undefined, not error: {error:?}"),
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
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
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
async fn native_await_park_hits_the_backstop_and_poisons() {
  let (_tmp, ctx) = make_ctx();
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  // The interrupt handler only runs while bytecode executes; a script
  // parked on a never-resolving native promise would otherwise hold the
  // session slot forever. The tokio-level backstop must fire instead.
  let timed = session
    .execute(
      "await new Promise(() => {});",
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
  assert!(timed.poisoned, "a backstop fire must poison the session");
}

#[tokio::test(flavor = "multi_thread")]
async fn finished_call_deadline_does_not_halt_later_vm_entry() {
  let (_tmp, ctx) = make_ctx();
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  let quick = session
    .execute(
      "return 1;",
      &[],
      RunOptions {
        timeout: Some(Duration::from_millis(100)),
        ..RunOptions::default()
      },
      &ctx,
    )
    .await;
  assert!(matches!(quick.result.outcome, Outcome::Ok { .. }));
  assert!(!quick.poisoned);

  tokio::time::sleep(Duration::from_millis(250)).await;

  // Route / exposeFunction / screencast dispatch re-enters the VM
  // between calls via the session's VM event loop. An armed deadline
  // left over from the finished call would force-halt this entry.
  let vm = session.vm_handle();
  let entered = ferridriver_script::vm_with!(vm => |c| {
    c.eval::<f64, _>("let s = 0; for (let i = 0; i < 1e6; i++) s += i; s")
  })
  .await
  .expect("VM loop gone");
  assert!(
    entered.is_ok(),
    "between-call VM entry must not be halted by the previous call's deadline: {entered:?}"
  );
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
  // Arrays render structurally with strings quoted (`[ 'x', 'y' ]`,
  // Node's util.inspect shape) rather than via JSON.stringify
  // (`["x","y"]`).
  assert!(
    console[1].message.contains("[ 'x', 'y' ]"),
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

async fn binding_from(src: &str) -> (tempfile::TempDir, Result<PluginBinding, String>) {
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("ext.ts");
  std::fs::write(&path, src).expect("write plugin");
  let (compiled, failures) = compile_and_extract_plugins(&[path]).await;
  if let Some((_, e)) = failures.into_iter().next() {
    return (tmp, Err(e.message));
  }
  let cp = compiled.into_iter().next().expect("one compiled plugin");
  (
    tmp,
    Ok(PluginBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
    }),
  )
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_tool_name_is_rejected_at_load() {
  // Two defineTool calls with the same name must fail the file (the
  // shared registry rejects the second) instead of silently letting the
  // last registration clobber the binding.
  let (_tmp, res) = binding_from(
    "defineTool({ name: 'dup', handler: async () => 1 });\n\
     defineTool({ name: 'dup', handler: async () => 2 });\n",
  )
  .await;
  let err = res.expect_err("duplicate tool name must fail compilation");
  assert!(err.contains("duplicate tool name `dup`"), "unexpected error: {err}");

  // An empty name is likewise rejected.
  let (_tmp2, res2) = binding_from("defineTool({ name: '  ', handler: async () => 1 });\n").await;
  let err2 = res2.expect_err("empty tool name must fail compilation");
  assert!(err2.contains("non-empty string"), "unexpected error: {err2}");
}

#[tokio::test(flavor = "multi_thread")]
async fn per_tool_timeout_ms_is_enforced_for_every_caller() {
  // `timeoutMs` races the handler natively in dispatch_tool, so the
  // bound holds for an in-VM `tools.<name>()` call (not only the MCP
  // entry point). A handler that sleeps past the bound rejects; a fast
  // one resolves.
  let (_tmp, binding) = binding_from(
    "defineTool({ name: 'slow', timeoutMs: 50, handler: async () => { \
       await new Promise(r => setTimeout(r, 400)); return 'late'; } });\n\
     defineTool({ name: 'fast', timeoutMs: 5000, handler: async () => 'quick' });\n",
  )
  .await;
  let binding = binding.expect("compiles");

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
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  let slow = session
    .execute(
      "try { await tools['slow'](); return 'resolved'; } catch (e) { return String(e); }",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match slow.result.outcome {
    Outcome::Ok { success } => {
      let s = success.value.as_str().unwrap_or_default();
      assert!(
        s.contains("timed out after 50ms"),
        "slow tool should have timed out, got: {s}"
      );
    },
    Outcome::Error { error } => panic!("expected caught rejection, not engine error: {error:?}"),
  }

  let fast = session
    .execute("return await tools['fast']();", &[], RunOptions::default(), &ctx)
    .await;
  match fast.result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("quick")),
    Outcome::Error { error } => panic!("fast tool within its timeout must resolve: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn plugin_top_level_await_registers_tools_in_session() {
  // `install_plugins` must drive the module's eval promise to
  // completion: a tool registered after a top-level `await` is in the
  // extracted manifest (extraction awaits), so the session binding must
  // exist too — otherwise the manifest advertises a tool the VM lacks.
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("late.js");
  std::fs::write(
    &path,
    "const v = await Promise.resolve('deferred');\n\
     defineTool({ name: 'late', handler: async () => v });\n",
  )
  .expect("write plugin");
  let (compiled, failures) = compile_and_extract_plugins(&[path]).await;
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
      name: cp.path.display().to_string(),
    }],
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  let r = session
    .execute("return await tools['late']();", &[], RunOptions::default(), &ctx)
    .await;
  match r.result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("deferred")),
    Outcome::Error { error } => panic!("top-level-await plugin tool must be callable: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn broken_plugin_is_skipped_without_killing_the_session() {
  // Session-time install must isolate per file like startup does: one
  // plugin whose top-level throws (here: only under the script host, so
  // it passes manifest extraction, which runs as host 'mcp') must not
  // take down the whole VM — the healthy plugin's tool stays callable.
  let tmp = tempfile::tempdir().expect("tempdir");
  let bad = tmp.path().join("bad.js");
  std::fs::write(
    &bad,
    "if (globalThis.ferridriver?.host === 'script') { throw new Error('boom'); }\n\
     defineTool({ name: 'bad', handler: async () => 'never' });\n",
  )
  .expect("write bad plugin");
  let good = tmp.path().join("good.js");
  std::fs::write(&good, "defineTool({ name: 'good', handler: async () => 'fine' });\n").expect("write good plugin");
  let (compiled, failures) = compile_and_extract_plugins(&[bad, good]).await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let plugins: Vec<PluginBinding> = compiled
    .into_iter()
    .map(|cp| PluginBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
    })
    .collect();
  assert_eq!(plugins.len(), 2);

  let sb_tmp = tempfile::tempdir().expect("tempdir");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(sb_tmp.path()).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    plugins,
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("one broken plugin must not fail session create");
  let r = session
    .execute("return await tools['good']();", &[], RunOptions::default(), &ctx)
    .await;
  match r.result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("fine")),
    Outcome::Error { error } => panic!("healthy plugin must survive a sibling's failure: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn assertion_failures_throw_a_named_assertion_error() {
  // Binding-level failures must be real `Error` instances with a
  // semantic `name` (here AssertionError), not the TypeError-with-
  // mangled-message that `Error::new_from_js_message` produces —
  // user scripts branch on `e.name` exactly like with core errors.
  let (_tmp, ctx) = make_ctx();
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  let r = session
    .execute(
      "try { expect(1).toBe(2); return 'no-throw'; } \
       catch (e) { return { name: e.name, isError: e instanceof Error }; }",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match r.result.outcome {
    Outcome::Ok { success } => {
      assert_eq!(
        success.value["name"],
        serde_json::json!("AssertionError"),
        "{:?}",
        success.value
      );
      assert_eq!(success.value["isError"], serde_json::json!(true), "{:?}", success.value);
    },
    Outcome::Error { error } => panic!("expect failure must be catchable in JS: {error:?}"),
  }
}

// ── Node-compat details of the native timers / URLSearchParams / console ──

#[tokio::test(flavor = "multi_thread")]
async fn set_timeout_passes_extra_args_and_clear_tolerates_garbage() {
  let (_tmp, ctx) = make_ctx();
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  let r = session
    .execute(
      "clearTimeout(undefined); clearTimeout(null); clearTimeout(42); clearInterval();\n\
       return await new Promise((resolve) => setTimeout((a, b) => resolve(a + b), 10, 'x', 'y'));",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match r.result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("xy")),
    Outcome::Error { error } => panic!("timer args/clear tolerance failed: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn url_search_params_node_semantics() {
  let (_tmp, ctx) = make_ctx();
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  let r = session
    .execute(
      "const fromNull = new URLSearchParams(null).toString();\n\
       const enc = new URLSearchParams('a=1 2&b=%C3%A9');\n\
       const encoded = enc.toString();\n\
       const decoded = enc.get('b');\n\
       const live = new URLSearchParams('a=1&b=2&c=3');\n\
       for (const [k] of live) { live.delete(k); }\n\
       const empty = new URLSearchParams('').size;\n\
       const s = new URLSearchParams('b=2&a=1&a=0'); s.sort();\n\
       return [fromNull, encoded, decoded, live.size, empty, s.toString()];",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match r.result.outcome {
    Outcome::Ok { success } => {
      assert_eq!(
        success.value,
        // Live-iterator deletion skips every other entry (index-based,
        // exactly like Node/WHATWG): a and c deleted, b survives.
        serde_json::json!(["null=", "a=1+2&b=%C3%A9", "\u{e9}", 1, 0, "a=1&a=0&b=2"]),
        "{:?}",
        success.value
      );
    },
    Outcome::Error { error } => panic!("URLSearchParams node semantics failed: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn console_printf_and_inspect_rendering() {
  let (_tmp, ctx) = make_ctx();
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  let r = session
    .execute(
      "console.log('%s scored %d%%', 'amy', 97, 'extra');\n\
       console.log(['x', 1]);\n\
       console.log(new Map([['a', 1]]));\n\
       console.log(new Set([1, 2]));\n\
       console.log(/ab+c/gi);\n\
       return null;",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  assert!(r.result.is_ok(), "{:?}", r.result);
  let console = &r.result.console;
  assert_eq!(console[0].message, "amy scored 97% extra", "{:?}", console[0]);
  assert_eq!(console[1].message, "[ 'x', 1 ]", "{:?}", console[1]);
  assert_eq!(console[2].message, "Map(1) { 'a' => 1 }", "{:?}", console[2]);
  assert_eq!(console[3].message, "Set(2) { 1, 2 }", "{:?}", console[3]);
  assert_eq!(console[4].message, "/ab+c/gi", "{:?}", console[4]);
}
