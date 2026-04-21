# Handover — next Playwright-parity session

Read-first for any session continuing work. Overwrite this file with a
fresh summary at the end of each block.

## Cross-device setup

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — gap tracker. Tier 1 done. §3.1, §3.12, §2.9
   landed this session.
3. This file — block summary below.
4. `docs/NEXT_SESSION.md` — next-block brief.

`git clone https://github.com/microsoft/playwright /tmp/playwright` if missing.

## Landed this session

### §3.1 — Navigation returns Response (`3c26547`)

`page.goto` / `reload` / `goBack` / `goForward` (+ `frame.goto`) return
`Promise<Response | null>`. New `NavRequestSlot` helper shared between
the backend network listener and the Page; CDP + BiDi capture the
main-doc request via `is_navigation_request` and return the resolved
Response. WebKit returns `null` (stock `WKWebView` has no public API
for main-doc response observability — documented §1.4 gap).

### §3.12 — Regex on `getBy*` (`bc6aff4`)

`StringOrRegex` enum; every `getBy*` matcher and `RoleOptions.name`
accepts `string | RegExp`. Selector builders emit Playwright-native
`internal:text=` / `internal:label=` / `internal:attr=[name=…]` /
`internal:testid=[data-testid=…]` / `internal:role=…` bodies with Rust
ports of `escapeForTextSelector` / `escapeForAttributeSelector` /
`escapeRegexForSelector`. NAPI uses `JsRegExpLike` prototype-walker;
QuickJS reads RegExp `source`/`flags` via prototype getters. Injected
engine adapter passes `internal:*` bodies through unchanged so the
bundled verbatim Playwright engine does regex matching natively.

### §2.9 — Dialog as first-class handle (this commit)

Live `Dialog` handle with `type` / `message` / `defaultValue` / async
`accept(promptText?)` / `dismiss()`. Dispatch follows Playwright's
`DialogManager.dialogDidOpen` **synchronously** — no broadcast race,
no grace-window timing hack, no `has_listener` check at dispatch.

- **`crates/ferridriver/src/dialog.rs`** — new module:
  - `Dialog` with one-shot `handled: AtomicBool`. `accept` / `dismiss`
    run the backend-supplied async `DialogResponder` closure; second
    call rejects with Playwright's exact wording `"Cannot accept
    dialog which is already handled!"`.
  - `DialogManager` per-page registry. `add_handler(Fn(&Dialog) -> bool) ->
    DialogHandlerId`, `remove_handler(id)`, `did_open(dialog)`.
    `did_open` iterates handlers synchronously; each returns `true`
    to claim. If nobody claims, detaches a task running
    `Dialog::auto_close` — accept for `beforeunload`, dismiss
    otherwise (matches `Dialog._close`). `remove_handler`
    auto-closes orphaned open dialogs when the last handler drops,
    matching Playwright's `removeDialogHandler`.
  - `DialogManager::register_emitter_bridge(events)` — default
    handler installed once per page in each backend's
    `attach_listeners`. Checks `events.has_listener("dialog")` at
    `did_open` time; when a `page.events().on("dialog", cb)` listener
    is present, emits `PageEvent::Dialog(clone)` on the broadcast and
    returns `true` synchronously. Preserves the existing broadcast
    API while keeping the claim synchronous.

- **Backends** — all three wire the same pattern:
  - CDP (`backend/cdp/mod.rs::spawn_dialog_listener`): on
    `Page.javascriptDialogOpening`, builds a `Dialog` whose responder
    sends `Page.handleJavaScriptDialog`, then calls
    `dialog_manager.did_open(dialog)` synchronously.
  - BiDi (`backend/bidi/page.rs`): same for
    `browsingContext.userPromptOpened` + `browsingContext.handleUserPrompt`.
  - WebKit: the Obj-C host's `WKUIDelegate` decides before the event
    reaches Rust. Dialog is still dispatched so listeners observe
    `type`/`message`; the responder returns a documented
    `FerriError::Unsupported` naming the limitation. Rule-4 honest.

- **Page API** — new `Page::wait_for_dialog(timeout_ms) -> Result<Dialog>`:
  registers a one-shot handler with `DialogManager`, awaits a
  `tokio::sync::oneshot`, removes the handler on resolve / timeout.
  Typed `Timeout` / `TargetClosed` errors. NAPI + QuickJS route
  `page.waitForEvent('dialog')` through it so the claim is synchronous
  with the browser event — no broadcast round-trip.

