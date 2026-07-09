//! Event-path benchmarks: emitter throughput, delivery loss, non-matching
//! wakeup overhead, dispatch latency, and an opt-in end-to-end browser
//! console storm.
//!
//! Uses only the public emitter API (`EventEmitter::new` / `on` / `emit` /
//! `wait_for`) so the same binary measures the event system before and
//! after internal rewrites.
//!
//! Timing uses the last-callback timestamp (stamped inside the listener)
//! rather than quiesce wall time, so settle-polling dwell never inflates
//! the numbers. All arithmetic is integer nanoseconds — same
//! precision-loss-free formatting approach as the transport RTT stats.
//!
//! Run: `cargo bench -p ferridriver --bench event_bench`
//! End-to-end (needs Chromium): `FERRI_EVENT_BENCH_E2E=1 cargo bench -p ferridriver --bench event_bench`

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ferridriver::{EventEmitter, PageEvent};

const STORM_EVENTS: u64 = 100_000;
const FANOUT_LISTENERS: u64 = 8;
const NONMATCH_LISTENERS: u64 = 50;
const LATENCY_SAMPLES: usize = 10_000;

/// Count of received events plus the elapsed-nanos stamp of the most
/// recent callback, so drain time is exact instead of quiesce-derived.
struct Stamped {
  count: AtomicU64,
  last_nanos: AtomicU64,
}

impl Stamped {
  fn new() -> Arc<Self> {
    Arc::new(Self {
      count: AtomicU64::new(0),
      last_nanos: AtomicU64::new(0),
    })
  }
}

