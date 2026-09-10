#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Persistent-session semantics: a `Session` reuses one `QuickJS` VM
//! across many `execute` calls so user `globalThis` state survives
//! REPL-style, while a poisoning timeout marks the VM for rebuild.

use std::sync::Arc;

use ferridriver_script::{
  ExtensionBinding, InMemoryVars, Outcome, RunContext, RunOptions, ScriptEngineConfig, Session,
  compile_and_extract_extensions,
};

/// A one-tool plugin whose handler bumps a `globalThis` counter so a
/// second invocation in the same session observes the first's state.
const DEMO_PLUGIN: &str = "defineTool({ name: 'demo', handler: async ({ args }) => { \
  globalThis.__n = (globalThis.__n || 0) + 1; return { n: globalThis.__n, got: args }; } });";

const NAMESPACED_PLUGIN: &str =
  "defineTool({ name: 'acme.login', handler: async ({ args }) => ({ ok: true, user: args.user }) });";

/// Bundle + compile the demo plugin through the production pipeline
/// (rolldown -> bytecode) and wrap it as a `ExtensionBinding`.
async fn demo_binding() -> (tempfile::TempDir, ExtensionBinding) {
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("demo.js");
  std::fs::write(&path, DEMO_PLUGIN).expect("write plugin");
  let (compiled, failures) =
    compile_and_extract_extensions(&[vec![path]], &ferridriver_config::ExtensionPolicyConfig::default()).await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let cp = compiled.into_iter().next().expect("one compiled plugin");
  assert!(!cp.bytecode.is_empty(), "compiled bytecode must be non-empty");
  (
    tmp,
    ExtensionBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
      source_map: None,
      provides: None,
    },
  )
}

/// Boxed: it awaits `Session::create`, so a caller awaiting it would
/// otherwise carry the engine config in its own future.
fn run_demo_plugin_twice() -> impl std::future::Future<Output = ()> + Send {
  Box::pin(async move {
    let (_plugin_tmp, binding) = demo_binding().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let ctx = RunContext {
      vars: Arc::new(InMemoryVars::new()),
      script_root: tmp.path().into(),
      artifacts: None,
      page: None,
      browser_context: None,
      request: None,
      browser: None,
      extensions: vec![binding],
      host: ferridriver_script::ExtensionHost::Script,
      caps: ferridriver_script::ScriptCaps::default(),
      session: None,
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
  })
}

#[tokio::test(flavor = "multi_thread")]
async fn dotted_tool_names_are_projected_as_namespaces() {
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("acme.js");
  std::fs::write(&path, NAMESPACED_PLUGIN).expect("write plugin");
  let (compiled, failures) =
    compile_and_extract_extensions(&[vec![path]], &ferridriver_config::ExtensionPolicyConfig::default()).await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let cp = compiled.into_iter().next().expect("one compiled plugin");

  let sb_tmp = tempfile::tempdir().expect("tempdir");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: sb_tmp.path().into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: vec![ExtensionBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
      source_map: None,
      provides: None,
    }],
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  let r = session
    .execute(
      "return { \
        flat: await tools['acme.login']({ user: 'a' }), \
        nested: await tools.acme.login({ user: 'b' }), \
        tools: await tools.acme.login({ user: 'c' }), \
        ferridriver: await ferridriver.tools.acme.login({ user: 'd' }), \
        global: await acme.login({ user: 'e' }) \
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

  let (compiled, failures) = compile_and_extract_extensions(
    &[vec![tmp.path().join("plug.ts")]],
    &ferridriver_config::ExtensionPolicyConfig::default(),
  )
  .await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let cp = compiled.into_iter().next().expect("one compiled plugin");

  let sb_tmp = tempfile::tempdir().expect("tempdir");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: sb_tmp.path().into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: vec![ExtensionBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
      source_map: None,
      provides: None,
    }],
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
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
  let (compiled, failures) =
    compile_and_extract_extensions(&[vec![path]], &ferridriver_config::ExtensionPolicyConfig::default()).await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let cp = compiled.into_iter().next().expect("one compiled plugin");

  let sb_tmp = tempfile::tempdir().expect("tempdir");
  let request = Arc::new(ferridriver::http_client::HttpClient::new(
    ferridriver::http_client::HttpClientOptions::default(),
  ));
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: sb_tmp.path().into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: Some(request),
    browser: None,
    extensions: vec![ExtensionBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
      source_map: None,
      provides: None,
    }],
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
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
        error.message.contains("permission denied") && error.message.contains("blocked.test"),
        "expected a denial naming the host, got: {}",
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
      !error.message.contains("permission denied"),
      "allowed host must pass the guard; got a denial instead: {}",
      error.message
    ),
    Outcome::Ok { .. } => {},
  }
}

