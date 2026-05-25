# WebKit backend — Linux port (research)

Phase 1 deliverable for `feat/webkit-linux-port`. The goal: take the existing
macOS-only WebKit backend (`crates/ferridriver/src/backend/webkit/`) and grow a
second host implementation that drives WebKitGTK 6 on Linux behind the exact
same binary IPC wire protocol, so the Rust client stays unchanged. This doc
covers crate choice, the Op-by-Op API mapping, process model, build/CI
integration, and the parity gaps that will land as typed
`FerriError::Unsupported`.

---

## 1. Executive summary

| Decision | Choice | Reason |
|---|---|---|
| Rust binding crate | **`webkit6` 0.6.1** (bilelmoussaoui) | GTK4 + WebKitGTK 6.0, mainline, current. `webkit2gtk` is GTK3-only and Tauri-owned. |
| GTK version | **GTK4** | webkit6 is the only mainline binding that targets the supported `libwebkitgtk-6.0` ABI (Ubuntu 24.04+, Arch). |
| Process model | **Separate binary** (`crates/ferridriver-webkit-host`, Linux only) | Mirrors macOS `fd_webkit_host`. Crash isolation; single-threaded GTK main loop; identical spawn path on both OSes. |
| Wire protocol | **Byte-identical** to macOS | `crates/ferridriver/src/backend/webkit/ipc.rs` stays the canonical wire definition; Linux host reuses the encode/decode helpers. |
| Public-API parity scope | **Public WebKitGTK 6.0 API only** | Same constraint as macOS uses public WKWebView API. Same family of `Unsupported` features. |
| `cfg` gate | `target_os = "macos"` → `any(target_os = "macos", target_os = "linux")` everywhere it appears in the backend dispatch | One coordinated change. |
| CI matrix | `os: [ubuntu-latest, macos-latest]` | ubuntu-latest tracks 24.04 (libwebkitgtk-6.0 present). Both legs run all four backends. |

**This is NOT a port to Playwright's WebKit fork.** Playwright maintains a
patched WebKit at `/tmp/playwright/browser_patches/webkit/` exposing an
internal inspector protocol — 21k+ lines of patches, ~500MB binary, rebased on
every upstream WebKit release. We keep ferridriver's existing tradeoff
(`crates/ferridriver/src/backend/webkit/LIMITATIONS.md` documents it): public
API, no build cost, ~80% of features. The Linux port preserves that envelope —
public WebKitGTK 6.0 API only.

---

## 2. Why webkit6, not webkit2gtk

`webkit2gtk` (crates.io, Tauri-owned, latest 2.0.2 build broken 2025-12; last
green 2.0.1 from 2023-10) targets WebKitGTK 4.x under GTK3. WebKitGTK 4.x is in
LTS-only upstream maintenance, GTK3 is frozen, and modern distros (Ubuntu
24.04, Arch, Fedora 40+) ship `libwebkitgtk-6.0-dev` as the primary package.

`webkit6` 0.6.1 (bilelmoussaoui, MIT) is the gtk-rs ecosystem crate for
WebKitGTK 6.0. Confirmed via docs.rs: top-level types `WebView`, `Settings`,
`WebContext`, `UserScript`, `UserStyleSheet`, `CookieManager`,
`WebsiteDataManager`, `FindController`, `PrintOperation`. Dependencies:
`webkit6-sys`, `gtk4`, `gdk4`, `glib`, `gio`, `javascriptcore6`, `soup3`.

Distro coverage (the matrix that matters):

| Distro | `libwebkitgtk-6.0-dev` available? | Notes |
|---|---|---|
| Ubuntu 24.04 LTS (CI: `ubuntu-latest`) | Yes (`libwebkitgtk-6.0-4`, `libwebkitgtk-6.0-dev`) | Primary CI target |
| Ubuntu 22.04 LTS | No (only 4.x) | Not a supported target |
| Arch Linux (dev machine) | Yes (`webkitgtk-6.0`) | User's second dev machine per `CLAUDE.md` |
| Fedora 40+ | Yes (`webkitgtk6.0-devel`) | Reasonable secondary target |

`Cargo.toml` (new host crate):

```toml
[target.'cfg(target_os = "linux")'.dependencies]
webkit6 = "0.6"
gtk4 = "0.9"
glib = "0.20"
gio = "0.20"
javascriptcore6 = "0.6"
soup3 = "0.7"
```

System packages, distilled:

```sh
# Ubuntu / Debian
sudo apt-get install -y libwebkitgtk-6.0-dev libgtk-4-dev libsoup-3.0-dev \
                        libjavascriptcoregtk-6.0-dev xvfb

# Arch
sudo pacman -S webkitgtk-6.0 gtk4 libsoup3 xorg-server-xvfb

# Fedora
sudo dnf install webkitgtk6.0-devel gtk4-devel libsoup3-devel \
                 javascriptcoregtk-6.0-devel xorg-x11-server-Xvfb
```

`xvfb` is required for CI: WebKitGTK needs a display, headless mode runs under
`xvfb-run -a`. (WPE backend exists for true headless, but the `webkit6` crate
binds to GTK4 only — WPE would be a separate crate. Out of scope: we keep
xvfb.)

---

## 3. Process model

Recommendation: **separate `ferridriver-webkit-host` binary, spawned over a
Unix socketpair, identical control flow to macOS `fd_webkit_host`.**

Alternatives considered and rejected:

- **In-process GTK thread.** `gtk::init()` + `glib::MainLoop::run()` on a
  dedicated thread of the parent ferridriver process. Saves the spawn cost
  and one binary, but: (a) `WebKitWebView` can crash the renderer process,
  taking down the GTK main loop and any other ferridriver work running in
  the same process; (b) `WebKitNetworkSession` and `WebsiteDataManager`
  install signal handlers and shared singletons that would leak across test
  workers; (c) the macOS path spawns a child anyway, so we'd have to fork
  the parent path under `cfg` — more divergence, not less. Crash isolation
  alone justifies the binary.
- **Embed the WebKitGTK API directly in `ferridriver-cli`.** Same problem
  squared: `ferridriver-cli` would pull `gtk4` and `libwebkitgtk-6.0` as
  hard dependencies, breaking the CDP-only build on machines without
  WebKitGTK installed. The `cfg(target_os)` gate keeps the dep at the
  backend layer; promoting it to the CLI undoes that.
- **Re-use Playwright's WebKit fork** via their pre-built bundles. Pulls in
  the 500MB framework, breaks the LIMITATIONS.md tradeoff, and is a
  separate project's concern.

The Linux host process layout:

```
ferridriver (parent, tokio runtime)
  └─ Unix socketpair (binary frames, fd 3 in child)
     └─ ferridriver-webkit-host (Linux, single-threaded GTK main loop)
        ├─ glib::MainContext (default, on main thread)
        ├─ Frame reader: AsyncFd on the socket, schedules dispatch onto MainContext
        ├─ View registry (Rc<RefCell<HashMap<u64, ViewEntry>>>) on main thread
        └─ For each in-flight async op: gio::Cancellable + closure that
           writes the response frame on completion.
```

Single-threaded by design — `WebKitWebView` and almost all GTK4 APIs are
not Send/Sync. Every op funnels through the GTK main context, the same way
macOS funnels through the `NSRunLoop`. Concurrency comes from interleaving
`WebView::evaluate_javascript_future` / `WebView::call_async_javascript_function_future`
on the main loop's task queue, exactly like `WKWebView`'s
`evaluateJavaScript:completionHandler:`.

---

## 4. Wire-protocol compatibility (byte-identical)

`crates/ferridriver/src/backend/webkit/ipc.rs` defines the wire. Frame header
is 9 bytes LE: `u32 len, u32 req_id, u8 op`, then `len` bytes payload.
Strings: `u32 len + UTF-8`. All multi-byte integers little-endian.
Discriminants in `enum Op` (1..73, 255) and `enum Rep` (1..6 + 7..13 for
streamed events) are stable u8 codes.

The Linux host re-uses these exact codes and helpers. To make that mechanically
enforced rather than copy-pasted:

1. **Promote `ipc.rs` to a shared module.** Move the wire definitions
   (`Op`, `Rep`, `frame_write`, `str_encode`/`str_decode`, the response
   payload decoders) into a leaf submodule, e.g.
   `crates/ferridriver/src/backend/webkit/wire.rs`, with `pub use` from
   `ipc.rs` so existing call sites keep compiling. Both `ferridriver` (the
   client) and `ferridriver-webkit-host` (the new server) depend on this
   module.
2. **No `cfg`-gated divergence in the wire module.** Every constant and
   encoder is platform-agnostic.