- **Removed the old API**: `DialogHandler`, `DialogAction`,
  `PendingDialog`, `default_dialog_handler`, `Page::set_dialog_handler`,
  `AnyPage::set_dialog_handler`. BDD steps in
  `ferridriver-bdd/src/steps/dialog.rs` rewritten to use
  `events().on("dialog", cb)`. `tests/page_api.rs::dialog_handling_tests`
  rewritten.

- **NAPI** (`crates/ferridriver-node/src/dialog.rs`): new `#[napi]
  class Dialog`. `page.waitForEvent('dialog')` returns it via
  `Either5<Request, Response, WebSocket, Dialog, Value>` — generated
  `.d.ts` matches Playwright.

- **QuickJS** (`crates/ferridriver-script/src/bindings/dialog.rs`):
  new `DialogJs`. `page.waitForEvent('dialog')` instantiates it.

### Rule-9 tests

- `tests/backends_support/dialog.rs` — accept confirm, dismiss
  confirm, prompt with text, double-accept rejection, auto-dismiss
  without listener. All four backends green.
- `crates/ferridriver-node/test/dialog.test.ts` — same five cases
  × 2 CDP backends.

### Baseline after this commit (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings   # clean
cargo test --workspace --lib                            # 122 core
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                              # 809 bun (was 799)
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1   # 4/4 backends
```

## Next priorities

1. **§2.11 FileChooser** — follows the same live-handle pattern as §2.9
   Dialog. New `FileChooser { element, isMultiple, page, setFiles }`,
   `Page::wait_for_file_chooser(timeout)`, per-backend listener on
   `Page.fileChooserOpened` (CDP) / `browsingContext.fileDialogOpened`
   (BiDi) / new IPC op on WebKit. Routes through existing `setInputFiles`
   plumbing from §1.5.
2. **§4.1 BrowserContextOptions** — 28-field option bag at context
   creation (viewport, userAgent, locale, timezone, geolocation,
   permissions, …). Probably 2–3 sessions.
3. **§3.17 Auto-waiting deadline parity** — replace fixed backoff with
   Playwright's exponential polling + deadline propagation.
4. **§2.10 Download as handle** — event-handle pattern from §2.9
   applies.

## Carried-forward backend gaps (real protocol limits)

- **BiDi**: response body unavailable for non-intercepted responses
  (Firefox discards bytes; Playwright's own BiDi backend has the
  same limit). Multi-`Set-Cookie` collapses. `request.postData()`
  null for fetch-with-body.
- **WebKit**: stock `WKWebView` exposes no public API for main-doc
  Response observability (§3.1: returns `null`, documented),
  redirect chain, response body bytes, browser-set request headers,
  `Set-Cookie`, WebSocket frame events. Dialog accept/dismiss is
  decided by the host `WKUIDelegate` before the event reaches Rust
  (§2.9: `Dialog.accept/dismiss` returns typed `Unsupported`).
  `page.evaluate` runs in utility context isolated from the
  user-script's fetch wrap.

## Known flakes

- `context.setOffline toggles network` on WebKit bun occasionally
  fails under the full suite but passes in isolation. Pre-existing.

## Key source locations

| area | path |
|---|---|
| Dialog handle + manager | `crates/ferridriver/src/dialog.rs` |
| CDP dialog listener | `crates/ferridriver/src/backend/cdp/mod.rs::spawn_dialog_listener` |
| BiDi dialog listener | `crates/ferridriver/src/backend/bidi/page.rs` (`browsingContext.userPromptOpened` arm) |
| WebKit dialog drain | `crates/ferridriver/src/backend/webkit/mod.rs` (inside `attach_listeners`) |
| Page::wait_for_dialog | `crates/ferridriver/src/page.rs` |
| NAPI Dialog class | `crates/ferridriver-node/src/dialog.rs` |
| QuickJS DialogJs class | `crates/ferridriver-script/src/bindings/dialog.rs` |
| Rust integration tests | `crates/ferridriver-cli/tests/backends_support/dialog.rs` |
| NAPI dialog tests | `crates/ferridriver-node/test/dialog.test.ts` |
| Navigation `NavRequestSlot` | `crates/ferridriver/src/network.rs` |
| `StringOrRegex` + escapes | `crates/ferridriver/src/options.rs`, `locator.rs` |
| Rules + lessons | `CLAUDE.md` |
