# Handover — next Playwright-parity session

Read-first for any session continuing work. Overwrite this file with a
fresh summary at the end of each block.

## Cross-device setup

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — gap tracker. Tier 1 done. Tier 2.x and 4.x
   incremental wins through §2.15 BrowserType. Tier 7 (test runner)
   shipped: §7.2 / §7.5 / §7.6 / §7.8 / §7.9 / §7.18 / §7.19 /
   §7.27 / §7.28.
3. This file — block summary below.
4. `docs/NEXT_SESSION.md` — next-cluster brief + prompt.

`git clone https://github.com/microsoft/playwright /tmp/playwright` if missing.

## Landed this session — Cluster 1 + Cluster 2

### Cluster 1 — CLI flag surfacing (§7.2/§7.5/§7.6/§7.8/§7.9/§7.27/§7.28)

Single commit. Mechanical surface plus a real fix for `--max-failures`
(workers used to drain the buffered queue past the threshold).

New top-level config fields (Rust + NAPI + TS, names match Playwright):
`global_timeout`, `ignore_snapshots`, `pass_with_no_tests`, `tsconfig`,
`name`. `CliOverrides` mirror plus `max_failures`, `repeat_each`,
`fail_fast`, `fully_parallel`, `update_snapshots`. `parse_common_cli_args`
recognises `--max-failures`, `--repeat-each`, `--global-timeout`, `-x`,
`--pass-with-no-tests`, `--ignore-snapshots`, `--tsconfig`,
`--fully-parallel`, and `-u [all|changed|missing|none]`.

Runtime effects:

- `global_timeout` enforced via `tokio::time::timeout` inside
  `runner::TestRunner::run`.
- `ignore_snapshots` propagates to `model::TestInfo::ignore_snapshots`
  and short-circuits the text path of `crate::snapshot::assert_snapshot`.
  The screenshot path lands with §7.17.
- `Dispatcher` gained a `stopped: Arc<AtomicBool>` flag; the worker
  loop checks it after `recv()`, breaks before processing dropped
  items, and yields via `tokio::task::yield_now()` after each
  result-send so the runner trips the flag before the next pull.
- `pass_with_no_tests` gates both no-test exit paths in `cli.ts` —
  default exit is now 1 unless the flag is set.
- `tsconfig` rebuilds the jiti loader under Node; under Bun the
  loader prints a one-time warning since Bun reads its own
  `tsconfig.json` and lacks a programmatic override.

Rule 9 in `crates/ferridriver-node/test/cli-flags.test.ts` (11 cases).

### Cluster 2 — Built-in fixtures + auto enforcement (§7.18 / §7.19)

Single commit. Adds `browserVersion`, `playwright`, and `auto: true`
enforcement; reaffirms the existing `browserName` and `request`
fixtures.

#### NAPI surface

`crates/ferridriver-node/src/playwright_namespace.rs` (new):

```ts
class PlaywrightNamespace {
  get chromium(): BrowserType;     // ferridriver::BrowserType::chromium_with()
  get firefox(): BrowserType;      // ferridriver::BrowserType::firefox()
  get webkit(): BrowserType;       // ferridriver::BrowserType::webkit()
  get request(): PlaywrightRequest;
}
class PlaywrightRequest {
  newContext(options?): Promise<APIRequestContext>;
}
```

`TestFixtures` gained two getters:

- `browserVersion: string | null` — reads `Browser::version()` from
  the cached pool entry. `null` when the test opts out of `browser`.
- `playwright: PlaywrightNamespace` — static accessor.

#### Rust core

`fixture::FixtureDef` gained `auto: bool`. New helper
`FixturePool::auto_fixture_names_for(scope)` walks the full def
graph (including parent pools) and returns every auto-marked entry
whose scope matches or is narrower than the argument. The worker
calls it once per suite pool (Worker scope) before any `beforeAll`
runs, and once per test pool (Test scope) before `beforeEach` and
the body, resolving each via `pool.resolve(name)`.

