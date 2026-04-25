# Next session — Cluster 2 (built-in fixtures + auto enforcement)

Cluster 1 (CLI flag surfacing) is shipped. Cluster 2 is the next-lowest-
risk piece in the Tier 7 push: register the four first-class Playwright
fixtures (`browserName`, `browserVersion`, `playwright`, `request`) in
the Rust core fixture pool, and make the `auto: true` annotation that
the TS layer parses actually do its job in the resolver.

## Why Cluster 2 next

The `browserName` fixture in particular keeps cropping up — a lot of
Playwright fixture conventions (`test.skip(({ browserName }) => ...)`,
conditional `use` blocks) read it directly. Today a test that requests
`browserName` gets a "fixture not found" error from the pool because
the standard fixture set in `crates/ferridriver-test/src/fixture.rs`
only registers `browser`, `context`, `page`, `test_info`, `request`
(and `request` itself isn't auto-resolved as a top-level alias to
`APIRequestContext` the way Playwright wires it). `auto: true` fixtures
that the TS macro records get stripped at the boundary instead of
resolving regardless of the test's requested set.

## Read-first

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — §7.18 / §7.19 entries.
3. `HANDOVER.md` — Cluster 1 recap (CLI flag surface).
4. `/tmp/playwright/packages/playwright/src/index.ts` — the canonical
   built-in fixtures table (the part that registers
   `browserName`, `browserVersion`, `playwright`, `request`,
   `_combinedContextOptions`, etc. — search for `coreTestFixtures`).
5. `/tmp/playwright/packages/playwright/types/test.d.ts` — see the
   `PlaywrightTestArgs` / `PlaywrightWorkerArgs` interfaces for the
   exact field shapes (`browserName: 'chromium' | 'firefox' | 'webkit'`,
   `browserVersion: string`, `playwright: typeof import('playwright-core')`,
   `request: APIRequestContext`).
6. `crates/ferridriver-test/src/fixture.rs` — current pool / scope /
   `builtin_fixtures` registry. Look at how `browser` / `context` /
   `page` are registered today.
7. `crates/ferridriver-test/src/worker.rs::request_fixture_set` —
   the standard fixture list `STANDARD_FIXTURE_NAMES` is shadowed in
   `crates/ferridriver-node/src/test_runner.rs` (search the file for
   `STANDARD_FIXTURE_NAMES`); both must include the new auto-resolved
   names.
8. `crates/ferridriver-node/src/test_fixtures.rs` — how the TS-side
   `TestFixtures` struct exposes fields. The new fixtures need:
   - `browserName` and `browserVersion` as plain strings.
   - `playwright` reference (likely the `BrowserType` factory namespace
     `{ chromium, firefox, webkit }`).
   - `request` as a real `APIRequestContext` (already in NAPI as
     `ApiRequestContext` — verify the binding is exposed).

## Cluster scope

### §7.18 — first-class fixtures

Register these in `crate::fixture::builtin_fixtures` (or wherever the
worker pool seeds defaults):

| name | type | source |
|---|---|---|
| `browser_name` / `browserName` | `String` | `BrowserConfig.browser` |
| `browser_version` / `browserVersion` | `String` | `Browser::version()` after launch |
| `playwright` | factory namespace | NAPI: `{ chromium, firefox, webkit }` getters; QuickJS: same exposed via `install_browser_type` |
| `request` | `APIRequestContext` | already there as `request` — verify it resolves and that NAPI exposes the right binding |

