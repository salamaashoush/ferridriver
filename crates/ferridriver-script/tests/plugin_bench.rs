#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Re-runnable plugin-path microbench. `#[ignore]` so it stays out of
//! the green gate; run explicitly for before/after numbers:
//!
//! ```text
//! cargo test -p ferridriver-script --test plugin_bench -- --ignored --nocapture
//! ```
//!
//! Measures the three things the rolldown->bytecode migration changes:
//!   1. cold start  — compile every plugin file to loadable bytecode
//!   2. per-session — `Session::create` with the plugin bindings installed
//!   3. per-call    — one no-op and one setFeatureFlip-class dispatch
//!
//! Browser I/O is deliberately excluded: the migration changes plugin
//! machinery only, never the handler's page work, so a no-op isolates
//! exactly the delta. The setFeatureFlip-class handler does the same JS
//! work the real tool does (build the cookie array, JSON round-trip)
//! minus the `context.addCookies` call, which is identical pre/post.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use ferridriver_script::{
  InMemoryVars, Outcome, PathSandbox, PluginBinding, PluginToolBinding, RunContext, RunOptions, ScriptEngineConfig,
  Session, compile_and_extract_plugins,
};

/// Four representative plugin files mirroring the box-craft bundle
/// shapes: a single-export file, a `{ tools: [...] }` bundle, a bare
/// array, and a single-tool bundle.
const FILES: &[(&str, &str)] = &[
  (
    "login.js",
    "globalThis.exports = { name: 'box.login', description: 'login', \
       inputSchema: { type: 'object' }, allow: { commands: { resolveUser: 'true' } }, \
       exposeAsTool: true, async handler({ args }) { return { ok: true, user: args && args.user }; } };",
  ),
  (
    "core.js",
    "const V = 'yes';\nglobalThis.exports = { tools: [\
       { name: 'box.noop', description: 'noop', exposeAsTool: true, async handler() { return null; } },\
       { name: 'box.setFeatureFlip', description: 'ff', exposeAsTool: true, \
         async handler({ args }) { \
           const flags = Array.isArray(args.flag) ? args.flag : [args.flag]; \
           const cookies = flags.map((f) => ({ name: 'ff_' + f, value: V, domain: '.box.com', path: '/' })); \
           return { flags, value: V, cookies: JSON.parse(JSON.stringify(cookies)) }; } } ] };",
  ),
  (
    "ui.js",
    "globalThis.exports = [\
       { name: 'box.click', description: 'click', exposeAsTool: true, async handler() { return 1; } },\
       { name: 'box.type', description: 'type', exposeAsTool: true, async handler() { return 2; } } ];",
  ),
  (
    "sign.js",
    "globalThis.exports = { tools: [\
       { name: 'box.sign', description: 'sign', exposeAsTool: true, async handler() { return 'signed'; } } ] };",
  ),
];

struct Compiled {
  names: Vec<&'static str>,
  bytecode: Arc<[u8]>,
}

fn bindings(compiled: &[Compiled]) -> Vec<PluginBinding> {
  compiled
    .iter()
    .map(|c| PluginBinding {
      bytecode: c.bytecode.clone(),
      tools: c
        .names
        .iter()
        .map(|n| PluginToolBinding {
          name: (*n).to_string(),
          allowed_commands: HashMap::new(),
          allowed_net: Vec::new(),
        })
        .collect(),
    })
    .collect()
}

const ITERS: u32 = 200;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "perf microbench; run with --ignored --nocapture"]
async fn plugin_path_bench() {
  // ---- 1. cold start: bundle + compile + extract every file ----
  let names: Vec<Vec<&str>> = vec![
    vec!["box.login"],
    vec!["box.noop", "box.setFeatureFlip"],
    vec!["box.click", "box.type"],
    vec!["box.sign"],
  ];
  let src_tmp = tempfile::tempdir().expect("tempdir");
  let paths: Vec<_> = FILES
    .iter()
    .map(|(file, src)| {
      let p = src_tmp.path().join(file);
      std::fs::write(&p, src).expect("write plugin");
      p
    })
    .collect();
  let cold = Instant::now();
  let (cp, failures) = compile_and_extract_plugins(&paths).await;
  let cold_ms = cold.elapsed().as_secs_f64() * 1e3;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  assert_eq!(cp.len(), FILES.len(), "all files must compile");
  let compiled: Vec<Compiled> = cp
    .into_iter()
    .map(|c| Compiled {
      names: names[c.index].clone(),
      bytecode: c.bytecode,
    })
    .collect();

  // ---- 2. per-session install ----
  let tmp = tempfile::tempdir().expect("tempdir");
  let mk_ctx = || RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(tmp.path()).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    plugins: bindings(&compiled),
    trusted_modules: false,
  };
  let n_sessions = 50;
  let sess_t = Instant::now();
  for _ in 0..n_sessions {
    let ctx = mk_ctx();
    Session::create(ScriptEngineConfig::default(), &ctx)
      .await
      .expect("session create");
  }
  let per_session_ms = (sess_t.elapsed().as_secs_f64() * 1e3) / f64::from(n_sessions);

  // ---- 3. per-call dispatch (no-op + setFeatureFlip-class) ----
  let ctx = mk_ctx();
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  let warm = session
    .execute(
      "return await plugins['box.noop']({});",
      &[],
      RunOptions::default(),
      &ctx,
    )
    .await;
  assert!(matches!(warm.result.outcome, Outcome::Ok { .. }), "noop must succeed");

  let noop_t = Instant::now();
  for _ in 0..ITERS {
    let r = session
      .execute(
        "return await plugins['box.noop']({});",
        &[],
        RunOptions::default(),
        &ctx,
      )
      .await;
    assert!(matches!(r.result.outcome, Outcome::Ok { .. }));
  }
  let noop_us = (noop_t.elapsed().as_secs_f64() * 1e6) / f64::from(ITERS);

  let ff_src = "return await plugins['box.setFeatureFlip']({ flag: ['vega','nova','orion'] });";
  let ff_t = Instant::now();
  for _ in 0..ITERS {
    let r = session.execute(ff_src, &[], RunOptions::default(), &ctx).await;
    assert!(matches!(r.result.outcome, Outcome::Ok { .. }));
  }
  let ff_us = (ff_t.elapsed().as_secs_f64() * 1e6) / f64::from(ITERS);

  println!("\n=== plugin path bench ({} files, {ITERS} iters) ===", FILES.len());
  println!("cold start (compile all files) : {cold_ms:8.2} ms");
  println!("per-session install            : {per_session_ms:8.3} ms");
  println!("per-call no-op dispatch        : {noop_us:8.2} us");
  println!("per-call setFeatureFlip-class  : {ff_us:8.2} us");
  println!("================================================\n");
}
