#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Operator extension-policy ceiling (`[extensions.policy]`) and the
//! handler `ctx.signal` cancellation surface: the effective grants a
//! tool dispatches with are its declared `allow` intersected with the
//! operator ceiling, enforced at `defineTool` registration inside the
//! session VM.

use std::sync::Arc;

use ferridriver_config::{ExtensionCommandsCeiling, ExtensionPolicyConfig};
use ferridriver_script::{
  ExtensionBinding, InMemoryVars, Outcome, PathSandbox, RunContext, RunOptions, ScriptCaps, ScriptEngineConfig,
  Session, compile_and_extract_extensions,
};

async fn binding_from(name: &str, src: &str) -> (tempfile::TempDir, ExtensionBinding) {
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join(name);
  std::fs::write(&path, src).expect("write extension");
  let (compiled, failures) = compile_and_extract_extensions(&[path]).await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let cp = compiled.into_iter().next().expect("one compiled extension");
  (
    tmp,
    ExtensionBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
    },
  )
}

fn run_context(extensions: Vec<ExtensionBinding>, policy: ExtensionPolicyConfig) -> (tempfile::TempDir, RunContext) {
  let tmp = tempfile::tempdir().expect("tempdir");
  let request = Arc::new(ferridriver::http_client::HttpClient::new(
    ferridriver::http_client::HttpClientOptions::default(),
  ));
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(tmp.path()).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: Some(request),
    browser: None,
    extensions,
    host: ferridriver_script::ExtensionHost::Script,
    caps: ScriptCaps::default().with_extension_policy(policy),
  };
  (tmp, ctx)
}

async fn run(session: &Session, ctx: &RunContext, source: &str) -> Outcome {
  session
    .execute(source, &[], RunOptions::default(), ctx)
    .await
    .result
    .outcome
}

fn error_message(outcome: Outcome) -> String {
  match outcome {
    Outcome::Error { error } => error.message,
    Outcome::Ok { success } => panic!("expected an error outcome, got: {:?}", success.value),
  }
}

fn ok_value(outcome: Outcome) -> serde_json::Value {
  match outcome {
    Outcome::Ok { success } => success.value,
    Outcome::Error { error } => panic!("expected success, got: {error:?}"),
  }
}

/// A ceiling flips tools with NO `allow.net` declaration to
/// default-deny: they get exactly the ceiling, instead of today's
/// unrestricted back-compat default.
#[tokio::test(flavor = "multi_thread")]
async fn net_ceiling_applies_default_deny_to_undeclared_tools() {
  const SRC: &str = "defineTool({ name: 'open', handler: async ({ args, request }) => { await request.get(args.url); return 'ok'; } });";
  let (_ext_tmp, binding) = binding_from("open.js", SRC).await;
  let policy = ExtensionPolicyConfig {
    net: Some(vec!["127.0.0.1".into()]),
    commands: ExtensionCommandsCeiling::Any,
  };
  let (_tmp, ctx) = run_context(vec![binding], policy);
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  let blocked = error_message(
    run(
      &session,
      &ctx,
      "return await tools['open']({ url: 'http://blocked.test/' });",
    )
    .await,
  );
  assert!(
    blocked.contains("not in allow.net") && blocked.contains("blocked.test"),
    "undeclared tool must inherit the ceiling as default-deny, got: {blocked}"
  );

  let allowed = run(
    &session,
    &ctx,
    "return await tools['open']({ url: 'http://127.0.0.1:1/' });",
  )
  .await;
  if let Outcome::Error { error } = allowed {
    assert!(
      !error.message.contains("allow.net"),
      "ceiling host must pass the guard; got: {}",
      error.message
    );
  }
}

