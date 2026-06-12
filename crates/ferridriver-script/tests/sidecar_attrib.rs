#![allow(
  clippy::expect_used,
  clippy::unwrap_used,
  clippy::cast_precision_loss,
  clippy::cast_possible_truncation,
  clippy::cast_sign_loss,
  unsafe_code
)]
//! Differential attribution harness for the sidecar transport. NOT a gate;
//! each test isolates one layer of the per-call cost so the deltas attribute
//! the ~13us sequential floor:
//!
//!   L0  blocking std RTT          = IPC floor (2 syscalls + child schedule)
//!   L1  tokio async inline RTT    = L0 + reactor (kqueue) + waker
//!   L2  `Sidecar::send` (full)    = L1 + oneshot + `read_loop` cross-task hop
//!                                     + tokio Mutex(pending) + serde frame
//!
//!   L1 - L0 = tokio async I/O overhead
//!   L2 - L1 = the oneshot/cross-task/mutex/serde overhead (the Q2 lever)
//!
//! Plus CPU-only microbenches (frame build, parse, `json_to_js`) with NO IPC,
//! to split L2-L1 into serde vs scheduling.
//!
//! Run:
//! ```bash
//! cargo test -p ferridriver-script --test sidecar_attrib --release -- --ignored --nocapture --test-threads=1
//! ```
//! Override the child via `FERRIDRIVER_SIDECAR_BENCH_CMD="<argv>"`.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::sync::Arc;
use std::time::Instant;

use ferridriver_script::sidecar::{Sidecar, SidecarSpec};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const FIXTURE: &str = env!("CARGO_BIN_EXE_sidecar_echo");
const WARMUP: usize = 2_000;
const ITERS: usize = 50_000;
const CPU_ITERS: usize = 2_000_000;

fn bench_command() -> Vec<String> {
  std::env::var("FERRIDRIVER_SIDECAR_BENCH_CMD").map_or_else(
    |_| vec![FIXTURE.to_string()],
    |s| s.split_whitespace().map(str::to_string).collect(),
  )
}

fn bench_spec() -> SidecarSpec {
  SidecarSpec {
    name: "attrib".into(),
    command: bench_command(),
    env: vec![],
    cwd: None,
  }
}

/// Set fd 3/4 in the child to the given sockets and clear `CLOEXEC` + `O_NONBLOCK`
/// (hand the child blocking descriptors). Mirrors `sidecar.rs::spawn_child`.
fn pre_exec_fds(read_fd: i32, write_fd: i32) {
  unsafe {
    if libc::dup2(read_fd, 3) == -1 || libc::dup2(write_fd, 4) == -1 {
      return;
    }
    for fd in [3i32, 4] {
      let df = libc::fcntl(fd, libc::F_GETFD);
      if df != -1 {
        libc::fcntl(fd, libc::F_SETFD, df & !libc::FD_CLOEXEC);
      }
      let sf = libc::fcntl(fd, libc::F_GETFL);
      if sf != -1 {
        libc::fcntl(fd, libc::F_SETFL, sf & !libc::O_NONBLOCK);
      }
    }
  }
}

/// Spawn the echo fixture wired to BLOCKING std sockets (parent ends returned).
fn spawn_blocking() -> (
  std::process::Child,
  std::os::unix::net::UnixStream,
  std::os::unix::net::UnixStream,
) {
  use std::os::unix::net::UnixStream;
  let (parent_in, child_in) = UnixStream::pair().expect("pair");
  let (parent_out, child_out) = UnixStream::pair().expect("pair");
  let read_fd = child_in.as_raw_fd();
  let write_fd = child_out.as_raw_fd();
  let mut cmd = std::process::Command::new(FIXTURE);
  unsafe {
    cmd.pre_exec(move || {
      pre_exec_fds(read_fd, write_fd);
      Ok(())
    });
  }
  let child = cmd.spawn().expect("spawn");
  drop(child_in);
  drop(child_out);
  (child, parent_in, parent_out)
}

