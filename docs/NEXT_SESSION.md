# Next session — §2.15 BrowserType class

§4.1 `BrowserContextOptions` is now at **18 of 28 fields** with
apply_context_options plumbing + Rule-9 tests across all four
backends. Four fields are formally deferred with clear blockers
(see `PLAYWRIGHT_COMPAT.md` §4.1 + Section B). The natural next pick
is **§2.15 BrowserType** — the Playwright-shaped factory for
launching / connecting to browsers.

## Why BrowserType next

Today:
```rust
let browser = Browser::launch(LaunchOptions {
  backend: BackendKind::CdpPipe,
  browser: Some(BrowserType::Chromium),
  ...
}).await?;
```

Playwright:
```js
import { chromium, firefox, webkit } from 'playwright';
const browser = await chromium.launch(options);
```

The difference is user-facing ergonomics and Rule-2 parity: Playwright
users learn `chromium.launch()` / `firefox.launch()` / `webkit.launch()`,
not `Browser.launch({ backend, browser })`. Every example in the
Playwright docs starts with `chromium.`. Exposing the same shape
unlocks a lot of example-compatibility for free.

## Read-first

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — §2.15 entry, §4.1 tracker (mostly
   done), Section B deferred-field notes.
3. `HANDOVER.md` — §4.1 session recap.
4. `/tmp/playwright/packages/playwright-core/src/client/browserType.ts`
   — canonical BrowserType surface.
5. `/tmp/playwright/packages/playwright-core/types/types.d.ts` —
   search `interface BrowserType` for the public TS shape.

## §2.15 surface

```ts
interface BrowserType {
  name(): string;
  executablePath(): string;
  launch(options?: LaunchOptions): Promise<Browser>;
  launchPersistentContext(userDataDir: string, options?: ...): Promise<BrowserContext>;
  launchServer(options?: ...): Promise<BrowserServer>;
  connect(wsEndpoint: string | { wsEndpoint, headers?, timeout?, slowMo?, exposeNetwork? }): Promise<Browser>;
  connectOverCDP(endpointURL: string, options?: ...): Promise<Browser>;
}
```

Three instances on the `playwright` module: `chromium`, `firefox`,
`webkit`. Each carries its own `name()` / `executablePath()`; the
rest is shared plumbing differing only in how the backend handshake
runs.

## Implementation sketch

1. **Core** (`crates/ferridriver/src/browser_type.rs` — new module):

   ```rust
   pub struct BrowserType {
     kind: BrowserKind, // Chromium | Firefox | WebKit
   }

   impl BrowserType {
     pub fn chromium() -> Self { ... }
     pub fn firefox() -> Self { ... }
     pub fn webkit() -> Self { ... }

     pub fn name(&self) -> &'static str;
     pub fn executable_path(&self) -> Option<PathBuf>;

     pub async fn launch(&self, opts: LaunchOptions) -> Result<Browser>;
     pub async fn connect(&self, ws_endpoint: &str, opts: ConnectOptions) -> Result<Browser>;
     pub async fn connect_over_cdp(&self, endpoint: &str, opts: ConnectOverCDPOptions) -> Result<Browser>;
     pub async fn launch_persistent_context(&self, user_data_dir: &Path, opts: PersistentContextOptions) -> Result<ContextRef>;
   }
   ```

   `launch` infers the default backend from `BrowserKind` (Chromium
   → CdpPipe, Firefox → Bidi, WebKit → WebKit); callers can still
   override via `LaunchOptions::backend`.

   `launch_persistent_context` is substantial — it's a launch +
   default-context in one that shares storage with the user-data-dir.
   Playwright's shape:
   `launchPersistentContext(userDataDir, { ...LaunchOptions,
   ...BrowserContextOptions })`. The context-options fields go
   through the same `apply_context_options` path §4.1 already
   provides — so the wiring is mostly "merge LaunchOptions +
   BrowserContextOptions, launch, create default context, apply".

