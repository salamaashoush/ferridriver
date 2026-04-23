# Next session — §2.6 HAR recording (or alt picks)

§2.15 BrowserType is shipped (single commit, replaces
`Browser::launch` / `Browser::connect` with the Playwright-shaped
`chromium()` / `firefox()` / `webkit()` factories across all three
layers). The natural next pick is **§2.6 HAR recording**, which
unblocks `BrowserContextOptions::recordHar` (one of the four §4.1
deferred fields).

## Why §2.6 next

Playwright's `recordHar` ships an HTTP Archive (`.har`) file
containing every request/response observed in a context. ferridriver's
§4.1 work shipped the `recordHar` option-bag field but NOT the writer
— `BrowserContextOptions` accepts `recordHar: { path, ... }` and
silently drops it. Implementing the writer closes that loop.

## Read-first

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — §2.6 entry. §4.1 `recordHar` in the
   "Section B" deferred list (line ~150 in §4.1).
3. `HANDOVER.md` — §2.15 session recap.
4. `/tmp/playwright/packages/playwright-core/src/server/har/harTracer.ts`
   — canonical HAR tracer.
5. `/tmp/playwright/packages/playwright-core/src/server/har/harRecorder.ts`
   — context-side recorder hooks.
6. `/tmp/playwright/packages/har-format/index.d.ts` — HAR 1.2 schema.

## §2.6 surface

`BrowserContextOptions::recordHar` accepts:

```ts
{
  path: string;
  omitContent?: boolean;
  content?: 'omit' | 'embed' | 'attach';
  mode?: 'full' | 'minimal';
  urlFilter?: string | RegExp;
}
```

Behaviour:

- Every request/response observed in the context is appended to the
  HAR's `entries` array.
- `omitContent: true` drops the response body (matches the legacy
  Playwright option).
- `content: 'omit' | 'embed' | 'attach'` — preferred Playwright shape.
- `mode: 'minimal'` skips request/response bodies and headers smaller
  than a threshold.
- `urlFilter` filters by URL pattern.
- The file is written on `context.close()` (or browser shutdown when
  the context is the persistent default).

## Implementation sketch

1. **Core** (`crates/ferridriver/src/har/` — new module):
   - `HarRecorder` struct holding an `Arc<Mutex<HarLog>>`.
   - Each context that opts into HAR registers a recorder bound to
     its composite session key. The recorder hooks the existing
     network event stream (already piped to `Request` / `Response`
     handles via the per-context `network_log`).
   - On `context.close`, flush the log to disk as JSON matching the
     HAR 1.2 schema. Use `serde_json::Serializer::pretty` since HAR
     files are commonly hand-inspected.

2. **`BrowserContextOptions::record_har` plumbing**: already shipped
   in §4.1 — just wire `apply_context_options` to register the
   recorder when the field is set. The write happens at close time;
   no per-page configuration needed.

3. **Backend reach**: works on every backend that already exposes a
   `network_log` (CDP, BiDi, WebKit). WebKit's network observability
   is limited (no main-doc Response, no Set-Cookie) — those gaps
   surface as missing HAR fields, not failures. Document the
   limitation under `recordHar` like §4.1's other backend caveats.

4. **NAPI / QuickJS**: no new bindings needed — `recordHar` already
   parses through the existing `BrowserContextOptions` lowering. Only
   the core writer changes.

5. **Tests** (`crates/ferridriver-cli/tests/backends_support/`):
   - Open a context with `recordHar: { path: tmp/output.har }`.
   - Drive a request through a page (navigate to a known URL +
     `page.evaluate(() => fetch(...))`).
   - Close context.
   - Read the HAR file, assert the entries array contains the
     expected request, status, headers, body (or omitted body for
     `omit` modes).
   - One test per content-mode + one urlFilter test = 4 tests, run on
     all 4 backends per Rule 9.

## Alternative picks

- **§2.3 Tracing** (`context.tracing.start/stop`): bigger lift —
  needs CDP `Tracing.start`, screencast frames, and a `.zip`
  packager. Unblocks §4.5 `context.tracing` directly.
- **§4.1 `clientCertificates`**: needs a TLS-intercepting proxy.
  Major undertaking — defer until after HAR.
