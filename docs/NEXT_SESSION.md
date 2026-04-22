# Next session — finish §4.1 and pick the next tier

§4.1 BrowserContextOptions has its struct + registry + apply helper +
NAPI/QuickJS bindings + Rule-9 tests for the first cluster of fields.
Next sessions either close the deferred subset OR move on to §2.15
(BrowserType class).

## Read-first

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — §4.1 is `[~]` with the deferral list.
3. `HANDOVER.md` — full §4.1 summary.
4. `/tmp/playwright/packages/playwright-core/types/types.d.ts:22229`
   for the canonical option-bag shape.
5. `/tmp/playwright/packages/playwright-core/src/server/browserContext.ts`
   for Playwright's apply order.

## Recommended pick — §4.1.x deferred fields (small, focused)

Each is a small per-field PR. Per-field work shape:

1. Add the apply line in `crates/ferridriver/src/context.rs::apply_context_options`.
2. Verify the existing per-page setter handles the field (most of
   these already exist on `Page`).
3. Add a Rule-9 test in `crates/ferridriver-cli/tests/backends_support/browser_context_options.rs`.
4. Add the matching NAPI test in
   `crates/ferridriver-node/test/browser-context-options.test.ts`.
5. Update `PLAYWRIGHT_COMPAT.md` §4.1 — move the field from
   "Deferred" to "Applied this commit".

### Quick wins (one-line apply + one Rule-9 test each)

* **`acceptDownloads`** → `page.set_download_behavior(...)`. Test:
  navigate to a URL that triggers a download; assert the download
  fires (or doesn't, when `acceptDownloads: false`).
* **`bypassCSP`** → `page.set_bypass_csp(true)`. Test: page with a
  strict CSP `script-src 'none'`; with bypass, an injected
  `<script>` mutates the DOM; without, it doesn't.
* **`ignoreHTTPSErrors`** → `page.set_ignore_certificate_errors(true)`.
  Test: spin up a self-signed HTTPS server, navigate, assert no
  error.
* **`serviceWorkers`** → `page.set_service_workers_blocked(...)`.
  Test: page registers a service worker; with `block`, registration
  rejects; with `allow`, it succeeds.

### Medium

* **`baseURL`** → store on `ContextRef` (or directly on the options
  bag as already), apply in `page.goto` + `page.waitForURL` URL
  resolver. Test: `await page.goto('/about')` resolves against the
  baseURL.
* **`httpCredentials`** → finish origin scoping + the `send` policy.
  The per-page setter exists (`page.set_http_credentials`); needs to
  filter by origin (so credentials only attach to the origin in
  `httpCredentials.origin`) and route the `send` policy through
  `APIRequestContext`.

### Larger (multi-session each)

* **`proxy`** → per-context proxy on `Browser::launch`. CDP supports
  it via launch flags + `Browser.setProxyOverride` (newer); BiDi
  via session capabilities. Most pragmatic: route through
  `LaunchOptions::args` extension (chrome-flags-based proxy) for
  CDP, document BiDi as launch-time only.
* **`recordHar`** → blocks on §2.6 (HAR writer). Once that lands,
  fold the option into `apply_context_options` similarly to
  `recordVideo`.
* **`storageState`** → blocks on §4.2/§4.3 (IndexedDB capture +
  restore). Then expose as a context-level setter that loads
  cookies + localStorage + indexedDB.

### Backend gaps to close (independent work)

* **WebKit multi-context**: stock `WKWebView` is single-context. To
  unblock §4.1 across WebKit, plumb multiple
  `WKWebViewConfiguration` instances (each with its own
  `WKProcessPool` for cookie isolation) through our IPC. Then
  remove the `skip_if_no_new_context` early-returns in the §4.1
  Rule-9 tests.
* **BiDi `userAgent` / media overrides / geolocation**: Firefox BiDi
  has these on recent drafts (`browsingContext.setUserContextOverride`,
  `emulation.setEmulatedMediaFeatures`, `permissions.setPermission`).
  Wire them on the BiDi backend.

## Alternative pick — §2.15 BrowserType class

Move `Browser::launch` / `Browser::connect` off `Browser` and into a
dedicated `BrowserType` class with:

* `BrowserType::launch(options)`
* `BrowserType::connect(wsEndpoint, options?)`
* `BrowserType::connectOverCDP(wsEndpoint, options?)`
* `BrowserType::launchPersistentContext(userDataDir, options?)`
  (huge — pairs with §4.1's BrowserContextOptions).
* `BrowserType::name()`, `BrowserType::executablePath()`.

Playwright surface is well-known; the work is mostly mechanical
plus the persistent-context path which is non-trivial.

## Ground rules (from CLAUDE.md)

- Rule 1/2/3: core is source of truth; three layers update in the
  same commit; no wire shapes leak.
- Rule 4: every backend real (or typed `Unsupported` for genuine
  protocol limits — document under §4.1 backend-coverage gaps).
- Rule 6: read `/tmp/playwright/...` first.
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
cd <repo root> && bun test                                      # 871
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 153, cdp-raw 153, bidi 148, webkit 149
```

## Notes from §4.1 that generalise

- **Sync setter on async state** — `Browser::new_context` is sync
  but the per-context options registry lives behind an outer
  `Arc<RwLock<BrowserState>>`. Solved by cloning the inner
  `Arc<std::sync::Mutex<HashMap<...>>>` onto the `Browser` handle
  itself at launch time. Same trick used by `record_video`.
- **`browser` global in QuickJS** — added as a third optional
  binding (`page` / `context` / `browser`). `RunContext.browser:
  Option<Arc<ferridriver::Browser>>`. The MCP run_script tool
  constructs the Browser via `from_shared_state`. Rule-9 tests
  consume it via `await browser.newContext({...})`.
- **`ts_args_type` is the way to force Playwright unions** — for any
  options bag that needs string-literal unions (e.g.
  `'light' | 'dark' | 'no-preference' | null`), put the full inline
  TS in `#[napi(ts_args_type = "...")]` rather than relying on
  napi-rs's struct-based inference (which widens to `string`).
- **`Browser.grantPermissions` needs `browserContextId`** — without
  it, grants apply to the default context only and a fresh
  `browser.newContext()` always rejects geolocation. CDP is silent
  about this. Mirrors Playwright's `crBrowser.ts::doGrantPermissions`.
- **`Emulation.setTouchEmulationEnabled` needs `maxTouchPoints`** —
  Chrome leaves `navigator.maxTouchPoints` at 0 when the param is
  omitted on some channels. Playwright passes 5; we do too now.
- **WebKit multi-context** — stock `WKWebView` host is
  single-context. `Browser::new_context` rejects unconditionally.
  All WebKit Rule-9 tests for §4.1 early-return via
  `skip_if_no_new_context`. Closing this requires plumbing
  multiple `WKWebViewConfiguration` instances through host IPC.
- **`browser.newContext()` as the Rule-9 entry point** — for
  features that bind at context-creation time (options bag), tests
  open a fresh context per test rather than mutating the ambient
  one. Matches Playwright's actual usage shape.
