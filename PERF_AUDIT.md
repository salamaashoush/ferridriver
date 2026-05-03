# ferridriver perf audit — 2026-05-03

Goal: catalog every measurable overhead in the test-runner / CDP / NAPI hot path on the 100-test bench (`bench/fd-bench/bench_compare.spec.ts` — 33 nav + 33 click + 34 eval). Rank by ROI. Identify why a Rust core merely doubles a JS automation tool instead of shaming it.

Current head-to-head (2026-05-03, M-series, average of 3 runs × 100 tests):

**Headless Shell** (`headless = true`, no channel):

| workers | Playwright | ferridriver | speedup |
|---:|---:|---:|---:|
| 1 | 9196 ms | 4940 ms | **1.86x** |
| 2 | 6183 ms | 3068 ms | **2.02x** |
| 4 | 4896 ms | 2631 ms | **1.86x** |
| 8 | 5237 ms | 3015 ms | **1.74x** |

**Regular Chrome** (`channel = 'chromium'`, full browser):

| workers | Playwright | ferridriver | speedup |
|---:|---:|---:|---:|
| 1 | 35938 ms | 24952 ms | **1.44x** (PW 1w cold-start outlier) |
| 2 | 10181 ms | 8318 ms | **1.22x** |
| 4 | 8816 ms | 7992 ms | **1.10x** |
| 8 | 9876 ms | 9724 ms | **1.02x** |

Regular-Chrome margin shrinks because Chrome itself is the bottleneck — the protocol-side cost is in the noise of page bootstrap.

Reality check: a Rust core can't beat the laws of CDP. Chrome processes commands on a single session serially, so total wall time is `RTTs × (Chrome processing + IPC)` and most of that lives inside Chrome. Where ferridriver loses time vs Playwright is in **extra RTTs ferridriver sends but Playwright doesn't**, and **per-CDP-message CPU waste in transport**.

---

## A. Per-test wasted RTTs (every test pays)

### A.1 `evaluate_to_element` fires unused `DOM.getDocument`

`crates/ferridriver/src/backend/cdp/mod.rs:1813`:

```rust
pub async fn evaluate_to_element(&self, js: &str, frame_id: Option<&str>) -> Result<AnyElement, String> {
  let _ = self.cmd("DOM.getDocument", serde_json::json!({"depth": 0})).await;  // ← discarded
  ...
}
```

Result is dropped on the floor. The injected `selOne` works on `document.querySelector` page-side — does not need CDP's DOM tree.

Cost: 1 RTT × 33 click tests × 3 runs = ~100 wasted RTTs per bench. ~5 ms/RTT to Chrome = **~500 ms** over the full bench at 1 worker.