3. **Cross-platform conformance test.** A new round-trip test in the host
   crate encodes a sample of each Op payload, decodes via the client-side
   helpers, asserts byte-equality with a hard-coded golden vector. Catches
   any host that drifts from the wire (e.g. a Linux-host author adding a
   field).

The two platform-specific bits — shared-memory screenshot transport (POSIX
`shm_open`) and `setsid()` in `pre_exec` — are both Linux/macOS POSIX APIs
and stay byte-compatible. Screenshot wire: `Rep::Binary(7)` payload =
`u32 nameLen + name + u32 pngLen`. Linux uses the same `shm_open` /
`mmap` / `shm_unlink` dance; the existing client decoder
(`decode_shm_screenshot` in `ipc.rs`) needs no change.

---

## 5. Op-by-Op API mapping

Every `Op::*` declared in `crates/ferridriver/src/backend/webkit/ipc.rs` and
implemented in `host.m`, mapped to the WebKitGTK 6.0 (`webkit6`) API the Linux
host will call.

Three status values used in the table:

- **Native** — direct webkit6 API call, no JS shim required. Behaviorally
  equivalent to the macOS path.
- **JS-shim** — implemented via injected WKUserScript on macOS; Linux uses
  the equivalent `WebKit::UserScript`. Functional but engine-level features
  (e.g. CSS `@media (forced-colors)`, navigation-time HTTP headers) are
  not affected. Same envelope as macOS.
- **Unsupported** — WebKitGTK 6.0 has no public API; host returns
  `Rep::Error` with text prefixed `unsupported: …`, which `IpcClient`
  surfaces as `FerriError::Unsupported { reason }`.

