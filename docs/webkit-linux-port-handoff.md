# WebKit Linux port — session handoff

Branch: `main` (current working branch — uncommitted changes; review `git status` before commit).
Last session ended: 2026-05-20. Approach: ferridriver-webkit-host (`crates/ferridriver-webkit-host`) driving webkit6 0.6.1 on GTK4 with v2_52 feature.

## Status

**Test result on Linux (last run):** 181 passed, 5 failed of 186 backend integration tests.

Run with:

```sh
FERRIDRIVER_WEBKIT_HOST=$(pwd)/target/debug/ferridriver-webkit-host \
  cargo test -p ferridriver-cli --test backends all_tests_webkit -- --nocapture --test-threads=1
```

### Remaining 5 failures

| Test | Root cause |
|---|---|
| `test_script_check_uncheck` | Under headless GTK4 the checkbox click toggles via JS (`target.checked = !target.checked`) but `ferridriver`'s `check()` re-reads state via the selector engine and sees the old value. Likely the engine's `getChecked` runs in a JSCValue scope that doesn't see the JS-side property mutation across separate `evaluate_javascript_future` calls — possibly a webkit6 web-process snapshot issue. |
| `test_script_check_behavior` | Same root cause as `check_uncheck`. |
| `test_element_handle_temp_tag_actions` | Same — element handle's `.check()` path hits the same checkbox toggle issue. |
| `test_script_frame_sync_accessors` | iframe child-frame discovery on webkit. `get_frame_tree` finds the iframes via `querySelectorAll('iframe')`, but the frame-cache update happens via `PageEvent::FrameAttached` which webkit doesn't currently emit. Sync `frame.childFrames()` returns empty. Needs per-iframe event emission from `get_frame_tree` OR a polling listener. |
| `test_script_frame_selector_union` | Same — `page.frame('name')` reads the cache. |

`test_video_recording_lifecycle` flickers between pass / fail across runs — it's an `Unsupported` reply path that doesn't always wire through cleanly.

## Headless / window-pop problem