Both Rust core and NAPI fixture sets need updating. QuickJS exposure
is opt-in (BDD steps don't usually request these), but the NAPI side
must resolve them when listed in `requestedFixtures`.

### §7.19 — `auto: true` enforcement

TS-side annotation already parses (`packages/ferridriver-test/src/
test.ts` — search `auto`). Rust `FixturePool::resolve()` ignores the
annotation today. The fix is in `worker.rs` (or whichever code path
builds the `requested_fixtures` Vec for each test): for every
registered fixture marked `auto: true`, force-add its name to the
request set before the pool resolves.

Definition of `auto: true` in Playwright: "this fixture runs whether
the test requests it or not." So the resolver must instantiate it
during the test setup phase and tear it down after, regardless of
`fixture_requests` containing the name.

Important: the auto-set must compose with worker-scope vs. test-scope
correctly. Worker-scope auto fixtures run once per worker; test-scope
auto fixtures run per test.

### Tests (Rule 9)

Add to `crates/ferridriver-node/test/cli-flags.test.ts` or a new
`builtin-fixtures.test.ts`:

- A test that lists only `["browserName"]` in `requestedFixtures` and
  asserts `fixtures.browserName === 'chromium'` (assuming default).
- Same for `browserVersion`, returning a non-empty string starting
  with a digit.
- A test that lists no fixtures but registers an auto fixture via
  `test.use({ ... })` and asserts the auto factory ran (e.g. records
  to a side-channel).
- All four backends should pass the browserName/browserVersion test —
  add a `tests/backends_support/builtin_fixtures.rs` that runs through
  the QuickJS `run_script` path.

## Ground rules (CLAUDE.md)

- Rule 1: Rust core defines the fixture registry. NAPI/QuickJS only
  copy strings out.
- Rule 2: signatures match Playwright's `PlaywrightTestArgs` —
  `browserName` not `browser_name` on the JS side.
- Rule 4: each fixture must work on every backend. `browserVersion`
  uses `Browser::version()` which already returns real strings on
  every backend (per §2.15).
- Rule 9: per-fixture integration test on each backend.

## Baseline (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p ferridriver --lib                                 # 125
cargo test -p ferridriver-test --lib                            # 11
cargo test -p ferridriver-script --lib                          # 13
cargo test -p ferridriver-mcp --lib                             # 38
cargo test -p ferridriver-test --test new_features_e2e          # 14
cd crates/ferridriver-node && bun run build:debug && bun test   # 883
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 175 / cdp-raw / bidi / webkit (latest matrix sizes from CI)
```

## Prompt for the next session

> Continue ferridriver Playwright parity — Tier 7 cluster 2 (built-in
> fixtures + `auto: true` enforcement, §7.18 / §7.19). Read first, in
> order:
>
> 1. `CLAUDE.md` — rules + lessons.
> 2. `PLAYWRIGHT_COMPAT.md` — §7.18 / §7.19.
> 3. `HANDOVER.md` — Cluster 1 recap (last session).
> 4. `docs/NEXT_SESSION.md` — this file.
> 5. `/tmp/playwright/packages/playwright/src/index.ts` —
>    `coreTestFixtures` registration.
> 6. `/tmp/playwright/packages/playwright/types/test.d.ts` —
>    `PlaywrightTestArgs` / `PlaywrightWorkerArgs` shapes.
> 7. `crates/ferridriver-test/src/fixture.rs` and
>    `crates/ferridriver-test/src/worker.rs` for the current pool path.
> 8. `crates/ferridriver-node/src/test_runner.rs::STANDARD_FIXTURE_NAMES`
>    and `crates/ferridriver-node/src/test_fixtures.rs` for the NAPI
>    binding shape.
>
> Task: register `browserName`, `browserVersion`, `playwright`, and
> `request` as first-class fixtures in the Rust pool, expose them on
> NAPI / QuickJS with the exact Playwright field names and types, and
> make `auto: true` annotations actually resolve regardless of whether
> the test asked for the fixture.
>
> Per-backend Rule 9: at least one integration test per backend
> exercising `browserName` and `browserVersion`. The "auto fixture
> ran" assertion can be NAPI-only (the QuickJS side mirrors the
> registration but BDD scenarios don't typically declare auto
> fixtures).
>
> Commit shape: one commit (`feat: built-in test fixtures + auto
> enforcement (§7.18 / §7.19)`).
>
> Baseline that must stay green is in HANDOVER.md.
>
> Non-negotiables (CLAUDE.md): no shortcuts; if a backend's
> `Browser::version()` returns a placeholder, fix the backend (§2.15
> already paid that bill — verify before assuming). All three layers
> update in the same commit. No emojis, no AI attribution, no
> task/cluster annotations in source comments.