| Op (u8) | Name | macOS impl (host.m) | Linux impl (webkit6) | Status |
|---|---|---|---|---|
| 1 | CreateView | `WKWebView` + `WKWebViewConfiguration`, attach `WKUserScript`s for console/dialog/network shims | `WebView::with_context(&WebContext)`, `UserContentManager::add_script(&UserScript)` for the same JS shims, `Window` containing the view (offscreen via `gtk_offscreen_window_new` for headless) | Native + JS-shim (same envelope) |
| 2 | Navigate | `WKWebView.loadRequest:` w/ optional `Referer` header | `WebView::load_uri` plus `URIRequest::set_http_headers` when referer is set | Native |
| 3 | Evaluate | `WKWebView.callAsyncJavaScript:arguments:inFrame:inContentWorld:completionHandler:` | `WebView::call_async_javascript_function_future` (or `evaluate_javascript_future` for non-async) | Native |
| 4 | Screenshot | `WKSnapshotConfiguration` + `takeSnapshotWithConfiguration:completionHandler:` → PNG → shm | `WebView::snapshot_future(SnapshotRegion::Visible / FullDocument, SnapshotOptions::TRANSPARENT_BACKGROUND)` → `cairo::ImageSurface` → PNG via `cairo::ImageSurface::write_to_png` → shm | Native |
| 5 | Close | `WKWebView.removeFromSuperview` + drop from registry | `WebView::try_close()` then drop from registry; `Window::close()` for the wrapping window | Native |
| 7 | GoBack | `WKWebView.goBack` | `WebView::go_back` | Native |
| 8 | GoForward | `WKWebView.goForward` | `WebView::go_forward` | Native |
| 9 | Reload | `WKWebView.reload` | `WebView::reload` | Native |
| 10 | Click | `NSEvent mouseEventWithType:NSEventTypeLeftMouseDown/Up + sendEvent:` to `NSWindow` | Synth `GdkEventButton` (press + release) and post via `gdk_display_put_event` on the WebView's GdkWindow; OR fall back to JS dispatch via `MouseEvent` synthesis. See §5a below. | Native (with caveat) |
| 11 | Type | `WKWebView._executeEditCommand:` + per-char NSEvent | `WebView::execute_editing_command("InsertText")` with the text staged via clipboard, OR per-char `GdkEventKey` via the same dispatch path as Click | Native (with caveat) |
| 12 | PressKey | NSEvent keyDown+keyUp | `GdkEventKey` press+release. Key-name → `gdk::keys::constants` mapping. | Native (with caveat) |
| 13 | KeyDown | NSEvent keyDown | GdkEventKey press, latching held-modifier bitmask the same way macOS does | Native (with caveat) |
| 14 | KeyUp | NSEvent keyUp | GdkEventKey release | Native (with caveat) |
| 20 | GetUrl | `WKWebView.URL.absoluteString` | `WebView::uri()` → `String` | Native |
| 21 | GetTitle | `WKWebView.title` | `WebView::title()` | Native |
| 22 | ListViews | registry keys | registry keys (same) | Native |
| 30 | SetUserAgent | `WKWebView._customUserAgent` (public on 10.11+) | `Settings::set_user_agent(Some(&ua))` via `WebView::settings()` | Native |
| 40 | WaitNav | observe `WKNavigationDelegate.didFinishNavigation:` | observe `WebView::connect_load_changed` (`LoadEvent::Finished`) | Native |
| 50 | SetFileInput | dispatched JS sets `input.files`; `WKUIDelegate._webView:runOpenPanel...` accepts | `WebView::run_javascript` posts to JS that builds a `FileList` from `File(Blob)` objects (no native open-panel interception API — same approach as macOS) | JS-shim (same envelope) |
| 51 | SetViewport | resize host `NSWindow` + emulated scale factor (private SPI) | resize wrapping `Window` + `WebView::set_zoom_level` for the device-scale factor; **`device-scale-factor` ≥ 1.0 is honored, fractional DPR via `WebView::set_zoom_level` does NOT change DPR** (it changes layout). DPR override is `Unsupported` on Linux. | Mixed: viewport=Native; DPR=Unsupported |
| 60 | GetCookies | `WKHTTPCookieStore.getAllCookies:` | `WebsiteDataManager::cookie_manager().cookies_future(uri: "/")` returns `Vec<soup::Cookie>` | Native |
| 61 | SetCookie | `WKHTTPCookieStore.setCookie:` | `CookieManager::add_cookie_future(&soup::Cookie::new(...))` | Native |
| 62 | DeleteCookie | `WKHTTPCookieStore.deleteCookie:` | `CookieManager::delete_cookie_future(&cookie)` | Native |
| 63 | ClearCookies | enumerate + delete | `WebsiteDataManager::clear_future(WebsiteDataTypes::COOKIES, Duration::ZERO)` | Native |
| 64 | LoadHtml | `WKWebView.loadHTMLString:baseURL:` | `WebView::load_html(html, base_uri)` | Native |
| 65 | AddInitScript | `WKUserScript` at `WKUserScriptInjectionTimeAtDocumentStart` | `UserScript::new(source, InjectedFrames::AllFrames, InjectionTime::Start, allow_list, block_list)` added via `UserContentManager::add_script` | Native |
| 66 | MouseEvent | NSEvent dispatch (move / wheel / button / drag) | `GdkEventMotion` / `GdkEventScroll` / `GdkEventButton`. Coalescing differences vs NSEvent are the same kind of platform quirk as documented under CLAUDE.md "Backend / wire / binding quirks." | Native (with caveat) |
| 67 | SetLocale | `WKUserScript` overriding `navigator.language` / `Intl` | Same `UserScript` (the JS shim is OS-independent). Native: `Settings` has no per-WebView locale setter. | JS-shim (same envelope) |
| 68 | SetTimezone | `WKUserScript` overriding `Intl.DateTimeFormat` / `Date` | Same UserScript. Native: WebKitGTK has no per-view TZ. The process-wide `TZ` env var works only at spawn time. | JS-shim (same envelope) |
| 69 | EmulateMedia | `WKWebView.setMediaType:` (native), `NSAppearance` for color-scheme, JS for forced-colors/contrast/reduced-motion | `WebView::set_default_content_security_policy` no-op; native: `WebView::set_color_scheme` (light/dark/auto, since 6.0); `WebView::set_media_type` exists in newer ABI (gated). Forced-colors/contrast/reduced-motion via JS shim (same envelope). | Native + JS-shim (same envelope) |
| 70 | AccessibilityTree | `WKWebView._accessibilityHitTest:` and recursive AX walk via NSAccessibility | `WebView::accessible()` returns `atk::Object`; recursive walk via `Atk::ObjectExt::n_children` / `ref_child` / role / name | Native |
| 71 | RouteRequest | bi-directional: JS shim sends fdRoute message → host forwards to parent → parent's RouteHandler replies → host fulfils JS promise | Same JS shim; on Linux, message handler is `UserContentManager::connect_script_message_with_reply_received` (since WebKitGTK 2.40) | Native |
| 72 | GetWebKitVersion | `CFBundleShortVersionString` from `com.apple.WebKit` bundle | `webkit::get_major_version()` / `get_minor_version()` / `get_micro_version()` formatted as `"WebKitGTK/{major}.{minor}.{micro}"` | Native |
| 73 | ReleaseRef | JS: `window.__wr.delete(refId)` via `evaluateJavaScript` | Same JS, via `WebView::evaluate_javascript_future` | Native |
| 255 | Shutdown | `_exit(0)` | `glib::MainLoop::quit()` then `process::exit(0)` | Native |

