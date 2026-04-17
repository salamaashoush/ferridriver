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

- [ ] Add full Playwright option bags to every action method.
- **Playwright ref**: `LocatorClickOptions`, `LocatorHoverOptions`, `LocatorFillOptions`, `LocatorPressOptions`, `LocatorTypeOptions`, `LocatorCheckOptions`, `LocatorSetCheckedOptions`, `LocatorTapOptions`, `LocatorDblClickOptions`, `LocatorDragToOptions`, `LocatorScreenshotOptions`, `LocatorWaitForOptions` in `types.d.ts`.
- **Files**: `crates/ferridriver/src/options.rs` (new structs); `crates/ferridriver/src/locator.rs`; `crates/ferridriver/src/page.rs`; `crates/ferridriver-node/src/locator.rs`; `crates/ferridriver-node/src/page.rs`.
- **Per-option coverage** (all fields, not a subset):
  - Click: `button`, `click_count`, `delay`, `force`, `modifiers`, `no_wait_after`, `position`, `timeout`, `trial`.
  - Hover: `force`, `modifiers`, `no_wait_after`, `position`, `timeout`, `trial`.
  - Fill: `force`, `no_wait_after`, `timeout`.
  - Type: `delay`, `no_wait_after`, `timeout`.
  - Press: `delay`, `no_wait_after`, `timeout`.
  - Check/Uncheck/SetChecked: `force`, `no_wait_after`, `position`, `timeout`, `trial`.
  - Tap: `force`, `modifiers`, `no_wait_after`, `position`, `timeout`, `trial`.
  - DragTo: `force`, `no_wait_after`, `source_position`, `target_position`, `timeout`, `trial`.
  - DispatchEvent: `event_init` (serde_json::Value), `timeout`.
  - SelectOption: accept `string | { value, label, index } | ElementHandle` plus arrays; options `force`, `no_wait_after`, `timeout`.
  - SetInputFiles: `FilePayload { name, mime_type, buffer }` plus path; options `no_wait_after`, `timeout`.
- **Acceptance**: passing each option through NAPI reaches Rust core and lands on CDP.
- **Tests**: an option-matrix test crate exercising every field on every method.

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

- [ ] Add `referer: Option<String>` to `GotoOptions`.
- **Files**: `crates/ferridriver/src/options.rs`; CDP `Page.navigate` call site in `page.rs`.

### 3.3 ScreenshotOptions complete

- [ ] Add: `mask: Vec<Locator>`, `mask_color`, `clip`, `animations: Allow|Disabled`, `caret: Hide|Initial`, `scale: Css|Device`, `omit_background`, `style` (CSS injection), `path`, `timeout`, `type: Png|Jpeg`.
- **Files**: `options.rs`, `page.rs` screenshot path, `locator.rs` screenshot path.
- **Tests**: mask redacts regions; `omit_background: true` yields transparent png; `animations: 'disabled'` stops CSS animations.

### 3.4 PDFOptions complete

- [ ] Full 15-field struct: `format, path, scale, display_header_footer, header_template, footer_template, print_background, landscape, page_ranges, width, height, margin { top, right, bottom, left }, prefer_css_page_size, outline, tagged`.
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

- [ ] Playwright's `mainFrame`, `frames`, `frame`, `parentFrame`, `childFrames`, `isDetached`, `name`, `url` are sync. Make them sync in ferridriver by caching in `Page`/`Frame` state.
- **Files**: `page.rs`, `frame.rs`, NAPI bindings.
- **Tests**: sync access after navigation mutates reflected state.

### 3.9 Frame action methods

- [ ] Port `click/dblclick/fill/type/press/hover/check/uncheck/set_checked/tap/drag_and_drop/dispatch_event/select_option/set_input_files/text_content/inner_text/inner_html/get_attribute/input_value/is_checked/is_disabled/is_editable/is_enabled/is_hidden/is_visible/focus` to `Frame`.
- **Files**: `frame.rs`; NAPI `frame.rs`.
- **Implementation**: each method scopes the Locator call to the frame's execution context.

### 3.10 NAPI dragAndDrop signature

- [ ] Replace coord-based `(fromX, fromY, toX, toY)` at `crates/ferridriver-node/src/page.rs:623` with selector-based `(source, target, options)` matching Rust core and Playwright.

### 3.11 Locator filter `visible` + Locator-object `has`/`has_not`

- [ ] `FilterOptions { visible: Option<bool>, has: Option<Locator>, has_not: Option<Locator> }` — current strings-only `has`/`hasNot` must accept `Locator`.
- **Files**: `options.rs`, `locator.rs`.

### 3.12 Locator / getBy regex args

- [ ] `StringOrRegex` param on `get_by_role.name`, `get_by_text`, `get_by_label`, `get_by_placeholder`, `get_by_alt_text`, `get_by_title`, `get_by_test_id`; on `get_attribute` compare, `wait_for_url`, etc.
- **Files**: `options.rs`, `locator.rs`.
- **NAPI**: accept `string | RegExp`.

