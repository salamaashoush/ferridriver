# Playwright API Parity — ferridriver

Canonical gap tracker, derived from a full sweep of Playwright v1.x (`/tmp/playwright/packages/playwright-core/src/client/*` + `types.d.ts` + `playwright-test` package) against ferridriver's Rust core (`crates/ferridriver/`), NAPI bindings (`crates/ferridriver-node/`), and test runner (`crates/ferridriver-test/`).

## Principles (non-negotiable)

1. **Rust core is source of truth**; NAPI is a thin target; TS package is a thin wrapper. No logic duplicated in NAPI/TS.
2. **No backward-compat shims.** Breaking changes are acceptable (clippy's `avoid-breaking-exported-api = false`).
3. **No stringly-typed errors.** Every failure path goes through a `thiserror` enum.
4. **No escape hatches.** Every `unwrap`/`expect` in non-test code must have a justification or be replaced.
5. **Every Playwright matcher has a Rust implementation** — the TS expect wrapper calls into Rust via NAPI for all polling/assertions.
6. **Auto-waiting and polling logic lives in Rust**, with progress + deadline parity against Playwright's `_wrapApiCall`.
7. **Every new public method has**: (a) a core Rust unit/integration test, (b) a NAPI test in `crates/ferridriver-node/__test__/`, (c) a TS-side test, (d) a BDD step if the action is user-facing.
8. **Always verify against the cloned Playwright source** at `/tmp/playwright/` before implementing any parity feature. Read `packages/playwright-core/src/client/*.ts`, `packages/playwright/types/test.d.ts`, and `packages/playwright-core/src/utils/` to confirm exact API shapes, option fields, protocol encoding, and semantics. Never guess or infer from docs — the source is the only authority.

## Status legend

- [ ] not started
- [~] partial (details in "Current state")
- [x] complete + tests green

---

## Tier 1 — Foundation (blocks most downstream work)

### 1.1 Structured error taxonomy

- [x] Replace `Result<T, String>` across public API with `thiserror` enums.
- **Playwright ref**: `packages/playwright-core/src/client/errors.ts` (`TimeoutError`, `TargetClosedError`, `parseError`).
- **Files**: new `crates/ferridriver/src/error.rs`; touches every `pub` signature in `page.rs`, `locator.rs`, `frame.rs`, `context.rs`, `browser.rs`, `route.rs`, `api_request.rs`.
- **Design**:
  ```
  pub enum FerriError {
    Timeout { operation: String, timeout_ms: u64 },
    TargetClosed { reason: Option<String> },
    StrictModeViolation { selector: String, count: usize },
    Navigation { url: String, source: NavError },
    Protocol(ProtocolError),
    Backend(BackendError),
    InvalidSelector { selector: String, reason: String },
    NotConnected,
    Interrupted,
    ... one variant per failure category
  }
  ```
- **Acceptance**: `TimeoutError::is_timeout_error()` helper matches Playwright shape; NAPI surfaces `error.name === 'TimeoutError'` and `error.name === 'TargetClosedError'` via `napi::Error::new` with `Status::GenericFailure` + custom reason.
- **Tests**: each error variant has a dedicated test that triggers it; `locator.rs` strict-mode test must catch `StrictModeViolation`.

### 1.2 ElementHandle

- [ ] Introduce `ElementHandle` as a lifecycle object backed by a CDP `RemoteObjectId` / WebKit node ref.
- **Playwright ref**: `packages/playwright-core/src/client/elementHandle.ts`.
- **Files**: new `crates/ferridriver/src/element_handle.rs`; NAPI `crates/ferridriver-node/src/element_handle.rs`; updates to `page.rs` and `locator.rs`.
- **Surface** (all methods take option structs; all auto-wait where Playwright does):
  - `query_selector`, `query_selector_all`, `eval_on_selector`, `eval_on_selector_all`.
  - `bounding_box`, `check`, `uncheck`, `set_checked`, `click`, `dblclick`, `tap`, `hover`.
  - `content_frame`, `dispatch_event`, `fill`, `focus`, `get_attribute`.
  - `inner_html`, `inner_text`, `input_value`, `text_content`.
  - `is_checked`, `is_disabled`, `is_editable`, `is_enabled`, `is_hidden`, `is_visible`.
  - `owner_frame`, `press`, `screenshot`, `scroll_into_view_if_needed`.
  - `select_option`, `select_text`, `set_input_files`, `type`, `wait_for_element_state`, `wait_for_selector`.
  - `dispose`, `[Symbol.asyncDispose]` on NAPI.
- **Acceptance**: handle survives navigations of unrelated frames; `dispose()` releases CDP reference; handle usable as argument to `page.evaluate(fn, handle)`.
- **Tests**: each method + lifecycle (dispose, detached-element error, cross-frame handle error).

### 1.3 JSHandle

- [ ] Introduce `JSHandle` as a lifecycle object for arbitrary JS values.
- **Playwright ref**: `packages/playwright-core/src/client/jsHandle.ts`.
- **Files**: new `crates/ferridriver/src/js_handle.rs`; NAPI `crates/ferridriver-node/src/js_handle.rs`.
- **Surface**: `as_element`, `dispose`, `evaluate`, `evaluate_handle`, `get_properties`, `get_property`, `json_value`.
- **Key requirement**: `Page::evaluate(fn, arg)` must serialize `arg` through the same protocol Playwright uses — a tagged union that preserves `JSHandle` references, `undefined`, `NaN`, `+/-Infinity`, `Date`, `RegExp`, `URL`, `Map`, `Set`, `Error`, typed arrays, BigInt. See `packages/playwright-core/src/protocol/serializers.ts`.
- **Acceptance**: passing a `JSHandle` to `evaluate(fn, handle)` lands in the page as the live object; `jsonValue()` round-trips all the preserved types.
- **Tests**: every preserved type; handle-as-arg; cross-context handle error.

### 1.4 Request / Response / WebSocket as lifecycle objects

- [ ] Replace event-DTO `NetRequest`/`NetResponse` (`context.rs:28`, `events.rs:59`) with full lifecycle objects backed by CDP network events.
- **Playwright ref**: `packages/playwright-core/src/client/network.ts`.
- **Files**: new `crates/ferridriver/src/network.rs`; delete `NetRequest`/`NetResponse` from `events.rs` / `context.rs`; update event plumbing; NAPI `crates/ferridriver-node/src/network.rs`.
- **Request surface**: `all_headers`, `failure`, `frame`, `header_value`, `headers`, `headers_array`, `is_navigation_request`, `method`, `post_data`, `post_data_buffer`, `post_data_json`, `redirected_from`, `redirected_to`, `resource_type`, `response` (awaitable), `service_worker`, `sizes`, `timing`, `url`.
- **Response surface**: `all_headers`, `body`, `finished` (awaitable), `frame`, `from_service_worker`, `header_value`, `header_values`, `headers`, `headers_array`, `json`, `ok`, `request`, `security_details`, `server_addr`, `status`, `status_text`, `text`, `url`.
- **Acceptance**: `page.on('request')` / `on('response')` / `on('requestfinished')` / `on('requestfailed')` emit full lifecycle objects; `response.body()` works for already-received responses (CDP `Network.getResponseBody`).
- **Tests**: redirect chain, failure event, body retrieval, post-data JSON parse, WebSocket frames.

### 1.5 Action option bags on Locator and Page

- [~] **Signatures shipped, semantic fidelity partial, per-option integration tests missing.** The commit `8fa8afb` wired option-bag structs across Rust core + NAPI + QuickJS for every action, but the end-to-end parity claim in that commit message was premature. Concrete gaps, every one a session's worth of work, are listed below.

#### Shipped (real, tested)

- [x] **Click** — full option surface with per-option NAPI + backends tests (commit `d1e36ee`). `button`, `click_count`, `delay`, `force`, `modifiers`, `no_wait_after`, `position`, `steps`, `timeout*`, `trial`. `*timeout`: accepted but not actually propagated to the retry loop — same gap as every other method below.
  - Core: `MouseButton`, `Modifier`, `modifiers_bitmask`, `ClickOptions` + `resolved_*` helpers + `actions::click_with_opts(el, page, opts)` that scroll-into-view, honor `force` / `trial`, and delegate to `BackendClickArgs`.
  - Backends: every backend grew `click_at_with(x, y, args)` + `press_modifiers(&mods)` + `release_modifiers(&mods)`. CDP uses `Input.dispatchKeyEvent` + `Input.dispatchMouseEvent{modifiers}`. BiDi `input.performActions` with separate `key` + `pointer` sources. WebKit's host.m tracks `held_modifier_flags` on `ViewEntry` so `OP_KEY_DOWN` / `OP_KEY_UP` update the mask and the next `OP_MOUSE_EVENT` carries it on the `NSEvent`. Middle-click takes a CGEvent path because `NSEvent mouseEventWithType:` leaves `buttonNumber=0`; offscreen WKWebView also needs a JS `MouseEvent` belt for `auxclick`.
  - Tests: 9 NAPI `bun test` cases, 1 backends live-browser test covering button / clickCount / modifiers / position / delay / trial + the error paths. Passing on all 4 backends.

#### Partial — signatures + wire plumbing only (commit `8fa8afb`)

- [~] **Dblclick** — `DblClickOptions` → `ClickOptions { click_count: Some(2) }` lowering. No new option-specific tests.
- [~] **Hover** — core helper, `BackendHoverArgs` dispatch on all 3 backends. **BUG: `HoverOptions` includes a `steps` field Playwright doesn't have on `hover` — strict spec divergence. Remove it.** No option-specific tests.
- [~] **Tap** — **implemented via JS `TouchEvent`/`PointerEvent` dispatch across all backends, producing `isTrusted: false` events**. CDP has `Input.dispatchTouchEvent` and should be used for Chromium. This is a Rule 4 violation — fix with a native path per backend. Also: **`TapOptions = HoverOptions` alias inherits the bogus `steps` field — remove**. No option-specific tests.
- [~] **Fill** — `FillOptions { force, no_wait_after, timeout }`. `force` only skips the Locator-level actionability check; the inner `actions::fill` still runs its JS-side state guards, so `force=true` isn't actually bypassing everything. No option-specific tests.
- [~] **Press** — `PressOptions { delay, no_wait_after, timeout }`. Delay wires into keyDown + sleep + keyUp. No option-specific tests.
- [~] **Type / pressSequentially** — `TypeOptions { delay, no_wait_after, timeout }`. Delay wires into per-character press loop. No option-specific tests.
- [~] **Check / Uncheck / SetChecked** — reads `this.checked` once, clicks once if state differs. **BUG: Playwright retries the read/click cycle until state matches for elements that need multiple clicks to toggle (e.g. custom components that intercept the first click); mine doesn't loop.** Per `/tmp/playwright/packages/playwright-core/src/server/dom.ts::_setChecked`. No option-specific tests.
- [~] **DispatchEvent** — picks Event / MouseEvent / KeyboardEvent / TouchEvent / PointerEvent / DragEvent / FocusEvent / InputEvent / WheelEvent by type; applies `eventInit`. **BUG: Playwright runs actionability checks + scrolls-into-view before dispatch; mine skips straight to `this.dispatchEvent`.** No option-specific tests.
- [~] **SelectOption** — polymorphic `string | string[] | {value,label,index} | array` via custom `FromNapiValue` + `parse_select_option_values`. Uses Playwright's own injected `selectOptions` helper (verbatim). **BUG: `force` is unused — no actionability check is performed at any level.** Ignores `ElementHandle` variant (blocks on 1.2, documented). No option-specific tests.
- [~] **SetInputFiles** — polymorphic `string | string[] | FilePayload | FilePayload[]`. Payloads write to temp files, upload via `set_file_input`, delete after. No option-specific tests.
- [x] **DragTo** — pre-existing full `DragAndDropOptions` with `force`, `no_wait_after`, `source_position`, `target_position`, `steps`, `strict`, `timeout*`, `trial` (commit `b6e0f6c`). `*timeout` not propagated — same gap.

#### Cross-cutting gaps (all ~[~] methods share these)

1. **`timeout` is accepted but never honored.** The `Locator::RETRY_BACKOFFS_MS` constant schedule is used on every retry; the per-call option is ignored. The NAPI/QuickJS TS surface advertises `timeout` as working, but it doesn't. This is half of task **3.17**; the other half is `context.setDefaultTimeout` overrides. Fix both at once: thread a deadline through `retry_resolve!` and honor `ClickOptions::timeout` / `HoverOptions::timeout` / etc.
2. **`force` is partial.** At the Locator level we skip `wait_for_actionable`; at the `actions::*` JS level the existing state guards still run. Audit every `actions::` helper and add a `force` bypass where Playwright skips the check (see `server/dom.ts::_dispatchEvent`, `_setChecked`, `_selectOption`).
3. **No per-option integration tests on 12 of 13 methods.** Rule 9 is explicit: "Signatures alone are not parity — prove it works end-to-end on every backend." The only methods with real per-option NAPI + backends tests are Click and addInitScript. Every other method needs: a NAPI `bun test` per option field + a QuickJS backends test exercising the option across all 4 backends + a DOM-side probe proving the option *took effect* (not just that the call didn't error).

#### Rule-4 native-path gaps

4. **`tap` should use `Input.dispatchTouchEvent` on CDP** (Chromium supports it). BiDi has no touch pointerType in stable — typed `Unsupported` is correct there. WebKit has no public touch injection — typed `Unsupported` there. JS-fallback across the board isn't "real implementation on every backend" per Rule 4.

- **Playwright ref**: `LocatorClickOptions`, `LocatorHoverOptions`, `LocatorFillOptions`, `LocatorPressOptions`, `LocatorTypeOptions`, `LocatorCheckOptions`, `LocatorSetCheckedOptions`, `LocatorTapOptions`, `LocatorDblClickOptions`, `LocatorDragToOptions`, `LocatorDispatchEventOptions`, `LocatorSelectOptionOptions`, `LocatorSetInputFilesOptions` in `/tmp/playwright/packages/playwright-core/types/types.d.ts`.
- **Files touched**: `crates/ferridriver/src/options.rs` (13 option structs, `SelectOptionValue`, `FilePayload`, `InputFiles`, `MouseButton`, `Modifier`); `crates/ferridriver/src/{locator,page,frame}.rs` (signature changes, every action method); `crates/ferridriver/src/actions.rs` (shared `click_with_opts`, `hover_with_opts`, `tap_with_opts`, `select_options`); `crates/ferridriver/src/backend/{cdp,bidi,webkit}` (`click_at_with`, `hover_at_with`, `press_modifiers`, `release_modifiers`, `BackendClickArgs`, `BackendHoverArgs`); `crates/ferridriver-node/src/{types,locator,page,frame}.rs` (13 NAPI option structs, 2 custom `FromNapiValue` enums for polymorphic inputs); `crates/ferridriver-script/src/bindings/{convert,locator,page,frame}.rs` (11 `parse_*` helpers).
- **Acceptance for real completion**: per-option integration test proving the option takes visible effect on all 4 backends, for every field on every method. Rule 9.
- **Next-session remediation plan**: see the `1.5 remediation` section in `HANDOVER.md`.

---

## Tier 2 — Entirely missing subsystems

### 2.1 CDPSession

- [ ] Expose CDP transport that already exists in `backend/cdp/transport.rs` as a user-facing `CDPSession`.
- **Playwright ref**: `packages/playwright-core/src/client/cdpSession.ts`.
- **Surface**: `send<T>(method, params)`, `detach`, `on(event, listener)`.
- **Files**: new `crates/ferridriver/src/cdp_session.rs`; NAPI binding; exposed via `browser.new_browser_cdp_session()` and `context.new_cdp_session(page_or_frame)`.
- **Tests**: send `Runtime.evaluate` and assert result; subscribe to `Network.requestWillBeSent`.

### 2.2 Clock API

- [ ] Install virtual clock using CDP `Emulation.setVirtualTimePolicy` + `Runtime.evaluate` shim for `Date.now` / `setTimeout` / etc.
- **Playwright ref**: `packages/playwright-core/src/client/clock.ts` + `packages/playwright-core/src/server/browserContext.ts` clock harness.
- **Surface**: `install(options)`, `fast_forward(ms)`, `pause_at(time)`, `resume`, `run_for(ms)`, `set_fixed_time(time)`, `set_system_time(time)`.
- **Files**: new `crates/ferridriver/src/clock.rs`; attaches to `BrowserContext`.
- **Tests**: `page.evaluate("Date.now()")` advances deterministically; `setTimeout` fires after `run_for`.

### 2.3 Tracing (Playwright trace.zip v8)

- [ ] Rewrite `crates/ferridriver-test/src/tracing.rs` to emit the full Playwright trace v8 format.
- **Playwright ref**: `packages/trace/src/trace.ts` (BeforeActionTraceEvent, AfterActionTraceEvent, InputActionTraceEvent, EventTraceEvent, FrameSnapshotTraceEvent, ResourceSnapshotTraceEvent, ScreencastFrameTraceEvent) + `packages/trace/src/snapshot.ts`.
- **Deliverables**:
  - `trace.trace` (JSONL of typed events).
  - `trace.network` (network events).
  - `resources/` (sha1-keyed screenshot PNGs + DOM snapshot HTML).
  - Full stack frames via `backtrace` crate or `std::backtrace::Backtrace`.
  - Screencast via CDP `Page.startScreencast`.
  - `context-options` event up front.
  - `before`/`after`/`input` for every Playwright-style action.
- **Acceptance**: `npx playwright show-trace trace.zip` opens a full trace with DOM snapshots, network, screencast, and timeline.
- **Files**: overhaul `tracing.rs`; add `crates/ferridriver-test/src/trace/` module with `format.rs`, `snapshot.rs`, `network.rs`, `screencast.rs`.
- **Tests**: round-trip — record a simple test, open the trace zip, assert the full event taxonomy is present.

### 2.4 Coverage

- [ ] Implement `JSCoverage` and `CSSCoverage` via CDP `Profiler.startPreciseCoverage` + `CSS.startRuleUsageTracking`.
- **Playwright ref**: `packages/playwright-core/src/client/coverage.ts`.
- **Surface**: `start_js_coverage(options)`, `stop_js_coverage() -> Vec<JSCoverageEntry>`, `start_css_coverage(options)`, `stop_css_coverage() -> Vec<CSSCoverageEntry>`.
- **Files**: new `crates/ferridriver/src/coverage.rs`; exposed via `page.coverage()`.
- **Tests**: load a page with known-executed + dead code, verify ranges.

### 2.5 Selectors registry

- [ ] Implement `selectors.register(name, script, options)` + `selectors.set_test_id_attribute(name)`.
- **Playwright ref**: `packages/playwright-core/src/client/selectors.ts` + `packages/playwright-core/src/server/injected/selectorEvaluator.ts`.
- **Files**: `crates/ferridriver/src/selectors.rs` (extend); `crates/ferridriver/src/injected/` — expose runtime registration path into the injected engine.
- **Acceptance**: `selectors.register('my-engine', '...')` works across all pages created after registration; `setTestIdAttribute('data-qa')` flows into both injected code and `getByTestId` Rust path.
- **Tests**: custom engine queries; changing test-id attribute affects `getByTestId`.

### 2.6 HAR recording + routing

- [ ] Ignoring the old plan — design fresh.
- **Playwright ref**: `packages/playwright-core/src/client/harRouter.ts` + `packages/playwright-core/src/server/recorders/har.ts` + context option `recordHar`.
- **Surface**:
  - `BrowserContextOptions { record_har: Option<RecordHarOptions> }` where `RecordHarOptions = { path, mode: Full|Minimal, content: Omit|Embed|Attach, url_filter: Option<UrlMatcher> }`.
  - `context.route_from_har(har, { url, not_found: Abort|Fallback, update, update_content, update_mode })`.
  - `page.route_from_har(...)`.
- **Files**: new `crates/ferridriver/src/har/` with `recorder.rs`, `router.rs`, `format.rs` (HAR 1.2 serde types).
- **Tests**: record → replay round-trip; `update: true` mode; URL filter.

### 2.7 Worker / ServiceWorker / BackgroundPage

- [ ] Expose CDP `Target.targetCreated` / `Runtime.evaluate` in worker contexts as `Worker` objects.
- **Playwright ref**: `packages/playwright-core/src/client/worker.ts`.
- **Surface**: `Worker { url, evaluate, evaluate_handle, on('close') }`.
- **Files**: new `crates/ferridriver/src/worker.rs`; emits `page.on('worker')`, `context.on('serviceworker')`, `context.background_pages()`.
- **Tests**: spawn a web worker, evaluate in it, capture close.

### 2.8 WebSocket + WebSocketRoute

- [ ] Expose `WebSocket` events and `routeWebSocket` interception.
- **Playwright ref**: `packages/playwright-core/src/client/network.ts` (`WebSocket` class + `WebSocketRoute`).
- **Surface**: `WebSocket { url, is_closed, wait_for_event, on('framereceived'|'framesent'|'socketerror'|'close') }`, `context.route_web_socket(url, handler)`, `page.route_web_socket(url, handler)`.
- **Files**: new `crates/ferridriver/src/websocket.rs` + `websocket_route.rs`.
- **Tests**: observe a WebSocket handshake + frames; mock a ws server with `route_web_socket`.

### 2.9 Dialog as handle

- [ ] Promote `Dialog` from a handler callback to a first-class handle.
- **Playwright ref**: `packages/playwright-core/src/client/dialog.ts`.
- **Surface**: `Dialog { accept(prompt_text?), dismiss, default_value, message, page, type }`; `page.on('dialog', listener)` where listener receives the handle.
- **Files**: new `crates/ferridriver/src/dialog.rs`; remove `set_dialog_handler` in `page.rs:1710` in favor of event-based model.
- **Tests**: accept-with-prompt, dismiss, beforeunload auto-dismiss.

### 2.10 Download as handle

- [ ] Promote `Download` to a handle.
- **Playwright ref**: `packages/playwright-core/src/client/download.ts`.
- **Surface**: `Download { cancel, create_read_stream, delete, failure, page, path, save_as, suggested_filename, url }`.
- **Files**: new `crates/ferridriver/src/download.rs`.
- **Tests**: save-as, cancel, failure event, stream read.

### 2.11 FileChooser

- [ ] Add `FileChooser` class + `page.on('filechooser')`.
- **Playwright ref**: `packages/playwright-core/src/client/fileChooser.ts`.
- **Surface**: `FileChooser { element, is_multiple, page, set_files(files, options) }`.
- **Files**: new `crates/ferridriver/src/file_chooser.rs`; hook into CDP `Page.fileChooserOpened`.
- **Tests**: trigger an `<input type=file>` programmatically, set files via chooser event.

### 2.12 ConsoleMessage rich

- [ ] Replace `ConsoleMsg { type, text }` with full `ConsoleMessage { args: Vec<JSHandle>, location, page, text, type, timestamp }`.
- **Playwright ref**: `packages/playwright-core/src/client/consoleMessage.ts`.
- **Files**: new `crates/ferridriver/src/console_message.rs`; remove `ConsoleMsg` from `context.rs:20`.
- **Tests**: verify args resolve to JSHandles, location has URL/line/column.

### 2.13 WebError

- [ ] Add `WebError { error, page }` for `context.on('weberror')`.
- **Playwright ref**: `packages/playwright-core/src/client/webError.ts`.
- **Files**: new `crates/ferridriver/src/web_error.rs`.
- **Tests**: throw an unhandled error in a page, assert it arrives on context.

### 2.14 Video as handle

- [ ] Expose `Video` class via `page.video()`.
- **Playwright ref**: `packages/playwright-core/src/client/video.ts`.
- **Surface**: `Video { path, save_as, delete }`.
- **Files**: wrap existing `crates/ferridriver/src/video.rs` `VideoRecordingHandle` in a public `Video` API; wire into `record_video` context option.
- **Tests**: record + save-as + delete.

### 2.15 BrowserType class

- [ ] Introduce `BrowserType` — remove ad-hoc `Browser::launch` / `Browser::connect` on `Browser`.
- **Playwright ref**: `packages/playwright-core/src/client/browserType.ts`.
- **Surface**: `name()`, `executable_path()`, `launch(options)`, `launch_persistent_context(user_data_dir, options)`, `launch_server(options)`, `connect(endpoint, options)`, `connect_over_cdp(endpoint, options)`.
- **Files**: new `crates/ferridriver/src/browser_type.rs`; NAPI module.
- **Tests**: launch each browser type, connect-over-CDP to an externally-launched Chrome.

---

## Tier 3 — Shape mismatches

### 3.1 Navigation returns Response

- [ ] `page.goto`, `page.reload`, `page.go_back`, `page.go_forward` return `Result<Option<Response>, FerriError>`.
- **Files**: `crates/ferridriver/src/page.rs`.
- **Blocks on**: 1.4.

### 3.2 `goto` accepts `referer`

- [x] Add `referer: Option<String>` to `GotoOptions`.
- **Files**: `crates/ferridriver/src/options.rs`; CDP `Page.navigate` call site in `page.rs`.

### 3.3 ScreenshotOptions complete

- [x] Full 13-field Playwright surface: `animations`, `caret`, `clip`, `fullPage`, `type` (emitted in `.d.ts` via `#[napi(js_name = "type")]` — Rust field renamed to `format` because `type` is reserved), `mask`, `maskColor`, `omitBackground`, `path`, `quality`, `scale`, `style`, `timeout`. Matches `/tmp/playwright/packages/playwright-core/types/types.d.ts:23280` byte-for-byte.
  - **Core**: `ScreenshotOptions` + `ClipRect` in `options.rs`; the backend-level `ScreenshotOpts` wire struct in `backend/mod.rs` gains matching fields plus three sibling enums (`ScreenshotScale`, `ScreenshotAnimations`, `ScreenshotCaret`). `Page::screenshot` lowers the Playwright bag into the wire struct, handles Rust-side `path` write-to-disk, and wraps the whole capture in a `tokio::time::timeout` race when `timeout > 0`.
  - **Shared JS helpers** (`backend::screenshot_js`): `build_css`, `install_style_js`, `uninstall_style_js`, `install_mask_js`, `uninstall_mask_js`. All three backends install/teardown through the same helpers so caret-hide, `animations: "disabled"` (via CSS `animation-play-state: paused`), user `style`, and mask overlays produce the same observable DOM state regardless of protocol.
  - **CDP** (`cdp/mod.rs:screenshot`): caret/style/mask via `Runtime.evaluate`; `omitBackground` via `Emulation.setDefaultBackgroundColorOverride` (transparent RGBA); `clip`/`fullPage` via `Page.captureScreenshot` + `captureBeyondViewport`; `scale: "css"` via `clip.scale = 1 / devicePixelRatio`. Mirrors `crPage.ts:_screenshotter`.
  - **BiDi** (`bidi/page.rs:screenshot`): caret/style/mask via `script.callFunction`; `clip` via the native `browsingContext.captureScreenshot` clip field. `omitBackground` and `scale: "css"` return typed `Unsupported` errors — Firefox/BiDi has no protocol command for either, and Playwright's own BiDi path leaves both unwired.
  - **WebKit** (`webkit/mod.rs:screenshot`): caret/style/mask via `evaluate`. `clip`, `omitBackground`, and `scale: "css"` return typed `Unsupported` — `WKWebView`'s `takeSnapshotWithConfiguration:` has no clip parameter, always composites against the view background, and captures at device-pixel scale.
  - **NAPI** (`types.rs`, `page.rs`): `ScreenshotOptions` + `ClipRect` `#[napi(object)]` surfaces; `mask: Option<Vec<LocatorRef>>` accepts the selector string; `scale` / `caret` / `animations` / `type` carry `ts_type` annotations so the generated `.d.ts` emits precise string-literal unions matching Playwright.
  - **QuickJS** (`bindings/page.rs`): `JsScreenshotOptions` covers every field, lowers into `ScreenshotOptions` with path → `PathBuf`, clip → `ClipRect`, `mask` as selector strings.
  - **Tests**: 18 new NAPI live-browser cases across three backends (full page / element / type / clip / omitBackground / path-to-disk / mask / style) plus the existing full-page / element-screenshot coverage. All green on cdp-pipe, cdp-raw, bidi, webkit — where a backend refuses an option (clip/omit/scale), the test skips that backend explicitly rather than asserting a fiction.
- **Playwright ref**: `/tmp/playwright/packages/playwright-core/types/types.d.ts:23280`, `packages/playwright-core/src/server/chromium/crPage.ts:_screenshotter`.

### 3.4 PDFOptions complete

- [x] Full 15-field struct: `format, path, scale, display_header_footer, header_template, footer_template, print_background, landscape, page_ranges, width, height, margin { top, right, bottom, left }, prefer_css_page_size, outline, tagged`.
- **Files**: `options.rs`, `page.rs:pdf`.

### 3.5 URL matching unification

- [x] Introduce `UrlMatcher = Glob | Regex | Predicate`.
- Used by: `page.route`, `context.route`, `page.wait_for_url`, `page.wait_for_request`, `page.wait_for_response`, `route_from_har.url`.
- **Files**: new `crates/ferridriver/src/url_matcher.rs`; call sites in `page.rs`, `context.rs`, `route.rs`.
- **NAPI**: accept `string | RegExp | (url: string) => boolean`.

### 3.6 Selector `and` semantic fix

- [x] `Locator::and` is currently implemented as `>>` (descendant chain) at `locator.rs:763` — must be `internal:and=<json>`.
- **Files**: `locator.rs`.
- **Tests**: `locator.locator('button').and(locator.locator(':visible'))` matches elements that are both.

### 3.7 Strict mode

- [x] Locator actions default to strict (error on multi-match); add `LocatorOptions { strict: bool }` override.
- **Files**: `locator.rs`.
- **Blocks on**: 1.1 (needs `StrictModeViolation`).
- **Tests**: multi-match click throws strict error.

### 3.8 Frame async-vs-sync parity

- [x] Playwright's `mainFrame`, `frames`, `frame`, `parentFrame`, `childFrames`, `isDetached`, `name`, `url` are sync. Shipped as sync across Rust core, NAPI, and QuickJS.
  - **Core**: new `crates/ferridriver/src/frame_cache.rs` is a Page-owned cache (`FxHashMap<Arc<str>, FrameRecord>` + insertion-order Vec + cached main-frame id). Seeded by `Page::init_frame_cache().await` (called from `BrowserContext::new_page`, `BrowserContext::pages`, and all three MCP page-creation paths in `ferridriver-mcp`), then kept fresh by a tokio task that consumes `FrameAttached` / `FrameDetached` / `FrameNavigated` from the page's own emitter. `Frame` now stores just `(Arc<Page>, Arc<str>)` — all accessors read live state from the cache. `Page::main_frame()`, `Page::frames()`, `Page::frame(selector)`, `Frame::name()`, `Frame::url()`, `Frame::parent_frame()`, `Frame::child_frames()`, `Frame::is_detached()`, `Frame::is_main_frame()` are all sync. New `FrameSelector { name, url }` lookup struct (`From<&str>` / `From<String>`) mirrors Playwright's `string | { name?, url? }` union; URL matcher stays string-equality for now — task 3.12 extends to `StringOrRegex`.
  - **NAPI** (`crates/ferridriver-node/src/frame.rs`, `page.rs`, `types.rs`): `frame.name()`, `frame.url()`, `frame.isMainFrame()`, `frame.parentFrame()`, `frame.childFrames()`, `frame.isDetached()`, `page.mainFrame()`, `page.frames()` are all sync methods (no Promise). `page.frame(selector)` takes `string | { name?, url? }` via `napi::Either<String, FrameSelectorBag>` + `ts_args_type` forcing the exact union in the generated `.d.ts`. Verified against `crates/ferridriver-node/index.d.ts`.
  - **QuickJS** (`crates/ferridriver-script/src/bindings/frame.rs` — new): `FrameJs` class with sync `name`/`url`/`isMainFrame`/`parentFrame`/`childFrames`/`isDetached` plus async `evaluate`/`evaluateStr`/`title`/`content`/`locator`. `PageJs` gains `mainFrame`, `frames`, `frame(selector)` — the `frame` union walks the JS object by hand so both `frame("alpha")` and `frame({ name: "alpha" })` reach the core `FrameSelector`. Registered in `bindings/mod.rs::define_classes`. Action methods (`click`/`fill`/`hover`/etc.) are deferred to task 3.9.
  - **WebKit** (`crates/ferridriver/src/backend/webkit/mod.rs`): `get_frame_tree` previously returned only the main frame. Now also probes the DOM via JS for `<iframe>` elements and emits one `FrameInfo` per iframe (synthesized `iframe-<view>-<idx>` ids). Frame-scoped JS evaluation still falls back to the main frame — tracked separately.
  - **Selector-engine fixes** (pre-existing failures surfaced while fixing this task): `crates/ferridriver/src/selectors.rs` now accepts `internal:has`, `internal:has-text`, `internal:has-not`, `internal:has-not-text` as aliases for the bare engines (Playwright's `client/locator.ts:51-67` emits the `internal:` prefix; `server/selectors.ts:42-43` accepts both). This unblocks `FilterOptions { has_text, has, has_not_text, has_not, visible }` at runtime — the signatures shipped in 3.11 but the engine dispatch was missing. Also fixed a stale test in `tests/page_api.rs` that expected `.box.and(.text)` to behave like `locator(inner)` (descendant) when Playwright's `.and()` is intersection on the same element.
- **Playwright refs**:
  - `/tmp/playwright/packages/playwright-core/src/client/frame.ts:258-276` (Frame sync accessors)
  - `/tmp/playwright/packages/playwright-core/src/client/page.ts:258-275` (Page main-frame/frames/frame)
  - `/tmp/playwright/packages/playwright-core/types/types.d.ts:2755-2765` (page.frame union)
- **Tests**:
  - NAPI: three new cases in `crates/ferridriver-node/test/frame-events.test.ts` (`sync accessors: mainFrame + parentFrame`, `sync accessors: frames + frame(name) + childFrames stay consistent`, `sync accessors: frame({ name, url }) union — object form`).
  - QuickJS: `test_script_frame_sync_accessors` and `test_script_frame_selector_union` in `crates/ferridriver-cli/tests/backends.rs` — run on all four backends (cdp-pipe, cdp-raw, bidi, webkit).
  - Unit: four `FrameCache` tests in the module itself (`seed`, `attach`, `detach`, `navigated` + `child_ids_filters_detached`).

### 3.9 Frame action methods

- [x] Full Playwright-faithful Frame surface, end-to-end across Rust core + NAPI + QuickJS. Architectural refactor merged: `Frame` is now the resolution primitive; `Page` is a pure facade over `mainFrame`; `Locator` carries a `Frame` (not `(Arc<Page>, Option<frame_id>)`). Mirrors `client/page.ts:658+` (Page action delegators) and `client/frame.ts:296-447` (Frame action surface).
  - **Action methods on Frame** (Rust core, `crates/ferridriver/src/frame.rs`): `click`, `dblclick`, `hover`, `tap`, `focus`, `fill`, `type`, `press`, `check`, `uncheck`, `set_checked`, `select_option`, `set_input_files`, `drag_and_drop`, `dispatch_event`, `text_content`, `inner_text`, `inner_html`, `get_attribute`, `input_value`, `is_visible`, `is_hidden`, `is_enabled`, `is_disabled`, `is_editable`, `is_checked`, plus the previously-missing locator builders `get_by_alt_text`, `get_by_title`, `frame_locator`. Each delegates to `self.locator(selector, None).<method>().await`.
  - **Page becomes a facade**: `Page::locator/get_by_*/click/dblclick/fill/.../is_checked/frame_locator` all reduce to `self.main_frame().<method>(...)`. `Page::main_frame()` returns `Frame` (non-null), seeded inside `Page::new` / `Page::with_context` via async constructors. The previous `init_frame_cache().await?` plumbing collapses into the constructor — every page-construction path (`BrowserContext::new_page`, `BrowserContext::pages`, `MCP::page`, `MCP::page_and_context`, `tools/navigation::page` "new"/"select") now does `Page::new(any_page).await?`.
  - **Locator-on-Frame** (`crates/ferridriver/src/locator.rs`): struct is now `Locator { frame: Frame, selector: String, strict: bool }` — was `(Arc<Page>, String, Option<Arc<str>>, bool)`. `Locator::new(frame, selector)` is the single constructor; `chain`/`filter`/`first`/`last`/`nth`/`get_by_*`/`strict` all preserve the frame. `Locator::page() -> &Arc<Page>` derives from `frame.page_arc()`. The `retry_resolve!` macro and every action path threads `self.frame.is_main_frame() ? None : Some(self.frame.id())` to the backend so element resolution runs in the right execution context.
  - **`FrameLocator` as a sync selector-builder**: `crates/ferridriver/src/locator.rs::FrameLocator` is a builder (no separate Locator type) that produces standard parent-frame `Locator`s with `>> internal:control=enter-frame >>` selector chains — verbatim Playwright's `client/locator.ts::FrameLocatorImpl` model. Sync `locator`, `get_by_*`, `owner`, `frame_locator`, `first`, `last`, `nth`. Async `resolve_frame_id` is gone.
  - **Backend parity**: `AnyPage::evaluate_to_element(js, frame_id: Option<&str>)` is the single resolution method (CDP threads `Runtime.evaluate.contextId`; BiDi threads the browsing-context realm; WebKit falls back to the main page until per-frame `WKFrameInfo` evaluation lands). `selectors::query_one` and `query_all` similarly take `frame_id: Option<&str>` so the strict-mode tagging path also runs in the iframe's context.
  - **Selector engine = verbatim Playwright**: replaced ferridriver's port of `crates/ferridriver/src/injected/{ariaSnapshot, consoleApi, domUtils, highlight, injectedScript, layoutSelectorUtils, roleSelectorEngine, roleUtils, selectorEngine, selectorEvaluator, selectorGenerator, selectorUtils, utilityScript, xpathSelectorEngine}.ts` with the upstream files from `/tmp/playwright/packages/injected/src/`. Same for `crates/ferridriver/src/injected/isomorphic/{ariaSnapshot, cssParser, cssTokenizer, locatorGenerators, locatorParser, locatorUtils, selectorParser, stringUtils, utilityScriptSerializers, yaml}.ts` from `/tmp/playwright/packages/isomorphic/`. The build (`bun build.ts`) gained an `inlineCssPlugin` that resolves Playwright's `import css from './highlight.css?inline'` to the file contents as a string, so the upstream sources stay literally byte-for-byte. Engine bundle: 163.9 KB minified.
  - **CDP engine injection upgrade**: `InjectedScriptManager::ensure` now uses `Page.addScriptToEvaluateOnNewDocument({source, runImmediately: true})` instead of `Runtime.evaluate`. Auto-injects `window.__fd` into every current document (main frame + already-loaded iframes) AND every future document (page navigations + new iframes). Without this, an iframe-bound `Locator`'s `evaluate_to_element(js, Some(iframe_id))` would query an execution context with no `window.__fd` and silently fail.
  - **Iframe click coordinates**: `CdpElement::click()` walks the frame chain via `window.frameElement.getBoundingClientRect()` and accumulates per-iframe offsets so a button inside an iframe lands at the right top-level page coords. Playwright achieves this via per-frame CDP sessions; we have a single session, so the offset math runs in JS at click time.
  - **NAPI** (`crates/ferridriver-node/src/{frame,page}.rs`): `Frame` exposes the full action surface (sync getters + async actions). `Page::main_frame()` returns `Frame` (non-null). `Page::set_checked` and `Page::tap(selector)` added. `innerHTML` uses `js_name = "innerHTML"` so the generated `.d.ts` matches Playwright (not `innerHtml`). Verified `crates/ferridriver-node/index.d.ts` against `/tmp/playwright/packages/playwright-core/types/types.d.ts`.
  - **QuickJS** (`crates/ferridriver-script/src/bindings/frame.rs`): `FrameJs` exposes the full action surface — `click`/`dblclick`/`hover`/`tap`/`focus`/`fill`/`type`/`press`/`check`/`uncheck`/`setChecked`/`selectOption`/`setInputFiles`/`dragAndDrop`/`dispatchEvent`/`textContent`/`innerText`/`innerHTML`/`getAttribute`/`inputValue`/`isVisible`/`isHidden`/`isEnabled`/`isDisabled`/`isEditable`/`isChecked`. `PageJs::main_frame()` returns `FrameJs` (non-null).
- **Playwright refs**:
  - `/tmp/playwright/packages/playwright-core/src/client/frame.ts:296-447` (Frame action methods)
  - `/tmp/playwright/packages/playwright-core/src/client/page.ts:658+` (Page-as-facade pattern)
  - `/tmp/playwright/packages/playwright-core/src/client/locator.ts::FrameLocatorImpl` (FrameLocator as selector builder)
- **Tests**:
  - NAPI: 4 new cases in `crates/ferridriver-node/test/frame-events.test.ts` (`frame.fill + frame.inputValue write/read inside iframe`, `frame.click + frame.textContent affect iframe DOM only`, `frame.check + frame.isChecked toggle a checkbox in the iframe`, `frame.getAttribute + frame.isVisible read iframe DOM`).
  - 4 backend suites green (cdp-pipe, cdp-raw, bidi, webkit).

### 3.10 NAPI dragAndDrop signature

- [x] Replace coord-based `(fromX, fromY, toX, toY)` with selector-based `(source, target, options)` matching Playwright's `page.dragAndDrop(source, target, options?)` and `locator.dragTo(target, options?)`. Options surface: `force`, `noWaitAfter`, `sourcePosition`, `targetPosition`, `steps` (default `1`), `strict` (page-only), `timeout`, `trial`. Wired through Rust core + NAPI + QuickJS with live-browser tests on all four backends (cdp-pipe, cdp-raw, bidi, webkit). WebKit backend gained per-drag `mouseDragged`/`_doAfterProcessingAllPendingMouseEvents:` drain handling so `steps` produces one DOM `mousemove` per step instead of AppKit-coalesced pairs.
- **Playwright ref**: `/tmp/playwright/packages/playwright-core/types/types.d.ts:2486` (page), `:13293` (locator).

### 3.11 Locator filter `visible` + Locator-object `has`/`has_not`

- [x] `FilterOptions { visible: Option<bool>, has: Option<Locator>, has_not: Option<Locator> }` — current strings-only `has`/`hasNot` must accept `Locator`.
- **Files**: `options.rs`, `locator.rs`.

### 3.12 Locator / getBy regex args

- [ ] `StringOrRegex` param on `get_by_role.name`, `get_by_text`, `get_by_label`, `get_by_placeholder`, `get_by_alt_text`, `get_by_title`, `get_by_test_id`; on `get_attribute` compare, `wait_for_url`, etc.
- **Files**: `options.rs`, `locator.rs`.
- **NAPI**: accept `string | RegExp`.

### 3.13 Locator-as-argument to `.locator()`

- [x] `locator.locator(selector_or_locator, options)` where the arg can be another Locator — Playwright encodes as `internal:chain=<json>`.
- **Files**: `locator.rs`.

### 3.14 Locator `evaluate` with arg

- [ ] `evaluate(fn, arg, options)` — serialize arg through JSHandle protocol.
- **Blocks on**: 1.3.

### 3.15 `getAttribute` return type

- [x] Return raw attribute string (not JSON-stringified). Current `locator.rs:473` JSON-stringifies non-string values.

### 3.16 `Locator.waitFor` `attached` state

- [x] Treat `attached` distinctly from `visible` (current code at `locator.rs:612` conflates them).

### 3.17 Auto-waiting deadline parity

- [ ] Replace fixed backoff `[0,0,20,50,100,100,500]` at `locator.rs:922` with Playwright's exponential polling + deadline propagation; per-call timeout overrides `context.set_default_timeout`.

### 3.18 `Locator.or` semantics

- [x] Current impl uses CSS `:is()` — must use `internal:or=<json>`.
- **Files**: `locator.rs`.

### 3.19 `Browser.version` returns real version

- [x] Currently returns engine name; must return CDP `Browser.getVersion().product`.
- **Files**: `browser.rs:167`.

### 3.20 `Browser.close({reason})`

- [x] Accept reason string; forward to CDP `Browser.close` with reason persisted on downstream `TargetClosedError`.

### 3.21 `Page.close({runBeforeUnload, reason})`

- [x] Add options.
- **Files**: `page.rs`.

### 3.22 `Page.opener` / `page.on('popup')`

- [ ] Track creator page for new targets; emit popup event.
- **Files**: `page.rs`, `events.rs`.

### 3.23 `Page.setDefaultNavigationTimeout`

- [x] Split from `set_default_timeout`.
- **Files**: `page.rs`, `context.rs`.

### 3.x Unified launch surface

- [x] `BrowserState` now has a single construction path —
  `with_options(ConnectMode, LaunchOptions)`. The previous
  `BrowserState::new(mode, backend)` shortcut that hard-coded
  `headless = false` is deleted, eliminating the class of bugs that
  surface when `headless` and chromium-binary selection drift (the
  cause of the MCP `emulateMedia` null-reset regression). MCP's
  `McpServer::with_options` constructs a `LaunchOptions`; the five
  test callers in `state.rs` go through a shared `test_state()`
  helper that also takes the bag. CLI `--headless` help text is
  corrected (it defaults to `false` — MCP's canonical use case is
  interactive).

### 3.24 `emulateMedia` full field set

- [x] Accept the full Playwright options bag (`media`, `colorScheme`, `reducedMotion`, `forcedColors`, `contrast`) as a single object argument across all three layers — no more positional NAPI args. Every field is `T | null | undefined` per Playwright's TS declaration: absent = no change, `null` = reset override, string = apply.
  - **Core**: new three-state `MediaOverride` enum + per-page persistent state so multiple `emulateMedia` calls compose (each call is a partial update).
  - **Backends**: CDP sends all four features every call with empty-string for disabled (mirrors Playwright's `_updateEmulateMedia`); WebKit wire protocol now carries a per-field action byte (unchanged / disabled / set) and installs a JS `matchMedia` interceptor that composes with the native `_setOverrideAppearance:` dark-mode override; BiDi honours `colorScheme` via `emulation.setForcedColorsModeThemeOverride` and returns a typed `Unsupported` for the four fields Firefox/BiDi has no protocol for (`media`, `reducedMotion`, `forcedColors`, `contrast`) rather than silently no-op'ing.
  - **NAPI**: `EmulateMediaOptions` fields use `Option<Either<String, Null>>` + `ts_type` so the generated `.d.ts` matches Playwright byte-for-byte (`null | 'light' | 'dark' | 'no-preference'` unions).
  - **QuickJS**: `parse_emulate_media_options` walks the JS object manually so `undefined` → Unchanged, `null` → Disabled, string → Set — serde-based parsing conflates null and undefined, which breaks the Playwright contract.
  - **Tests**: eight new live-browser NAPI cases (per-field + compose + no-op + single-field null-disables) plus two QuickJS-via-MCP cases (all-fields compose + single-field null-disable). All green on cdp-pipe, cdp-raw, webkit; BiDi skips the four fields Firefox has no protocol for. An earlier divergence where the MCP path appeared to silently ignore the null-reset turned out to be `BrowserState::new` picking full Chrome (retains macOS dark-mode) instead of Headless Shell — rewrote as `new_with_headless` to resolve the binary against the real headless flag (see Section B #4).
- **Playwright ref**: `/tmp/playwright/packages/playwright-core/types/types.d.ts:2580`.

### 3.25 `addInitScript` with arg

- [x] Full Playwright union at every layer: `Function | string | { path?, content? }` + optional `arg`. Wire stays source-only (per Playwright's client); all semantic lowering lives in Rust core:
  - **Core**: new `options::InitScriptSource` enum (`Function { body } | Source | Content | Path`) + `options::evaluation_script(script, arg)` helper mirroring `/tmp/playwright/packages/playwright-core/src/client/clientHelper.ts:31` — composes `(body)(arg)` with `arg` JSON-stringified, renders absent `arg` as the literal `undefined`, preserves `null` (JSON `"null"`), reads `{ path }` from disk and appends `//# sourceURL=…`, and rejects `(source|content|path) + arg` with Playwright's exact `"Cannot evaluate a string with arguments"` error via `FerriError::InvalidArgument`. 10 unit tests cover every branch (undefined/null/object args on Function; string/content/path + arg rejection; path read + sourceURL; missing-path error).
  - **NAPI**: `NapiInitScript` custom `FromNapiValue` synchronously turns `Function | string | object` into the `Send`-safe enum (function `.toString()` is called at unmarshal time, sidestepping the `!Send` `Unknown<'_>` across-await problem); `NapiInitScriptArg` custom `FromNapiValue` distinguishes JS `undefined` (→ `None` → renders as `undefined`) from explicit `null` (→ `Some(Value::Null)` → renders as `"null"`). `#[napi(ts_args_type = …)]` forces the generated `.d.ts` union byte-for-byte; six new `bun test` cases cover all forms + the string+arg error path.
  - **QuickJS**: shared `bindings/convert::init_script_from_js` does the same lowering (reads `String(fn)` for function source, recognises `.is_null()`/`.is_undefined()`, `content` wins over `path`). `PageJs::addInitScript` and `PageJs::removeInitScript` added to the Page surface (previously Context-only); one backends test exercises Function+arg / Function+no-arg / Function+null / `{ content }` / string+arg error across all four backends.
- **Playwright ref**: `/tmp/playwright/packages/playwright-core/src/client/page.ts:520`.

### 3.26 `exposeBinding`

- [ ] Promote `page.expose_function` to `exposeBinding` with `source = { page, frame, context }`; add `{ handle: bool }` option.
- **Files**: `page.rs`, `context.rs` (context-level exposeFunction/exposeBinding).

---

## Tier 4 — BrowserContext creation options

### 4.1 BrowserContextOptions

- [ ] Accept the full 28-field option object at context creation.
- **Playwright ref**: `types.d.ts` `BrowserContextOptions`.
- **Fields**: `accept_downloads`, `base_url`, `bypass_csp`, `client_certificates`, `color_scheme`, `device_scale_factor`, `extra_http_headers`, `forced_colors`, `contrast`, `geolocation`, `has_touch`, `http_credentials`, `ignore_https_errors`, `is_mobile`, `javascript_enabled`, `locale`, `offline`, `permissions`, `proxy`, `record_har`, `record_video`, `reduced_motion`, `screen`, `service_workers`, `storage_state`, `strict_selectors`, `timezone_id`, `user_agent`, `viewport`.
- **Files**: `crates/ferridriver/src/options.rs` add `BrowserContextOptions`; `context.rs` consume; `browser.rs:new_context`.
- **Tests**: each field applied to a fresh context and verified page-side.

### 4.2 `context.storageState({ path?, indexedDB? })`

- [ ] Move from `page.rs:1026` to `context.rs`; add `indexedDB` capture via CDP `IndexedDB` domain.
- **Files**: `context.rs`, delete from `page.rs`.

### 4.3 `context.setStorageState(state)` with indexedDB restore

- [ ] Symmetric with 4.2.

### 4.4 `context.request` accessor

- [ ] Attach `APIRequestContext` to `ContextRef`, share cookies/headers.
- **Files**: `context.rs`, `api_request.rs`.

### 4.5 `context.tracing` accessor

- [ ] Attach tracing controller to context.
- **Blocks on**: 2.3.

### 4.6 `context.clock` accessor

- [ ] Attach clock.
- **Blocks on**: 2.2.

### 4.7 `context.route / unroute / unroute_all`

- [ ] Both Rust core and NAPI — current state: Rust has `context.route` (`context.rs:502`) + `unroute`, but NAPI has no binding. Add `unroute_all({behavior})` too.
- **Files**: `context.rs`; NAPI `context.rs`.

### 4.8 `context.route_from_har`

- [ ] **Blocks on**: 2.6.

### 4.9 `context.route_web_socket`

- [ ] **Blocks on**: 2.8.

### 4.10 `context.background_pages` + `context.service_workers`

- [ ] **Blocks on**: 2.7.

### 4.11 Context events

- [ ] Emit `close, console, dialog, page, request, requestfailed, requestfinished, response, serviceworker, weberror, backgroundpage`.
- **Files**: `context.rs`, `events.rs`.

### 4.12 `context.wait_for_event` / `wait_for_condition`

- [ ] Context-level polling.

### 4.13 `context.set_http_credentials`

- [ ] Current at page-level only (`page.rs:956`); move to context with per-page apply.

### 4.14 `context.cookies(urls?)` URL filter

- [ ] Per `cookies()` at `context.rs:313` — add filter.

### 4.15 `context.clear_cookies(options)` regex filters

- [ ] `name|domain|path` as string-or-regex.

### 4.16 `partitionKey` (CHIPS) on Cookie

- [ ] Add field to `CookieData`; backend must round-trip via CDP `Network.Cookie.partitionKey`.

### 4.17 Context-level `set_geolocation/set_offline` apply to future pages

- [ ] Current impl iterates existing pages; new pages need to inherit. Store on `ContextRef` and apply at `new_page`.

---

## Tier 5 — Route / APIRequest

### 5.1 `Route.fallback(options)`

- [ ] Chain-of-handlers fallback.
- **Files**: `route.rs`.

### 5.2 `Route.fetch(options)`

- [ ] Wrapped network call returning `APIResponse`.
- **Blocks on**: 1.4.

### 5.3 `Route.fulfill` shortcuts

- [ ] Accept `response: APIResponse`, `json: Value`, `path: PathBuf`.
- **Files**: `route.rs`.

### 5.4 Route URL matcher regex/function

- [ ] **Blocks on**: 3.5.

### 5.5 `Route.times` option

- [ ] Retire route after N calls.

### 5.6 `APIRequestContext.storage_state`

- [ ] Capture cookies from request jar.
- **Files**: `api_request.rs`.

### 5.7 `APIRequestContext` multipart

- [ ] `FormData` body with `filePayload`.
- **Files**: `api_request.rs`.

### 5.8 `APIRequestContext.fetch(request, options)` Request overload

- [ ] Accept `Request` object and copy method/headers/body.
- **Blocks on**: 1.4.

### 5.9 `APIRequestContext` max-redirects enforcement

- [ ] Current TODO at `api_request.rs:340-344`.

### 5.10 `APIRequestContext` per-request `ignore_https_errors`

- [ ] Plumb option to reqwest builder per call.

### 5.11 `APIResponse.headers_array`, `header_value`, `header_values`, `from_service_worker`, `dispose`-then-error

- [ ] Full surface on APIResponse.

### 5.12 `APIRequestContext` `client_certificates`, `proxy`, `base_url` path-join semantics

- [ ] Match Playwright's base-URL join rules (percent-decode, trailing-slash behavior).

---

## Tier 6 — Events

### 6.1 Page: `crash`, `popup`, `requestfailed`, `requestfinished`, `worker`, `websocket`, `filechooser`

- [ ] Emit all of these via `PageEvent` additions; wire from CDP.
- **Files**: `events.rs`, `page.rs`.

### 6.2 Context events

- [ ] Per 4.11.

### 6.3 Frame events on Page

- [ ] `frameattached`, `framedetached`, `framenavigated` already present — verify payload shape matches Playwright's `Frame` object (currently a `FrameInfo` struct).

---

## Tier 7 — Test runner

### 7.1 `--project` flag + project-dependency DAG

- [ ] Wire `ProjectConfig[]` (already at `config.rs:431`) through the runner. Add `--project` multi-flag, `dependencies`, `teardown`, `--no-deps`, `--teardown`, `-x`.
- **Files**: `crates/ferridriver-test/src/config.rs`, `discovery.rs`, `runner.rs`, `bin/*.rs`, `packages/ferridriver-test/src/cli.ts`.

### 7.2 `globalTimeout`, `--global-timeout`

- [ ] Top-level timeout across whole run.

### 7.3 `--only-changed`

- [ ] Git-diff-based file selection.

### 7.4 `--fail-on-flaky-tests`

- [ ] Non-zero exit on any flaky (retried-then-passed) test.

### 7.5 `--ignore-snapshots`

- [ ] Skip snapshot comparisons at runtime.

### 7.6 `--tsconfig`

- [ ] Pass through to TS compile step.

### 7.7 `--ui` mode

- [ ] Web UI (Playwright renders with a Vite-served React app under `packages/playwright/src/uiMode/`). For ferridriver, decide: ship a minimal web UI or keep TUI-only. **Spec**: match Playwright's watch/inspect/time-travel, served at `127.0.0.1:<port>/ui`. Multi-week item.

### 7.8 `--update-source-method` + proper `-u [mode]` parsing

- [ ] `mode = all | changed | missing | none`.

### 7.9 `--pass-with-no-tests`, `--repeat-each`, `--max-failures`, `-x` CLI flags

- [ ] Surface existing config fields to CLI.

### 7.10 `TestInfo` helpers

- [ ] Add `output_path(...)`, `snapshot_path(...)`, `pause()`, `fn`, `project`, `config`, `errors[]`, `snapshot_suffix`, column on location.
- **Files**: `crates/ferridriver-test/src/model.rs`; NAPI `test_info.rs`.

### 7.11 Generic Jest matchers in TS wrapper

- [ ] Implement `toBe, toBeCloseTo, toBeDefined, toBeFalsy, toBeGreaterThan, toBeGreaterThanOrEqual, toBeInstanceOf, toBeLessThan, toBeLessThanOrEqual, toBeNaN, toBeNull, toBeTruthy, toBeUndefined, toContain, toContainEqual, toEqual, toHaveLength, toHaveProperty, toMatch, toMatchObject, toStrictEqual, toThrow, toThrowError`.
- **Rule**: core matching logic in Rust (as `Matcher` trait), TS wrapper calls NAPI — do not implement in TS alone.

### 7.12 Asymmetric matchers

- [ ] `expect.any, anything, arrayContaining, closeTo, objectContaining, stringContaining, stringMatching`.

### 7.13 `.resolves` / `.rejects`

- [ ] Promise-unwrapping modifiers.

### 7.14 `.soft` + `.poll` exposed in TS

- [ ] Rust has them; TS wrapper missing.

### 7.15 `expect.extend`

- [ ] Register custom matchers from TS into NAPI registry.

### 7.16 APIResponse matcher `toBeOK`

### 7.17 Locator matcher advanced options

- [ ] `toHaveScreenshot`: `mask, mask_color, animations, caret, clip, scale, style_path, max_diff_pixels, max_diff_pixel_ratio, threshold`.
- [ ] `toBeInViewport { ratio }`.
- [ ] `toHaveCSS { pseudo }`.
- [ ] `toMatchAriaSnapshot` — rewrite to call the bundled `injected/ariaSnapshot.ts` YAML generator and structural matcher (currently a naive substring walker at `expect/locator.rs:697`).

### 7.18 Fixtures: `browserName`, `browserVersion`, `playwright`, `request` as first-class

- [ ] Register in `fixture.rs` built-ins.

### 7.19 Fixture `auto: true` enforcement

- [ ] TS parses it; Rust pool ignores it (`fixture.rs`). Must resolve auto fixtures regardless of test request.

### 7.20 Reporters: `dot`, `github`, `blob`, `null`

- [ ] Implement under `crates/ferridriver-test/src/reporter/`.

### 7.21 `merge-reports` subcommand

- [ ] Merge blob reports across shards.

### 7.22 TS `Reporter` interface

- [ ] Allow user-authored JS/TS reporters with `onBegin/onTestBegin/onStepBegin/onStepEnd/onTestEnd/onEnd/onError/onStdOut/onStdErr/onExit/printsToStdio`. Bridge into Rust event bus.

### 7.23 CT adapters

- [ ] Ship `@ferridriver/ct-react`, `@ferridriver/ct-react17`, `@ferridriver/ct-vue`, `@ferridriver/ct-svelte`, `@ferridriver/ct-solid`. Vite plugin for TSX component-reference extraction (port `playwright-ct-core/src/tsxTransform.ts`).

### 7.24 CT `update()`, `beforeMount`, `afterMount`

- [ ] Extend `ct/mod.rs` mount API.

### 7.25 WebServer option polish

- [ ] Add `ignore_https_errors`, `graceful_shutdown`, `name` to `WebServerConfig`.

### 7.26 `captureGitInfo`

- [ ] Annotate test results with git metadata.

### 7.27 `updateSnapshots` mode parsing

- [ ] `all | changed | missing | none`.

### 7.28 Config top-level `name`, `tsconfig`

---

## Tier 8 — CLI subcommands (ferridriver binary + ferridriver-test)

- [ ] `ferridriver show-trace [trace.zip]` — trace viewer (or delegate to `npx playwright show-trace` once 2.3 produces a compatible zip).
- [ ] `ferridriver show-report [dir]` — HTML report server.
- [ ] `ferridriver merge-reports [dirs...]` — see 7.21.
- [ ] `ferridriver open [url]` — headed browser with inspector.
- [ ] `ferridriver screenshot <url> <path>` — one-shot screenshot.
- [ ] `ferridriver pdf <url> <path>` — one-shot pdf.
- [ ] `ferridriver install-deps` — OS deps installer (`apt-get install ...`).
- [ ] `ferridriver uninstall` — remove installed browsers.
- [ ] `ferridriver clear-cache`.
- [ ] `ferridriver run-server` — relay server for remote drive.
- [ ] Codegen action vocabulary extension: Hover, Drag, Upload, Frame, Keyboard shortcut.

---

## Tier 9 — NAPI surface gaps (Rust exists, binding missing)

- [ ] `page.expose_function` (Rust `page.rs:1614`).
- [ ] `page.set_bypass_csp`, `page.set_ignore_certificate_errors`, `page.set_download_behavior`, `page.set_http_credentials`, `page.set_service_workers_blocked`.
- [ ] `page.touchscreen()` — Mouse/Keyboard bound, Touchscreen missing.
- [ ] `page.start_screencast` / `stop_screencast`.
- [ ] `page.snapshot_for_ai`.
- [ ] `expect_navigation / expect_response / expect_request / expect_download` as awaitable returns.
- [ ] `frame.get_by_title`, `frame.page()`.
- [ ] `locator.content_frame`, `locator.frame_locator`, `locator.page`.
- [ ] `FrameLocator` entire class (Rust has it at `locator.rs:1030`).
- [ ] `context.clear_cookies_filtered` with regex options.
- [ ] Dialog / Download / ConsoleMessage / Worker / CDPSession / Video / WebSocket bindings — blocked on Tier 2 implementations first.

---

## Execution ordering (dependency DAG)

1. **Foundation round**: 1.1 (errors) → 1.2 (ElementHandle) + 1.3 (JSHandle) in parallel → 1.4 (Request/Response).
2. **Option-bag round**: 1.5 across all action methods; 3.3 (ScreenshotOptions), 3.4 (PDFOptions), 3.5 (URL matcher).
3. **Strict mode + Locator polish**: 3.6, 3.7, 3.11–3.18.
4. **Event classes**: 2.9 Dialog, 2.10 Download, 2.11 FileChooser, 2.12 ConsoleMessage, 2.13 WebError, 2.14 Video; then Tier 6 events.
5. **Frame parity**: 3.8, 3.9.
6. **BrowserContext options**: 4.1 + 4.7 (route) + 4.14–4.17 (cookies) concurrently.
7. **Missing subsystems**: 2.1 CDPSession → 2.2 Clock → 2.4 Coverage → 2.5 Selectors registry → 2.7 Worker → 2.8 WebSocket → 2.15 BrowserType → 2.6 HAR → 2.3 Tracing-v8 (largest single item, do last of Tier 2).
8. **Route / APIRequest**: Tier 5.
9. **Test runner**: 7.1 projects → 7.11/7.12/7.13/7.14/7.15 matchers → 7.17 advanced matcher options → 7.20/7.21/7.22 reporters → 7.23/7.24 CT → remaining CLI.
10. **NAPI sweep**: Tier 9 last, after all Rust pieces land.

---

## Per-item definition-of-done

Every checklist item must satisfy, before ticking `[x]`:

1. Rust core has the method with a structured-error return.
2. `just lint` passes with zero warnings (clippy `-D warnings`).
3. Rust unit or integration test exists and exercises every option field + the failure path.
4. NAPI binding added (unless intentionally Rust-only); exported in `packages/ferridriver/index.d.ts` via NAPI-RS.
5. `crates/ferridriver-node/__test__/` has a Bun test calling the new binding.
6. TS-side `packages/ferridriver-test/src/` uses or re-exports the feature where applicable.
7. If user-facing, a BDD step exists under `crates/ferridriver-bdd/src/steps/` and a feature file at `tests/features/`.
8. `just test` runs green for the changed crate backends (`cdp_pipe`, `cdp_raw`, and, where platform permits, `webkit`).
9. This doc's checkbox is updated in the same commit.

---

## Known non-goals

- **Electron / Android** clients (`packages/playwright-core/src/client/electron.ts`, `android.ts`) — out of scope unless explicitly requested.
- **Playwright Inspector** desktop app — `--ui` mode (7.7) replaces it.
- **Pyright / pytest integration** — ferridriver is TS + Rust only.

---

## Gaps surfaced by scripting bindings (`ferridriver-script`)

The `ferridriver-script` crate exposes core Rust types to QuickJS via rquickjs class/methods macros that mirror core's signatures **strictly** — no JS-side shims, no accept-and-drop of unsupported args. LLM-authored scripts written in Playwright style will hit the gaps below directly.

### A. Core-level gaps (core doesn't have it yet; fixing core fixes scripts)

1. **`evaluate(fn, arg?)` function argument** — see **1.3 JSHandle**. Core's `evaluate(&str)` accepts strings only; scripts must pass a literal string. The single most-used Playwright idiom (`page.evaluate(() => document.title)`) does not work until core accepts a serialized function.
2. **Action-method options (`click`/`fill`/`hover`/`press`/`type`/`dblclick`/`check`/`uncheck`/`tap`/`selectOption`/`dispatchEvent`/`dragTo`/`setInputFiles`)** — see **1.5 Action option bags**. Scripts passing `{ timeout, force, noWaitAfter, position, trial, modifiers, button, clickCount, delay }` will fail with an arity error; bindings refuse the extra arg rather than silently dropping it.
3. **`screenshot` / `pdf` option coverage** — see **3.3 ScreenshotOptions complete** and **3.4 PDFOptions complete**.
4. **`selectOption` value shape** — see **1.5**. Core takes a single string; Playwright accepts `string | { value, label, index } | ElementHandle` plus arrays.
5. **`setInputFiles` payload shape** — see **1.5**. Core takes paths only; Playwright accepts `FilePayload { name, mimeType, buffer }`.
6. **`dispatchEvent` `eventInit`** — see **1.5**.
7. **`addInitScript` with `arg`** — see **3.25**.
8. **`Locator.evaluate` / `evaluateAll` function + arg** — see **3.14**.
9. **`mouse.move(x, y)`** — missing from `ferridriver::page::Mouse` entirely. Playwright has it; many patterns (hover-then-wheel, free-form drag via down/move/up) require it. Today scripts must use `page.moveMouseSmooth(fromX, fromY, toX, toY, steps)` (page-level helper) or `page.clickAt`/`page.mouse.click(x, y)` which moves-and-clicks.
10. **Wait family** — core has `wait_for_selector`, `wait_for_url`, `wait_for_load_state`, `wait_for_function`, `wait_for_event`, `wait_for_response`, `wait_for_request`, `wait_for_download`, `wait_for_navigation`, `wait_for_timeout`. Scripts today only have `page.waitForSelector`. The rest need bindings (not core work — these already exist in core).
11. **Event handling** — core has `page.on/once/off`, `page.expect_navigation/response/request/download`. Scripts have no event surface yet. This is pure binding work once we decide on the JS callback lifetime model (fires-while-script-runs only, vs session-persistent).
12. **Routing** — `page.route/unroute`, `context.route/unroute` exist in core; scripts have no binding. See **4.7**, **4.8**, **4.9**, **5.1-5.5**.
13. **Locator chain methods not yet bound** — core has `locator.filter(opts)`, `locator.and(other)`, `locator.or(other)`, `locator.all()`. Scripts currently expose `first`/`last`/`nth`/`locator` only.
14. **Frames** — `page.main_frame`, `page.frames`, `page.frame`, `FrameLocator` exist in core; scripts have no frame binding. See **3.8**, **3.9**.
15. **Context-level gaps** — `context.storageState({ path, indexedDB })`, `clearCookies(options)` regex filters, `cookies(urls?)` URL filter — see **4.2**, **4.14**, **4.15**, **3.2**.
16. **`locator.evaluateHandle`, `elementHandle`** — core doesn't have `ElementHandle`/`JSHandle` yet (Tier **1.2** / **1.3**).
17. **Timeouts** — `page.setDefaultTimeout`, `page.setDefaultNavigationTimeout` bindings missing.

### B. Backend-specific failures surfaced by the test suite

These are not ferridriver bugs per se — they are backend-surface gaps. The tests document them.

1. ~~**WebKit: `context.addCookies` → `context.cookies()` round-trip drops the cookie**~~ — **fixed** alongside task #3.4. The Obj-C `OP_GET_COOKIES` handler was emitting `"http_only"` (snake_case) but Rust `CookieData` switched to `#[serde(rename_all = "camelCase")]` in `c820caf`, so serde rejected the whole entry (`.unwrap_or_default()` then collapsed to `[]`). Obj-C now emits `"httpOnly"` to match the Rust wire contract. All three backends round-trip cookies.
2. ~~**BiDi (Firefox): `page.dragAndDrop` fails with `scrollIntoViewIfNeeded is not a function`**~~ — **fixed** alongside task #3.10. The bounding-rect JS probe now does `try { this.scrollIntoViewIfNeeded(); } catch (e) { this.scrollIntoView(); }` so Firefox/BiDi falls back to the standards-compliant method. `page.dragAndDrop` and `locator.dragTo` pass on all four backends.
3. **CDP: `page.mouse.wheel(dx, dy)` does not reliably produce a page scroll**. CDP's `Input.dispatchMouseEvent` with `type: "mouseWheel"` requires mouse position routing that doesn't always land on the scrollable viewport. `test_script_mouse_wheel` asserts only that the call does not error, not that `window.scrollY` changed.
4. ~~**MCP `run_script` + CDP: `emulateMedia({colorScheme: null})` doesn't update `matchMedia`**~~ — **fixed** alongside 3.24. Root cause: `BrowserState::new(mode, backend)` hard-coded `resolve_chromium(false)`, so every MCP-launched browser picked the full `/Applications/Google Chrome.app` binary regardless of `--headless`, while the direct NAPI `Browser.launch` path went through `with_options(mode, LaunchOptions)` and correctly picked Chrome Headless Shell. Full Chrome retains the macOS system appearance (prefers-color-scheme: dark on dark-mode hosts), so CDP's `Emulation.setEmulatedMedia({features: [{name: 'prefers-color-scheme', value: ''}]})` correctly "cleared the override" — the override was `dark`, the cleared-state baseline was *also* `dark`, so the reset looked like a no-op. CDP wire commands were byte-identical; the divergence was in which browser was listening. Fix (commits `d6f810c` and `a3a42f0`): `BrowserState::new` is deleted entirely; every caller — MCP server, NAPI, in-tree tests — goes through `with_options(mode, LaunchOptions)`. Binary selection can no longer drift from the headless flag. `test_script_emulate_media_null_disables_single_field` restored and green on all backends.

### C. Test-level workarounds (honest list of relaxed assertions)

1. **`test_script_mouse_wheel`** — asserts `status === 'ok'` only; does not verify `window.scrollY > 0`. See **B.3**.
2. **`test_script_keyboard_press`** — accepts any non-empty input value OR any of `A`/`a`/`B`/`b`. Character-key CDP events (`page.keyboard.press('A')`) do not always insert the corresponding character in text inputs across backends. Playwright uses a richer key-code mapping we have not mirrored.

### Principle

Most Category A items resolve at the core layer; scripting bindings regenerate automatically when core signatures change. Category A items 9–17 are the ones waiting on new bindings rather than new core code. Category B items reflect real backend surface differences we need to harmonise. Category C assertions should tighten as B resolves.