/// Rule 9: the `allow.net` allow-list binds the `fetch` capability a
/// handler is handed the same way it binds its `request`: one engine,
/// one policy. Proven page-visible: a disallowed host is refused before
/// any I/O; an allowed host passes the guard and fails only for an
/// unrelated connection reason.
#[tokio::test(flavor = "multi_thread")]
async fn allow_net_capability_is_enforced_on_the_handler_fetch() {
  const NET_PLUGIN: &str = "defineTool({ name: 'netf', \
    allow: { net: ['127.0.0.1'] }, \
    handler: async ({ args, fetch }) => { const r = await fetch(args.url); return r.status; } });";
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("netf.js");
  std::fs::write(&path, NET_PLUGIN).expect("write plugin");
  let (compiled, failures) =
    compile_and_extract_extensions(&[vec![path]], &ferridriver_config::ExtensionPolicyConfig::default()).await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let cp = compiled.into_iter().next().expect("one compiled plugin");

  let sb_tmp = tempfile::tempdir().expect("tempdir");
  let request = Arc::new(ferridriver::http_client::HttpClient::new(
    ferridriver::http_client::HttpClientOptions::default(),
  ));
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: sb_tmp.path().into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: Some(request),
    browser: None,
    extensions: vec![ExtensionBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
      source_map: None,
      provides: None,
    }],
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
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
      error.message.contains("permission denied") && error.message.contains("blocked.test"),
      "fetch to a disallowed host must be refused, got: {}",
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
      !error.message.contains("permission denied"),
      "allowed fetch host must pass the guard; got an allow.net error: {}",
      error.message
    ),
    Outcome::Ok { .. } => {},
  }
}

