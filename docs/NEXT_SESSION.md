# Next session — §4.1 BrowserContextOptions

Tier 1 done. §3.1, §3.12, §2.9, §2.11, §2.10, §2.12, §2.13, §2.14
landed. Next pick: **§4.1 BrowserContextOptions** — the 28-field
option bag at context creation. Folds today's transitional
`set_record_video` / `set_extra_http_headers` / `set_geolocation`
setters into a single `browser.new_context(options)` struct that
matches Playwright's shape.

## Read-first

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — §4.1 is next; §2.14 just landed.
3. `HANDOVER.md` — §2.14 Video summary.
4. `/tmp/playwright/packages/playwright-core/types/types.d.ts:22397`
   (around `newContext(options?)`) for the full field list.
5. `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts`
   for the client-side option wiring.

## §4.1 canonical surface (per types.d.ts)

```ts
browser.newContext(options?: {
  acceptDownloads?: boolean;
  baseURL?: string;
  bypassCSP?: boolean;
  colorScheme?: 'light' | 'dark' | 'no-preference' | null;
  contrast?: 'no-preference' | 'more' | null;
  deviceScaleFactor?: number;
  extraHTTPHeaders?: Record<string, string>;
  forcedColors?: 'active' | 'none' | null;
  geolocation?: { latitude: number; longitude: number; accuracy?: number };
  hasTouch?: boolean;
  httpCredentials?: { username: string; password: string; origin?: string };
  ignoreHTTPSErrors?: boolean;
  isMobile?: boolean;
  javaScriptEnabled?: boolean;
  locale?: string;
  logger?: Logger;
  offline?: boolean;
  permissions?: string[];
  proxy?: { server: string; bypass?: string; username?: string; password?: string };
  recordHar?: { ... };
  recordVideo?: { dir: string; size?: { width: number; height: number } };
  reducedMotion?: 'reduce' | 'no-preference' | null;
  screen?: { width: number; height: number };
  serviceWorkers?: 'allow' | 'block';
  storageState?: string | { ... };
  strictSelectors?: boolean;
  timezoneId?: string;
  userAgent?: string;
  viewport?: { width: number; height: number } | null;
}): Promise<BrowserContext>;
```

## Implementation sketch

1. **New struct** (`crates/ferridriver/src/options.rs`):
   - `BrowserContextOptions` with Option fields matching Playwright
     byte-for-byte. Include `RecordVideoOptions` as a field; drop the
     standalone `ContextRef::set_record_video` transitional setter
     (or keep it as a deprecated no-op that routes into the bag).

2. **`Browser::new_context_with_options(options)`**: new method that
   takes the bag and returns a ContextRef pre-loaded with everything.
   `Browser::new_context()` becomes sugar for the no-options case.

3. **State wiring**: each option that affects page creation (viewport,
   userAgent, locale, timezone, deviceScaleFactor, …) needs to land
   somewhere the backend can read at page-open time. Many already
   have setters — refactor into a single "apply context options"
   helper that runs once per new_page.

4. **Per-backend propagation**:
   - CDP: use `Emulation.setUserAgentOverride`,
     `Emulation.setGeolocationOverride`, `Emulation.setLocaleOverride`,
     `Emulation.setTimezoneOverride`, `Emulation.setDeviceMetricsOverride`,
     `Network.setExtraHTTPHeaders`, etc. Already present as per-setter
     methods; consolidate.
   - BiDi: `browsingContext.setViewport`, `emulation.setGeolocationOverride`
     (bidi 0.30+), etc. Some options are CDP-only — typed `Unsupported`
     with the reason.
   - WebKit: many options via the existing `host.m` userScripts
     (locale, timezone already present); others typed `Unsupported`.

5. **NAPI + QuickJS**: accept the full options bag. ts_args_type
   forces the exact TS shape.

6. **Rule-9 integration tests**: per-option effect observable through
   the page. e.g. `userAgent` → `navigator.userAgent` in page.evaluate;
   `colorScheme: 'dark'` → `matchMedia('(prefers-color-scheme: dark)').matches`;
   `geolocation` → `navigator.geolocation.getCurrentPosition` resolves
   with the supplied coords; `recordVideo` already tested in §2.14.

Expect 2–3 sessions — the option bag is large and each knob needs its
own integration test.

## Ground rules (from CLAUDE.md)

- Rule 1/2/3: core is source of truth; three layers update in the
  same commit; no wire shapes leak.
- Rule 4: every backend real (or typed `Unsupported` for genuine
  protocol limits).
- Rule 6: read `/tmp/playwright/...` first — types.d.ts AND the
  client/browserContext.ts server wiring.
- Rule 7: rebuild NAPI + diff `.d.ts` against Playwright's types.
- Rule 9: per-option integration test observable at the page level
  before flipping `[x]`.
- Rule 10: no escape hatches.

## Baseline (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p ferridriver --lib                                 # 125 core
cargo test -p ferridriver-script --lib                          # 13 script
cargo test -p ferridriver-mcp --lib                             # 38 MCP
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                                      # 853
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 140, cdp-raw 140, bidi 135, webkit 136
```

## Commit shape

Likely multiple commits given the scope — one per cluster of fields
(emulation, geolocation+permissions, HTTP extras, recording, etc.).
Each commit updates `PLAYWRIGHT_COMPAT.md` § with the set of fields
landed. Final commit flips §4.1 `[x]` and rewrites HANDOVER.md +
`docs/NEXT_SESSION.md`.

## Notes from §2.14 that generalise

- **Per-context registry pattern** — when an option applies at
  context creation but the setter must be callable from non-async
  code (e.g. NAPI sync methods), use the `BrowserState::record_video`
  / `BrowserState::context_events` pattern: `Arc<std::sync::Mutex<
  HashMap<composite_key, T>>>` on state, with `get_or_create` /
  `get` / `set` helpers. `ContextRef::new` reads the registry; any
  `set_*` method writes it. §4.1's options bag can keep this pattern
  OR store the full options struct by context-key in a single map.
- **`context.newPage()` as the Rule-9 escape hatch** — the MCP
  harness binds `page` globally; closing the ambient page breaks
  sibling tests. For any feature that needs `page.close()` to fire a
  terminal state (video finalisation, dialog close, download
  complete), open a FRESH page via `context.newPage()` in the test
  script.
- **QuickJS `setTimeout` absence** — still the biggest gotcha. For
  tests that need "do X then wait before Y", use multiple page
  round-trips (each `page.goto` blocks on lifecycle) instead of
  sleeps. The §2.14 lifecycle test does two `goto`s to give the
  encoder state transitions to capture without `await sleep(400)`.
- **ffmpeg `pad` filter refuses to shrink** — when using
  `video::start_recording`, the recordVideo size must be >= the
  backend's rendered frame size. Firefox/BiDi renders at 1280x720-ish
  by default; the 800x450 default size triggers `Padded dimensions
  cannot be smaller than input`. §4.1's default should probably scale
  viewport down rather than up — or switch the encoder filter from
  `pad` to `scale`.