Currently `op_create_view` calls `window.present()` to realize the WebView's GdkSurface (`crates/ferridriver-webkit-host/src/linux/dispatch.rs` around `// Window keeps the WebView's GdkSurface alive`). On Wayland/X11 desktops this briefly flashes a visible window. Without `present()` the surface never realizes → `LoadEvent::Finished` never fires → every nav hangs.

Three options to land true headless:

1. **xvfb wrapper (recommended short-term).** Install `xorg-server-xvfb` (`sudo pacman -S xorg-server-xvfb`) then run tests via `xvfb-run -a`. With `xvfb-run` setting `$DISPLAY`, GTK4 still calls `present()` internally but renders against the virtual X server — no visible window. The host should auto-detect and self-exec under xvfb when `FERRIDRIVER_WEBKIT_HEADLESS=1` is set; that bit is **not yet wired**. See `crates/ferridriver-webkit-host/src/linux/mod.rs::run` for the plug-in point.
2. **Embed Xvfb spawn in the host.** On startup, if `$DISPLAY`/`$WAYLAND_DISPLAY` are unset (or `FERRIDRIVER_WEBKIT_HEADLESS=1`), fork+exec `Xvfb :NN` on a free display and set `$DISPLAY` before `gtk4::init`. Tear down on exit.
3. **WPE backend.** WPE is the truly-headless WebKit port (renders direct to framebuffer, no GTK). Requires the `wpe-webkit` Rust crate family (less mature than `webkit6`). Largest refactor of the three. Document under §8 of `docs/webkit-linux-port.md` already flags this as out-of-scope.

For continuing the work locally, option 1 is the fastest unlock.

## Recently shipped (Phase 2e)

Most fixes in `crates/ferridriver-webkit-host/src/linux/dispatch.rs`. Highlights:

- **Engine pre-injection.** `engine.min.js` from `crates/ferridriver/src/injected/dist/engine.min.js` is bundled via `include_str!` and registered as a `UserScript` at `DocumentStart` (`crates/ferridriver-webkit-host/src/linux/userscripts.rs`). `window.__fd` is now available on every navigation without needing the parent's `InjectedScriptManager::ensure` lazy round-trip. Fixed the 3 `expect_*` tests + `expose_function`.
- **Wire-format alignment.** Every `Op::*` payload layout now matches `crates/ferridriver-webkit-host/src/macos/host.m` byte-for-byte — `MouseEvent`, `LoadHtml`, `SetUserAgent`, `AddInitScript`, `SetViewport`, `SetLocale`, `SetTimezone`, `EmulateMedia` (5 × action+value), `Screenshot`, `SetCookie` (9 fields), `DeleteCookie`, `SetFileInput`, `Click`, `AccessibilityTree`. Fixed all "no such view 17213423616" errors.
- **Engine via UserScript fixes `LoadEvent::Started` + Document NetRequestEvent.** Latch reset on navigation works.
- **Writer offloaded to dedicated thread + `std::sync::mpsc` channel** (`crates/ferridriver-webkit-host/src/linux/mod.rs::spawn_writer_thread`). Main loop never blocks on socket. Fixed the "host wedges after N ops" hang.
- **`evaluate_javascript_future` await for state-changing input ops.** Fire-and-forget `evaluate_javascript` was replying Ok before JS ran. Now reply only after the future resolves (`evaluate_then_ok` in dispatch.rs).
- **`view_id` threaded into fdConsole/fdDialog/fdNetwork closures** via shared `Rc<Cell<u64>>` set after `registry.insert`. Was hardcoded 0 → parent filtered out events.
- **Touchscreen tap try/catches `new Touch(...)`** (webkit6 has Touch constructor but throws "Illegal constructor"). Falls through to PointerEvent path. Fix in `crates/ferridriver/src/page.rs::Touchscreen::tap`.
- **EmulateMedia state-tracking shim.** `window.__fdMedia` persistent map; Disabled sets `null` (intercept but no preference), Set assigns value, Unchanged leaves alone. matchMedia uses `'key' in O` to detect presence. Fixes cross-call merge (set dark + reduced, then disable dark, expect reduced still on).
- **Mouse input: mousedown stashes target → mouseup uses stashed.** `state.lastDownEl` + `findCheckableNear(x, y)` fallback for headless layout drift. `<label>.control` forwarding. Click event dispatched with modifier flags from `state.modifiers`.
- **Native file chooser suppression** via `connect_run_file_chooser` (`request.cancel(); true`).
- **GTK widget size pinned with `set_size_request(1280, 800)`** before `present()` to give the WebView a real layout viewport even under headless.
- **All input ops install `window.__fdInput`** with `mouseEvent`, `keyEvent`, `typeText`. Per-char keydown/keypress/keyup. Default-action emulation for Enter (newline in textarea, submit on form), Backspace (delete prev char), Tab (focus next).
- **Cookies use `all_cookies_future`** (requires webkit6 `v2_42` feature; enabled in `Cargo.toml`). Was filtering by URL and missing cross-domain cookies.
- **`gdk4` `v4_6` feature enabled** for `Texture::save_to_png_bytes` (screenshot path).
- **URL captured eagerly in `op_navigate`.** `WebView::uri()` returns about:blank for data: URLs at every load phase. The caller's URL is stashed in `ViewEntry::committed_uri` and surfaced by `Op::GetUrl`.
- **No more `minimize()`.** Was suspending the surface and collapsing `getBoundingClientRect`. Window stays present (briefly visible on a desktop). See "Headless" section above.

## Architecture

```
crates/ferridriver-webkit-wire/        Shared wire crate (Op/Rep enums, frame I/O, str codec)
  src/lib.rs                            Frame protocol primitives
  src/js.rs                             Shared JS shims (CONSOLE/ERROR/DIALOG/NETWORK/AX_TREE/EVAL_BODY/RELEASE_REF)
  shared_js/*.js                        Source-of-truth JS files

crates/ferridriver-webkit-host/        Cross-platform host binary
  src/main.rs                           cfg-gated entry: macOS → extern C; Linux → linux::run()
  src/linux/                            Linux host implementation
    mod.rs                                Orchestrator: GTK init, fd-3 socket, writer thread, reader thread
    view.rs                               ViewEntry, ViewRegistry, NetworkSession lifecycle
    dispatch.rs                           Per-Op handlers (~1500 lines now)
    writer.rs                             Frame helpers feeding writer thread
    userscripts.rs                       Standard UserScript bundle (engine + JS shims)
  src/macos/host.m                      Obj-C host (moved from ferridriver crate in Phase 2c)
  build.rs                              JS shim header generation + Obj-C compile on macOS

crates/ferridriver/src/backend/webkit/
  ipc.rs                                Parent-side IpcClient (wire defs re-exported from wire crate)
  mod.rs                                WebKitBrowser/WebKitPage/WebKitElement Rust-side clients
  LIMITATIONS.md                        Public WKWebView (and now webkit6) parity envelope
```

`cfg(webkit_backend)` is emitted by each crate's `build.rs` (`crates/ferridriver/build.rs`, `crates/ferridriver-cli/build.rs`, `crates/ferridriver-mcp/build.rs`, `crates/ferridriver-test/build.rs`) and expands to `any(target_os = "macos", target_os = "linux")`. Replace `#[cfg(webkit_backend)]` at WebKit dispatch sites; genuine macOS-only paths (Library/Caches lookups) stay `#[cfg(target_os = "macos")]`.

## Test gates

```sh
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo test -p ferridriver-webkit-wire
cargo test -p ferridriver-webkit-host           # 4 host integration tests
# Full backend matrix:
FERRIDRIVER_WEBKIT_HOST=$(pwd)/target/debug/ferridriver-webkit-host \
  cargo test -p ferridriver-cli --test backends all_tests_webkit -- --nocapture --test-threads=1
```

All green except the 5 failures listed above.

## Phase 2f → Phase 5

| Phase | Status | Description |
|---|---|---|
| 1 | done | Research doc (`docs/webkit-linux-port.md`) |
| 2 | done | Scaffold wire + host crates |
| 2b | done | First webkit6 cluster (CreateView, Navigate, Evaluate, etc.) |
| 2c | done | Move macOS host into webkit-host crate |
| 2d | done | Wire all 33 ops |
| 2e | done (this session) | Drive backend matrix from 30 failures down to 5 |
| 2f | pending | iframe attach events + childFrames; checkbox toggle JSCValue scope; truly headless surface (xvfb spawn or WPE) |
| 3 | done | cfg gate drop (webkit_backend alias) |
| 4 | pending | Fix the 5 originally-failing macOS tests (now subsumed into Phase 2f's checkbox issue) |
| 5 | pending | CI matrix [ubuntu-latest, macos-latest] with `xvfb-run -a` on Linux leg |

## Next steps for fresh session

1. **Install xvfb** (`sudo pacman -S xorg-server-xvfb` on Arch). Then verify the suite passes the rest cleanly under `xvfb-run -a just test` so no window pops up during dev iteration.
2. **Investigate checkbox toggle.** Open `crates/ferridriver-webkit-host/src/linux/dispatch.rs::FD_INPUT_INSTALL`. The JS sets `target.checked = !target.checked` and dispatches click. Then ferridriver's `check()` (`crates/ferridriver/src/locator.rs:680`) reads state via the engine and sees the OLD value. Hypotheses to test (in order):
   - Add `console.log` to the mouseup handler logging `target.tagName`, `target.checked`, `x`, `y` — verify the right element gets toggled.
   - Verify the engine's `getChecked` reads `el.checked` (not `el.getAttribute('checked')`). Check `crates/ferridriver/src/injected/dist/engine.min.js` for the `tt(i)` function.
   - Try forcing a separate Op::Evaluate refresh between toggle and read_state — if that fixes it, webkit6 has a JSCValue snapshot gap across `evaluate_javascript_future` calls.
3. **iframe events.** In `crates/ferridriver/src/backend/webkit/mod.rs::get_frame_tree`, after discovering iframes, `events.emit(PageEvent::FrameAttached(info))` for each. The seed_frame_cache listener in `crates/ferridriver/src/page.rs:144` already handles `FrameAttached`. The cache update is async but happens before the test's `childFrames()` call if the parent's `waitForSelector` flow ends up calling `get_frame_tree` (it currently doesn't — wire it).
4. **CI matrix.** `.github/workflows/ci.yml` needs the Linux backend test job wrapped in `xvfb-run -a` and the `libwebkitgtk-6.0-dev` + `libgtk-4-dev` + `libsoup-3.0-dev` + `xvfb` system deps. Currently the clippy job on ubuntu-latest only installs `libclang-dev pkg-config`.

## Memories

User feedback to honor:

- `feedback_no_duplication_extract_aggressively.md` — extract shared code, no "later" placeholders.
- `feedback_no_half_measures.md` — finish what you start.
- `feedback_core_rust_not_napi.md` — logic in Rust core, bindings are thin.

## Files in flight (uncommitted)

All Phase 2e changes are uncommitted. Review `git status` + `git diff` before committing. Suggested commit decomposition:

1. wire-crate + host-crate scaffold + JS extraction (Phase 2-2c)
2. Op handlers + JS shims (Phase 2d)
3. cfg(webkit_backend) refactor (Phase 3)
4. Phase 2e fixes (this session's work — many bugfixes)

Or one big squash commit if the user prefers.
