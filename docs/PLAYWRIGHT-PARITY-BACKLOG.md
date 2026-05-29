# Playwright parity backlog

Tracks Playwright client-API surface that ferridriver does not yet fully
implement, with the concrete blocker for each. Everything else from the
2026-05 parity sweep has landed (ElementHandle option bags + `$`/`$$`,
Locator `drop`/`highlight`/`normalize`/`selector`/`isStrict`/`setStrict`/
`selectText`/`rightClick`/`boundingBox`, screenshot `mask` as `Locator[]`,
BrowserContext `exposeBinding`/`exposeFunction`/`setHTTPCredentials`/
`isClosed`/`browser`/`storageState`, Page `pickLocator`/`unrouteAll`/
`addLocatorHandler`, Frame `waitForSelector` handle return, Disposable
contract, `route({ times })`, `routeFromHAR`, keyboard `namedKeys`,
Browser `newPage`/`page`, accessor batch).

## Not yet implemented

### CDPSession (`browser.newBrowserCDPSession`, `context.newCDPSession`)
- **Status:** not implemented (any layer).
- **Blocker:** `CdpTransport::send_command` returns `impl Future` (async fn in
  trait), so the trait is **not dyn-compatible**. `AnyPage` can't hand out a
  type-erased transport for a `CdpSession` to hold.
- **Design to unblock:** add a small dyn-compatible shim trait
  (`trait CdpSend { fn send(&self, ..) -> BoxFuture<Result<Value>>; }`)
  implemented for each `CdpTransport`, store `Arc<dyn CdpSend>` on the CDP
  `AnyPage` variants, expose a core `CdpSession { send(method, params), on(event) }`.
  WebKit/BiDi return `FerriError::Unsupported`. Then wrap in NAPI (a
  `CDPSession` class) + QuickJS. Event subscription rides
  `transport.subscribe_event_method`.
- **Effort:** medium-large (new public type + event plumbing across 3 layers).

## Partial / known limitations

### Actionability "receives pointer events" hit-test
- ferridriver's click actionability checks visible/stable/enabled but does
  **not** gate on the target being the topmost element at the click point
  (Playwright's obscured-element retry). An overlay therefore does not make a
  click time out; the click dispatches at the coordinates regardless.
- Consequence: `addLocatorHandler` handlers still fire on each actionability
  retry (and dismiss overlays), but the "action blocks until the overlay is
  gone" guarantee is not enforced. The injected `hitTargetInterceptor`
  (`injected/injectedScript.ts`) exists but is not wired into the click gate.

### `addLocatorHandler` on the QuickJS scripting engine
- Returns `FerriError::Unsupported`. A handler must run *during* an in-progress
  action, but every script action runs inside an exclusive `async_with` over
  the single session VM, so a nested handler callback would deadlock. Core +
  NAPI implement it fully. Fixing QuickJS needs the action to yield the VM
  while a handler runs (non-trivial engine change).

### `BrowserContext.route({ times })` counter scope
- `times` is tracked **per page**, not shared across all pages of a context.
  Single-page / `times: 1` usage matches Playwright; a multi-page context that
  shares one budget across pages would need a context-level shared counter.

### `routeFromHAR`
- Replay only. HAR **recording** (`update: true`, `updateContent`,
  `updateMode`) is unsupported. URL filter accepts a glob string; `RegExp`
  url-filter is not yet wired.
