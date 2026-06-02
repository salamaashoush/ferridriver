#![allow(
  clippy::expect_used,
  clippy::unwrap_used,
  clippy::cast_precision_loss,
  clippy::cast_possible_truncation,
  clippy::cast_sign_loss
)]
//! Sidecar timing harness, measured through the `QuickJS` API that scripts
//! actually use (`sidecars.connect(name).send(...)`), not the raw Rust
//! transport — so the numbers include the full per-call cost: JS→serde
//! lowering, `async_with` re-entry, promise plumbing, and the fd-3/4 round
//! trip. `#[ignore]` (a measurement, not a gate). Run:
//!
//! ```bash
//! cargo test -p ferridriver-script --test sidecar_bench --release -- --ignored --nocapture
//! ```
//!
//! Defaults to the in-tree `sidecar_echo` fixture (pure-transport baseline).
//! Point it at any fd-3/4 NUL-JSON sidecar via `FERRIDRIVER_SIDECAR_BENCH_CMD`
//! (space-separated argv) to measure a real child:
//!
//! ```bash
//! FERRIDRIVER_SIDECAR_BENCH_CMD="some-tool pipe" \
//!   cargo test -p ferridriver-script --test sidecar_bench --release -- --ignored --nocapture
//! ```
//!
//! Every benched call is `ping` (the cheapest method), so the figures are
//! framework + IPC overhead, not the child's business logic.

use std::sync::Arc;
use std::time::Instant;

use ferridriver_script::sidecar::SidecarSpec;
use ferridriver_script::{InMemoryVars, Outcome, PathSandbox, RunContext, RunOptions, ScriptEngineConfig, Session};

const FIXTURE: &str = env!("CARGO_BIN_EXE_sidecar_echo");
const WARMUP: usize = 300;
const SEQUENTIAL: usize = 5_000;
const CONCURRENT: usize = 5_000;

fn bench_spec() -> SidecarSpec {
  let command = std::env::var("FERRIDRIVER_SIDECAR_BENCH_CMD").map_or_else(
    |_| vec![FIXTURE.to_string()],
    |s| s.split_whitespace().map(str::to_string).collect(),
  );
  SidecarSpec {
    name: "bench".into(),
    command,
    env: vec![],
    cwd: None,
    startup_timeout_ms: 10_000,
  }
}

fn ctx(tmp: &tempfile::TempDir) -> RunContext {
  RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(tmp.path()).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    plugins: Vec::new(),
    trusted_modules: false,
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
  }
}

/// Run one script against the persistent session, asserting it succeeded,
/// and return the wall-clock duration of the `execute` call.
async fn timed(session: &Session, rc: &RunContext, src: &str) -> std::time::Duration {
  let t = Instant::now();
  let run = session.execute(src, &[], RunOptions::default(), rc).await;
  let elapsed = t.elapsed();
  match run.result.outcome {
    Outcome::Ok { .. } => elapsed,
    Outcome::Error { error } => panic!("script failed: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "timing harness; run with --ignored --nocapture"]
async fn bench_round_trip_via_quickjs() {
  let spec = bench_spec();
  eprintln!("\n=== sidecar bench (QuickJS API): {:?} ===", spec.command);

  let tmp = tempfile::tempdir().expect("tempdir");
  let rc = ctx(&tmp);
  let cfg = ScriptEngineConfig {
    sidecars: vec![spec],
    ..Default::default()
  };
  let session = Session::create(cfg, &rc).await.expect("session");

  // Connect (spawn) + stash the warm handle on globalThis so later executes
  // reuse it. The connect cost is the spawn + transport wiring.
  let connect = timed(
    &session,
    &rc,
    "globalThis.sc = await sidecars.connect('bench'); const r = await sc.send('ping'); if (r.ok !== true) throw new Error('handshake'); return 'ok';",
  )
  .await;
  eprintln!("connect + handshake : {} us", connect.as_micros());

  // Warm up the child (caches/pool) — not timed.
  timed(
    &session,
    &rc,
    &format!("const sc = globalThis.sc; for (let i = 0; i < {WARMUP}; i++) await sc.send('ping'); return 'warm';"),
  )
  .await;

  // Sequential: await each send before the next. Aggregate throughput +
  // mean per-call latency through the whole JS stack.
  let seq = timed(
    &session,
    &rc,
    &format!(
      "const sc = globalThis.sc; for (let i = 0; i < {SEQUENTIAL}; i++) await sc.send('ping'); return {SEQUENTIAL};"
    ),
  )
  .await;
  eprintln!(
    "sequential ({SEQUENTIAL}) : {:.1} req/s | mean {:.1} us/req",
    SEQUENTIAL as f64 / seq.as_secs_f64(),
    seq.as_micros() as f64 / SEQUENTIAL as f64,
  );

  // Concurrent: fire all sends, then await Promise.all — the real
  // user-facing concurrency path (id correlation under one VM, shared
  // writer lock).
  let conc = timed(
    &session,
    &rc,
    &format!(
      "const sc = globalThis.sc; const ps = []; for (let i = 0; i < {CONCURRENT}; i++) ps.push(sc.send('ping')); await Promise.all(ps); return {CONCURRENT};"
    ),
  )
  .await;
  eprintln!(
    "concurrent ({CONCURRENT}, Promise.all) : {:.1} req/s | {:.1} us/req wall",
    CONCURRENT as f64 / conc.as_secs_f64(),
    conc.as_micros() as f64 / CONCURRENT as f64,
  );

  timed(&session, &rc, "await globalThis.sc.close(); return 'closed';").await;
  eprintln!();
}