fn elapsed_ns(t0: Instant) -> u64 {
  u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn stamped_listener(s: &Arc<Stamped>, t0: Instant) -> Arc<dyn Fn(PageEvent) + Send + Sync> {
  let s = Arc::clone(s);
  Arc::new(move |_| {
    s.count.fetch_add(1, Ordering::AcqRel);
    s.last_nanos.store(elapsed_ns(t0), Ordering::Release);
  })
}

/// Fixed-point milliseconds with one decimal, integer math.
fn fmt_ms(ns: u64) -> String {
  format!("{}.{}", ns / 1_000_000, (ns % 1_000_000) / 100_000)
}

/// Fixed-point microseconds with one decimal, integer math.
fn fmt_us(ns: u64) -> String {
  format!("{}.{}", ns / 1_000, (ns % 1_000) / 100)
}

/// `num/den` as a percentage with two decimals, integer math.
fn fmt_pct(num: u64, den: u64) -> String {
  if den == 0 {
    return "0.00".to_string();
  }
  let bp = u128::from(num) * 10_000 / u128::from(den);
  format!("{}.{:02}", bp / 100, bp % 100)
}

/// Events per second over a nanosecond window, integer math.
fn per_sec(count: u64, ns: u64) -> u128 {
  if ns == 0 {
    return 0;
  }
  u128::from(count) * 1_000_000_000 / u128::from(ns)
}

/// Poll the counters until their sum stops changing for `stable_for`, or
/// `max_wait` elapses. Returns the final sum.
async fn quiesce_sum(counters: &[Arc<Stamped>], stable_for: Duration, max_wait: Duration) -> u64 {
  let start = Instant::now();
  let sum = || counters.iter().map(|c| c.count.load(Ordering::Acquire)).sum::<u64>();
  let mut last = sum();
  let mut last_change = Instant::now();
  loop {
    tokio::time::sleep(Duration::from_millis(10)).await;
    let now = sum();
    if now != last {
      last = now;
      last_change = Instant::now();
    } else if last_change.elapsed() >= stable_for {
      return now;
    }
    if start.elapsed() >= max_wait {
      return now;
    }
  }
}

fn last_stamp_ns(counters: &[Arc<Stamped>]) -> u64 {
  counters
    .iter()
    .map(|c| c.last_nanos.load(Ordering::Acquire))
    .max()
    .unwrap_or(0)
}

/// Emit `n` `Load` events back-to-back into an emitter with `listeners`
/// counting listeners; report delivered count, emit wall time, and exact
/// drain time (last callback stamp). `pace` yields every 64 events to
/// model a producer that isn't hot-spinning.
async fn storm(name: &str, n: u64, listeners: u64, pace: bool) {
  let emitter = EventEmitter::new();
  let t0 = Instant::now();
  let counters: Vec<Arc<Stamped>> = (0..listeners).map(|_| Stamped::new()).collect();
  for c in &counters {
    emitter.on("load", stamped_listener(c, t0));
  }
  tokio::time::sleep(Duration::from_millis(50)).await;

  let emit_offset_ns = elapsed_ns(t0);
  for i in 0..n {
    emitter.emit(PageEvent::Load);
    if pace && i % 64 == 63 {
      tokio::task::yield_now().await;
    }
  }
  let emit_ns = elapsed_ns(t0).saturating_sub(emit_offset_ns);

  let received = quiesce_sum(&counters, Duration::from_millis(300), Duration::from_secs(30)).await;
  let total_sent = n * listeners;
  let lost = total_sent.saturating_sub(received);
  let drain_ns = last_stamp_ns(&counters).saturating_sub(emit_offset_ns);
  println!(
    "{name}: emitted={total_sent} received={received} lost={lost} ({}%) emit={}ms drain={}ms delivered/s={}",
    fmt_pct(lost, total_sent),
    fmt_ms(emit_ns),
    fmt_ms(drain_ns),
    per_sec(received, drain_ns),
  );
}

/// `NONMATCH_LISTENERS` listeners filtering for "load"; emit `n`
/// `DomContentLoaded` events they all ignore, then one matching `Load`
/// sentinel. Reports the exact time until every listener saw the
/// sentinel — pure wakeup/filter overhead of non-matching traffic.
async fn nonmatching_wakeups(n: u64) {
  let emitter = EventEmitter::new();
  let t0 = Instant::now();
  let counters: Vec<Arc<Stamped>> = (0..NONMATCH_LISTENERS).map(|_| Stamped::new()).collect();
  for c in &counters {
    emitter.on("load", stamped_listener(c, t0));
  }
  tokio::time::sleep(Duration::from_millis(50)).await;

  let emit_offset_ns = elapsed_ns(t0);
  for i in 0..n {
    emitter.emit(PageEvent::DomContentLoaded);
    if i % 64 == 63 {
      tokio::task::yield_now().await;
    }
  }
  emitter.emit(PageEvent::Load);

  let all_seen = || counters.iter().all(|c| c.count.load(Ordering::Acquire) >= 1);
  let deadline = Instant::now() + Duration::from_secs(60);
  while !all_seen() && Instant::now() < deadline {
    tokio::time::sleep(Duration::from_millis(2)).await;
  }
  let sentinels = counters.iter().filter(|c| c.count.load(Ordering::Acquire) >= 1).count();
  let drain_ns = last_stamp_ns(&counters).saturating_sub(emit_offset_ns);
  println!(
    "nonmatching_wakeups: {NONMATCH_LISTENERS} listeners x {n} ignored events drained in {}ms (sentinels {sentinels}/{NONMATCH_LISTENERS})",
    fmt_ms(drain_ns),
  );
}

/// Per-event dispatch latency: emit one `Load`, spin (yielding) until the
/// callback bumps the counter, record the delta. Reports p50/p99/max.
async fn dispatch_latency(samples: usize) {
  let emitter = EventEmitter::new();
  let t0 = Instant::now();
  let counter = Stamped::new();
  emitter.on("load", stamped_listener(&counter, t0));
  tokio::time::sleep(Duration::from_millis(50)).await;

  let mut deltas_ns: Vec<u64> = Vec::with_capacity(samples);
  let mut expected = 0u64;
  for _ in 0..samples {
    expected += 1;
    let start = Instant::now();
    emitter.emit(PageEvent::Load);
    while counter.count.load(Ordering::Acquire) < expected {
      tokio::task::yield_now().await;
    }
    deltas_ns.push(elapsed_ns(start));
  }
  deltas_ns.sort_unstable();
  let p = |q: usize| deltas_ns[(deltas_ns.len() - 1) * q / 100];
  println!(
    "dispatch_latency: p50={}us p99={}us max={}us over {samples} samples",
    fmt_us(p(50)),
    fmt_us(p(99)),
    fmt_us(deltas_ns[deltas_ns.len() - 1]),
  );
}

/// `wait_for` round-trip with a 1ms pre-emit delay so the pre-rewrite
/// emitter (which subscribes on first poll) can still win the race.
/// Kept unchanged for baseline comparability.
async fn wait_for_roundtrip(iters: usize) {
  let emitter = EventEmitter::new();
  let start = Instant::now();
  for _ in 0..iters {
    let em = emitter.clone();
    let waiter = tokio::spawn(async move { em.wait_for_event("load", 5_000).await });
    tokio::time::sleep(Duration::from_millis(1)).await;
    emitter.emit(PageEvent::Load);
    match waiter.await {
      Ok(Ok(_)) => {},
      Ok(Err(e)) => {
        println!("wait_for_roundtrip: waiter error: {e}");
        break;
      },
      Err(e) => {
        println!("wait_for_roundtrip: join error: {e}");
        break;
      },
    }
  }
  println!(
    "wait_for_roundtrip: {iters} iters in {}ms (1ms/iter sleep floor)",
    fmt_ms(elapsed_ns(start)),
  );
}

/// Emit BEFORE the returned future is ever polled. Only passes when
/// `wait_for` subscribes synchronously inside the call; an emitter that
/// subscribes on first poll misses every event and times out.
async fn wait_for_prearm(iters: usize) {
  let emitter = EventEmitter::new();
  let start = Instant::now();
  let mut ok = 0usize;
  for _ in 0..iters {
    let fut = emitter.wait_for_event("load", 250);
    emitter.emit(PageEvent::Load);
    if fut.await.is_ok() {
      ok += 1;
    }
  }
  println!(
    "wait_for_prearm: {ok}/{iters} caught pre-poll emits in {}ms",
    fmt_ms(elapsed_ns(start)),
  );
}

/// End-to-end: real Chromium, `page.on('console')` counter, `n`
/// console.log calls issued in evaluate bursts of `chunk`. Measures
/// delivered count + wall time across transport -> tracker -> emitter
/// -> callback. `FERRI_EVENT_BENCH_E2E_CHUNK` splits the storm to
/// probe burst-size sensitivity.
async fn e2e_console_storm(n: u64, chunk: u64) {
  use ferridriver::options::LaunchOptions;
  let browser = match ferridriver::chromium().launch(LaunchOptions::default()).await {
    Ok(b) => b,
    Err(e) => {
      println!("e2e_console_storm: launch failed, skipping: {e}");
      return;
    },
  };
  let counter = Stamped::new();
  let result = async {
    let page = browser.page().await?;
    page.goto("data:text/html,<title>bench</title>").await?;
    let t0 = Instant::now();
    page.events().on("console", stamped_listener(&counter, t0));
    tokio::time::sleep(Duration::from_millis(100)).await;

    let start_offset_ns = elapsed_ns(t0);
    let mut issued = 0u64;
    while issued < n {
      let batch = chunk.min(n - issued);
      page
        .evaluate(
          &format!(
            "for (let i = {issued}; i < {}; i++) console.log('storm-' + i);",
            issued + batch
          ),
          ferridriver::protocol::SerializedArgument::default(),
          None,
        )
        .await?;
      issued += batch;
    }
    let counters = [Arc::clone(&counter)];
    let received = quiesce_sum(&counters, Duration::from_secs(1), Duration::from_secs(60)).await;
    let drain_ns = last_stamp_ns(&counters).saturating_sub(start_offset_ns);
    let lost = n.saturating_sub(received);
    println!(
      "e2e_console_storm: emitted={n} received={received} lost={lost} ({}%) drain={}ms delivered/s={}",
      fmt_pct(lost, n),
      fmt_ms(drain_ns),
      per_sec(received, drain_ns),
    );
    Ok::<(), ferridriver::FerriError>(())
  }
  .await;
  if let Err(e) = result {
    println!(
      "e2e_console_storm: error after {} events received: {e}",
      counter.count.load(Ordering::Acquire)
    );
  }
  if let Err(e) = browser.close().await {
    println!("e2e_console_storm: close error: {e}");
  }
}

fn main() {
  let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
    Ok(rt) => rt,
    Err(e) => {
      eprintln!("failed to build tokio runtime: {e}");
      std::process::exit(1);
    },
  };
  rt.block_on(async {
    println!("== event_bench ==");

    storm("storm_hot_1_listener", STORM_EVENTS, 1, false).await;
    storm("storm_paced_1_listener", STORM_EVENTS, 1, true).await;
    storm("storm_hot_8_listeners", STORM_EVENTS, FANOUT_LISTENERS, false).await;
    storm("storm_paced_8_listeners", STORM_EVENTS, FANOUT_LISTENERS, true).await;
    nonmatching_wakeups(STORM_EVENTS).await;
    dispatch_latency(LATENCY_SAMPLES).await;
    wait_for_roundtrip(1_000).await;
    wait_for_prearm(1_000).await;

    if std::env::var("FERRI_EVENT_BENCH_E2E").is_ok_and(|v| v == "1") {
      let n = std::env::var("FERRI_EVENT_BENCH_E2E_N")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(5_000);
      let chunk = std::env::var("FERRI_EVENT_BENCH_E2E_CHUNK")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(n)
        .max(1);
      e2e_console_storm(n, chunk).await;
    } else {
      println!("e2e_console_storm: skipped (set FERRI_EVENT_BENCH_E2E=1)");
    }
  });
}