- **§4.1 `httpCredentials.send`**: needs APIRequestContext
  preemptive-header wiring. Smaller than HAR but specific to a niche
  Playwright feature.
- **§4.1 `strictSelectors`**: needs strict-mode counting threaded
  through every backend's selector path. Large surface.
- **§3.17 Auto-waiting deadline parity**: small focused surface —
  easy win if the next session is short.

## Ground rules (from CLAUDE.md)

- Rule 1/2/3: core is source of truth; three layers update in the
  same commit; no wire shapes leak.
- Rule 4: every backend real; typed `Unsupported` for genuine
  protocol gaps.
- Rule 6: read `/tmp/playwright/packages/playwright-core/src/server/har/`
  FIRST before implementing.
- Rule 9: per-content-mode integration test on each backend.
- Rule 10: no escape hatches.

## Baseline (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p ferridriver --lib                                 # 125 pass
cargo test -p ferridriver-script --lib                          # 13 pass
cargo test -p ferridriver-mcp --lib                             # 38 pass
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                                      # 859 pass
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 164, cdp-raw 164, bidi 159, webkit 161
```

## Prompt for the next session

> Continue ferridriver Playwright parity. Read first, in order:
>
> 1. `CLAUDE.md` — parity rules (1–10) and consolidated lessons.
> 2. `PLAYWRIGHT_COMPAT.md` — §2.6 is the target. §4.1
>    `recordHar` is the dependent deferred field (Section B).
> 3. `HANDOVER.md` — §2.15 session recap.
> 4. `docs/NEXT_SESSION.md` — this file, for the §2.6 brief +
>    surface.
> 5. `/tmp/playwright/packages/playwright-core/src/server/har/`
>    — canonical HAR tracer + recorder. Read this BEFORE coding.
> 6. `/tmp/playwright/packages/har-format/index.d.ts` — HAR 1.2
>    schema.
>
> Task: implement §2.6 **HAR recording**. Add a `crates/ferridriver/src/har/`
> module exposing `HarRecorder`. Wire it through
> `apply_context_options` so `browser.newContext({ recordHar: { path,
> mode?, content?, omitContent?, urlFilter? } })` actually writes a
> spec-compliant HAR file on `context.close()`. The option field
> already parses; only the writer needs to ship.
>
> Per-backend defaults: every backend with a `network_log` honours
> the recorder. WebKit's network observability limits surface as
> missing HAR fields per the existing §1.4 doc.
>
> NAPI / QuickJS: no new bindings — the `recordHar` field already
> exists in both `NapiBrowserContextOptions` and the QuickJS
> `JsBrowserContextOptions`. Just confirm the lowering reaches the
> new writer.
>
> Rule-9 tests per content-mode + urlFilter (4 tests) on each
> backend. Read the HAR file post-close and assert the entries array
> contains the expected request shape — content presence/absence
> for `embed` vs `omit`, body bytes for `attach`.
>
> Commit shape: one commit (`feat: HAR recording (§2.6)`).
>
> Baseline that must stay green:
> ```
> cargo clippy --workspace --all-targets -- -D warnings
> cargo test -p ferridriver --lib                           # 125
> cargo test -p ferridriver-script --lib                    # 13
> cargo test -p ferridriver-mcp --lib                       # 38
> cd crates/ferridriver-node && bun run build:debug && bun test   # 859
> FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
>   cargo test -p ferridriver-cli --test backends -- --test-threads=1
> # cdp-pipe 164, cdp-raw 164, bidi 159, webkit 161
> ```
>
> Non-negotiables (CLAUDE.md): no grace windows, no timing hacks,
> no broadcast races. No stubs, no placeholders on any backend —
> typed `FerriError::Unsupported` only where the protocol genuinely
> can't. All three layers (Rust core / NAPI / QuickJS) update in
> the same commit. Rebuild NAPI and diff the generated
> `index.d.ts` against Playwright's `types.d.ts` before flipping
> `[x]` in PLAYWRIGHT_COMPAT.md.
>
> Read `/tmp/playwright/packages/playwright-core/src/server/har/`
> FIRST. Do not reconstruct HAR fields from memory. Rule 9 is
> load-bearing: every content-mode shipped needs a per-backend
> integration test that observes the actual HAR file content.
>
> No emojis, no AI attribution in commit messages, no task/phase/
> rule-number annotations in source comments or filenames.