/// Declared entries OUTSIDE the ceiling are dropped from the effective
/// grant; entries inside it survive.
#[tokio::test(flavor = "multi_thread")]
async fn net_ceiling_clamps_declared_entries() {
  const SRC: &str = "defineTool({ name: 'clamped', allow: { net: ['127.0.0.1', 'evil.example'] }, \
    handler: async ({ args, request }) => { await request.get(args.url); return 'ok'; } });";
  let (_ext_tmp, binding) = binding_from("clamped.js", SRC).await;
  let policy = ExtensionPolicyConfig {
    net: Some(vec!["127.0.0.1".into()]),
    commands: ExtensionCommandsCeiling::Any,
  };
  let (_tmp, ctx) = run_context(vec![binding], policy);
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  let dropped = error_message(
    run(
      &session,
      &ctx,
      "return await tools['clamped']({ url: 'http://evil.example/' });",
    )
    .await,
  );
  assert!(
    dropped.contains("not in allow.net") && dropped.contains("evil.example"),
    "a declared entry outside the ceiling must be dropped, got: {dropped}"
  );

  let kept = run(
    &session,
    &ctx,
    "return await tools['clamped']({ url: 'http://127.0.0.1:1/' });",
  )
  .await;
  if let Outcome::Error { error } = kept {
    assert!(
      !error.message.contains("allow.net"),
      "a declared entry inside the ceiling must survive; got: {}",
      error.message
    );
  }
}

/// An explicit empty ceiling denies every extension HTTP request.
#[tokio::test(flavor = "multi_thread")]
async fn empty_net_ceiling_denies_all_hosts() {
  const SRC: &str =
    "defineTool({ name: 'noop', handler: async ({ args }) => { const r = await fetch(args.url); return r.status; } });";
  let (_ext_tmp, binding) = binding_from("noop.js", SRC).await;
  let policy = ExtensionPolicyConfig {
    net: Some(Vec::new()),
    commands: ExtensionCommandsCeiling::Any,
  };
  let (_tmp, ctx) = run_context(vec![binding], policy);
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  let denied = error_message(
    run(
      &session,
      &ctx,
      "return await tools['noop']({ url: 'http://127.0.0.1:1/' });",
    )
    .await,
  );
  assert!(
    denied.contains("not in allow.net"),
    "empty ceiling must deny every host, got: {denied}"
  );
}

/// `commands = "argvOnly"` fails registration of a shell-form tool (its
/// file is skipped) while an argv-form tool in another file registers
/// and runs.
#[tokio::test(flavor = "multi_thread")]
async fn argv_only_ceiling_rejects_shell_form_tools() {
  const SHELL: &str = "defineTool({ name: 'sh', allow: { commands: { echo: 'echo hi' } }, \
    handler: async ({ commands }) => commands.run('echo') });";
  const ARGV: &str = "defineTool({ name: 'argv', allow: { commands: { echo: { run: ['echo', 'hi'] } } }, \
    handler: async ({ commands }) => commands.run('echo') });";
  let (_t1, shell_binding) = binding_from("sh.js", SHELL).await;
  let (_t2, argv_binding) = binding_from("argv.js", ARGV).await;
  let policy = ExtensionPolicyConfig {
    net: None,
    commands: ExtensionCommandsCeiling::ArgvOnly,
  };
  let (_tmp, ctx) = run_context(vec![shell_binding, argv_binding], policy);
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  let missing = error_message(run(&session, &ctx, "return await tools['sh']();").await);
  assert!(
    missing.contains("not a function") || missing.contains("undefined"),
    "shell-form tool must not register under argvOnly, got: {missing}"
  );

  let argv_ok = ok_value(run(&session, &ctx, "return await tools['argv']();").await);
  assert_eq!(argv_ok, serde_json::json!("hi"), "argv-form tool must run normally");
}

/// `commands = "none"` fails registration of any command-declaring tool
/// but leaves command-free tools untouched.
#[tokio::test(flavor = "multi_thread")]
async fn none_ceiling_rejects_command_declaring_tools() {
  const WITH_CMD: &str = "defineTool({ name: 'cmd', allow: { commands: { echo: { run: ['echo', 'hi'] } } }, \
    handler: async ({ commands }) => commands.run('echo') });";
  const PLAIN: &str = "defineTool({ name: 'plain', handler: async () => 'fine' });";
  let (_t1, cmd_binding) = binding_from("cmd.js", WITH_CMD).await;
  let (_t2, plain_binding) = binding_from("plain.js", PLAIN).await;
  let policy = ExtensionPolicyConfig {
    net: None,
    commands: ExtensionCommandsCeiling::None,
  };
  let (_tmp, ctx) = run_context(vec![cmd_binding, plain_binding], policy);
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  let missing = error_message(run(&session, &ctx, "return await tools['cmd']();").await);
  assert!(
    missing.contains("not a function") || missing.contains("undefined"),
    "command-declaring tool must not register under `none`, got: {missing}"
  );

  let plain_ok = ok_value(run(&session, &ctx, "return await tools['plain']();").await);
  assert_eq!(plain_ok, serde_json::json!("fine"));
}

