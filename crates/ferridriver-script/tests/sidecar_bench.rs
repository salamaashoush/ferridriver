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

use ferridriver_script::sidecar::{Sidecar, SidecarSpec};
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

/// Raw transport, NO `QuickJS` — the same `ping` loop driven straight through
/// `Sidecar::send`. Subtract from `bench_round_trip_via_quickjs` to attribute
/// the per-call cost between the IPC round trip and the JS/serde/async stack.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "timing harness; run with --ignored --nocapture"]
async fn bench_round_trip_transport_only() {
  let spec = bench_spec();
  eprintln!("\n=== sidecar bench (raw transport): {:?} ===", spec.command);
  let s = Sidecar::connect(&spec).await.expect("connect");
  let first = s.send("ping", None, 10_000).await.expect("first ping");
  assert_eq!(first.get("ok").and_then(serde_json::Value::as_bool), Some(true));

  for _ in 0..WARMUP {
    s.send("ping", None, 10_000).await.expect("warmup");
  }

  let seq_start = Instant::now();
  for _ in 0..SEQUENTIAL {
    s.send("ping", None, 10_000).await.expect("seq ping");
  }
  let seq = seq_start.elapsed();
  eprintln!(
    "sequential ({SEQUENTIAL}) : {:.1} req/s | mean {:.1} us/req",
    SEQUENTIAL as f64 / seq.as_secs_f64(),
    seq.as_micros() as f64 / SEQUENTIAL as f64,
  );

  let s = Arc::new(s);

  // (a) 64 tasks across the worker pool — true multi-thread parallelism.
  let conc_start = Instant::now();
  let per = CONCURRENT / 64;
  let mut handles = Vec::new();
  for _ in 0..64 {
    let s = s.clone();
    handles.push(tokio::spawn(async move {
      for _ in 0..per {
        s.send("ping", None, 10_000).await.expect("conc ping");
      }
    }));
  }
  for h in handles {
    h.await.expect("join");
  }
  let conc = conc_start.elapsed();
  let n = per * 64;
  eprintln!(
    "concurrent ({n}, 64 tasks/threads) : {:.1} req/s | {:.1} us/req wall",
    n as f64 / conc.as_secs_f64(),
    conc.as_micros() as f64 / n as f64,
  );

  // (b) ONE issuing task — all futures multiplexed on a single thread via
  // join_all. This mirrors the QuickJS `Promise.all` topology (one thread
  // drives every in-flight send) but with NO JS/serde overhead, isolating
  // the single-issuer cost from the JS-stack cost.
  let single = Arc::clone(&s);
  let one_start = Instant::now();
  tokio::spawn(async move {
    let futs = (0..CONCURRENT).map(|_| single.send("ping", None, 10_000));
    let results = futures::future::join_all(futs).await;
    for r in results {
      r.expect("single-issuer ping");
    }
  })
  .await
  .expect("join");
  let one = one_start.elapsed();
  eprintln!(
    "concurrent ({CONCURRENT}, 1 task/thread, join_all) : {:.1} req/s | {:.1} us/req wall",
    CONCURRENT as f64 / one.as_secs_f64(),
    one.as_micros() as f64 / CONCURRENT as f64,
  );

  s.close().await.expect("close");
  eprintln!();
}
