# [DONE] Feature: Verbose Protocol Logging

## Context
When tests fail in unexpected ways, developers need to see what's happening under the hood: CDP protocol messages, step matching decisions, hook execution, fixture lifecycle. The codebase already uses `tracing` — this feature is about exposing it properly through env vars and CLI flags with useful category-based filtering.

## Design

### Architecture
| Crate/Package | Role |
|---|---|
| `ferridriver` | CDP protocol logging (already has `tracing` calls) |
| `ferridriver-test` | Worker, fixture, hook lifecycle logging |
| `ferridriver-bdd` | Step matching, hook execution logging |
| `ferridriver-cli` | `--verbose` flag, `FERRIDRIVER_DEBUG` env var |
| `packages/ferridriver-test` | Forward `--verbose` to Rust |

### Core Changes (ferridriver)
- Ensure all CDP send/receive in backends are instrumented with `tracing::debug!`:
  - `target = "ferridriver::cdp"` for raw protocol messages.
  - `target = "ferridriver::cdp::send"` for outgoing commands.
  - `target = "ferridriver::cdp::recv"` for incoming events/responses.
  - Truncate large payloads (screenshots, DOM snapshots) to first 200 chars in logs.
- Page-level action logging: `target = "ferridriver::action"` for click, fill, navigate, etc.

### Core Changes (ferridriver-test)
- Add tracing instrumentation where missing:
  - `target = "ferridriver::worker"` — test dispatch, retry decisions.
  - `target = "ferridriver::fixture"` — fixture creation, teardown, DAG resolution.
  - `target = "ferridriver::reporter"` — event emission.
  - `target = "ferridriver::runner"` — plan filtering, shard assignment.

### BDD Integration (ferridriver-bdd)
- `target = "ferridriver::bdd::step"` — step matching: which regex matched, which was tried.
- `target = "ferridriver::bdd::hook"` — Before/After hook execution.
- `target = "ferridriver::bdd::world"` — World state changes.

### CLI (ferridriver-cli)
- `--verbose` / `-v` flag: sets `RUST_LOG=ferridriver=debug`.
- `-vv` flag: sets `RUST_LOG=ferridriver=trace` (includes CDP protocol messages).
- `FERRIDRIVER_DEBUG` env var: custom filter syntax mapped to `tracing` targets.
  - `FERRIDRIVER_DEBUG=*` -> `ferridriver=trace` (everything).
  - `FERRIDRIVER_DEBUG=cdp` -> `ferridriver::cdp=trace`.
  - `FERRIDRIVER_DEBUG=cdp,steps` -> `ferridriver::cdp=trace,ferridriver::bdd::step=trace`.
  - `FERRIDRIVER_DEBUG=worker,fixture` -> specific targets at trace level.
- Category mapping:
  | Debug value | Tracing target |
  |---|---|
  | `cdp` | `ferridriver::cdp` |
  | `steps` / `step` | `ferridriver::bdd::step` |
  | `hooks` | `ferridriver::bdd::hook` |
  | `worker` | `ferridriver::worker` |
  | `fixture` | `ferridriver::fixture` |
  | `reporter` | `ferridriver::reporter` |
  | `action` | `ferridriver::action` |
- Tracing subscriber setup in CLI `main.rs`:
  - Use `tracing-subscriber` with `EnvFilter`.
  - Format: `[target] message` with colors for terminal, plain for CI.
  - Timestamps only at trace level.

### NAPI + TypeScript (ferridriver-node, packages/ferridriver-test)
- `FERRIDRIVER_DEBUG` env var works the same (read by Rust code).
- `--verbose` flag on TS CLI forwarded to Rust process.

### Component Testing (ferridriver-ct-*)
- No CT-specific changes. Same debug categories apply.

## Implementation Steps
1. Audit existing `tracing` calls in `ferridriver` — ensure CDP backends log send/recv.
2. Add `tracing` instrumentation to `ferridriver-test` worker, fixture, runner.
3. Add `tracing` instrumentation to `ferridriver-bdd` step matching and hooks.
4. Implement `FERRIDRIVER_DEBUG` env var parser in CLI — maps to `EnvFilter` directives.
5. Add `--verbose` / `-v` / `-vv` flags to CLI.
6. Configure `tracing-subscriber` in `main.rs` with appropriate format.
7. Add payload truncation for large CDP messages.
8. Document debug categories in `--help` output.

## Key Files
| File | Action |
|---|---|
| `crates/ferridriver-cli/src/main.rs` | Modify — tracing subscriber setup |
| `crates/ferridriver-cli/src/cli.rs` | Modify — `--verbose` flag |
| `crates/ferridriver/src/backend/cdp_pipe/mod.rs` | Verify/modify — CDP logging |
| `crates/ferridriver/src/backend/cdp_raw/mod.rs` | Verify/modify — CDP logging |
| `crates/ferridriver-test/src/worker.rs` | Modify — add tracing |
| `crates/ferridriver-test/src/fixture.rs` | Modify — add tracing |
| `crates/ferridriver-bdd/src/step.rs` | Modify — add step matching tracing |

## Verification
- Manual: `FERRIDRIVER_DEBUG=cdp ferridriver test` shows CDP messages.
- Manual: `FERRIDRIVER_DEBUG=steps ferridriver bdd` shows step matching decisions.
- Manual: `ferridriver test -vv` shows all trace-level output.
- Verify large CDP payloads are truncated in logs.
- Verify no debug output when `--verbose` is not set and env var is absent.