/// Each tool's `fetch` capability carries its own attenuation, so two
/// tools in one VM cannot see each other's. `restricted` (allow.net =
/// [127.0.0.1]) and `open` (no net capability) run concurrently via
/// `Promise.all` so their handler futures interleave at awaits:
/// `restricted`'s fetch to a disallowed host is refused, while `open`'s
/// answers to the session's policy alone.
#[tokio::test(flavor = "multi_thread")]
async fn fetch_net_policy_does_not_leak_between_concurrent_tools() {
  const PLUGIN: &str = "defineTool({ name: 'restricted', allow: { net: ['127.0.0.1'] }, \
      handler: async ({ args, fetch }) => { try { await fetch(args.url); return 'reached'; } \
        catch (e) { return 'denied:' + String(e.message || e); } } }); \
    defineTool({ name: 'open', \
      handler: async ({ args, fetch }) => { try { await fetch(args.url); return 'reached'; } \
        catch (e) { return 'err:' + String(e.message || e); } } });";
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("leak.js");
  std::fs::write(&path, PLUGIN).expect("write plugin");
  let (compiled, failures) =
    compile_and_extract_extensions(&[vec![path]], &ferridriver_config::ExtensionPolicyConfig::default()).await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let cp = compiled.into_iter().next().expect("one compiled plugin");

  let sb_tmp = tempfile::tempdir().expect("tempdir");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: sb_tmp.path().into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: Some(Arc::new(ferridriver::http_client::HttpClient::new(
      ferridriver::http_client::HttpClientOptions::default(),
    ))),
    browser: None,
    extensions: vec![ExtensionBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
      source_map: None,
      provides: None,
    }],
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
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
        restricted.contains("denied:") && restricted.contains("permission denied"),
        "restricted tool's fetch must be refused even under concurrency, got: {restricted}"
      );
      assert!(
        !open.contains("permission denied"),
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
  let (compiled, failures) =
    compile_and_extract_extensions(&[vec![path]], &ferridriver_config::ExtensionPolicyConfig::default()).await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let cp = compiled.into_iter().next().expect("one compiled");

  let mk = |host| {
    let sb = tempfile::tempdir().expect("tempdir");
    let ctx = RunContext {
      vars: Arc::new(InMemoryVars::new()),
      script_root: sb.path().into(),
      artifacts: None,
      page: None,
      browser_context: None,
      request: None,
      browser: None,
      extensions: vec![ExtensionBinding {
        bytecode: cp.bytecode.clone(),
        name: cp.path.display().to_string(),
        source_map: None,
        provides: None,
      }],
      host,
      caps: ferridriver_script::ScriptCaps::default(),
      session: None,
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

async fn binding_from(src: &str) -> (tempfile::TempDir, Result<ExtensionBinding, String>) {
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("ext.ts");
  std::fs::write(&path, src).expect("write plugin");
  let (compiled, failures) =
    compile_and_extract_extensions(&[vec![path]], &ferridriver_config::ExtensionPolicyConfig::default()).await;
  if let Some((_, e)) = failures.into_iter().next() {
    return (tmp, Err(e.message));
  }
  let cp = compiled.into_iter().next().expect("one compiled plugin");
  (
    tmp,
    Ok(ExtensionBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
      source_map: None,
      provides: None,
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
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: tmp.path().into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: vec![binding],
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
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
  // `install_extensions` must drive the module's eval promise to
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
  let (compiled, failures) =
    compile_and_extract_extensions(&[vec![path]], &ferridriver_config::ExtensionPolicyConfig::default()).await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let cp = compiled.into_iter().next().expect("one compiled plugin");

  let sb_tmp = tempfile::tempdir().expect("tempdir");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: sb_tmp.path().into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: vec![ExtensionBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
      source_map: None,
      provides: None,
    }],
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
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
  // plugin whose top-level throws under the SCRIPT host only must not
  // take down the whole VM — the healthy plugin's tool stays callable.
  // Extraction evaluates under every host, so the throw is recorded
  // against the script host's snapshot; a file that threw under every
  // host would be a compile failure instead.
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
  let (compiled, failures) = compile_and_extract_extensions(
    &[vec![bad], vec![good]],
    &ferridriver_config::ExtensionPolicyConfig::default(),
  )
  .await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let extensions: Vec<ExtensionBinding> = compiled
    .into_iter()
    .map(|cp| ExtensionBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
      source_map: None,
      provides: None,
    })
    .collect();
  assert_eq!(extensions.len(), 2);

  let sb_tmp = tempfile::tempdir().expect("tempdir");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: sb_tmp.path().into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions,
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
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

/// A session carrying one tool whose manifest declares `127.0.0.1` and
/// nothing else, over an unrestricted session policy. Boxed for the same
/// reason as [`Session::create`], which it awaits.
fn net_tool_session()
-> impl std::future::Future<Output = (tempfile::TempDir, tempfile::TempDir, RunContext, Session)> + Send {
  Box::pin(async move {
    const NET_PLUGIN: &str = "defineTool({ name: 'netg', \
      allow: { net: ['127.0.0.1'] }, \
      handler: async ({ args, request }) => { \
        if (args.via === 'global') { await globalThis.request.get(args.url); return 'ok'; } \
        await request.get(args.url); return 'ok'; } });";
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("netg.js");
    std::fs::write(&path, NET_PLUGIN).expect("write plugin");
    let (compiled, failures) =
      compile_and_extract_extensions(&[vec![path]], &ferridriver_config::ExtensionPolicyConfig::default()).await;
    assert!(failures.is_empty(), "compile failures: {failures:?}");
    let cp = compiled.into_iter().next().expect("one compiled plugin");

    let sb_tmp = tempfile::tempdir().expect("tempdir");
    let ctx = RunContext {
      vars: Arc::new(InMemoryVars::new()),
      script_root: sb_tmp.path().into(),
      artifacts: None,
      page: None,
      browser_context: None,
      request: Some(Arc::new(ferridriver::http_client::HttpClient::new(
        ferridriver::http_client::HttpClientOptions::default(),
      ))),
      browser: None,
      extensions: vec![ExtensionBinding {
        bytecode: cp.bytecode,
        name: cp.path.display().to_string(),
        source_map: None,
        provides: None,
      }],
      host: ferridriver_script::ExtensionHost::Script,
      caps: ferridriver_script::ScriptCaps::default(),
      session: None,
    };
    let session = Session::create(ScriptEngineConfig::default(), &ctx)
      .await
      .expect("session create");
    (tmp, sb_tmp, ctx, session)
  })
}

/// Authority is the object a handler was handed, not a scope around
/// its stack: the `request` capability a restricted tool receives
/// refuses a host outside its list, and the global `request` the tool
/// can also reach answers to the session's policy, which here is
/// everything. That is the model (privilege is per realm); an extension
/// that must be confined below the session gets a session of its own.
#[tokio::test(flavor = "multi_thread")]
async fn allow_net_capability_attenuates_the_handler_request_not_the_global() {
  let (_tmp, _sb_tmp, ctx, session) = net_tool_session().await;

  let blocked = session
    .execute(
      "return await tools['netg']({ url: 'http://blocked.test/' });",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match blocked.result.outcome {
    Outcome::Error { error } => assert!(
      error.message.contains("permission denied") && error.message.contains("blocked.test"),
      "the handler's request must refuse a host outside its list, got: {}",
      error.message
    ),
    Outcome::Ok { .. } => panic!("the handler's request to a disallowed host must be rejected"),
  }

  // The global answers to the session's policy: no denial, only the
  // connection failure a host that does not resolve produces.
  let via_global = session
    .execute(
      "return await tools['netg']({ url: 'http://blocked.test/', via: 'global' });",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  if let Outcome::Error { error } = via_global.result.outcome {
    assert!(
      !error.message.contains("permission denied"),
      "the global request answers to the session, not the tool's list: {}",
      error.message
    );
  }

  let allowed = session
    .execute(
      "return await tools['netg']({ url: 'http://127.0.0.1:1/' });",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match allowed.result.outcome {
    Outcome::Error { error } => assert!(
      !error.message.contains("permission denied"),
      "allowed host must pass the guard: {}",
      error.message
    ),
    Outcome::Ok { .. } => {},
  }

  // The restriction ends with the handler: a top-level script call on
  // the SAME session sees the unrestricted resting policy again.
  let after = session
    .execute(
      "try { await globalThis.request.get('http://blocked.test/'); return 'reached'; } \
       catch (e) { return String(e.message || e); }",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match after.result.outcome {
    Outcome::Ok { success } => {
      let s = success.value.as_str().unwrap_or_default();
      assert!(
        !s.contains("allow.net"),
        "top-level request must be unrestricted after the handler returned, got: {s}"
      );
    },
    Outcome::Error { error } => panic!("top-level probe failed unexpectedly: {error:?}"),
  }
}

/// The attenuated capability keeps its attenuation wherever it
/// travels: a `setTimeout` callback armed inside a restricted handler
/// that closes over the handler's `fetch` is still refused when it
/// fires, after the dispatch returned; a timer armed at top level uses
/// the global, which answers to the session.
#[tokio::test(flavor = "multi_thread")]
async fn allow_net_follows_the_capability_into_a_timer_callback() {
  const NET_PLUGIN: &str = "defineTool({ name: 'nett', \
    allow: { net: ['127.0.0.1'] }, \
    handler: ({ args, fetch }) => new Promise((resolve) => { \
      setTimeout(async () => { \
        try { await fetch(args.url); resolve('reached'); } \
        catch (e) { resolve('denied:' + String(e.message || e)); } \
      }, 10); }) });";
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("nett.js");
  std::fs::write(&path, NET_PLUGIN).expect("write plugin");
  let (compiled, failures) =
    compile_and_extract_extensions(&[vec![path]], &ferridriver_config::ExtensionPolicyConfig::default()).await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let cp = compiled.into_iter().next().expect("one compiled plugin");

  let sb_tmp = tempfile::tempdir().expect("tempdir");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: sb_tmp.path().into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: Some(Arc::new(ferridriver::http_client::HttpClient::new(
      ferridriver::http_client::HttpClientOptions::default(),
    ))),
    browser: None,
    extensions: vec![ExtensionBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
      source_map: None,
      provides: None,
    }],
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  let r = session
    .execute(
      "return await tools['nett']({ url: 'http://blocked.test/' });",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match r.result.outcome {
    Outcome::Ok { success } => {
      let s = success.value.as_str().unwrap_or_default();
      assert!(
        s.contains("denied:") && s.contains("permission denied"),
        "the handler's fetch must refuse the host from a timer too, got: {s}"
      );
    },
    Outcome::Error { error } => panic!("timer tool run failed: {error:?}"),
  }

  // Control: a top-level timer (no active tool) stays unrestricted —
  // fetch reaches the client and fails only for a connection reason.
  let control = session
    .execute(
      "return await new Promise((resolve) => setTimeout(async () => { \
         try { await fetch('http://127.0.0.1:1/'); resolve('reached'); } \
         catch (e) { resolve('err:' + String(e.message || e)); } }, 10));",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  match control.result.outcome {
    Outcome::Ok { success } => {
      let s = success.value.as_str().unwrap_or_default();
      assert!(
        !s.contains("allow.net"),
        "a top-level timer must stay unrestricted, got: {s}"
      );
    },
    Outcome::Error { error } => panic!("control timer run failed: {error:?}"),
  }
}

/// Startup manifest extraction must evaluate the module in the same
/// ambient environment a session VM provides: a plugin whose top level
/// uses standard globals (`TextEncoder`, `setTimeout`, `crypto`, `console`,
/// `expect`) loads fine instead of failing extraction while working
/// in-session.
#[tokio::test(flavor = "multi_thread")]
async fn extraction_environment_matches_session_for_top_level_globals() {
  const EXT: &str = "console.log('extension booting');\n\
    const enc = new TextEncoder().encode('hi');\n\
    const id = crypto.randomUUID();\n\
    expect(enc.length).toBe(2);\n\
    await new Promise((r) => setTimeout(r, 5));\n\
    defineTool({ name: 'ambient', handler: async () => ({ len: enc.length, hasId: id.length > 0 }) });\n";
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("ambient.js");
  std::fs::write(&path, EXT).expect("write plugin");
  let (compiled, failures) =
    compile_and_extract_extensions(&[vec![path]], &ferridriver_config::ExtensionPolicyConfig::default()).await;
  assert!(
    failures.is_empty(),
    "top-level standard globals must not fail extraction: {failures:?}"
  );
  let cp = compiled.into_iter().next().expect("one compiled plugin");
  assert!(
    cp.manifests_json().contains("\"ambient\""),
    "manifest extracted: {}",
    cp.manifests_json()
  );

  let sb_tmp = tempfile::tempdir().expect("tempdir");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: sb_tmp.path().into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: vec![ExtensionBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
      source_map: None,
      provides: None,
    }],
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  let r = session
    .execute("return await tools['ambient']();", &[], RunOptions::default(), &ctx)
    .await;
  match r.result.outcome {
    Outcome::Ok { success } => {
      assert_eq!(success.value["len"], serde_json::json!(2));
      assert_eq!(success.value["hasId"], serde_json::json!(true));
    },
    Outcome::Error { error } => panic!("ambient plugin tool failed: {error:?}"),
  }
}

/// `Session::execute_tool` — the native path behind the MCP
/// `invoke_extension_tool` / promoted-tool routes — must dispatch through the
/// same registry body as `tools.<name>` (args delivered, value
/// returned) without compiling a synthesized script, and must name the
/// tool in the error when it is not installed in this session.
#[tokio::test(flavor = "multi_thread")]
async fn execute_tool_invokes_natively_and_reports_missing_tools() {
  let (_plugin_tmp, binding) = demo_binding().await;
  let tmp = tempfile::tempdir().expect("tempdir");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: tmp.path().into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: vec![binding],
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  let r1 = session
    .execute_tool("demo", serde_json::json!({ "x": 1 }), RunOptions::default(), &ctx)
    .await;
  match r1.result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!({ "n": 1, "got": { "x": 1 } })),
    Outcome::Error { error } => panic!("native tool call failed: {error:?}"),
  }

  // Handler `globalThis` state persists across native invocations, and
  // the native path and the JS `tools.demo(...)` binding share one
  // registry entry (the counter keeps climbing across both).
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
    Outcome::Error { error } => panic!("JS tool call after native call failed: {error:?}"),
  }

  let missing = session
    .execute_tool("nope", serde_json::json!({}), RunOptions::default(), &ctx)
    .await;
  match missing.result.outcome {
    Outcome::Error { error } => assert!(
      error.message.contains("`nope`") && error.message.contains("not installed"),
      "missing tool must be named in the error, got: {}",
      error.message
    ),
    Outcome::Ok { .. } => panic!("unknown tool must be an error"),
  }
}

/// A handler rejection through the native path surfaces as a run error
/// (the MCP layer turns it into `is_error`), and the per-tool `timeoutMs`
/// bound holds exactly as it does for the JS entry point.
#[tokio::test(flavor = "multi_thread")]
async fn execute_tool_propagates_handler_failures_and_timeouts() {
  let (_tmp, binding) = binding_from(
    "defineTool({ name: 'boom', handler: async () => { throw new Error('handler exploded'); } });\n\
     defineTool({ name: 'slow', timeoutMs: 50, handler: async () => { \
       await new Promise(r => setTimeout(r, 400)); return 'late'; } });\n",
  )
  .await;
  let binding = binding.expect("compiles");

  let tmp = tempfile::tempdir().expect("tempdir");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: tmp.path().into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: vec![binding],
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  let boom = session
    .execute_tool("boom", serde_json::json!({}), RunOptions::default(), &ctx)
    .await;
  match boom.result.outcome {
    Outcome::Error { error } => assert!(
      error.message.contains("handler exploded"),
      "handler throw must surface: {}",
      error.message
    ),
    Outcome::Ok { .. } => panic!("throwing handler must be an error"),
  }
  assert!(!boom.poisoned, "a plain handler throw must not poison the VM");

  let slow = session
    .execute_tool("slow", serde_json::json!({}), RunOptions::default(), &ctx)
    .await;
  match slow.result.outcome {
    Outcome::Error { error } => assert!(
      error.message.contains("timed out after 50ms"),
      "timeoutMs must bind the native path too: {}",
      error.message
    ),
    Outcome::Ok { .. } => panic!("slow handler must hit its timeoutMs"),
  }
}
