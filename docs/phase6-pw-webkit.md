# Phase 6 — Playwright WebKit backend

Replace the native WebKit backend (macOS WKWebView via Obj-C host, Linux
webkit6/gtk4 via gtk4 host) with a single backend that speaks
Playwright's WebKit Inspector protocol. Same backend code on every OS;
true native headless on Linux (no xvfb); feature parity with CDP / BiDi.

## Why

| Property | Phase 2 native | Phase 6 PW WebKit |
|---|---|---|
| macOS | Obj-C + WKWebView | shared protocol client |
| Linux | webkit6/gtk4 host | shared protocol client |
| Headless on Linux | needs `xvfb` | native (WPE) |
| Lines maintained | ~5–6k (host + IPC + JS shims) | ~2k (protocol client) |
| Feature gaps vs CDP | 30–40% (no PDF, no video, no main-doc Response, no per-frame ctx, etc.) | ≈0% (PW patched WebKit to close them) |
| Bundled binary | 0 | ~200MB/install (same model as Chromium/Firefox under `ferridriver install`) |
| Safari authenticity | high (Apple WKWebView) | medium (PW fork) |

The parity win is the real driver, not headless. Closes the
PLAYWRIGHT_COMPAT §1.4 gap matrix.

## Protocol reference

Source of truth: `/tmp/playwright/packages/playwright-core/src/server/webkit/`.

- `webkit.ts` — launch flags (`--inspector-pipe`, `--headless`, …).
- `wkConnection.ts` — message envelope, session routing.
- `wkBrowser.ts` — `Playwright.enable`, `Playwright.createContext`,
  `Playwright.pageProxyCreated`, …
- `wkPage.ts` — `Page.*`, `Runtime.*`, `DOM.*`, `Network.*`, `Input.*`,
  `Console.*`, `Dialog.*`, ~1335 lines of per-target ops.
- `wkInput.ts` — `Input.dispatchMouseEvent`, `Input.dispatchKeyEvent`,
  `Input.dispatchTapEvent`.
- `wkExecutionContext.ts` — `Runtime.evaluate`,
  `Runtime.callFunctionOn`, `Runtime.releaseObject`.
- `wkInterceptableRequest.ts` — network interception.
- `protocol.d.ts` — full generated type surface (9796 lines).

Transport:
- NUL-byte-delimited JSON over fd 3 (host writes / parent reads) +
  fd 4 (host reads / parent writes). Same model as our CDP-pipe
  backend — reuse the byte-pipe transport with NUL framing instead of
  CDP's 0x00 / length-prefix variants.

Envelope:
- Request: `{ id, method, params, pageProxyId? }`
- Response: `{ id, result?, error?, pageProxyId? }`
- Event: `{ method, params, pageProxyId? }`

Session model:
- Root session: empty `sessionId`. Methods: `Playwright.*`.
- Per-page session: keyed by `pageProxyId`. Each page session
  has its own monotonic id space; messages routed via the
  `pageProxyId` envelope at the connection layer (NOT a separate
  websocket-like `Target.attached` session id like CDP).

## Architecture

```
crates/ferridriver/src/backend/pw_webkit/
  mod.rs              Re-exports + AnyPage::PwWebKit variant wiring.
  launcher.rs         Locate + spawn pw_run.sh / Playwright.app.
  transport.rs        Pipe I/O: NUL-delimited JSON over fd 3/4.
  connection.rs       Root + per-page sessions, id allocation,
                      request callbacks, event broadcast.
  protocol.rs         Strongly-typed method+param structs (subset of
                      protocol.d.ts that we actually consume). Keep
                      in serde shape.
  browser.rs          PwWebKitBrowser: launch, `Playwright.enable`,
                      createContext / deleteContext.
  page.rs             PwWebKitPage: navigate, evaluate, find_element,
                      content, screenshot, frame tree, ...
  element.rs          PwWebKitElement: call_js_fn, click, fill, ...
  events.rs           Translate PW events (Page.frameNavigated,
                      Console.messageAdded, Network.requestWillBeSent,
                      ...) into our `PageEvent` enum.
```

