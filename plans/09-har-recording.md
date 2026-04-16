# Feature: HAR Recording and Replay

## Context
HAR (HTTP Archive) recording captures all network traffic during a test, enabling deterministic replay without hitting real servers. This is essential for: (1) creating stable tests against flaky APIs, (2) recording API responses once and replaying forever, (3) debugging network issues by inspecting the HAR file.

## Design

### Architecture
| Crate/Package | Role |
|---|---|
| `ferridriver` | CDP Network event capture -> HAR format, route-from-HAR replay |
| `ferridriver-test` | `recordHar` config, auto-attach HAR files to test artifacts |
| `ferridriver-bdd` | Auto-record HAR per scenario when configured |
| `ferridriver-cli` | `--record-har <path>` flag |
| `packages/ferridriver-test` | `recordHar` config option |

### Core Changes (ferridriver)
- New module `crates/ferridriver/src/har.rs`:
  - `HarRecorder` struct:
    - Subscribes to CDP events: `Network.requestWillBeSent`, `Network.responseReceived`, `Network.loadingFinished`, `Network.loadingFailed`, `Network.requestWillBeSentExtraInfo` (for cookies/headers).
    - Collects `Network.getResponseBody` for each completed request.
    - Builds HAR 1.2 spec JSON (`HarLog`, `HarEntry`, `HarRequest`, `HarResponse`, `HarContent`).
    - URL filter: `Option<Regex>` to record only matching URLs.
  - `HarReplayer`:
    - `page.route_from_har(har_path, options)` — intercept requests and serve from HAR.
    - Match by URL + method. Options: `url_filter`, `update: bool` (re-record if no match).
    - Uses `page.route()` interception under the hood.
  - HAR 1.2 types (serde-serializable):
    ```rust
    pub struct HarLog { pub version: String, pub entries: Vec<HarEntry>, ... }
    pub struct HarEntry { pub request: HarRequest, pub response: HarResponse, pub time: f64, ... }
    ```

- `Page` API additions:
  - `page.route_from_har(path, options)` — set up HAR replay routing.
  - `page.unroute_from_har()` — remove HAR routing.

### Core Changes (ferridriver-test)
- Add to `TestConfig`:
  ```rust
  pub record_har: Option<HarConfig>,
  ```
  ```rust
  pub struct HarConfig {
    pub path: PathBuf,
    pub url_filter: Option<String>,  // regex
    pub mode: HarMode,  // Full | Minimal (omit response bodies)
  }
  ```
- In `Worker`: when `record_har` is set, start `HarRecorder` before test, save HAR after test.
- Attach HAR as test artifact.

### BDD Integration (ferridriver-bdd)
- Config-driven: `record_har` in config applies to all scenarios.
- Per-scenario HAR: `@har(api-responses.har)` tag could replay from a specific HAR file.
- Steps:
  - `Given I replay network from {string}` — sets up HAR routing.
  - `Given I record network to {string}` — starts HAR recording.

### NAPI + TypeScript (ferridriver-node, packages/ferridriver-test)
- Config: `recordHar: { path: 'network.har', urlFilter: /api/ }`.
- API: `page.routeFromHAR('recording.har', { url: /api/ })`.

### CLI (ferridriver-cli)
- `--record-har <path>` — override HAR recording path from CLI.

### Component Testing (ferridriver-ct-*)
- HAR replay is useful for CT: mock API calls that the component makes during rendering.

## Implementation Steps
1. Define HAR 1.2 types in `crates/ferridriver/src/har.rs` with serde.
2. Implement `HarRecorder` — CDP Network event subscription + entry building.
3. Implement response body capture via `Network.getResponseBody`.
4. Implement `HarReplayer` — URL matching + `page.route()` interception.
5. Add `page.route_from_har()` to Page API.
6. Add `HarConfig` to `TestConfig`.
7. Integrate HAR lifecycle into `Worker`.
8. Add BDD steps for HAR replay.
9. Add `--record-har` flag to CLI.
10. Test with real network traffic.

## Key Files
| File | Action |
|---|---|
| `crates/ferridriver/src/har.rs` | Create |
| `crates/ferridriver/src/page.rs` | Modify — add `route_from_har()` |
| `crates/ferridriver-test/src/config.rs` | Modify — add `record_har` config |
| `crates/ferridriver-test/src/worker.rs` | Modify — HAR lifecycle |
| `crates/ferridriver-bdd/src/steps/` | Modify — HAR steps |
| `crates/ferridriver-cli/src/cli.rs` | Modify — `--record-har` flag |

## Verification
- Unit test: `HarRecorder` produces valid HAR 1.2 JSON for mock CDP events.
- Integration test: navigate to a page, record HAR, verify entries contain request/response pairs.
- Integration test: replay from HAR — page loads with mocked responses, no real network calls.
- Verify HAR file opens in Chrome DevTools HAR viewer or `har-viewer.com`.
- Round-trip test: record -> replay -> compare page content is identical.