Streamed event Reps (host → parent, no req_id reply expected):

| Rep (u8) | Name | macOS source | Linux source |
|---|---|---|---|
| 8 | ConsoleEvent | `fdConsole` `WKScriptMessageHandler` | `UserContentManager::connect_script_message_received("fdConsole", ...)` |
| 9 | DialogEvent | `fdDialog` script message (alert/confirm/prompt JS shim) | Same JS shim, `connect_script_message_received("fdDialog", ...)`. Also wire `WebView::connect_script_dialog` so native `script-dialog` fires when JS hasn't been shimmed yet on early documents. |
| 10 | NetRequestEvent | `fdNetwork` script message (fetch/XHR shim) | Same JS shim, plus `WebView::connect_resource_load_started` for `Request` events that happen outside JS-initiated fetch (navigation, images, css). Note: response bodies still `Unsupported`. |
| 11 | RouteRequest | `fdRoute` reply-handler shim | `connect_script_message_with_reply_received("fdRoute", ...)` |
| 12 | NetResponseEvent | derived from `fdNetwork` payload `kind:'response'` | Same JS shim; also from `WebView::connect_resource_load_started` → `Resource::connect_finished` for resource-level responses |
| 13 | NetFailureEvent | derived from `fdNetwork` payload `kind:'failure'` | Same JS shim; also from `Resource::connect_failed` |

### 5a. Mouse/keyboard input on Linux — caveat detail

macOS uses `NSEvent.mouseEventWithType:…:` + `sendEvent:` to inject events
*into* the `NSWindow`'s event queue, so the WebKit content area sees them as
real OS events. Linux equivalent: `gdk_display_put_event(GdkEvent*)`. The
GdkEvent variants we need (`GDK_BUTTON_PRESS`, `GDK_MOTION_NOTIFY`,
`GDK_SCROLL`, `GDK_KEY_PRESS`, `GDK_KEY_RELEASE`) all exist and target the
WebView's `GdkSurface`. Two practical wrinkles:

1. **GTK4 deprecated direct event injection.** `gdk_display_put_event` is
   GTK3 vintage; in GTK4 the recommended path for synthetic input is
   `GtkEventControllerLegacy` or the test-helper `gtk_test_widget_send_key`
   (gtk4 only exposes a subset). For our case — driving real DOM event
   listeners — we want the event to flow through WebKit's input pipeline,
   not stop at GTK widget controllers. The pragmatic answer: post events
   directly to the WebView's GdkSurface via the low-level
   `gdk_surface_emit_event`-style path (`webkit6` re-exports `gdk4`, so
   we have the unsafe `gdk::ffi::gdk_display_put_event` available). If
   that turns out to land events at the GTK layer rather than reaching
   the WebKit renderer, fall back to **JS-shim mouse/key dispatch** via
   `MouseEvent` / `KeyboardEvent` constructed in the page and dispatched
   to the target element — that bypasses GTK entirely. Both options stay
   inside the public webkit6 API.
2. **`isTrusted` flag.** Real OS events arrive with `event.isTrusted ===
   true`; JS-synthesised events arrive with `isTrusted === false`. Some
   page code (security-sensitive UI, drag-and-drop) gates on
   `isTrusted`. macOS achieves `isTrusted === true` because NSEvent
   injection goes through the OS event queue. Linux GdkEvent injection
   may or may not, depending on whether WebKitGTK respects `synthesized`
   GdkEvents — needs empirical verification once the host is wired up.
   **Phase 2 will spike-test this before settling on the input
   strategy.** If GdkEvent injection produces `isTrusted === true`, we
   match macOS. If not, we document the gap (same family as the
   CDP/BiDi vs WebKit `tap` story already in `LIMITATIONS.md`).

This single area is the highest-risk piece of the port. Phase 2's first
deliverable should be a 1-test smoke that drives a `click` against
`example.com` on Linux and confirms either path works.

---

## 6. Build integration