No new crate. `pw_webkit` is just another backend module alongside
`cdp/` and `bidi/`. Wired through `AnyPage` / `AnyBrowser` dispatch
macros.

## Backend kind

Add `BackendKind::PwWebKit` (gated on `cfg(pw_webkit_backend)`). For
the migration window, both `BackendKind::WebKit` (legacy native) and
`BackendKind::PwWebKit` exist. Once the PW backend reaches feature
parity and the CI matrix is green, the legacy variant + its crates
get deleted (Phase 6 final step).

## Binary discovery

Mirror `crates/ferridriver/src/backend/cdp/launcher.rs` — search order:
1. `FERRIDRIVER_PW_WEBKIT` env var (explicit override).
2. Playwright cache (`~/.cache/ms-playwright/webkit-*/pw_run.sh` on
   Linux, `~/Library/Caches/ms-playwright/webkit-*/pw_run.sh` on macOS).
3. Our own cache at `~/.cache/ferridriver/pw-webkit/<version>/pw_run.sh`.
4. `ferridriver install webkit` populates path 3.

Same install model as we already have for Chromium / Firefox.

## Migration order

1. **Skeleton** — module structure, launcher, transport, connection
   handshake (`Playwright.enable`).
2. **Core ops** — `Playwright.createContext`,
   `Playwright.createPage`, `Playwright.navigate`, `Runtime.evaluate`.
   Make `test_navigate` + `test_evaluate_*` green on
   `BackendKind::PwWebKit`.
3. **DOM / element handles** — `DOM.querySelector`,
   `Runtime.callFunctionOn`, `Runtime.releaseObject`.
4. **Input** — `Input.dispatchMouseEvent`, `Input.dispatchKeyEvent`,
   `Input.dispatchTapEvent`.
5. **Events** — `Page.frameNavigated`, `Console.messageAdded`,
   `Network.requestWillBeSent` / `responseReceived`,
   `Dialog.javaScriptDialogOpening`.
6. **Coverage** — screenshot, PDF, content, cookies, storage,
   emulate media, geolocation, viewport, timezone, locale, file
   chooser interception, downloads, video recording.
7. **Test matrix migration** — flip `BackendKind::WebKit` →
   `BackendKind::PwWebKit` in `tests/backends.rs::all_tests_webkit`.
8. **Cleanup** — delete `ferridriver-webkit-host`,
   `ferridriver-webkit-wire`, all native host code. Drop
   `BackendKind::WebKit` variant. Update CI to drop xvfb / gtk4 / webkit6
   deps. Update PLAYWRIGHT_COMPAT.md gap matrix.

## Open questions

- **Versioning**: pin a specific PW WebKit binary version (download
  hash) or follow latest Playwright stable? Decision affects
  reproducibility vs. churn.
- **Install flow**: ferridriver's `install` subcommand currently
  downloads Chromium / Firefox / WebKit (macOS-native build). Add a
  branch for PW WebKit (`ferridriver install pw-webkit`) or replace
  the WebKit branch entirely?
- **Macros**: `crates/ferridriver/src/backend/mod.rs::page_dispatch!`
  and friends fan out across `AnyPage` variants. Adding `PwWebKit`
  variant cascades through ~40 method dispatches — mechanical but
  large diff.

## Performance expectation

JSON-RPC over pipe vs current binary-frame IPC:
- Per-call RTT: ~100–500 µs vs ~10–30 µs. ≈10× slower per call.
- Process spawn: ~250 ms vs ~80 ms.
- Test wall-time: ~1.5–2× slower for typical scripts.

Acceptable trade for the parity + headless wins. Playwright themselves
ship CI at scale on this protocol; absolute numbers stay sub-second
for ordinary tests.

## Reference paths used in this plan

- `/tmp/playwright/packages/playwright-core/src/server/webkit/`
- `/tmp/playwright/packages/playwright-core/src/server/pipeTransport.ts`
- `crates/ferridriver/src/backend/cdp/` (model for the new module)