No built-in is `auto: true` today, but the infrastructure unblocks
trace recorders / artifact hooks / `_setupArtifacts`-style fixtures
in future clusters.

#### TS

`packages/ferridriver-test/src/test.ts::FIXTURE_NAME_MAP` was
upgraded from `Record<string, string>` to `Record<string, string[]>`
so the inference can map a single destructured name to the union of
pool fixtures it implies. New mappings:

- `browserName` → `[]` (BrowserConfig is always available)
- `browserVersion` → `["browser"]` (needs the launched Browser)
- `playwright` → `[]` (static namespace)

#### Tests (Rule 9)

`crates/ferridriver-node/test/builtin-fixtures.test.ts`:

- `browserName + browserVersion` resolve on `cdp-pipe`, `cdp-raw`,
  `bidi` — full launch path. `browserVersion` is asserted non-empty
  and not the literal `"Unknown"` placeholder.
- `browserName` on `webkit` via the request-only path (the test
  runner's per-test `browser.new_context(None)` is rejected by
  webkit; tracked as a separate gap).
- `playwright` namespace exposes three `BrowserType` instances and a
  `PlaywrightRequest` whose `newContext()` returns a usable
  `APIRequestContext`. `BrowserType.name()` echoes
  `chromium`/`firefox`/`webkit`.
- `request` fixture is a usable `APIRequestContext` (`get` method
  present).

`crates/ferridriver-test/tests/new_features_e2e.rs::test_auto_fixture_runs_without_explicit_request`
asserts the auto walker returns auto-marked fixtures and skips
auto:false ones.

#### Webkit gap (carried forward)

WebKit's backend rejects `new_context` calls — only the persistent
default context is exposed. The test runner's worker spawns a fresh
context per test (`browser.new_context(None)`), which fires
`backend.new_context` on the first `new_page()` and fails. The MCP
path (used by the `crates/ferridriver-cli/tests/backends.rs`
integration matrix) sidesteps this by going through the persistent
default context, so the matrix stays green. Tests that need the
test-runner's `page` fixture on webkit are blocked until the worker
learns to reuse `Browser::default_context()` when the backend can't
support multiple contexts.

### Baseline (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings            # clean
cargo test -p ferridriver --lib                                   # 125 pass
cargo test -p ferridriver-test --lib                              # 11 pass
cargo test -p ferridriver-script --lib                            # 13 pass
cargo test -p ferridriver-mcp --lib                               # 38 pass
cargo test -p ferridriver-test --test new_features_e2e            # 15 pass (was 14)
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                                        # 889 pass (was 883)
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 175 / cdp-raw 175 / bidi 170 / webkit 171
```

### Cluster 3 — TestInfo helpers (§7.10)

Single commit. Adds the missing read-only and read/write fields on
`TestInfo` plus reaffirms the existing variadic `outputPath` /
`snapshotPath` helpers.

#### Rust core changes

`model::TestInfo` gained:

- `errors: Arc<Mutex<Vec<TestFailure>>>` — primary errors pushed by
  the worker after the body returns; afterEach hooks read the full
  list. Composes with the existing `soft_errors`.
- `snapshot_suffix: Arc<Mutex<String>>` — read/write suffix.
- `column: Option<u32>` — placeholder for the discovery layer's
  column position. Always `None` today; surfaced for parity.
- `project: Option<ProjectConfig>` — per-project clone, `None` for
  single-project runs.
- `config_snapshot: Option<Arc<TestConfig>>` — cloned config so the
  `testInfo.config` accessor stays cheap.

The worker constructs both the per-suite and per-test `TestInfo`
with `config_snapshot: Some(Arc::clone(&self.config))` so the
accessor surfaces the live config for any test that asks.

#### NAPI surface

`crates/ferridriver-node/src/test_info.rs` gained accessors:

| accessor | TS type | source |
|---|---|---|
| `column` | `number` | `inner.column` |
| `project` | `Record<string, unknown> \| null` | serialised `ProjectConfig` |
| `config` | `Record<string, unknown> \| null` | serialised `TestConfig` |
| `errors` | `Array<{ message; stack? }>` | soft + primary errors |
| `error` | `{ message; stack? } \| null` | first of `errors` |
| `snapshotSuffix` (get/set) | `string` | `inner.snapshot_suffix` |
| `fn` | `string` | test title (JS Function placeholder) |

#### Why no `pause()`

Playwright's `TestInfo` interface has no `pause()` method
(`page.pause()` is the related API). Cluster prompt suggested it,
but adding it would diverge from Playwright. Real pause-on-debug
integration belongs to the `--ui` mode work (§7.7) and is omitted
here.

#### Tests (Rule 9)

`crates/ferridriver-node/test/test-info.test.ts` — 9 NAPI cases
exercising `outputPath` (variadic + nested), `snapshotPath`,
`errors` (empty when no soft errors pushed), `error` (null when
empty), `snapshotSuffix` (default empty + read-after-write),
`config` (surfaces `name` and structural fields), `project` (null
for single-project), `fn` (test title), and `column` (defaults to
0).

### Baseline (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings            # clean
cargo test -p ferridriver --lib                                   # 125 pass
cargo test -p ferridriver-test --lib                              # 11 pass
cargo test -p ferridriver-script --lib                            # 13 pass
cargo test -p ferridriver-mcp --lib                               # 38 pass
cargo test -p ferridriver-test --test new_features_e2e            # 15 pass
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                                        # 898 pass (+9)
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 175 / cdp-raw 175 / bidi 170 / webkit 171
```

