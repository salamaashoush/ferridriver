//! Wraps the shared JS shims (from `ferridriver_webkit_wire::js`) as
//! `webkit6::UserScript` objects that get added to every new view's
//! `UserContentManager`. Same JS bytes as the macOS host's `WKUserScript`
//! — see `crates/ferridriver-webkit-wire/shared_js/README.md`.

use ferridriver_webkit_wire::js;

/// The pre-built selector engine that defines `window.__fd`. Included
/// directly from the ferridriver crate (`src/injected/dist/engine.min.js`)
/// so every navigated page gets the engine at document-start without
/// needing a separate lazy-inject round-trip from the parent.
///
/// macOS host doesn't inject this as a `UserScript` — it relies on the
/// parent's `InjectedScriptManager::ensure` lazy path. On webkit6
/// (Linux) that path is racy: `call_async_javascript_function`'s side
/// effects on `window` can be lost when the `WebKit` web process
/// recycles its content context across calls. Injecting at
/// document-start sidesteps that.
const ENGINE_JS: &str = include_str!("../../../ferridriver/src/injected/dist/engine.min.js");

/// Build the standard set of `UserScript`s injected at document-start
/// when a `WebView` is created: selector engine, console capture,
/// uncaught-error capture, dialog auto-dismiss, network observation.
/// Each script is `AllFrames` so iframes get the shim too (parity
/// with `forMainFrameOnly:NO` in host.m).
pub(crate) fn standard_scripts() -> Vec<webkit6::UserScript> {
  [ENGINE_JS, js::CONSOLE, js::ERROR, js::DIALOG, js::NETWORK]
    .into_iter()
    .map(make_script)
    .collect()
}

fn make_script(source: &str) -> webkit6::UserScript {
  webkit6::UserScript::new(
    source,
    webkit6::UserContentInjectedFrames::AllFrames,
    webkit6::UserScriptInjectionTime::Start,
    &[], // allow-list (empty = all URIs)
    &[], // block-list
  )
}
