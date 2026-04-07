# [DONE] Feature: Trace Recording + Viewer

## Context
Playwright's trace viewer is its killer debugging feature. It records DOM snapshots, network requests, console logs, and screenshots at each action, packaged into a ZIP file. Developers can step through the trace like a debugger, seeing exactly what the page looked like at each point. This is the single most requested debugging tool for browser test frameworks.

## Design

### Architecture
| Crate/Package | Role |
|---|---|
| `ferridriver` | CDP tracing hooks, DOM snapshot capture, network/console event collection |
| `ferridriver-test` | Trace lifecycle (start/stop per test), config, ZIP packaging |
| `ferridriver-bdd` | Auto-trace per scenario |
| `ferridriver-cli` | `--trace` flag, `ferridriver show-trace` command |
| `packages/ferridriver-test` | `trace` config option |

### Core Changes (ferridriver)
- New module `crates/ferridriver/src/trace.rs`:
  - `TraceRecorder` struct: subscribes to CDP events and captures data per action.
  - Per action (click, fill, navigate, etc.), record:
    - **Before-screenshot**: PNG of page state before action.
    - **After-screenshot**: PNG of page state after action.
    - **DOM snapshot**: `DOMSnapshot.captureSnapshot` (full DOM + computed styles).
    - **Action metadata**: selector, value, timestamp, duration.
  - Continuous collection:
    - **Network events**: `Network.requestWillBeSent`, `Network.responseReceived` -> HAR-like entries.
    - **Console messages**: `Runtime.consoleAPICalled` -> structured log entries.
    - **Page errors**: `Runtime.exceptionThrown`.
  - `stop() -> TraceData`: returns all collected data.

### Core Changes (ferridriver-test)
- Add to `TestConfig`:
  ```rust
  pub trace: TraceMode,  // Off | On | OnFirstRetry | RetainOnFailure
  ```
- `TraceMode` enum: `Off`, `On`, `OnFirstRetry`, `RetainOnFailure`.
- In `Worker::run_test()`:
  - Before test: if trace is active for this run, create `TraceRecorder`.
  - Inject recorder into the page so every action triggers snapshot capture.
  - After test: package `TraceData` into a ZIP file.
  - `OnFirstRetry`: only record on retry >= 1.
  - `RetainOnFailure`: delete ZIP if test passed.
- ZIP format (compatible with potential future web viewer):
  ```
  trace.zip/
    metadata.json        # test name, timestamps, actions list
    actions/
      0000-navigate.json # action metadata
      0000-before.png    # screenshot before
      0000-after.png     # screenshot after
      0000-snapshot.json # DOM snapshot
    network.json         # all network entries
    console.json         # all console messages
  ```

### BDD Integration (ferridriver-bdd)
- Each BDD step becomes a trace action with before/after snapshots.
- Step name used as action label in the trace.

### CLI (ferridriver-cli)
- `--trace <mode>` flag on `test` and `bdd` commands.
- New `show-trace` subcommand: `ferridriver show-trace trace.zip`
  - Serves a local HTML viewer (embedded static assets) that loads the ZIP.
  - Opens browser to `localhost:PORT`.

### Component Testing (ferridriver-ct-*)
- Works identically since CT uses real browser + CDP.

## Implementation Steps
1. Create `crates/ferridriver/src/trace.rs` with `TraceRecorder`.
2. Implement CDP `DOMSnapshot.captureSnapshot` in page.rs.
3. Hook `TraceRecorder` into page actions (navigate, click, fill, etc.) via an action middleware pattern.
4. Add `TraceMode` to `ferridriver-test/src/config.rs`.
5. Integrate trace lifecycle into `Worker::run_test()`.
6. Implement ZIP packaging using `zip` crate.
7. Build `show-trace` CLI command with embedded HTML viewer.
8. Add `--trace` flag to CLI.
9. Update HTML reporter to link trace ZIP files.
10. Test with real browser actions.

## Key Files
| File | Action |
|---|---|
| `crates/ferridriver/src/trace.rs` | Create |
| `crates/ferridriver/src/page.rs` | Modify — action hooks for trace capture |
| `crates/ferridriver-test/src/config.rs` | Modify — add trace config |
| `crates/ferridriver-test/src/worker.rs` | Modify — trace lifecycle |
| `crates/ferridriver-cli/src/cli.rs` | Modify — `--trace` flag + `show-trace` command |

## Verification
- Unit test: `TraceRecorder` captures snapshots for a sequence of mock actions.
- Integration test: run test with `trace: 'on'`, verify ZIP contains expected structure.
- Integration test: `on-first-retry` — no trace on first attempt, trace on retry.
- Manual: `ferridriver show-trace trace.zip` — opens viewer, step through actions.
- Verify DOM snapshots contain full element tree + computed styles.