/// Every handler receives a standard `AbortSignal` as `ctx.signal`; the
/// `timeoutMs` bound fires it, so the still-running JS continuation can
/// observe the cancellation instead of running on blind.
#[tokio::test(flavor = "multi_thread")]
async fn timeout_fires_the_handler_abort_signal() {
  const SRC: &str = "defineTool({ name: 'slow', timeoutMs: 200, handler: ({ signal }) => new Promise(() => { \
    signal.addEventListener('abort', (r) => { globalThis.__abort_name = r && r.name; }); }) });";
  let (_ext_tmp, binding) = binding_from("slow.js", SRC).await;
  let (_tmp, ctx) = run_context(vec![binding], ExtensionPolicyConfig::default());
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  let timed_out = error_message(run(&session, &ctx, "return await tools['slow']();").await);
  assert!(
    timed_out.contains("timed out after 200ms"),
    "expected the timeout error, got: {timed_out}"
  );

  let observed = ok_value(run(&session, &ctx, "return globalThis.__abort_name;").await);
  assert_eq!(
    observed,
    serde_json::json!("TimeoutError"),
    "the handler's abort listener must have fired with a TimeoutError reason"
  );
}

/// Capability follows the registrar through `queueMicrotask` too: the
/// job queue drains outside the handler's policy bracket, so a
/// microtask queued by a net-restricted handler must keep that grant.
#[tokio::test(flavor = "multi_thread")]
async fn queue_microtask_keeps_registrar_net_policy() {
  const SRC: &str = "defineTool({ name: 'micro', allow: { net: ['127.0.0.1'] }, \
    handler: () => new Promise((resolve) => { queueMicrotask(async () => { \
      try { await fetch('http://blocked.test/'); resolve('unexpectedly allowed'); } \
      catch (e) { resolve(String((e && e.message) || e)); } }); }) });";
  let (_ext_tmp, binding) = binding_from("micro.js", SRC).await;
  let (_tmp, ctx) = run_context(vec![binding], ExtensionPolicyConfig::default());
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  let message = ok_value(run(&session, &ctx, "return await tools['micro']();").await);
  let message = message.as_str().unwrap_or_default().to_string();
  assert!(
    message.contains("not in allow.net") && message.contains("blocked.test"),
    "microtask must run under the registrar's allow.net, got: {message}"
  );
}

/// `title` / `outputSchema` / `annotations` survive manifest extraction
/// so the MCP layer can surface them in `tools/list`.
#[tokio::test(flavor = "multi_thread")]
async fn manifest_extraction_carries_title_output_schema_annotations() {
  const SRC: &str = "defineTool({ name: 'meta', title: 'Meta Tool', \
    outputSchema: { type: 'object', properties: { ok: { type: 'boolean' } }, required: ['ok'] }, \
    annotations: { readOnlyHint: true, openWorldHint: false }, \
    handler: async () => ({ ok: true }) });";
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("meta.js");
  std::fs::write(&path, SRC).expect("write extension");
  let (compiled, failures) = compile_and_extract_extensions(&[path]).await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let cp = compiled.into_iter().next().expect("one compiled extension");

  let manifests: serde_json::Value = serde_json::from_str(&cp.manifests_json).expect("manifest JSON");
  let tool = &manifests[0];
  assert_eq!(tool["name"], "meta");
  assert_eq!(tool["title"], "Meta Tool");
  assert_eq!(tool["outputSchema"]["required"][0], "ok");
  assert_eq!(tool["annotations"]["readOnlyHint"], true);
  assert_eq!(tool["annotations"]["openWorldHint"], false);
}
