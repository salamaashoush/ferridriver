# Shared JS shims

Single source of truth for the JavaScript blobs both WebKit hosts inject:

- **macOS host** (`crates/ferridriver/src/backend/webkit/host.m`) consumes
  the strings via a build.rs-generated header
  (`<OUT_DIR>/ferridriver_shared_js.h`) that re-emits each `.js` file as a
  `static NSString *FD_JS_<NAME> = @"…";` literal.
- **Linux host** (`crates/ferridriver-webkit-host`) consumes them via
  `include_str!` through `ferridriver_webkit_wire::js::<NAME>`.

The wire crate's build.rs is the only place that escapes the source for
Obj-C. The Linux host gets the raw file contents.

## Adding a new shim

1. Add `shared_js/my_shim.js` (raw JS — no quoting, no Obj-C string
   concatenation).
2. Add an entry to `crates/ferridriver-webkit-wire/src/js.rs` so the
   Rust side picks it up via `include_str!`.
3. Add it to the `JS_FILES` array in
   `crates/ferridriver/build.rs::generate_shared_js_header()` so the
   macOS host gets a matching `FD_JS_<NAME>` constant.
4. Replace the corresponding `NSString *…JS = @"…"` literal in `host.m`
   with the `FD_JS_<NAME>` constant.

Tests in `crates/ferridriver-webkit-wire/tests/` assert that the JS
constants parse as valid JS (executed by `webkit6::WebView` in CI under
xvfb, and by JavaScriptCore-direct in a future smoke test).