2. **Keep `Browser::launch` / `Browser::connect`** as sugar —
   Playwright-parity requires the `BrowserType` entry point, but
   a lot of ferridriver callers already use the Browser factories.
   Deprecate with a doc note pointing at `chromium().launch(...)`
   but don't break.

3. **NAPI** (`crates/ferridriver-node/src/browser_type.rs` — new):

   ```rust
   #[napi]
   pub struct BrowserType { ... }

   #[napi]
   impl BrowserType {
     #[napi(getter)] pub fn name(&self) -> String;
     #[napi(getter)] pub fn executable_path(&self) -> Option<String>;

     #[napi] pub async fn launch(&self, options: Option<LaunchOptions>) -> Result<Browser>;
     // etc.
   }
   ```

   Export three top-level functions (or a module object) mirroring
   Playwright's `{ chromium, firefox, webkit }`:

   ```rust
   #[napi]
   pub fn chromium() -> BrowserType { BrowserType::chromium() }
   #[napi]
   pub fn firefox() -> BrowserType { ... }
   #[napi]
   pub fn webkit() -> BrowserType { ... }
   ```

   Generated `.d.ts` should carry `export function chromium():
   BrowserType;` etc. — diff against Playwright's `types.d.ts` per
   Rule 7.

4. **QuickJS** (`crates/ferridriver-script/src/bindings/browser_type.rs`
   — new): a `BrowserTypeJs` class + three globals `chromium`,
   `firefox`, `webkit` wired via `install_browser_type` similar to
   how §4.1 added `install_browser`.

5. **Rule-9 tests**: launch each type, assert `browser.version()`
   returns the expected product family. Connect-over-CDP: launch
   Chrome separately via raw child, connect via
   `chromium().connectOverCDP(ws)`. Persistent-context: launch with
   a temp dir as user-data-dir, set a cookie, close, re-launch with
   same dir, assert cookie persists.

6. **Commit shape**: one commit. The structural change is small
   (new module + bindings); the behaviour change is mostly "new
   entry point that calls existing code".

## Ground rules (from CLAUDE.md)

- Rule 1/2/3: core is source of truth; three layers update in the
  same commit; no wire shapes leak.
- Rule 4: every backend real; typed `Unsupported` for genuine gaps
  (e.g. WebKit `launch_persistent_context` may need gating on
  multi-context support — already documented as a §4.1 gap).
- Rule 6: read `/tmp/playwright/packages/playwright-core/src/client/browserType.ts`
  FIRST before implementing any signature.
- Rule 7: rebuild NAPI + diff `.d.ts` against Playwright's
  `types.d.ts` — the `BrowserType` interface is in there.