The macOS build is `crates/ferridriver/build.rs` (101 lines): compile
`host.m` + `host_main.c` via `cc::Build`, link with `-framework Cocoa
-framework WebKit -framework CoreFoundation`, copy the resulting
`fd_webkit_host` binary to a discoverable cache directory.

Linux changes:

1. **New crate `crates/ferridriver-webkit-host`** with a binary target. Its
   own `Cargo.toml` pulls `webkit6`, `gtk4`, `glib`, `gio`,
   `javascriptcore6`, `soup3`, `tokio`, `serde_json`, plus a path
   dependency on `ferridriver` for the shared `wire.rs` module (or, to
   avoid the dep cycle, hoist `wire.rs` into a third tiny crate
   `ferridriver-webkit-wire` that both depend on — recommended).
2. **`crates/ferridriver/build.rs`** grows a Linux arm. Pseudo-shape:
   ```rust
   match target_os.as_str() {
     "macos" => build_macos_host(),
     "linux" => build_linux_host_via_cargo(),  // cargo build -p ferridriver-webkit-host
     _ => return,
   }
   ```
   The Linux arm shells out to `cargo build -p ferridriver-webkit-host
   --release` and copies the resulting `target/release/ferridriver-webkit-host`
   into the same discoverable locations the macOS path uses
   (`target/{profile}/`, `~/.cache/ferridriver/`). Naming: keep
   `fd_webkit_host` on macOS, use `ferridriver-webkit-host` on Linux —
   `resolve_host_binary` already takes an env override
   (`FERRIDRIVER_WEBKIT_HOST`) so a single discovery function with two
   filename probes is straightforward.
3. **`HOST_BINARY_NAME` becomes per-OS.** Currently `#[cfg(target_os =
   "macos")] const HOST_BINARY_NAME: &str = "fd_webkit_host";`. Add a
   Linux arm with `"ferridriver-webkit-host"`.
4. **`build.rs` `cargo:rerun-if-changed`** stanzas extend to the host crate's
   `src/`.

A subtle gotcha worth flagging: the parent's `build.rs` shelling out to
`cargo build` for the host crate creates a recursive cargo invocation. The
usual safe pattern (`CARGO_TARGET_DIR` isolation + lockfile sharing) works
fine, but the alternative — declaring the host as a workspace binary and
letting the parent runtime locate it via `cargo metadata` — is cleaner.
Recommendation: **make `ferridriver-webkit-host` a normal workspace member
that gets built as part of `cargo build --workspace`**, then
`resolve_host_binary` only needs to find the binary in `target/{profile}/`
or `~/.cache/ferridriver/`, no recursive cargo call.

Tradeoff: a `cargo build` of any crate in the workspace now pulls gtk4 /
webkit6 system libraries when on Linux. That's acceptable — the existing
macOS workspace build already requires WebKit.framework be available.
Developers without `libwebkitgtk-6.0-dev` will hit a clear `pkg-config`
error from `webkit6-sys/build.rs` rather than a runtime spawn failure.
Documented in the new crate's README.

---

## 7. CI matrix

`.github/workflows/ci.yml` currently runs `macos-latest`. Change the test
job:

```yaml
strategy:
  fail-fast: false
  matrix:
    os: [ubuntu-latest, macos-latest]
runs-on: ${{ matrix.os }}
steps:
  - uses: actions/checkout@v4
  - name: Install system deps (Linux)
    if: matrix.os == 'ubuntu-latest'
    run: |
      sudo apt-get update
      sudo apt-get install -y libwebkitgtk-6.0-dev libgtk-4-dev \
                              libsoup-3.0-dev libjavascriptcoregtk-6.0-dev \
                              xvfb
  - uses: dtolnay/rust-toolchain@nightly
  - uses: Swatinem/rust-cache@v2
  - name: Install Chrome (for CDP backends)
    run: cargo run --bin ferridriver -- install --with-deps chromium
  - name: cargo clippy
    run: cargo clippy --workspace --all-targets -- -D warnings
  - name: cargo test (Linux under xvfb)
    if: matrix.os == 'ubuntu-latest'
    run: xvfb-run -a just test
  - name: cargo test (macOS)
    if: matrix.os == 'macos-latest'
    run: just test
```

Backend matrix to run on **both** legs:
`cdp_pipe`, `cdp_raw`, `bidi`, `webkit`. The backend integration
harness (`crates/ferridriver-cli/tests/backends.rs`) already keys off
`FERRIDRIVER_BIN` and per-backend `BackendKind`; no test code changes —
just remove the `#[cfg(target_os = "macos")]` gates that currently exclude
the webkit suite from Linux compilation.