### Cluster 4 — Matcher core (§7.11 – §7.16)

Single commit. `ValueAssertions` class in
`packages/ferridriver-test/src/expect.ts` ships the full Jest
generic-matcher set, asymmetric matchers, `.resolves` / `.rejects`,
`.soft`, `.poll`, `expect.extend`, and `APIResponse.toBeOK`.

Deviation from the original prompt: generic matcher logic lives TS-
side rather than in Rust core. Reason — Playwright itself routes
the generics through Jest's `expect` library (pure-value
comparison, no protocol surface). The Rust-side `Matcher` trait
still owns the polling matchers (`toBeVisible`, etc.) so the
"Rust is the source of truth" rule applies where it actually
matters.

Surface highlights:
- 22 generic matchers (`toBe`, `toEqual`, `toMatchObject`,
  `toThrow`, `toHaveProperty(path[, value])`, `toBeCloseTo`, …) plus
  `toPass(options?)` for the function-subject retry form.
- Asymmetric matchers as serde-tagged objects
  (`Symbol.for('ferridriver.asymmetric')`) that the deep-equality
  engine recognises and dispatches to `match()`. Nesting works.
- `.not`, `.resolves`, `.rejects` modifiers as getters returning new
  `ValueAssertions` chains.
- `expect.soft(...)` calls `testInfo.pushSoftError(message)` (new
  NAPI binding) via the existing AsyncLocalStorage-backed
  `_currentTestInfo()`. Outside a test body it silently no-ops.
- `expect.poll(probe, options?)` retries until match or timeout
  (5000ms default).
- `expect.extend({ name: fn })` mutates `ValueAssertions.prototype`
  so custom matchers compose with `.not`.
- `expect(response).toBeOK()` reads `ApiResponse.ok()`.

NAPI: `TestInfo.pushSoftError(message, stack?)` added.
`packages/ferridriver-test/src/test.ts` exports
`_currentTestInfo()` so the expect facade can read the
AsyncLocalStorage-backed test info without re-implementing the
plumbing.

Tests (Rule 9):
- `crates/ferridriver-node/test/value-matchers.test.ts` — 22 cases
  covering all generics + asymmetric matchers + `.resolves` /
  `.rejects` / `.not` / `.soft` (no-op path) / `.poll`. Pos + neg
  per group.