### 3.13 Locator-as-argument to `.locator()`

- [ ] `locator.locator(selector_or_locator, options)` where the arg can be another Locator — Playwright encodes as `internal:chain=<json>`.
- **Files**: `locator.rs`.

### 3.14 Locator `evaluate` with arg

- [ ] `evaluate(fn, arg, options)` — serialize arg through JSHandle protocol.
- **Blocks on**: 1.3.

### 3.15 `getAttribute` return type

- [ ] Return raw attribute string (not JSON-stringified). Current `locator.rs:473` JSON-stringifies non-string values.

### 3.16 `Locator.waitFor` `attached` state

- [ ] Treat `attached` distinctly from `visible` (current code at `locator.rs:612` conflates them).

### 3.17 Auto-waiting deadline parity

- [ ] Replace fixed backoff `[0,0,20,50,100,100,500]` at `locator.rs:922` with Playwright's exponential polling + deadline propagation; per-call timeout overrides `context.set_default_timeout`.

### 3.18 `Locator.or` semantics

- [x] Current impl uses CSS `:is()` — must use `internal:or=<json>`.
- **Files**: `locator.rs`.

### 3.19 `Browser.version` returns real version

- [ ] Currently returns engine name; must return CDP `Browser.getVersion().product`.
- **Files**: `browser.rs:167`.

### 3.20 `Browser.close({reason})`

- [ ] Accept reason string; forward to CDP `Browser.close` with reason persisted on downstream `TargetClosedError`.

### 3.21 `Page.close({runBeforeUnload, reason})`

- [ ] Add options.
- **Files**: `page.rs`.

### 3.22 `Page.opener` / `page.on('popup')`

- [ ] Track creator page for new targets; emit popup event.
- **Files**: `page.rs`, `events.rs`.

### 3.23 `Page.setDefaultNavigationTimeout`

- [ ] Split from `set_default_timeout`.
- **Files**: `page.rs`, `context.rs`.

### 3.24 `emulateMedia` full field set

- [ ] Verify `EmulateMediaOptions` at `options.rs:77-89` exposes all of `media`, `color_scheme`, `reduced_motion`, `forced_colors`, `contrast` through NAPI.

### 3.25 `addInitScript` with arg

- [ ] `add_init_script(script, arg)` — current signature is source-only.
- **Files**: `context.rs:445`, `page.rs`.

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

The `ferridriver-script` crate exposes core Rust types to QuickJS via a proc macro that mirrors core's signatures **strictly** — no JS-side shims, no accept-and-drop of unsupported args. This means LLM-authored scripts written in Playwright style will hit the gaps below directly. They are already tracked as Tier 1 / Tier 3 items; this section exists so future work knows which ones are highest-priority for the scripting surface.

1. **`evaluate(fn, arg?)` function argument** — see **1.3 JSHandle**. Core's `evaluate(&str)` accepts strings only; scripts must pass a literal string. The single most-used Playwright idiom (`page.evaluate(() => document.title)`) does not work until core accepts a serialized function.
2. **Action-method options (`click`/`fill`/`hover`/`press`/`type`/`dblclick`/`check`/`uncheck`/`tap`/`selectOption`/`dispatchEvent`/`dragTo`/`setInputFiles`)** — see **1.5 Action option bags**. Scripts passing `{ timeout, force, noWaitAfter, position, trial, modifiers, button, clickCount, delay }` will fail type-checking; QuickJS bindings refuse the extra arg rather than silently dropping it.
3. **`screenshot` / `pdf` option coverage** — see **3.3 ScreenshotOptions complete** and **3.4 PDFOptions complete**. Core accepts partial option sets today.
4. **`selectOption` value shape** — see **1.5**. Core takes a single string; Playwright accepts `string | { value, label, index } | ElementHandle` plus arrays.
5. **`setInputFiles` payload shape** — see **1.5**. Core takes paths only; Playwright accepts `FilePayload { name, mimeType, buffer }`.
6. **`dispatchEvent` `eventInit`** — see **1.5**. Core takes event type only; no `eventInit` dict.
7. **`addInitScript` with `arg`** — see **3.25**.
8. **`Locator.evaluate` / `evaluateAll` function + arg** — see **3.14**. Same shape as the Page-level gap above.
9. **Context-level features scripts commonly reach for** — `context.route` / `unroute` exist in core but are missing from NAPI; they will be exposed natively in QuickJS bindings. `context.storageState({ path, indexedDB })`, `clearCookies(options)` regex filters, and `cookies(urls?)` URL filter are all core-level gaps — see **4.7**, **4.14**, **4.15**, **3.2**, **4.2**.

**Principle**: resolving these gaps is a core concern, not a scripting concern. The `ferridriver-script` proc macro regenerates bindings automatically when core signatures change, so closing a Tier 1.5 item simultaneously closes the corresponding QuickJS gap.