Caveat from
[`MEMORY.md`](../../.claude/projects/-home-sashoush-Workspace-ferridriver/memory/MEMORY.md):
"plain `cargo test --workspace` mass-fails bidi/cdp_raw from browser
contention; re-run per-backend single-threaded before believing red."
Keep `--test-threads=1` for the WebKit suite on both legs.

---

## 8. Parity gaps (Linux-specific, beyond LIMITATIONS.md)

The existing macOS `LIMITATIONS.md` documents gaps from using public
WKWebView vs Playwright's patched fork. Linux inherits all of those (no
network interception of navigation/iframes/images, no CSP bypass, no
isolated worlds, no per-page permission control, etc.). Linux-only
additions:

| Feature | macOS impl | Linux impl | Status |
|---|---|---|---|
| Emulated device-pixel-ratio (`devicePixelRatio` > 1, fractional) | `NSWindow.backingScaleFactor` override (private SPI) | None — `Settings::set_zoom_level` changes layout, not DPR | `Unsupported` |
| Native open-panel interception for `<input type=file>` (already gap on macOS) | gap | gap | `Unsupported` (same) |
| `WKDownload` events | gap on macOS too | `WebView::connect_download_started` exists — potential win on Linux. **Out of scope for this port.** Documented for a future follow-up. | `Unsupported` (matches macOS for now) |
| Real headless | macOS: window off-screen | Linux: `xvfb-run` wraps the test process. WPE backend would give true headless but it's a different binding crate. | OK in CI |
| `isTrusted === true` on synthesised input | NSEvent native injection | TBD per §5a spike | Confirm in Phase 2 |
| Video screencast | `Unsupported` on macOS | `Unsupported` on Linux (no public webkit6 frame-grab API) | `Unsupported` (same) |
| Browser contexts (isolated cookie/cache) | `Unsupported` on macOS | `WebKitWebContext` is per-process on WebKitGTK; multiple `WebContext` instances DO give isolated `WebsiteDataManager`s. **Potential Linux-only win.** Out of scope for this port — match macOS behavior (single context) to keep cross-backend tests deterministic. Documented for follow-up. | `Unsupported` (matches macOS for now) |

Every `Unsupported` row above returns `FerriError::Unsupported { reason:
"…" }` from the host on Linux, just as macOS does. No placeholders, no
silent `Ok(default)`. Tests asserting `Unsupported` semantics continue
to pass on both legs.

---

## 9. The 5 currently-failing macOS WebKit tests

From the user prompt and confirmed in
`crates/ferridriver-cli/tests/backends.rs`:

| # | Test | Module |
|---|---|---|
| 1 | `test_expect_to_have_text` | `backends_support::expect` |
| 2 | `test_expect_to_have_count` | `backends_support::expect` |
| 3 | `test_expect_to_have_value` | `backends_support::expect` |
| 4 | `test_frame_get_by_methods` | `backends_support::binding_surface` |
| 5 | `test_page_expose_function` | `backends_support::binding_surface` |

Symptoms reported: `window.__fd.selOne undefined` and locator text returning
`""`. Two leading hypotheses:

- **Mode A — engine not injected.** `InjectedScriptManager::ensure` (in
  `crates/ferridriver/src/backend/webkit/mod.rs:269`) runs the selector
  engine via `Op::Evaluate`, then sets the latch. On WebKit the latch is
  per-WebKitPage instance, but a navigation can replace the page's JS
  global without resetting the latch — second evaluate then sees
  `window.__fd` undefined. Fix at the source: clear the latch on each
  `LoadEvent::Started` (Linux) / `didStartProvisionalNavigation:`
  (macOS).
- **Mode B — page text empty.** The locator's `textContent` evaluates in
  a frame whose document hasn't reached `Load` yet, or the host's
  `callAsyncJavaScript` resolved before the body was rendered. Fix at
  the source: `expect_to_have_text` should poll, which it already does
  via `ferridriver-test::expect`; the bug is more likely the engine
  injection (Mode A).

**Both hypotheses can be tested on Linux** with a debugger
(`rust-gdb --args target/debug/ferridriver-webkit-host`) attached to the
host binary while the parent runs `cargo test -p ferridriver-cli --test
backends -- test_expect_to_have_text`. Linux gives breakpoints inside
`webkit6::WebView::evaluate_javascript_future`'s GIO closures — better
than CI guessing on macOS.

