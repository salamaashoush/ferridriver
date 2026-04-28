# Next session — Cluster 3 (TestInfo helpers, §7.10)

Cluster 1 (CLI flags) and Cluster 2 (built-in fixtures + auto
enforcement) are shipped. Cluster 3 adds the missing surface on
`TestInfo`: `output_path()`, `snapshot_path()`, `pause()`, `fn`,
`project`, `config`, `errors[]`, `snapshot_suffix`, plus a `column`
field on `location`.

## Why §7.10 next

The matcher work (Clusters 4 + 5) reads several `TestInfo` fields
that don't exist yet:

- `errors[]` — soft assertions accumulate here in Playwright.
  Without it, `expect.soft` can't surface the running list.
- `snapshot_path` — `toMatchSnapshot` / `toHaveScreenshot` resolve
  paths via this method.
- `outputPath` — `toHaveScreenshot` writes diff/actual artefacts via
  this method.

Shipping §7.10 first means the matcher work in Clusters 4 and 5 can
read real fields rather than reaching into `model::TestInfo`
directly.

## Read-first

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — §7.10 entry.
3. `HANDOVER.md` — Cluster 2 recap.
4. `/tmp/playwright/packages/playwright/types/test.d.ts` — search for
   `interface TestInfo` and copy the canonical signatures into
   `crates/ferridriver-node/src/test_info.rs` doc comments before
   implementing.
5. `crates/ferridriver-test/src/model.rs::TestInfo` — current struct.
   Several Cluster-3 fields can compose from existing data
   (`title_path`, `output_dir`, etc.).
6. `crates/ferridriver-node/src/test_info.rs` — NAPI binding shape.

## Cluster scope

### Methods (NAPI)

- `outputPath(...paths: string[]): string` — joins onto
  `test_info.output_dir`. The variadic form accepts any number of
  path segments.
- `snapshotPath(...paths: string[]): string` — joins onto
  `test_info.snapshot_dir`, optionally honoring
  `snapshot_path_template`.
- `pause(): Promise<void>` — Playwright's pause-on-debug. For
  ferridriver the simplest semantics: log a tracing event and
  return. Real pause-on-keypress is `--ui` work (§7.7) — out of
  scope for this cluster.

### Fields (NAPI)

- `fn` — pointer back to the test function. JS-only; expose as a
  string (test name) or a thunk that throws when called twice.
  Playwright surfaces this for trace viewer integration.
- `project: { name, use, ... }` — cloned from
  `TestConfig::projects[currentProject]`. Until §7.1 lands the
  project DAG, `project` can return the active config's metadata
  (or null).
- `config: { rootDir, ... }` — read-only snapshot of `TestConfig`
  fields the user is likely to inspect.
- `errors: TestError[]` — concat of `soft_errors` (existing) plus
  the primary error if the test failed. Already collected; just
  expose.
- `snapshotSuffix?: string` — optional suffix for snapshot
  filenames. New field on `model::TestInfo`; default `None`.
- `location.column?: number` — already storing line; expose column
  too. Plumbing depends on whether the test discovery layer parses
  it. Default to `null`/`undefined` until the parser learns column
  positions.

### Tests (Rule 9)

`crates/ferridriver-node/test/test-info.test.ts`:

- `outputPath('foo.json')` returns `<output_dir>/<test_name>/foo.json`.
- `snapshotPath('snap.png')` returns the resolved snapshot dir +
  filename.
- `errors` returns soft errors collected via
  `expect.soft(...).toBe(...)` (cluster 4 will add the matchers; for
  this cluster, simulate by pushing to `soft_errors` directly via a
  custom fixture).
- `pause()` resolves quickly without hanging.
- `project` / `config` are non-null and surface known field values.

These don't need a per-backend matrix — TestInfo is backend-agnostic.

## Ground rules (CLAUDE.md)

- Rule 1: Rust core defines the canonical fields; NAPI is a thin
  mirror.
- Rule 2: `outputPath`, `snapshotPath`, etc. match Playwright's
  signatures verbatim — variadic strings, not `(name?: string)`.
- Rule 7: rebuild NAPI and diff `crates/ferridriver-node/index.d.ts`
  against `/tmp/playwright/packages/playwright/types/test.d.ts`
  before flipping `[x]`.
- Rule 9: per-method NAPI test exercising the page-visible effect.

## Baseline (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p ferridriver --lib                                 # 125
cargo test -p ferridriver-test --lib                            # 11
cargo test -p ferridriver-script --lib                          # 13
cargo test -p ferridriver-mcp --lib                             # 38
cargo test -p ferridriver-test --test new_features_e2e          # 15
cd crates/ferridriver-node && bun run build:debug && bun test   # 889
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 175 / cdp-raw 175 / bidi 170 / webkit 171
```

## Prompt for the next session

> Continue ferridriver Playwright parity — Tier 7 cluster 3
> (`TestInfo` helpers, §7.10). Read first, in order:
>
> 1. `CLAUDE.md` — rules + lessons.
> 2. `PLAYWRIGHT_COMPAT.md` — §7.10 entry.
> 3. `HANDOVER.md` — Cluster 2 recap.
> 4. `docs/NEXT_SESSION.md` — this file.
> 5. `/tmp/playwright/packages/playwright/types/test.d.ts` —
>    `interface TestInfo`. Copy the canonical signatures verbatim
>    into Rust doc comments before implementing.
> 6. `crates/ferridriver-test/src/model.rs::TestInfo` and
>    `crates/ferridriver-node/src/test_info.rs`.
>
> Task: ship `outputPath`, `snapshotPath`, `pause`, plus the
> `errors`, `snapshotSuffix`, `project`, `config`, `fn` fields and a
> `column` field on `location` (default null where the discovery
> layer doesn't parse it yet). Errors compose from `soft_errors` +
> primary error. `project` / `config` mirror enough of the active
> `TestConfig` for inspection without leaking internal flags.
>
> Rule 9 in `crates/ferridriver-node/test/test-info.test.ts` —
> backend-agnostic, just NAPI.
>
> Commit shape: one commit (`feat: TestInfo helpers (§7.10)`).
>
> Baseline that must stay green is in HANDOVER.md.
>
> Non-negotiables (CLAUDE.md): match Playwright's signatures
> verbatim; rebuild NAPI and diff index.d.ts; no shortcuts; no
> emojis; no AI attribution.