- Rule 9: per-method integration test (launch, connectOverCDP,
  launchPersistentContext) on each backend.
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
# cdp-pipe 159, cdp-raw 159, bidi 154, webkit 155
```

## Prompt for the next session

> Continue ferridriver Playwright parity. Read first, in order:
>
> 1. `CLAUDE.md` — parity rules (1–10) and consolidated lessons.
> 2. `PLAYWRIGHT_COMPAT.md` — §2.15 is the target. §4.1 is now
>    18/28 fields (three commits: 48cc794, 3ec1dc9, e0b3d51) with
>    four documented deferrals under "Section B" (recordHar,
>    clientCertificates, httpCredentials.send, strictSelectors).
> 3. `HANDOVER.md` — §4.1 session recap.
> 4. `docs/NEXT_SESSION.md` — this file, for the §2.15 brief +
>    surface.
> 5. `/tmp/playwright/packages/playwright-core/src/client/browserType.ts`
>    — canonical BrowserType client. Read this BEFORE coding.
> 6. `/tmp/playwright/packages/playwright-core/types/types.d.ts` —
>    grep for `interface BrowserType` to get the TS shape.
>
> Task: implement §2.15 **BrowserType class**. Playwright exposes
> `{ chromium, firefox, webkit }` as three `BrowserType` instances;
> ferridriver should too. The existing `Browser::launch` /
> `Browser::connect` factory methods stay for back-compat (with a
> doc-note pointing at the new entry) — the goal is to ADD the
> Playwright-shaped API, not remove the current one.
>
> Surface per `browserType.ts`:
> - `name() -> &'static str`
> - `executable_path() -> Option<PathBuf>`
> - `launch(opts) -> Result<Browser>`
> - `connect(ws_endpoint, opts) -> Result<Browser>`
> - `connect_over_cdp(endpoint, opts) -> Result<Browser>`
> - `launch_persistent_context(user_data_dir, opts) -> Result<ContextRef>`
> - `launch_server(opts) -> Result<BrowserServer>` (defer to
>   follow-up if BrowserServer infra isn't present)
>
> `launch_persistent_context` is the biggest piece: it's a
> launch + default-context that shares storage with the user-data
> dir. Its options bag is `LaunchOptions & BrowserContextOptions` —
> §4.1 already provides the apply_context_options path, so after
> launching just build a `BrowserContextOptions` from the overlap
> fields and feed `apply_context_options(&opts)` on the default
> page. Playwright's implementation is at
> `/tmp/playwright/packages/playwright-core/src/server/browserType.ts`.
>
> Per-backend defaults:
> - `chromium` → `BackendKind::CdpPipe`, Chrome binary resolution
>   via existing `resolve_chromium`.
> - `firefox` → `BackendKind::Bidi`, Firefox binary resolution via
>   existing `detect_firefox`.
> - `webkit` → `BackendKind::WebKit`, macOS-only (already gated).
>
> NAPI: export `chromium()`, `firefox()`, `webkit()` top-level
> functions returning `BrowserType` instances. Per-method
> `ts_args_type`/`ts_return_type` as needed so generated `.d.ts`
> matches Playwright's `types.d.ts` verbatim.
>
> QuickJS: install `chromium` / `firefox` / `webkit` as globals via
> a new `install_browser_type`. Use the same `BrowserJs`-wrapping
> pattern we added for §4.1's `browser` global.
>
> Rule-9 tests: launch each type, verify `browser.version()` +
> `browser.contexts()`. Connect-over-CDP test: launch Chrome via
> raw Command with `--remote-debugging-port=0`, read the
> `DevToolsActivePort` file, connect via
> `chromium().connectOverCDP(ws)`, assert page operations work.
> `launch_persistent_context` test: temp dir as user-data-dir,
> set a cookie on a page, close, re-launch with same dir, assert
> cookie persists — proves storage sharing.
>
> Alternative picks if BrowserType feels wrong:
> - Close §4.1 deferred fields: `recordHar` (requires §2.6 HAR
>   writer first), `clientCertificates` (requires TLS intercept
>   proxy), `httpCredentials.send` (requires APIRequestContext
>   preemptive header), `strictSelectors` (requires strict-mode
>   counting on every backend selector path — see Section B).
> - §2.6 HAR recording on its own (unblocks §4.1 recordHar).
> - §2.3 Tracing (unblocks §4.5 `context.tracing`).
> - §3.17 Auto-waiting deadline parity.
>
> Commit shape: one commit (`feat: BrowserType class (§2.15)`).
> Touches `crates/ferridriver/src/browser_type.rs` (new),
> `crates/ferridriver/src/lib.rs` (export),
> `crates/ferridriver-node/src/browser_type.rs` (new) +
> `lib.rs` / `types.rs`,
> `crates/ferridriver-script/src/bindings/browser_type.rs` (new) +
> `bindings/mod.rs` (install), tests in both Rust-backends and
> NAPI. Rewrite `HANDOVER.md` + `docs/NEXT_SESSION.md` at commit
> time.
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
> # cdp-pipe 159, cdp-raw 159, bidi 154, webkit 155
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
> Read `/tmp/playwright/packages/playwright-core/src/client/browserType.ts`
> FIRST. Do not reconstruct method signatures from memory. Rule 9
> is load-bearing: every method shipped needs a per-backend
> integration test observable at the Playwright-public API before
> flipping §2.15 `[x]`.
>
> No emojis, no AI attribution in commit messages, no task/phase/
> rule-number annotations in source comments or filenames.