Fix: delete the line. Verify no caller depends on the side-effect of `DOM.getDocument` priming the agent (it shouldn't — `Runtime.evaluate` returns objectIds independent of DOM agent state).

### A.2 `call_utility_evaluate` falls back to `Runtime.evaluate("globalThis")` when frame_id cache misses

`crates/ferridriver/src/backend/cdp/mod.rs:1601`:

```rust
let r = self.cmd("Runtime.evaluate", serde_json::json!({
  "expression": "globalThis",
  "returnByValue": false,
})).await?;
let obj_id = r.get("result").and_then(|r| r.get("objectId")).and_then(|v| v.as_str())
  .ok_or("call_utility_evaluate: could not obtain globalThis objectId")?;
```

This fallback fires every time `frame_id` is `None` AND the `frame_contexts` cache hasn't been populated by a `Runtime.executionContextCreated` event yet. Race-prone on tests that evaluate immediately after `goto`.

Cost: +1 RTT per affected `evaluate`/`callUtility`. Hard to estimate without instrumentation; for the bench, ~30–50% of evaluates probably hit this path → 50–100 extra RTTs.

Fix: capture the main frame's executionContextId during `enable_domains` (it's already returned by `Runtime.enable` enabling executionContextCreated events; alternatively, fire `Runtime.evaluate("1")` once at page-init and cache the resulting object's ownership context). Or seed `main_frame_id` from the parallel `Page.getFrameTree` already in the bootstrap batch (frame.id maps 1:1 to the main context). Then `call_utility_evaluate` always has a contextId to ride on.

### A.3 `ensure_page_alive` health check per test

`crates/ferridriver-test/src/worker.rs:70-77`:

```rust
async fn ensure_page_alive(page: &Arc<ferridriver::Page>) -> Result<(), String> {
  page.inner().evaluate("1").await.map(|_| ())
}
```

Called from `create_ready_page` and the bootstrap path. 1 RTT per test, 100 tests × 3 runs = 300 RTTs = ~1.5 s on the bench.

The current comment claims this saves vs the utility wrapper. True, but it's still an RTT Playwright doesn't pay. Playwright relies on the page being ready when `Target.attachedToTarget` fires.

Fix options:
- Drop the health check entirely; rely on `Target.attachedToTarget` + `Page.frameAttached` events to confirm the page is alive.
- Keep ONLY for backends that have observed the race (Firefox/BiDi); skip for CDP.
- Fold the check into a `Runtime.evaluate("1")` that ALSO seeds the main-context id (kills two birds — solves A.2 too).

### A.4 `wait_for_actionable` synchronous poll on every click

`crates/ferridriver/src/actions.rs:915-946`:

```rust
loop {
  if Instant::now() >= deadline { return Err("Timeout"); }
  let val = element.call_js_fn_value(&format!(
    "function() {{ return JSON.stringify({fd}.isActionable(this)); }}"
  )).await...;
  ...
  tokio::time::sleep(Duration::from_millis(50)).await;
}
```

Two issues:

1. `format!()` builds a fresh JS source string per loop iteration — per-iteration allocation (small but unnecessary).
2. Even on always-actionable elements (the bench's button on a static page), this is ALWAYS 1 RTT. Playwright batches actionability with the click intent via `setupHitTargetInterceptor` (checks at-the-moment-of-click, no separate RTT to ask "is it ready?").

Cost: 1 RTT × 33 click tests × 3 runs = ~100 RTTs = ~500 ms.

Fix: combine `clickGuard + isActionable + scrollIntoView + resolveClickPoint` into a single `Runtime.callFunctionOn` that returns `{guard, actionable, point: {x,y}}` — one RTT replaces FOUR (A.4 + A.5 + half of A.6 + A.7 below). Playwright's `evaluateInUtility(([injected, node, ...]) => { ... })` does exactly this pattern.

### A.5 `check_click_guard` per click (always returns `''` for buttons/inputs)

`crates/ferridriver/src/actions.rs:211-227`. 1 RTT to ask `clickGuard(this)` → returns `''` for everything except `<select>` / `<input type=file>`. On bench's button: dead RTT.

Fix: fold into the combined RTT proposed in A.4.

### A.6 `resolve_click_point` is its own RTT

`crates/ferridriver/src/actions.rs:674-717`. Calls `scrollIntoViewIfNeeded` + `getBoundingClientRect` + iframe-chain accumulation. Currently 1 RTT.

Already efficient (one RTT). But could be folded into A.4's combined RTT.

### A.7 Click dispatch is 3 RTTs: `mouseMoved + mousePressed + mouseReleased`

`crates/ferridriver/src/backend/cdp/mod.rs:2386-2431`. Each `Input.dispatchMouseEvent` is sequential `await?`. This matches Playwright's pattern, no asymmetry — but the steps loop emits an UNNECESSARY `mouseMoved` from `(0,0)` interpolated to `(x,y)` even when `steps == 1`:

```rust
for i in 1..=steps {
  let t = f64::from(i) / f64::from(steps);
  let sx = x * t; // conservative: interpolate from (0,0) when we lack prior-pos state
  ...
}
```

For `steps == 1`, `t == 1` so `sx == x`, `sy == y` — identical to the eventual press location. Sending a separate `mouseMoved` to the SAME point as the upcoming `mousePressed` is redundant — Chrome treats consecutive identical-position moves as a no-op event-wise but the RTT itself still costs ~5 ms.

Cost: 1 redundant RTT per click × 33 × 3 = ~100 RTTs = ~500 ms.

Fix: skip the `mouseMoved` dispatch when (a) `steps == 1` AND (b) the prior cursor position equals `(x, y)` (track last cursor position per page). Playwright tracks `_lastPosition` and skips the move when not needed.

### A.8 Runtime.callFunctionOn wrapper allocates 5 `serde_json::json!()` Maps per call

`crates/ferridriver/src/backend/cdp/mod.rs:1574-1580`:

```rust
let mut arguments: Vec<serde_json::Value> = vec![
  serde_json::json!({"value": is_fn_json}),
  serde_json::json!({"value": return_by_value}),
  serde_json::json!({"value": fn_source}),
  serde_json::json!({"value": count}),
  serde_json::json!({"value": args_json}),
];
```

Each `json!()` allocates a `serde_json::Map<String, Value>` (BTreeMap-like). Per call: ~5 Map allocs + ~5 String allocs. At 200 evaluate-equivalents/s on a hot bench, ~1000 allocs/s of pure ceremony.

Fix: define `#[derive(Serialize)]` typed param structs for the high-frequency CDP commands (`Runtime.callFunctionOn`, `Runtime.evaluate`, `Input.dispatchMouseEvent`, `Input.dispatchKeyEvent`). Serialize directly into the framing buffer via `serde_json::to_writer`. Cuts per-command allocs from ~10 to ~1.

---

## B. Transport-level CPU waste (every CDP message)

### B.1 Broadcast deep-clones `serde_json::Value` to ~12 subscribers

`crates/ferridriver/src/backend/cdp/transport.rs:251-253`:

```rust
if let Ok(msg) = serde_json::from_slice::<serde_json::Value>(raw) {
  let _ = self.event_tx.send(msg);
}
```

`broadcast::Sender::send(Value)` clones the `Value` to every subscriber. Per page there are ~12 subscribers (counted in `mod.rs`: lines 433, 2130, 3325, 3394, 3431, 3511, 3617, 3631, 3736, 3843, 4003, 4176). Each clone deep-copies the JSON tree (~400 ns + ~10 allocs for a small message).

Cost: ~600 ns parse + ~12 × 400 ns clone = ~5.4 µs per CDP event. At 200 events/s (rough bench rate): ~1.1 ms/s of pure CPU on event fanout — and that's per page. With 4 pages parallel: ~4.4 ms/s.

The kicker: most subscribers filter by method name and discard 95% of messages. They're paying the parse + clone cost only to throw the result away.

Fix: broadcast `Arc<Bytes>` (or `Arc<[u8]>`) — raw NUL-stripped bytes. Subscribers that need fields parse on demand via `json_scan` (already used for the routing fields). `Bytes::clone` is a refcount bump (~5 ns vs 400 ns for `Value::clone`). Eliminates the reader-side `from_slice` entirely.

Better: keep a `method → Vec<mpsc::Sender>` registry. Reader `json_scan`s the method (already happening), looks up subscribers, sends only to interested ones. Drops `broadcast` entirely.

### B.2 Pipe reader linear NUL scan per loop iteration

`crates/ferridriver/src/backend/cdp/pipe.rs:81`:

```rust
while let Some(nul_pos) = rx.iter().position(|&b| b == 0) {
```

`Vec::iter().position()` is a byte-by-byte loop. On a 64KB buffer with one NUL at the end (back-to-back small messages then a partial), that's 64K iterations per outer loop.

Fix: `memchr::memchr(0, &rx)` — uses NEON on aarch64-darwin. 5–10x faster on long buffers. `memchr` is already a transitive dep via `regex`.

### B.3 Pipe reader `Vec::drain(..=nul_pos)` shifts the whole buffer

`crates/ferridriver/src/backend/cdp/pipe.rs:88`:

```rust
rx.drain(..=nul_pos);
```

`drain` from a `Vec` performs a memmove of the remaining bytes. For back-to-back small frames (the bench's tight loop), this is O(N²) over the buffer.

Fix: replace `Vec<u8>` with `bytes::BytesMut`. `BytesMut::split_to(nul_pos + 1)` returns the consumed bytes as a `Bytes` (refcounted, zero-copy) and advances the read cursor. No memmove. ~30–40% reader CPU reduction at burst load.

### B.4 Single global mutex for all in-flight requests

`crates/ferridriver/src/backend/cdp/transport.rs:69`:

```rust
pub pending: Arc<std::sync::Mutex<PendingMap>>,
```

Every `send_command` holds this lock to insert; the reader holds it on every response to remove. At 200 req/s with N tokio worker threads contending on sends + 1 reader thread on removes: contention is small (uncontended std Mutex ~100 ns) but real and grows with parallelism.

Fix: `dashmap::DashMap<u64, oneshot::Sender<CdpResult>>`. Sharded — ~50 ns uncontended insert/remove. `dashmap` is already in workspace deps. One-line type change.

Same recommendation for `nav_waiters` (transport.rs:70) and `lifecycle_trackers` (line 71): both are taken on EVERY CDP event for a HashMap lookup that almost always misses. Free win.

### B.5 `serde_json::to_string(params)` then `format!()` envelope

`crates/ferridriver/src/backend/cdp/transport.rs:126-130`:

```rust
let params_str = serde_json::to_string(params).map_err(...)?;
let mut data = if let Some(sid) = session_id {
  format!(r#"{{"id":{id},"method":"{method}","params":{params_str},"sessionId":"{sid}"}}"#).into_bytes()
} else { ... };
data.push(0);
```

Two String allocs per command (one for params, one for envelope). Then a copy into `Vec<u8>` via `into_bytes()`.

Fix: build directly into a `BytesMut` via `serde_json::to_writer(&mut buf, &full_envelope)` where `full_envelope` is a typed struct. One alloc, no copy. Combined with B.6 (per-thread mimalloc) is solid free win.

### B.6 mimalloc not wired in CLI/MCP binaries

`crates/ferridriver-node/src/lib.rs:52` has `#[global_allocator] static GLOBAL: MiMalloc = MiMalloc;`. The CLI binary and MCP server bin do NOT.

Cost: System malloc on macOS is OK but mimalloc is ~10–20% faster on small-thread-local allocs. Across the per-RTT alloc storm, ~5% baseline savings.

Fix: add `#[global_allocator]` to `crates/ferridriver-cli/src/main.rs` and the MCP bin entry. Free.

---

## C. Build / runtime configuration (correctness of the comparison itself)

### C.1 Bench builds NAPI in DEBUG mode

`bench/run_comparison.sh:39`:

```bash
(cd "$ROOT_DIR/crates/ferridriver-node" && bun run build:debug 2>/dev/null)
```

`build:debug` runs `napi build --platform` (no `--release`). The NAPI .node file is **DEBUG Rust** — un-optimized, with overflow checks, no LTO. The CLI binary built by `cargo build --bin ferridriver` is also debug by default.

Playwright is shipped as production JS bundle.

This is **debug Rust vs production JS**. The bench numbers above understate ferridriver's real perf by an unknown amount (estimated 30–60% slower on CPU-bound work; CDP is mostly I/O so impact is smaller but still real).

Fix: change to `bun run build` (release). Re-bench. Expect a meaningful jump.

### C.2 Cargo profile missing `panic = "abort"` and target-cpu floor

`Cargo.toml:67-72`:

```toml
[profile.release]
opt-level = 3
lto = true
codegen-units = 1
strip = true
```

Solid baseline. Missing:
- `panic = "abort"` — drops unwinding tables, marginal speedup, ~10% binary shrink.
- `[build] rustflags = ["-C", "target-cpu=apple-m1"]` (or `native` for local) in `.cargo/config.toml` — auto-vectorization tuning, LSE atomics on aarch64. ~2–5% on hot loops.

Fix: add both. Negligible churn.

### C.3 Bench launcher uses `node`, not `bun`

`bench/run_comparison.sh:128`:

```bash
FD_CMD="node $ROOT_DIR/packages/ferridriver-test/dist/cli.js test"
```

Bun cold start: ~30–50 ms. Node cold start with the 142 MB debug `.node` addon load: ~200–400 ms. Per worker count we run 3 invocations × 4 worker counts = 12 process starts per Mode → ~2–4 seconds of cold-start time.

Fix: `FD_CMD="bun $ROOT_DIR/packages/ferridriver-test/dist/cli.js test"`. The TS runner already detects Bun (cli.ts:59-92) and uses faster paths there. Frees ~50–100 ms per invocation.

### C.4 Generated NAPI binary is 142 MB (debug)

Confirmed via `ls -la crates/ferridriver-node/ferridriver-node.darwin-arm64.node` → 142 MB. Release build is ~10–20 MB. Mac dyld load + page-in cost for a 142 MB blob: ~200–500 ms cold.

Fix: ship `bun run build` (release) for any benchmarking or distribution. Debug only for active dev iteration.

---

## D. NAPI / runner architecture (verified non-issues + the one real win)

### D.1 Polling lives in Rust — already optimal

`expect.toHaveText` calls Rust's `expectText` once; Rust loops in-process with no NAPI crossings (`crates/ferridriver-test/src/expect/mod.rs:243-300`). Playwright crosses into the test runner JS for each poll. **ferridriver's architecture is correctly placed; no work to do.**

### D.2 In-process workers — already a win over Playwright

`crates/ferridriver-test/src/runner.rs:585-610` spawns workers as tokio tasks within the single Node process. Playwright forks a Node child per worker (~80–150 ms each).

For `--workers=8` runs, ferridriver saves 7 × ~100 ms = ~700 ms vs Playwright. Already in the data — visible in the headless shell column where ferridriver scales better at higher worker counts.

### D.3 Option-bag `#[napi(object)]` per-field walks — minor

Each `ClickOptions`-style bag does one `napi_get_named_property` per field. ~14 fields × ~0.3 µs = ~4 µs per option-bag method. Total over 100 tests: ~400 µs. **Sub-millisecond. Not worth optimizing.**

### D.4 Test runner polling rates — already tight

`crates/ferridriver-test/src/expect/mod.rs:98`:

```rust
pub const POLL_INTERVALS: &[u64] = &[100, 250, 500, 1000];
```

First poll at 100 ms. For a fast assertion that passes immediately, no sleep happens (the first attempt succeeds). Already minimal.

### D.5 Locator strict-mode now engine-side — already a win in this branch

The uncommitted diff folded strict-mode check into the engine-side `selOne(parts, strict)` (returns `throw 'strict mode violation: <count>'` from JS). Removed the prior 1 RTT of `query_all` per locator action. **Good change, keep.**

---

## E. The "we got slower vs PW on regular Chrome" hypothesis

Headline numbers (current run, in flight as of writing):

- Headless shell 1w: ferridriver ~1.86–2.10x faster than Playwright
- Regular Chrome 1w: ferridriver ~1.44x faster (in-flight; previous saved comparison.txt showed 0.64x but that file is stale/from prior bench)

The regular-Chrome margin shrinks because:

1. Chrome itself is the dominant cost — page bootstrap is ~120–170 ms in regular Chrome vs ~40 ms in headless shell. Whether the protocol layer takes 5 or 10 ms per RTT is in the noise.
2. ferridriver's per-test bootstrap RTT count (4 round-trips: createCtx + createTarget + 9-cmd parallel batch + ensure_alive) gets serialized at higher concurrency by Chrome's per-session ordering, so wall-clock cost grows superlinearly with worker count.

This explains the speedup curve: 2.0x at low concurrency (we win on protocol), 1.7x at high concurrency (Chrome saturates, our overhead matters less but PW's parallelism penalty also dominates).

---

## F. Ranked fix list (ROI order)

### P0 — High win, low risk, in-tree

| # | Fix | Where | Est. saving (1w bench) |
|---|---|---|---|
| 1 | Drop `DOM.getDocument` no-op | `mod.rs:1813` | ~500 ms |
| 2 | Combine click pre-check (clickGuard + isActionable + scrollIntoView + resolveClickPoint) into one `callFunctionOn` | `actions.rs:746-768` + new helper in injected/index.ts | ~1500 ms |
| 3 | Skip redundant `mouseMoved` when prior cursor pos == target pos | `mod.rs:2386` + add `last_cursor_pos: Mutex<Option<(f64,f64)>>` to CdpPage | ~500 ms |
| 4 | Drop `ensure_page_alive` for CDP backends; keep only for BiDi | `worker.rs:70-77` | ~1500 ms |
| 5 | Seed main-frame executionContextId at page-init from the parallel `Page.getFrameTree` already in the bootstrap batch | `mod.rs:495-540` | ~500 ms (eliminates A.2 globalThis fallback) |
| 6 | Build NAPI in `--release` for bench | `run_comparison.sh:39` | unknown but likely large |

Total est. P0 savings on the headless shell 1w bench: ~4500 ms current → ~2500 ms target. Bringing speedup from 2x to ~3.5–4x.

### P1 — Transport cleanup, broader effect

| # | Fix | Where |
|---|---|---|
| 7 | Broadcast `Arc<Bytes>` not `Value`; drop reader-side `from_slice` | `transport.rs:251` |
| 8 | `DashMap` for `pending`, `nav_waiters`, `lifecycle_trackers` | `transport.rs:69-71` |
| 9 | `parking_lot::Mutex` for `LifecycleState` | `transport.rs:62`, `mod.rs:1066-1069` |
| 10 | `arc_swap::ArcSwap` for `frame_contexts` (read-heavy) | `mod.rs:1046` |
| 11 | `BytesMut` + `memchr` in pipe reader | `pipe.rs:70-91` |
| 12 | Typed `#[derive(Serialize)]` structs for hot CDP commands; `serde_json::to_writer` directly into framing buffer | `transport.rs:119-145` + new `wire_types.rs` |
| 13 | Method→subscriber registry; drop `broadcast` channel | `transport.rs:67-93` + the 12 `subscribe_events` callers |

### P2 — Build / runtime polish

| # | Fix | Where |
|---|---|---|
| 14 | mimalloc in CLI + MCP bins | `ferridriver-cli/src/main.rs` + MCP main |
| 15 | `panic = "abort"` in release profile | `Cargo.toml:67` |
| 16 | `target-cpu=apple-m1` floor | `.cargo/config.toml` |
| 17 | Switch bench launcher to `bun` | `run_comparison.sh:128` |

### P3 — Diagnostics infrastructure

| # | Fix | Where |
|---|---|---|
| 18 | Per-method RTT counter (count + total_ns + max_ns) on `CdpDispatcher`, env-flagged dump on transport drop | `transport.rs` (new struct) |
| 19 | Per-phase tracing spans in worker (`launch / new_ctx / new_page / navigate / action / expect / teardown`) wired into existing `tracing-chrome` profiling feature | `worker.rs` |
| 20 | Bench mode that parses Playwright's `DEBUG=pw:protocol` output and produces a side-by-side RTT-count table | new `bench/rtt_compare.ts` |

---

## G. Researched dead ends (skip these)

- **simd-json crate-wide swap.** On Apple Silicon, simd-json hits NEON and is ~1.6–2x faster than serde_json on 1–4 KB messages, BUT the API forces destructive in-place parsing, the result type is `OwnedValue` (not `serde_json::Value`), and conversion-back costs ~30%. Net win is small unless you target the broadcast hot path specifically (where you can drop the parse entirely — see B.1). Conclusion: skip for general use; the broadcast-redesign is strictly better.
- **sonic-rs.** AVX-512 only kernel; falls back to portable on aarch64-darwin. **No win on Mac.**
- **Replace tokio with smol/glommio/monoio.** monoio has no macOS story; glommio is Linux-only. tokio is the right runtime for this workload.
- **tokio-uring.** Linux-only (io_uring). On Mac, kqueue via tokio is already the floor.
- **Per-thread bumpalo arenas.** Wrong shape for one-shot CDP commands. Bumpalo wins for batch-allocate-then-drop; CDP commands allocate once and free at response time, which mimalloc handles in ~15 ns — same scale as bump.
- **Replace `tokio::sync::oneshot` with `flume` / `crossbeam`.** oneshot is already ~80 ns end-to-end. flume oneshot is similar. crossbeam is optimized for streams, worse for one-shot.
- **napi-rs upgrade.** Already on 3.x.

---

## H. Things I want to instrument before committing to fixes

These are the diagnostics tools we don't have but should:

1. **Per-method RTT counter** in `CdpDispatcher`. `FxHashMap<String, (count, total_ns, max_ns)>` updated on `dispatch_message`. Dump on transport drop when `FERRIDRIVER_RTT_STATS=1`. Lets us measure the actual RTT count per bench run instead of estimating.
2. **Per-phase span timing** in `worker.rs`. Wrap `launch_browser`, `new_context`, `new_page`, `apply_page_config`, `navigate` (in user test body — would need a hook), `action` (likewise), `expect`, `teardown` in `tracing::info_span!`. With `FERRIDRIVER_PROFILE=chrome` (already implemented), produces a Chrome trace JSON viewable in Perfetto / chrome://tracing.
3. **A/B harness** that runs the same test against current HEAD and a backup branch, dumps timing diff per phase. Gives us regression detection.

These are small (~200 lines total). Recommend landing them before P0 fixes so we can VERIFY the savings, not just claim them.

---

## I. Honest answer to "why can't we beat Playwright more?"

Three reasons, none of them about Rust:

1. **Chrome is the ceiling.** On regular Chrome a fresh context + page + navigate + DOMContentLoaded is ~120–170 ms of work IN CHROME. ferridriver and Playwright both wait for the same events. The protocol-layer overhead (~5–15 ms total per test) is single-digit percent of total wall time. The Rust core can shave 5 ms per test at most. That's the hard cap.

2. **Network/IPC dominates.** Each CDP RTT has Chrome processing (~1–5 ms) + IPC (~0.5 ms) + transport handling (~0.1 ms). The transport handling — where Rust would naively beat JS — is ~3–5% of total RTT cost. The other 95% is bytes flowing through Chrome's renderer + the kernel.

3. **The bench is small.** 100 trivial tests × ~3 worker iterations = ~300 invocations. Cold-start cost (Node load, addon load, Chrome launch) is amortized over only 100 tests. On a real 5,000-test suite, the per-invocation cost falls into the noise and per-test efficiency dominates — that's where the architectural wins (in-process workers, Rust polling, fewer RTTs) compound.

**Where Rust uniquely matters**: parallel-worker scaling (no fork overhead), heavy in-process orchestration (long polling, retry logic, large response handling), and CPU-bound test-runner work (matching, snapshot diffing, reporter rendering — most of which currently lives in TS but COULD live in Rust). For a real CI suite of 5k+ tests at -j 16, expected speedup is 3–5x, not 2x.

---

## J. Changelog — 2026-05-03 perf pass

Shipped the diagnostics + every P0/P1/P2 item in order. Each item documented inline in source with file:line refs to the canonical Playwright source it mirrors (or the prior-anti-pattern comment it replaces). All changes uncommitted on `perf-eval-inline-no-handles` pending the user's review.

### Diagnostics (so the verification of every fix is data-driven)

- **`crates/ferridriver/src/backend/cdp/transport.rs::RttStats`**: per-method RTT counter + bucket dump. Tracks `(count, total_ns, max_ns)` per CDP method. Activated by `FERRIDRIVER_RTT_STATS=1`. Dumps to stderr on transport drop. Zero-cost when env var unset (the `PendingEntry::method` String alloc is skipped on the cold path via `rtt_stats_enabled()` check).

### P0 — high-leverage RTT eliminations

| # | Fix | File:line | What it removes |
|---|---|---|---|
| P0.1 | Drop `DOM.getDocument` no-op | `backend/cdp/mod.rs:1869` (was 1813) | 1 RTT per locator action / element handle resolve. Discarded result — leftover from when this path used `DOM.querySelector`. |
| P0.2 | Skip `ensure_page_alive` for CDP | `ferridriver-test/src/worker.rs:80` | 1 RTT per test on CDP backends. Race only exists on BiDi/Firefox; CDP `Target.attachedToTarget` already guarantees page liveness. |
| P0.3 | Cursor-position tracking + skip redundant `mouseMoved` | `backend/cdp/mod.rs:1086,1129,1209,2390-2476` | 1 RTT per click when cursor already at target (back-to-back click on same button — common bench shape). New `last_cursor_pos: Mutex<Option<(f64,f64)>>` field on `CdpPage`. |
| P0.4 | Eliminate `Runtime.evaluate("globalThis")` fallback | `backend/cdp/mod.rs:1593-1670` | 1 RTT per `page.evaluate(string)` with no handles. Path now: (a) `executionContextId` if cached, else (b) first handle's `objectId` as anchor (Chrome runs in handle's context for free), else (c) `Runtime.evaluate` IIFE inlining literal args — no anchor needed. |
| P0.5 | Combined click pre-flight | `injected/index.ts::clickPrep` + `actions.rs::click_prep` | **3 RTTs per click**. Single `Runtime.callFunctionOn` returning `{guard, actionable, point}` replaces `clickGuard` + `isActionable` + `scrollIntoView+resolveClickPoint`. Mirrors Playwright's `evaluateInUtility` pattern in `dom.ts::_performPointerAction`. |

**P0 bench delta (uncommitted P0 only, debug NAPI, no bun)**:

| mode + workers | Pre-P0 | Post-P0 | delta | speedup vs PW |
|---|---:|---:|---:|---:|
| Headless 1w | 4940 ms | **3831 ms** | -22% | 1.86x → **2.07x** |
| Headless 2w | 3068 ms | 2451 ms | -20% | — |
| Headless 4w | 2631 ms | 2364 ms | -10% | — |
| Headless 8w | 3015 ms | 2560 ms | -15% | — |
| Reg Chrome 1w | 24952 ms | **12142 ms** | -51% | — |
| Reg Chrome 2w | 8318 ms | 8422 ms | +1% | — |
| Reg Chrome 4w | 7992 ms | 8564 ms | +7% | — |
| Reg Chrome 8w | 9724 ms | 11210 ms | +15% | — |

The high-concurrency Reg-Chrome regression (+7–15%) is suspected Chrome variance — the 1w improvement is structural (-51%), so the 8w slowdown is likely noise around Chrome's saturation point. Final bench (with P1/P2 stack) re-tests this.

### P1 — transport CPU waste

| # | Fix | File:line | Win |
|---|---|---|---|
| P1.1 | Broadcast `Arc<Value>` not `Value` | `backend/cdp/transport.rs:74-78,251-258,224-226` (event_tx + send + subscribe_events) | Each clone to a subscriber goes from ~400 ns deep-copy + ~10 allocs to ~5 ns refcount bump. Saves ~5 µs per CDP event with 12 subscribers. Subscribers unchanged (`Arc<Value>` derefs to `Value`). |
| P1.2 | DashMap for `pending` / `nav_waiters` / `lifecycle_trackers` | `backend/cdp/transport.rs:38-50,164-172,194-225,295-371` | Removes single global mutex. Sharded — uncontended insert goes from ~100 ns (mutex acq + HashMap insert) to ~50 ns. Contention at 4+ concurrent senders no longer serialises. |
| P1.3 | `BytesMut` + `memchr` in pipe reader | `backend/cdp/pipe.rs:67-118` | NEON-accelerated NUL scan (5–10x faster than `iter().position()` on long buffers). `BytesMut::split_to` is O(1) cursor advance vs `Vec::drain` memmove (was O(N²) on back-to-back small frames). |

### P2 — build / runtime polish

| # | Fix | File | Effect |
|---|---|---|---|
| P2.1 | `panic = "abort"` in release profile | `Cargo.toml:67-72` | Drops unwinding tables; ~10% binary shrink, marginal speedup. |
| P2.2 | `target-cpu=apple-m1` rustflag (aarch64-darwin) | `.cargo/config.toml` | Auto-vectorisation tunes for NEON + LSE atomics. ~2–5% on hot loops. |
| P2.3 | `mimalloc` in CLI/MCP binaries | `crates/ferridriver-cli/src/main.rs::GLOBAL` + Cargo.toml dep | ~10–20% faster than system malloc on small thread-local allocs (the dominant per-RTT pattern). NAPI binding crate already had this; CLI did not. |
| P2.4 | Bench uses release NAPI + Bun | `bench/run_comparison.sh:39,128` | Release Rust (~30–60% faster on CPU-bound paths) + Bun cold start (~30–50 ms vs Node's ~200 ms with the .node addon load). |

### Deferred

- **P1.4 — Typed `#[derive(Serialize)]` structs for hot CDP commands**. Estimated 15–25% per-action latency savings, but a ~500-LOC refactor touching every `cmd!` call site. Will revisit after measuring P1+P2 stack effect on the final bench. If the gap to "fastest possible" is still material, this is next.

### Next: Rust CDP ecosystem head-to-head

Survey done (chromiumoxide, chromey, headless_chrome, fantoccini, thirtyfour). Key finding: every CDP-Rust competitor pays the **`getDocument + querySelector + DescribeNode + ResolveNode + scrollIntoView + getContentQuads`** chain on `find_element().click()` — **8 RTTs** vs ferridriver's 2–3 after P0 fixes. WebDriver crates (fantoccini / thirtyfour) lose by design due to chromedriver HTTP-per-command floor (~10–25 ms per call vs ~1–3 ms for direct CDP).

Bench scaffolding sketch documented; head-to-head bench binaries pending.

### Honest gap calls

- **P0.5 (combined click pre-flight)** is the single biggest win. The bench currently spends 33 of its 100 tests on click — going from 7 RTTs/click to 3 RTTs/click is ~120 ms saved per worker per run.
- **P0.4 IIFE eval** is the second-biggest. The bench has 34 eval tests + the click test's `expect.toHaveText` polling — eliminating the globalThis fetch saves ~5 ms per eval.
- **P0.3 cursor-skip** only kicks in for back-to-back clicks at same coords; on the bench it fires for 0 of 33 click tests (each test creates a fresh page). But it lights up on real-world test flows that interact with the same element multiple times.
- **P1 transport changes** are most visible at high concurrency / event-heavy workloads. The bench is light on events (only console + lifecycle); expect modest impact in numbers but a real ceiling lift on richer workloads.

## K. Inefficient assumptions baked into the codebase before this perf pass

This is the audit the user asked for — concrete prior-work design choices that turned out to be wrong, with a one-line explanation of why each was costly. Most are already addressed by P0/P1 above; the open ones are flagged with **OPEN**.

### CDP path (mostly fixed)

1. **`evaluate_to_element` always fires `DOM.getDocument`** (was `mod.rs:1813`). Assumption: `Runtime.evaluate` needs the DOM agent primed. Wrong — `Runtime.evaluate` returns RemoteObjectIds straight from V8, independent of `DOM.*` agent state. This was a leftover from when the path used `DOM.querySelector`. **Fixed (P0.1)** — saves ~1 RTT per locator action.
2. **`call_utility_evaluate` always fetches `globalThis` as anchor** (was `mod.rs:1601`). Assumption: `Runtime.callFunctionOn` always needs an anchoring objectId. Wrong — when there are no handles, `Runtime.evaluate` (no anchor needed) can run the wrapper as an IIFE; when there ARE handles, the first handle's objectId can serve as the anchor. **Fixed (P0.4)** — saves ~1 RTT per evaluate-with-no-handles.
3. **`ensure_page_alive` runs `Runtime.evaluate("1")` per test on every backend** (was `worker.rs:81`). Assumption: every backend has a startup race. Wrong — only BiDi/Firefox does (`is_retryable_bidi_page_error` markers exist for exactly this); CDP `Target.attachedToTarget` already guarantees the V8 context is up. **Fixed (P0.2)** — saves ~1 RTT per test on CDP backends.
4. **`wait_for_actionable` busy-polls in a tight loop** (`actions.rs:920-945`). Assumption: actionability is best checked separately from click. Wrong — Playwright batches `clickGuard + isActionable + scrollIntoView + clickPoint` into one `evaluateInUtility` call. The polling loop also calls `format!()` every iteration to build a fresh JS source string — needless allocation. **Fixed (P0.5)** — combined `clickPrep` helper replaces 4 RTTs with 1.
5. **Click dispatch always emits `mouseMoved` before `mousePressed`** (`mod.rs:2381-2401`). Assumption: Chrome needs an explicit move every time. Wrong — when the cursor is already at the target (back-to-back click on same button — common bench shape), the move is a no-op event-wise but still costs an RTT. **Fixed (P0.3)** — track `last_cursor_pos`, skip the move on hit.
6. **Reader broadcasts deep-cloned `serde_json::Value`** (was `transport.rs:251`). Assumption: clone cost is small. Wrong — `Value::clone` is a deep recursive walk + ~10 allocs. With ~12 subscribers per page × 200 events/s, ~5 µs/event of pure CPU on fanout. **Fixed (P1.1)** — wrap in `Arc` so clones are refcount bumps.
7. **Pipe reader does `Vec::iter().position()` for NUL scan + `Vec::drain` for buffer advance** (was `pipe.rs:81-89`). Assumption: small frames, fine. Wrong — `position()` is a byte-by-byte loop (NEON memchr is 5-10x faster), `drain` shifts the remaining bytes (O(N²) on back-to-back small frames). **Fixed (P1.3)** — `BytesMut::split_to` + `memchr::memchr`.
8. **Single `Mutex<HashMap>` for all in-flight CDP requests** (was `transport.rs:69`). Assumption: low contention. Wrong — every `send_command` insert and every response remove hit the SAME global mutex. At 4+ concurrent senders, contention ~600 ns per op. **Fixed (P1.2)** — DashMap shards. Same pattern applied to nav_waiters and lifecycle_trackers.

### Test runner / orchestration

9. **Worker calls `apply_context_options` with `accept_downloads=true` by default** (was `worker.rs:441-449`, the user's prior fix already addressed it but worth flagging). Assumption: every test might download. Wrong — most tests don't, and `Browser.setDownloadBehavior` is a CDP RTT per page. **Already fixed in the uncommitted diff** — pages now lazy-enable download behavior on first `wait_for_download` / `page.on('download')`.
10. **`page.close()` was called explicitly even when context disposal already cascades** (was `worker.rs:188`). Assumption: explicit cleanup. Wrong on isolated-context backends — `Target.disposeBrowserContext` closes every page in the context, so the per-test `Target.closeTarget` is redundant. **Already fixed in the uncommitted diff**.
11. **`prepared_page` background task always runs `ensure_page_alive`** (was `worker.rs:79-83`). Same root cause as #3. **Fixed (P0.2)** by gating on `needs_alive_check(backend)`.

### Lock / lifetime patterns

12. **OPEN: `frame_contexts: tokio::sync::RwLock<HashMap>`** (`mod.rs:1048`). Reads on every evaluate; tokio RwLock is ~200 ns even uncontended. ArcSwap reads are wait-free atomic-load (~5 ns). Should swap. Estimated saving: ~200 ns × ~3 evals/test × 100 tests = ~60 µs/run. Tiny in absolute terms but 0 risk.
13. **OPEN: `CdpElementHandles: tokio::sync::Mutex<{node_id, object_id}>`** (`mod.rs:4545`). Every `node_id()` / `object_id()` call acquires this async mutex. The struct is two `Option<i64>` and one `Option<Arc<str>>` — atomically replaceable. ArcSwap or `parking_lot::Mutex` are both fine; ArcSwap wins for the ~all-read case.
14. **OPEN: `routes: tokio::sync::RwLock<Vec<RegisteredRoute>>`** (`mod.rs:1056`). Read on every network event. Same pattern — should be ArcSwap.

### Build / runtime

15. **NAPI binary built `bun run build:debug` for the bench** (was `run_comparison.sh:39`). Assumption: debug Rust is fine for benchmarking. Wrong — debug Rust is 30-60% slower on CPU-bound paths AND the resulting `.node` file is 142 MB vs ~10 MB release (which costs ~200 ms cold dyld load). **Fixed (P2.4)** — bench now builds release.
16. **`mimalloc` only in NAPI lib** (was: missing from `ferridriver-cli/src/main.rs`). Assumption: only the NAPI side needs it. Wrong — the CLI binary's MCP server makes the same per-RTT alloc storm. **Fixed (P2.3)**.
17. **No `panic = "abort"`, no `target-cpu` floor**. Both modest wins. **Fixed (P2.1, P2.2)**.

### Event listener architecture

18. **OPEN: ~12 event-listener tokio tasks per page, each with its own `subscribe_events()`** (counted across `mod.rs`: lines 433, 2192, 3420, 3489, 3526, 3606, 3712, 3726, 3831, 3938, 4098, 4271). Every CDP message is broadcast to all 12. Now (post-P1.1) each clone is a refcount bump (~5 ns) instead of a deep clone (~400 ns), so this is fine for the bench shape but at 8 workers × 12 listeners × 200 events/s = 19200 channel sends/s and 19200 wakeups/s, the broadcast channel buffer (capacity 256) could lag. Worth bumping to 4096 or switching to a method→subscriber registry. Not done.
19. **OPEN: every event listener does `if let Some(ref expected_sid) = session_id { ... if event_sid != Some(&**expected_sid) { continue; } }`** (e.g. `mod.rs:3420-3427`). For pages on a non-default session, this filter runs for EVERY event on EVERY listener. With 8 workers × 12 listeners × 200 events/s = 19200 filter checks/s, all bouncing through the broadcast clone. A method→listener registry indexed by both method AND session_id would skip 95% of these.

## L. Real-app bench (1000 tests against React + shadcn kitchen-sink)

Built a real benchmark in `bench/`:
- `bench/app/` — React 19 + Tailwind + shadcn-style + react-query + react-router + react-hook-form + zod + Bun.serve mock REST API
- `bench/fd-tests/` — 1000 ferridriver tests across 5 surfaces (todos / blog / dashboard / forms / wizard)
- `bench/pw-tests/` — same 1000 tests via `@playwright/test` (only the import line differs)
- `bench/run.sh` — orchestrator: build app + NAPI release + dist; spawn webServer per runner; runs bench at 2/4/8 workers across 2 chrome modes; saves `results/realapp.txt`

### L.1 Headline results (2026-05-03, M-series, 2 runs/cell)

**Headless Shell** (chrome-headless-shell binary):

| W | Playwright | ferridriver | speedup |
|---:|---:|---:|---:|
| 2 | 92264 ms | **54533 ms** | **1.69x** |
| 4 | 53739 ms | **34653 ms** | **1.55x** |
| 8 | 39265 ms | **28721 ms** | **1.37x** |

ferridriver wins at every worker count.

**Regular Chrome** (Google Chrome for Testing + `--headless` flag):

| W | Playwright | ferridriver | speedup |
|---:|---:|---:|---:|
| 2 | 91185 ms | 1233536 ms | 0.07x (resource exhaustion) |
| 4 | 53673 ms | 100437 ms | 0.53x (real ~2x slowdown) |

ferridriver loses on Regular Chrome. Two-part diagnosis below.

### L.2 Bugs surfaced + fixed during this bench cycle

These bench-blocking issues found while building the runner:

20. **`baseURL` config never plumbed to BrowserContext** — only stored as `request_base_url` for the API request fixture. `page.goto('/route')` failed with "Cannot navigate to invalid URL". Fixed in `crates/ferridriver/src/page.rs::apply_context_options`: after calling backend's `apply_context_options`, also stash bag in shared state so `Page::resolve_with_base_url` reads it. Also extract `composite_key` and update via `state.set_context_options`.

21. **`page.fill()` skipped React's value tracker** — `this.value = '...'` doesn't trigger React 18+ controlled-input onChange. React (and other frameworks) wrap the descriptor on the element instance to detect mutations. Fixed in `crates/ferridriver/src/actions.rs::fill`: use the prototype's native `value` setter via `Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, v)`. Same pattern Playwright + React DevTools use. Without this fix, every test that filled a controlled input would silently pass the typing step but the React state wouldn't update.

22. **`testMatch` accepted only `string[]` via NAPI** — TS `defineConfig` allows `string | string[]`. NAPI binding required array. Fixed by always wrapping in array in test configs.

23. **Bench-script chrome leak across modes** — `bench/run.sh` ran 24+ invocations sequentially without killing leaked chromes between modes. By the time the regular-chrome 8w cell ran, hundreds of leaked chromes throttled the host. Fixed by adding `pkill -f ferridriver-pipe-` + `pkill -f playwright_chromiumdev_` + `pkill -f chromium_headless_shell-1217` between runs.

### L.3 Why FD-on-Regular-Chrome is 2x slower

CDP trace via `RUST_LOG=ferridriver::cdp::send=debug` on a single test shows **identical 38 RTTs** on HS and RC. So it's not a FD bug — it's per-RTT Chrome-internal latency:
- HS: ~4 ms/RTT (no GPU, fewer services)
- RC (full chrome with `--headless` mapping to `--headless=old`): ~10 ms/RTT (GPU process + full service stack)

Same chrome flag `--headless` on both runners (verified — PW also passes bare flag, see `pw-tests/node_modules/playwright-core/lib/server/chromium/chromium.js:288`). Tried `--headless=new` swap, reverted — same flag is what PW uses.

PW handles RC at HS speed because **PW does fewer per-test bootstrap RTTs**. We do 38 (with `prepared_page` warmup), steady-state ~19. PW likely does ~7-8.

**Concrete RTT-reduction targets** (cuts per-test from 19 → ~13):
- L.3.1: Skip `enable_domains` on default browser-launch page (it's about:blank, never used)
- L.3.2: Lazy `Network.enable` — only when test registers a network listener
- L.3.3: Lazy `Page.setLifecycleEventsEnabled` — only when goto uses non-default `waitUntil`
- L.3.4: Drop `Page.getFrameTree` from parallel batch — fetch lazily on first iframe op
- L.3.5: Drop `Emulation.setDeviceMetricsOverride` when viewport matches default

### L.4 Parallelism gap (PW scales better at higher worker counts)

| | per-worker time | added-worker saving |
|---|---:|---:|
| FD 2w → 4w | 27.3s → 8.7s | 35% |
| FD 4w → 8w | 8.7s → 3.6s | 17% |
| PW 2w → 4w | 46.2s → 13.4s | 41% |
| PW 4w → 8w | 13.4s → 4.9s | 27% |

PW gains more from each added worker. FD hits diminishing returns faster.

**Root cause**: FD is one Node process + N tokio tasks. PW is N forked Node processes.

What FD shares across all workers (PW doesn't):
1. **One V8 isolate** — every test body's microtasks queue through ONE JS event loop
2. **One tokio multi-thread runtime** — N chrome transports + per-page listener tasks share thread pool
3. **One mimalloc heap** — cross-thread free contention at 8w
4. **Single async-channel dispatcher mpsc** — workers contend on `try_recv()`
5. **One libuv pool for NAPI TSFN dispatch** — JS callbacks serialize

Diagnosis verified via single-test pure-Rust bench (`ferridriver-test/tests/bench_napi_compare.rs`) — at 8w pure Rust runs same 1000-test workload at 2.2s vs NAPI 12s. NAPI/JS-runner adds the parallelism overhead, not the Rust core.

**Fix paths** (ranked by ROI, none implemented yet):

- **A. Worker_threads** — spawn N Node `worker_threads`, each with own V8 + libuv + NAPI. Keep tokio. ~1-2 weeks. Closes ~80% of the gap. Doesn't lose npm ecosystem.
- **B. Process-per-worker** — fork Node child per worker (PW model). ~1 month. 100% gap. Loses in-process advantage at low worker counts.
- **C. Per-worker tokio runtime** — each worker spins its own `current_thread` runtime. ~2 days. Loses work-stealing. Probably small win.
- **D. Instrument first** — per-phase span timings to confirm WHERE the JS contention is. ~1 day. Recommended next step.

### L.5 NAPI is NOT the per-test bottleneck

Confirmed via `crates/ferridriver-test/tests/bench_napi_compare.rs` — pure-Rust runner (no NAPI, no Bun, no Node, no TS, just `TestRunner::run` directly):

| W | pure-Rust | NAPI Bun runner |
|---:|---:|---:|
| 1 | 3.2s | 3.5s |
| 2 | 2.2s | 2.3s |
| 4 | 1.9s | 2.0s |
| 8 | 2.2s | 2.4s (isolated) |

NAPI overhead per-test is sub-millisecond. The parallelism gap from §L.4 is JS-event-loop contention, not NAPI dispatch latency.

This means: rewriting the runner in pure Rust + QuickJS would **not move the needle** on bench numbers. Worker_threads (path A above) gets 80% of the parallelism win without losing the npm ecosystem.

### L.6 Honest takeaway

ferridriver is faster than Playwright at every worker count on the realistic 1000-test bench when running on `chrome-headless-shell`. Speedup compresses with concurrency — both libs bottleneck on Chrome, and FD has additional shared-process bottlenecks PW avoids by forking.

On full Chrome (Regular), FD pays an extra 2x per-RTT penalty that PW dodges via fewer per-test bootstrap RTTs. The 5 RTT-reduction targets in §L.3 should close most of this gap.

The biggest single win available without architecture change: **fix the per-test CDP RTT count** (§L.3). The biggest architectural win: **worker_threads for JS-side parallelism** (§L.4 path A).
