# Salvaged audit — session bd270192


---
## USER
start a workflow to audit and fix all performance issues and playwright compatiblity in the core api and the binidings and ensure at least 5x performace improvements over playwright with core playwright api 100% parity using ideomatic rust and high perfomance and effcientcy concurenet and parrellal test runner and scripting


### assistant
Big mandate. Workflow opt-in confirmed. But "fix all + ensure 5x + 100% parity" = months work, not one shot. Honest plan: workflow audits comprehensively + applies high-confidence fixes, produces prioritized report for rest. Quick recon first, then launch.


### assistant
/tmp/playwright missing — parity audit needs it. Clone canonical `microsoft/playwright` (shallow) in background while I build the workflow.


### assistant
Recon enough. Core: page 2873L, locator 2266L, cdp/mod 5951L. 3 binding layers present. Note: uncommitted `binding_convert_bench.rs` exists — script conversion already under scrutiny. Wait for clone before launching (agents read it).


### assistant
Clone ready (50 client files). Launching comprehensive audit workflow: map → parity audit → perf audit → concurrency/scripting audit → adversarial verify → prioritized synthesis. Audit-only (no parallel edits — green-tree + no-conflict rules); I drive fixes after, serialized + verified.


### assistant
Workflow `wvv206xby` running in background. 5 phases: Map → Parity (audit+verify) → Perf (audit+verify) → adversarial Verify → Synthesize roadmap. Audit-only — applies no edits.

Scope honesty: "fix all + 5x + 100% parity" too big for one shot. This workflow finds + verifies every gap, ranks fixes by value/effort. Then I drive fixes serialized (parallel edits on same crate conflict + risk red tree). Watch live: `/workflows`. I'll report when it lands.


### assistant
Audit done. **72 agents, 5.5M tokens, 45 verified findings** (41 parity, 4 perf). Full roadmap = 18 ranked fixes.

## Honest verdict

**5x over Playwright: NOT provable today.**
- Only real E2E bench (`benchmark.ts`, 19 ops) = **2.14x median / 3.24x mean**. Screenshots at parity, network nav **35% SLOWER** (0.74x).
- Headline `click() 14.42x` is **confounded** — Playwright ran with `force:true` (skips actionability). Can't cite.
- Runner bench compares vs **hardcoded `~2200ms` self-reported** PW number (`bench_runner.rs:196`) — no real same-hardware PW run. Invalid.
- `results/benchmark.csv` header-only. No `BENCHMARKING.md`.
- Honest current claim: **~2-3x typical ops**. The parallelism story (per-worker browser reuse) is the real differentiator, not raw per-op.

**Parity: ~85-90% method-level, NOT 100%.** Gap clusters:
- **ElementHandle** worst — `click/dblclick/hover/type` drop all option bags in all 3 layers; no `$`/`$$`.
- **Locator** — missing `drop`, `highlight`/`hideHighlight`, `normalize`; `screenshot.mask` typed `String[]` not `Locator[]` (wire-shape leak, Rule 3 violation).
- **BrowserContext** — `exposeBinding`/`exposeFunction`, `setHTTPCredentials`, `isClosed`, `browser()`.
- **Disposable** contract absent entirely (blocks #7/#8 return types).
- QuickJS systematically trails NAPI (Frame `goto`/`evaluate`/`title`, Page `waitForNavigation`/event listeners).

## Top fixes (full 18 ranked)
| # | sev | eff/risk | what |
|---|---|---|---|
| 1 | perf | S/low | static empty `Value` — kill 5x `.clone()` per page-enable (`cdp/mod.rs`) |
| 2 | perf | S/low | drop ~20 `ctx.clone()` per evaluate return (`convert.rs`) |
| 3 | conc | S/low | stop holding sync Mutex across alloc in lifecycle tracker (`transport.rs`) |
| 4 | parity | M/low | ElementHandle action option bags ×3 layers |
| 5 | parity | S/low | ElementHandle `$`/`$$` |
| 6-7 | parity P0 | M/med | Locator `drop`, `highlight`/`hideHighlight` |
| 8-9 | parity P0 | L/med | ctx `exposeBinding`/`exposeFunction` + Disposable contract |
| 10 | parity | M/med | `screenshot.mask` Locator[] (wire-shape fix) |
| 11 | conc | M/med | parallelize independent test projects (5×30s: 150s→30s) |
| 12-17 | parity | S-M | normalize, pickLocator/unrouteAll, waitForSelector return, accessor batch, ctx credentials/isClosed, namedKeys |
| 18 | bench | M/low | real PW baseline + BENCHMARKING.md before any speed claim |

Now decide what I execute — serialized, each verified green before commit (no parallel edits, conflict + red-tree risk).


---
## USER
CONTINUE


---
## USER
Continue from where you left off.


---
## USER
hello


---
## USER
hello


---
## USER
hello


---
## USER
Continue from where you left off.


---
## USER
hello


---
## USER
Continue from where you left off.


---
## USER
continue


---
## USER
continue