- `expect-soft-runner.test.ts` — soft assertions through the live
  TestRunner: errors[] populates through the NAPI round-trip and
  the test still fails at the end (matches Playwright).
- `expect-extend-toBeOK.test.ts` — custom matchers via
  `expect.extend` compose with `.not`; `toBeOK` against a one-shot
  `Bun.serve` status server (deterministic, no network round-trip).

### Baseline (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings            # clean
cargo test -p ferridriver --lib                                   # 125 pass
cargo test -p ferridriver-test --lib                              # 11 pass
cargo test -p ferridriver-script --lib                            # 13 pass
cargo test -p ferridriver-mcp --lib                               # 38 pass
cargo test -p ferridriver-test --test new_features_e2e            # 15 pass
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                                        # 927 pass (+29)
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 175 / cdp-raw 175 / bidi 170 / webkit 171
```

## Open clusters (in order)

| # | scope | status |
|---|---|---|
| 1 | CLI flag surfacing (§7.2/§7.5/§7.6/§7.8/§7.9/§7.27/§7.28) | DONE |
| 2 | Built-in fixtures + auto enforcement (§7.18 / §7.19) | DONE |
| 3 | TestInfo helpers (§7.10) | DONE |
| 4 | Matcher core: generic + asymmetric + `.resolves`/`.rejects`/`.soft`/`.poll`/`expect.extend`/`toBeOK` (§7.11 – §7.16) | DONE |
| 5 | Locator matcher advanced options (§7.17) | next |
| 6 | Reporters (`dot`, `github`, `blob`, `null`) + `merge-reports` + TS Reporter interface (§7.20 – §7.22) | pending |
| 7 | Project DAG + git-aware filters + WebServer polish + git metadata (§7.1 / §7.3 / §7.4 / §7.25 / §7.26) | pending |

## Carried-forward backend gaps (real protocol limits)

- **WebKit + test runner**: `new_context` not supported — the worker
  must learn to reuse `default_context()` when launching webkit.
  Tracked separately from the regular WebKit network/UI gaps.
- **BiDi**: response body unavailable for non-intercepted responses;
  multi-`Set-Cookie` collapses; `request.postData()` null for
  fetch-with-body; `Download.cancel` typed `Unsupported`; spurious
  page-init `"Permission denied"` cross-origin error; `userAgent`,
  media overrides, geolocation+permissions, `setNetworkConditions`
  shape — Firefox BiDi protocol gaps.
- **WebKit** (stock `WKWebView`): no public API for main-doc
  Response, redirect chain, response body bytes, browser-set request
  headers, `Set-Cookie`, WebSocket frames, dialog intercept,
  download intercept, console args+location, WebError stack frames,
  screencast, multiple browser contexts.

## Key source locations (Cluster 2)

| area | path |
|---|---|
| `PlaywrightNamespace` + `PlaywrightRequest` NAPI classes | `crates/ferridriver-node/src/playwright_namespace.rs` |
| `TestFixtures.browserVersion` / `playwright` getters | `crates/ferridriver-node/src/test_fixtures.rs` |
| `BrowserType::wrap` visibility bump | `crates/ferridriver-node/src/browser_type.rs` |
| `FixtureDef::auto` + `auto_fixture_names_for` | `crates/ferridriver-test/src/fixture.rs` |
| Worker auto-resolve hooks (suite + test) | `crates/ferridriver-test/src/worker.rs` |
| `auto: false` defaults on built-in defs | `crates/ferridriver-test/src/fixture.rs`, `worker.rs` |
| TS `FIXTURE_NAME_MAP` upgrade | `packages/ferridriver-test/src/test.ts` |
| Rule 9 NAPI tests | `crates/ferridriver-node/test/builtin-fixtures.test.ts` |
| Rule 9 Rust pool test | `crates/ferridriver-test/tests/new_features_e2e.rs::test_auto_fixture_runs_without_explicit_request` |
| Compat tracker updates | `PLAYWRIGHT_COMPAT.md` (§7.18 / §7.19) |