**Important caveat the user should note:** WebKitGTK and macOS WebKit
share JavaScriptCore but the navigation/loader code paths are platform
code. The 5 failures may reproduce on Linux, may produce a *different*
failure (timing-sensitive race), or may not reproduce at all if the
underlying bug is in WKUserScript injection timing specifically. If
Linux passes all 5 cleanly and macOS still fails, we have a macOS
host.m fix to land independently — the Linux port unblocks the
debugging environment but the actual fixes will land in
`crates/ferridriver/src/backend/webkit/host.m` and the new Linux host
together (Rule 5: every API change updates both backends in the same
commit).

---

## 10. Phase 2 hand-off — concrete next steps

When the user greenlights Phase 2:

1. **Day 1**: create `crates/ferridriver-webkit-wire` (shared wire
   definitions), `crates/ferridriver-webkit-host` (Linux binary stub),
   add to workspace `Cargo.toml`. Make `cargo build --workspace`
   succeed on both Linux and macOS.
2. **Day 1**: spike the §5a input strategy. Hello-world host: create
   `WebView`, `load_uri("about:blank")`, dispatch a synthetic
   click on a known coordinate via `gdk_display_put_event`, check
   `event.isTrusted` in JS. Pick the strategy. Document the result
   in this doc (`webkit-linux-port.md` §5a) before continuing.
3. **Day 2-3**: implement the cluster `CreateView`, `Navigate`,
   `Evaluate`, `GetUrl`, `GetTitle`, `Close`, `Shutdown`. Verify
   end-to-end with a manual test: `cargo run --bin ferridriver -- bdd
   --steps 'tests/steps/**/*.{js,ts}' tests/features/basic.feature`
   under xvfb-run.
4. **Day 4-5**: the JS-shim cluster — `AddInitScript`, `Console`/`Dialog`/`Network`
   event capture, route handling. These share infrastructure (
   `UserContentManager`), so they land together.
5. **Day 6-7**: input cluster (`Click`, `Type`, `PressKey`, `KeyDown`,
   `KeyUp`, `MouseEvent`).
6. **Day 8**: cookies, viewport, emulate-media, screenshots.
7. **Day 9**: accessibility tree, init-script edge cases.
8. **Day 10**: Phase 3 — drop the `cfg(target_os = "macos")` gates,
   wire `WebKitBrowser::launch` to discover the Linux binary, run the
   full backend test matrix locally on both Arch (user's dev) and CI
   ubuntu-latest. Phase 4 fixes follow once the harness is green-ish.

### Open structural questions for the user

These are unresolved enough that **Phase 2 should not start** until the
user confirms:

1. **`ferridriver-webkit-host` as a workspace binary vs. nested cargo
   build via build.rs.** Recommendation: workspace binary. Trade: every
   `cargo build` on Linux now requires `libwebkitgtk-6.0-dev`. Are we
   OK forcing that for CDP-only developers on Linux? (Alternative:
   keep it as a separate cargo invocation, fed via build.rs, only when
   the `webkit-host` feature is enabled. More plumbing but optional.)
2. **`ferridriver-webkit-wire` as a third crate, or merge wire into the
   host crate and re-import on the parent side?** Recommendation:
   third crate, ~200 LOC, eliminates the symmetric-import problem.
3. **Are the 5 failing macOS tests blocking, or just the Linux port?**
   The user prompt frames the Linux port as *enabling* debugging of
   those tests, which implies the Linux port lands first and the
   macOS fixes come second. If the tests are blocking the PR matrix
   from going green, that's an additional Phase 4 deliverable that
   may run in parallel with Phase 2/3 on macOS. (Recommendation:
   the Linux port unblocks debugging, then we fix the 5 tests on
   *both* OSes in one commit per Rule 5, as Phase 4.)
4. **`xvfb` for CI is fine** — both legs run the same tests, just under
   different display servers. Confirming this is acceptable for the
   security model (xvfb runs as the test user, no escalation).
5. **WPE backend?** WPE (Web Platform for Embedded) gives true
   headless without xvfb but requires `wpewebkit` and a different Rust
   crate (`wpe-webkit`-`-rs` family — far less mature than `webkit6`).
   Recommendation: **out of scope.** xvfb is the standard CI answer
   for GTK apps.

Halt here for review.