/// Spawn the echo fixture wired to TOKIO sockets (parent ends returned). The
/// child still gets blocking fd 3/4 (`pre_exec` clears `O_NONBLOCK`).
fn spawn_tokio() -> (tokio::process::Child, tokio::net::UnixStream, tokio::net::UnixStream) {
  use tokio::net::UnixStream;
  let (parent_in, child_in) = UnixStream::pair().expect("pair");
  let (parent_out, child_out) = UnixStream::pair().expect("pair");
  let read_fd = child_in.as_raw_fd();
  let write_fd = child_out.as_raw_fd();
  let mut cmd = tokio::process::Command::new(FIXTURE);
  cmd.kill_on_drop(true);
  unsafe {
    cmd.pre_exec(move || {
      pre_exec_fds(read_fd, write_fd);
      Ok(())
    });
  }
  let child = cmd.spawn().expect("spawn");
  drop(child_in);
  drop(child_out);
  (child, parent_in, parent_out)
}

fn report(label: &str, n: usize, dur: std::time::Duration) {
  eprintln!(
    "{label:<46} : {:>9.1} req/s | mean {:>6.3} us/req",
    n as f64 / dur.as_secs_f64(),
    dur.as_micros() as f64 / n as f64,
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "attribution harness; run with --ignored --nocapture"]
async fn attrib_layers() {
  eprintln!("\n=== sidecar attribution: {:?} ===", bench_command());

  // ── L0: blocking std, same thread, no framing machinery ───────────────
  {
    let (mut child, mut win, mut rout) = spawn_blocking();
    let mut rbuf = Vec::with_capacity(256);
    let mut chunk = [0u8; 256];
    for _ in 0..WARMUP {
      win
        .write_all(b"{\"id\":1,\"method\":\"ping\",\"params\":{}}\0")
        .expect("w");
      rbuf.clear();
      loop {
        let n = rout.read(&mut chunk).expect("r");
        rbuf.extend_from_slice(&chunk[..n]);
        if rbuf.contains(&0) {
          break;
        }
      }
    }
    let t = Instant::now();
    for i in 0..ITERS {
      let frame = format!("{{\"id\":{},\"method\":\"ping\",\"params\":{{}}}}\0", i + 1);
      win.write_all(frame.as_bytes()).expect("w");
      rbuf.clear();
      loop {
        let n = rout.read(&mut chunk).expect("r");
        rbuf.extend_from_slice(&chunk[..n]);
        if rbuf.contains(&0) {
          break;
        }
      }
    }
    report("L0 blocking std RTT (IPC floor)", ITERS, t.elapsed());
    drop(win);
    let _ = child.kill();
    let _ = child.wait();
  }

  // ── L1: tokio async, same task, read inline (no oneshot/read_loop) ─────
  {
    let (mut child, mut win, mut rout) = spawn_tokio();
    let mut rbuf = Vec::with_capacity(256);
    let mut chunk = [0u8; 256];
    for _ in 0..WARMUP {
      win
        .write_all(b"{\"id\":1,\"method\":\"ping\",\"params\":{}}\0")
        .await
        .expect("w");
      rbuf.clear();
      loop {
        let n = rout.read(&mut chunk).await.expect("r");
        rbuf.extend_from_slice(&chunk[..n]);
        if rbuf.contains(&0) {
          break;
        }
      }
    }
    let t = Instant::now();
    for i in 0..ITERS {
      let frame = format!("{{\"id\":{},\"method\":\"ping\",\"params\":{{}}}}\0", i + 1);
      win.write_all(frame.as_bytes()).await.expect("w");
      rbuf.clear();
      loop {
        let n = rout.read(&mut chunk).await.expect("r");
        rbuf.extend_from_slice(&chunk[..n]);
        if rbuf.contains(&0) {
          break;
        }
      }
    }
    report("L1 tokio async inline RTT", ITERS, t.elapsed());
    drop(win);
    let _ = child.kill().await;
  }

  // ── L2: full Sidecar::send (oneshot + read_loop hop + mutex + serde) ───
  {
    let s = Sidecar::connect(&bench_spec()).await.expect("connect");
    for _ in 0..WARMUP {
      s.send("ping", None, 10_000).await.expect("warm");
    }
    let t = Instant::now();
    for _ in 0..ITERS {
      s.send("ping", None, 10_000).await.expect("seq");
    }
    report("L2 Sidecar::send full", ITERS, t.elapsed());
    s.close().await.expect("close");
  }

  cpu_microbenches();
  eprintln!();
}

/// CPU-only (no IPC): the serde cost of building a request frame and parsing
/// a response, isolating it from the L2-L1 scheduling overhead.
fn cpu_microbenches() {
  {
    let t = Instant::now();
    let mut sink = 0usize;
    for i in 0..CPU_ITERS {
      let frame = json!({ "id": i as u64, "method": "ping", "params": json!({}) });
      let bytes = serde_json::to_vec(&frame).expect("ser");
      sink = sink.wrapping_add(bytes.len());
    }
    report("CPU frame build (json! + to_vec)", CPU_ITERS, t.elapsed());
    std::hint::black_box(sink);
  }
  {
    let resp = b"{\"id\":1,\"result\":{\"ok\":true}}";
    let t = Instant::now();
    let mut sink = 0usize;
    for _ in 0..CPU_ITERS {
      let v: serde_json::Value = serde_json::from_slice(resp).expect("de");
      sink = sink.wrapping_add(v.as_object().map_or(0, serde_json::Map::len));
    }
    report("CPU response parse (from_slice Value)", CPU_ITERS, t.elapsed());
    std::hint::black_box(sink);
  }
}

/// Long-running concurrent `QuickJS` loop for sampling under `samply`. Drives
/// the `Promise.all(N)` path repeatedly so a 1kHz sampler gets enough frames to
/// attribute the per-call JS CPU (promise machinery vs `json_to_js` vs executor).
/// Size via `FERRIDRIVER_ATTRIB_BATCHES` (default 2000 batches of 200 sends).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "samply target; run under: samply record -- <binary> --ignored profile_concurrent_quickjs"]
async fn profile_concurrent_quickjs() {
  use ferridriver_script::{InMemoryVars, PathSandbox, RunContext, RunOptions, ScriptEngineConfig, Session};
  let batches: usize = std::env::var("FERRIDRIVER_ATTRIB_BATCHES")
    .ok()
    .and_then(|s| s.parse().ok())
    .unwrap_or(2_000);
  let tmp = tempfile::tempdir().expect("tempdir");
  let rc = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(tmp.path()).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    plugins: Vec::new(),
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
  };
  let cfg = ScriptEngineConfig {
    sidecars: vec![bench_spec()],
    ..Default::default()
  };
  let session = Session::create(cfg, &rc).await.expect("session");
  session
    .execute(
      "globalThis.sc = await sidecars.connect('attrib'); await sc.send('ping'); return 'ok';",
      &[],
      RunOptions::default(),
      &rc,
    )
    .await;
  let src = "const sc = globalThis.sc; const ps = []; for (let i = 0; i < 200; i++) ps.push(sc.send('ping')); await Promise.all(ps); return 200;";
  let t = Instant::now();
  let mut total = 0usize;
  for _ in 0..batches {
    let run = session.execute(src, &[], RunOptions::default(), &rc).await;
    assert!(matches!(run.result.outcome, ferridriver_script::Outcome::Ok { .. }));
    total += 200;
  }
  report("profile concurrent QuickJS (Promise.all 200)", total, t.elapsed());
  session
    .execute(
      "await globalThis.sc.close(); return 'closed';",
      &[],
      RunOptions::default(),
      &rc,
    )
    .await;
}

