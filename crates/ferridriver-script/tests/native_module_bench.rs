#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Re-runnable native-module-loader microbench, ignored by default so
//! it stays out of the green gate; run explicitly:
//!
//! ```text
//! cargo test -p ferridriver-script --test native_module_bench --release -- --ignored --nocapture
//! ```
//!
//! Measures what the native-module migration touches:
//! 1. `Session::create` — loader-chain registration cost
//! 2. bundle compile (cold, cache off) — eager declare-time resolution
//!    of the external native imports
//! 3. `execute_module` of a bundle importing 4 native modules — link +
//!    `ModuleDef` evaluate per fresh session
//! 4. repeated dynamic `import('path')` on a warm session — loader hit
//!    after the per-context module cache kicks in

use std::sync::Arc;
use std::time::Instant;

use ferridriver_script::{
  InMemoryVars, Outcome, PathSandbox, RunContext, RunOptions, ScriptEngineConfig, Session, bundle_and_compile,
};

fn ctx(dir: &std::path::Path) -> RunContext {
  RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(dir).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: Vec::new(),
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
  }
}

fn report(label: &str, mut samples: Vec<f64>) {
  samples.sort_by(f64::total_cmp);
  let n = samples.len();
  #[allow(clippy::cast_precision_loss)]
  let mean = samples.iter().sum::<f64>() / n as f64;
  let p50 = samples[n / 2];
  let p95 = samples[(n * 95 / 100).min(n - 1)];
  eprintln!("{label:<44} n={n:<4} mean={mean:>9.3}ms  p50={p50:>9.3}ms  p95={p95:>9.3}ms");
}

#[tokio::test]
#[ignore = "microbench, run explicitly with --ignored --nocapture"]
async fn native_module_loader_microbench() {
  // SAFETY: set before any cache access in this single-test binary.
  #[allow(unsafe_code)]
  unsafe {
    std::env::set_var("FERRIDRIVER_NO_BYTECODE_CACHE", "1");
  };

  let dir = tempfile::tempdir().expect("tempdir");
  std::fs::write(dir.path().join("data.txt"), "x").expect("data");
  let entry = dir.path().join("main.ts");
  std::fs::write(
    &entry,
    "import fs from 'node:fs';\n\
     import path from 'node:path';\n\
     import { Buffer } from 'node:buffer';\n\
     import { host } from 'ferridriver';\n\
     const d: string = await fs.readFile('data.txt');\n\
     export default [d, path.extname('a.txt'), Buffer.from('x').toString('hex'), host].join('|');\n",
  )
  .expect("entry");
  let context = ctx(dir.path());

  // 1. Session::create (loader-chain registration included).
  let mut create_ms = Vec::new();
  for _ in 0..30 {
    let t = Instant::now();
    let s = Session::create(ScriptEngineConfig::default(), &context)
      .await
      .expect("session");
    create_ms.push(t.elapsed().as_secs_f64() * 1e3);
    drop(s);
  }
  report("Session::create", create_ms);

  // 2. Cold bundle compile (rolldown + declare with native externals).
  let mut compile_ms = Vec::new();
  for _ in 0..15 {
    let t = Instant::now();
    let b = bundle_and_compile(std::slice::from_ref(&entry), dir.path())
      .await
      .expect("bundle");
    compile_ms.push(t.elapsed().as_secs_f64() * 1e3);
    drop(b);
  }
  report("bundle_and_compile (cache off)", compile_ms);

  let bundle = bundle_and_compile(std::slice::from_ref(&entry), dir.path())
    .await
    .expect("bundle");

  // 3. execute_module on a FRESH session each time: bytecode load +
  //    native link + 4 ModuleDef evaluations + the script body.
  let mut exec_fresh_ms = Vec::new();
  for _ in 0..30 {
    let session = Session::create(ScriptEngineConfig::default(), &context)
      .await
      .expect("session");
    let t = Instant::now();
    let run = session
      .execute_module(&bundle, &[], RunOptions::default(), &context)
      .await;
    exec_fresh_ms.push(t.elapsed().as_secs_f64() * 1e3);
    assert!(
      matches!(run.result.outcome, Outcome::Ok { .. }),
      "bench module must pass"
    );
  }
  report("execute_module (fresh session, 4 natives)", exec_fresh_ms);

  // 4. Dynamic import on a WARM session: after the first hit the
  //    per-context module cache serves it without touching the loader.
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");
  let mut dyn_first = Vec::new();
  let mut dyn_warm = Vec::new();
  for i in 0..200 {
    let t = Instant::now();
    let run = session
      .execute(
        "const p = (await import('path')).default; return p.extname('a.txt');",
        &[],
        RunOptions::default(),
        &context,
      )
      .await;
    let ms = t.elapsed().as_secs_f64() * 1e3;
    if i == 0 {
      dyn_first.push(ms);
    } else {
      dyn_warm.push(ms);
    }
    assert!(matches!(run.result.outcome, Outcome::Ok { .. }));
  }
  report("execute + dynamic import (first)", dyn_first);
  report("execute + dynamic import (warm)", dyn_warm);

  // Baseline execute without any import, same session, to isolate the
  // import's marginal cost.
  let mut baseline = Vec::new();
  for _ in 0..200 {
    let t = Instant::now();
    let run = session
      .execute("return 'a.txt'.slice(-4);", &[], RunOptions::default(), &context)
      .await;
    baseline.push(t.elapsed().as_secs_f64() * 1e3);
    assert!(matches!(run.result.outcome, Outcome::Ok { .. }));
  }
  report("execute baseline (no import)", baseline);
}