/// Batching: raw `send_many` vs single-issuer `join_all`, and `QuickJS`
/// `sendMany` vs `Promise.all`. The hypothesis is that collapsing N JS
/// promises + `Promise.all` into one call + one Rust-side `join_all` reclaims
/// most of the `QuickJS` concurrent gap (3.8us/call) toward the raw floor
/// (1.7us/call).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "batching bench; run with --ignored --nocapture"]
async fn bench_batching() {
  use ferridriver_script::{InMemoryVars, Outcome, PathSandbox, RunContext, RunOptions, ScriptEngineConfig, Session};
  const N: usize = 5_000;
  const ROUNDS: usize = 6;
  eprintln!(
    "\n=== sidecar batching: {:?} (N={N}/batch, {ROUNDS} rounds) ===",
    bench_command()
  );

  // ── raw transport: send_many ──────────────────────────────────────────
  {
    let s = Sidecar::connect(&bench_spec()).await.expect("connect");
    for _ in 0..3 {
      let calls: Vec<(String, Option<serde_json::Value>)> = (0..N).map(|_| ("ping".to_string(), None)).collect();
      let _ = s.send_many(calls, 10_000).await;
    }
    let mut best = f64::MAX;
    for _ in 0..ROUNDS {
      let calls: Vec<(String, Option<serde_json::Value>)> = (0..N).map(|_| ("ping".to_string(), None)).collect();
      let t = Instant::now();
      let res = s.send_many(calls, 10_000).await;
      let us = t.elapsed().as_micros() as f64 / N as f64;
      assert_eq!(res.len(), N);
      best = best.min(us);
    }
    eprintln!(
      "raw send_many                 : {:>9.1} req/s | best {:.3} us/req",
      1e6 / best,
      best
    );
    s.close().await.expect("close");
  }

  // ── QuickJS: sendMany vs Promise.all on the SAME warm handle ──────────
  let tmp = tempfile::tempdir().expect("tempdir");
  let rc = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(tmp.path()).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    plugins: Vec::new(),
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
  };
  let cfg = ScriptEngineConfig {
    sidecars: vec![bench_spec()],
    ..Default::default()
  };
  let session = Session::create(cfg, &rc).await.expect("session");
  let run = session
    .execute(
      "globalThis.sc = await sidecars.connect('attrib'); await sc.send('ping'); return 'ok';",
      &[],
      RunOptions::default(),
      &rc,
    )
    .await;
  assert!(matches!(run.result.outcome, Outcome::Ok { .. }));

  let promise_all = format!(
    "const sc = globalThis.sc; const ps = []; for (let i = 0; i < {N}; i++) ps.push(sc.send('ping')); const r = await Promise.all(ps); return r.length;"
  );
  let send_many = format!(
    "const sc = globalThis.sc; const calls = []; for (let i = 0; i < {N}; i++) calls.push({{ method: 'ping' }}); const r = await sc.sendMany(calls); return r.length;"
  );

  for label in ["warm", "warm"] {
    let _ = session.execute(&promise_all, &[], RunOptions::default(), &rc).await;
    let _ = session.execute(&send_many, &[], RunOptions::default(), &rc).await;
    let _ = label;
  }

  let mut best_pa = f64::MAX;
  let mut best_sm = f64::MAX;
  for _ in 0..ROUNDS {
    let t = Instant::now();
    let r = session.execute(&promise_all, &[], RunOptions::default(), &rc).await;
    assert!(matches!(r.result.outcome, Outcome::Ok { .. }));
    best_pa = best_pa.min(t.elapsed().as_micros() as f64 / N as f64);

    let t = Instant::now();
    let r = session.execute(&send_many, &[], RunOptions::default(), &rc).await;
    assert!(matches!(r.result.outcome, Outcome::Ok { .. }));
    best_sm = best_sm.min(t.elapsed().as_micros() as f64 / N as f64);
  }
  eprintln!(
    "QuickJS Promise.all           : {:>9.1} req/s | best {:.3} us/req",
    1e6 / best_pa,
    best_pa
  );
  eprintln!(
    "QuickJS sendMany              : {:>9.1} req/s | best {:.3} us/req",
    1e6 / best_sm,
    best_sm
  );
  eprintln!("  sendMany speedup vs Promise.all: {:.2}x", best_pa / best_sm);
  session
    .execute(
      "await globalThis.sc.close(); return 'closed';",
      &[],
      RunOptions::default(),
      &rc,
    )
    .await;
  eprintln!();
}

/// Discover the methods a real child supports (the in-tree fixture only does
/// ping/echo/emit; a real child advertises more). Prints whatever the common
/// discovery verbs return so the payload bench can pick a real method.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "discovery; run with --ignored --nocapture"]
async fn discover_methods() {
  let s = Sidecar::connect(&bench_spec()).await.expect("connect");
  for m in ["ping", "methods", "info", "version", "capabilities", "list", "help"] {
    match s.send(m, None, 5_000).await {
      Ok(v) => eprintln!(
        "{m:>14} -> {}",
        serde_json::to_string(&v)
          .unwrap_or_default()
          .chars()
          .take(900)
          .collect::<String>()
      ),
      Err(e) => eprintln!("{m:>14} ERR {e}"),
    }
  }
  let _ = Arc::new(s).close().await;
}
