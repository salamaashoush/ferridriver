//! Per-Op handlers. Runs on the GTK main thread (the
//! [`REGISTRY`](super::REGISTRY) and webkit6 widgets are not Send).
//! Async ops spawn a future via [`glib::MainContext::spawn_local`] and
//! reply when the future resolves.

use ferridriver_webkit_wire::{Op, js, str_decode};
use std::cell::RefCell;
use std::rc::Rc;
// webkit6::prelude re-exports gtk::prelude::* + soup::prelude::* +
// `auto::traits::*` (which contains `WebViewExt`). Single glob brings
// in everything we need; the `set_default_size` ambiguity between
// `WebViewExt` and `GtkWindowExt` is resolved at the one call site
// via UFCS.
use webkit6::prelude::*;

use super::view::ViewEntry;
use super::{REGISTRY, writer};

pub(crate) fn handle(req_id: u32, op_byte: u8, payload: &[u8]) {
  let Some(op) = Op::from_u8(op_byte) else {
    writer::error(req_id, &format!("unknown op code {op_byte}"));
    return;
  };
  match op {
    Op::CreateView => op_create_view(req_id, payload),
    Op::Navigate => op_navigate(req_id, payload),
    Op::Evaluate => op_evaluate(req_id, payload),
    Op::Reload => op_reload(req_id, payload),
    Op::GoBack => op_go_back(req_id, payload),
    Op::GoForward => op_go_forward(req_id, payload),
    Op::LoadHtml => op_load_html(req_id, payload),
    Op::SetUserAgent => op_set_user_agent(req_id, payload),
    Op::AddInitScript => op_add_init_script(req_id, payload),
    Op::WaitNav => op_wait_nav(req_id, payload),
    Op::SetViewport => op_set_viewport(req_id, payload),
    Op::SetLocale => op_set_locale(req_id, payload),
    Op::SetTimezone => op_set_timezone(req_id, payload),
    Op::EmulateMedia => op_emulate_media(req_id, payload),
    Op::Screenshot => op_screenshot(req_id, payload),
    Op::GetCookies => op_get_cookies(req_id, payload),
    Op::SetCookie => op_set_cookie(req_id, payload),
    Op::DeleteCookie => op_delete_cookie(req_id, payload),
    Op::ClearCookies => op_clear_cookies(req_id, payload),
    Op::AccessibilityTree => op_accessibility_tree(req_id, payload),
    Op::ReleaseRef => op_release_ref(req_id, payload),
    Op::SetFileInput => op_set_file_input(req_id, payload),
    Op::Click => op_click(req_id, payload),
    Op::MouseEvent => op_mouse_event(req_id, payload),
    Op::Type => op_type(req_id, payload),
    Op::PressKey => op_press_key(req_id, payload),
    Op::KeyDown => op_key_down(req_id, payload),
    Op::KeyUp => op_key_up(req_id, payload),
    Op::RouteRequest => op_route_request_reply(req_id, payload),
    Op::GetUrl => op_get_url(req_id, payload),
    Op::GetTitle => op_get_title(req_id, payload),
    Op::Close => op_close(req_id, payload),
    Op::Shutdown => op_shutdown(req_id),
    Op::ListViews => op_list_views(req_id),
    Op::GetWebKitVersion => op_get_webkit_version(req_id),
  }
}

// ─── Helpers ───────────────────────────────────────────────────────────────

fn read_u64(data: &[u8], off: &mut usize) -> u64 {
  if *off + 8 > data.len() {
    return 0;
  }
  let v = u64::from_le_bytes([
    data[*off],
    data[*off + 1],
    data[*off + 2],
    data[*off + 3],
    data[*off + 4],
    data[*off + 5],
    data[*off + 6],
    data[*off + 7],
  ]);
  *off += 8;
  v
}

fn read_u32(data: &[u8], off: &mut usize) -> u32 {
  if *off + 4 > data.len() {
    return 0;
  }
  let v = u32::from_le_bytes([data[*off], data[*off + 1], data[*off + 2], data[*off + 3]]);
  *off += 4;
  v
}

fn read_i32(data: &[u8], off: &mut usize) -> i32 {
  if *off + 4 > data.len() {
    return 0;
  }
  let bytes: [u8; 4] = data[*off..*off + 4].try_into().unwrap_or([0; 4]);
  *off += 4;
  i32::from_le_bytes(bytes)
}

/// Read a string property off a `JavaScriptCore` object. Returns `None`
/// if the property is absent or non-string. Used by the
/// `fdConsole`/`fdDialog`/`fdNetwork` message handlers.
fn jsc_get_string(value: &webkit6::javascriptcore::Value, name: &str) -> Option<String> {
  if !value.is_object() {
    return None;
  }
  // `object_get_property` returns `Option<Value>`; unwrap then check.
  let prop = value.object_get_property(name)?;
  if prop.is_string() {
    Some(prop.to_str().as_str().to_string())
  } else {
    None
  }
}

/// Re-encode a `JavaScriptCore` value as JSON via the page-side
/// `JSON.stringify`. Used to ship the network-response payload to
/// the parent unchanged so its decoder parses it identically to the
/// macOS host.
fn jsc_to_json(value: &webkit6::javascriptcore::Value) -> String {
  // JSCValue has `to_json` that mirrors `JSON.stringify` semantics
  // (returns "" on failure rather than panicking).
  value.to_json(0).map_or_else(String::new, |g| g.as_str().to_string())
}

fn read_u8(data: &[u8], off: &mut usize) -> u8 {
  if *off >= data.len() {
    return 0;
  }
  let v = data[*off];
  *off += 1;
  v
}

fn read_f64(data: &[u8], off: &mut usize) -> f64 {
  if *off + 8 > data.len() {
    return 0.0;
  }
  let bytes: [u8; 8] = data[*off..*off + 8].try_into().unwrap_or([0; 8]);
  *off += 8;
  f64::from_le_bytes(bytes)
}

/// Look up a view by id and either run `f` on a clone of the `WebView`
/// (cheap — webkit6 widgets are `Rc`-like under the hood) or send the
/// "no such view" error reply.
fn with_webview<F: FnOnce(webkit6::WebView)>(req_id: u32, view_id: u64, f: F) {
  let web_view = REGISTRY.with(|reg| reg.borrow().get(view_id).map(|e| e.web_view.clone()));
  match web_view {
    Some(wv) => f(wv),
    None => writer::error(req_id, &format!("no such view {view_id}")),
  }
}

/// Like `with_webview` but gives access to the full `ViewEntry`.
/// Used for ops that need to touch sibling fields (`nav_waiters`, `ucm`).
fn with_view<F: FnOnce(&ViewEntry)>(req_id: u32, view_id: u64, f: F) {
  REGISTRY.with(|reg| match reg.borrow().get(view_id) {
    Some(entry) => f(entry),
    None => writer::error(req_id, &format!("no such view {view_id}")),
  });
}

// ─── CreateView ─────────────────────────────────────────────────────────────

/// Register and wire up the three script-message handlers that
/// `engine.min.js` + the JS shims rely on (`fdConsole`, `fdDialog`,
/// `fdNetwork`) plus a stub for `fdRoute`. Extracted out of
/// [`op_create_view`] so that function stays under the clippy
/// too-many-lines threshold.
fn install_ucm_handlers(ucm: &webkit6::UserContentManager, view_id_cell: &Rc<std::cell::Cell<u64>>) {
  // Register the message-handler names BEFORE adding the user
  // scripts. The shims access `webkit.messageHandlers.fdConsole`
  // etc. at load time; without these registrations the access
  // throws a TypeError and the shim aborts BEFORE it can override
  // `window.alert/confirm/prompt`, falling back to native dialogs.
  ucm.register_script_message_handler("fdConsole", None);
  ucm.register_script_message_handler("fdDialog", None);
  ucm.register_script_message_handler("fdNetwork", None);
  // `fdRoute` uses reply-handler — registered separately; for now
  // we make the name exist so `fetch()`-interceptor access doesn't
  // throw. Real reply wiring is Phase 2e work.
  ucm.register_script_message_handler("fdRoute", None);

  // Wire the message-received signals onto the writer's event helpers.
  let vid_for_console = view_id_cell.clone();
  ucm.connect_script_message_received(Some("fdConsole"), move |_, value| {
    let level = jsc_get_string(value, "level").unwrap_or_default();
    let text = jsc_get_string(value, "text").unwrap_or_default();
    writer::console_event(&level, &text, vid_for_console.get());
  });
  ucm.connect_script_message_received(Some("fdDialog"), move |_, value| {
    let dtype = jsc_get_string(value, "type").unwrap_or_default();
    let message = jsc_get_string(value, "message").unwrap_or_default();
    let action = jsc_get_string(value, "action").unwrap_or_default();
    writer::dialog_event(&dtype, &message, &action);
  });
  ucm.connect_script_message_received(Some("fdNetwork"), move |_, value| {
    let kind = jsc_get_string(value, "kind").unwrap_or_default();
    match kind.as_str() {
      "response" => {
        // Re-serialize the whole object as JSON for the parent.
        let json = jsc_to_json(value);
        writer::network_response_event_json(&json);
      },
      "failure" => {
        let id = jsc_get_string(value, "id").unwrap_or_default();
        let err = jsc_get_string(value, "errorText").unwrap_or_default();
        writer::network_failure_event(&id, &err);
      },
      _ => {
        let id = jsc_get_string(value, "id").unwrap_or_default();
        let method = jsc_get_string(value, "method").unwrap_or_default();
        let url = jsc_get_string(value, "url").unwrap_or_default();
        let resource_type = jsc_get_string(value, "resourceType").unwrap_or_default();
        writer::network_request_event(&id, &method, &url, &resource_type);
      },
    }
  });
}

fn op_create_view(req_id: u32, payload: &[u8]) {
  use gtk4::prelude::WidgetExt;
  let mut off = 0;
  let url = str_decode(payload, &mut off);

  // Shared cell for the view_id — assigned by `reg.insert(...)` AFTER
  // the UCM closures are built. Closures read at message-fire time.
  let view_id_cell: Rc<std::cell::Cell<u64>> = Rc::new(std::cell::Cell::new(0));

  let result = REGISTRY.with(|reg| -> u64 {
    let mut reg = reg.borrow_mut();
    let network_session = reg.network_session().clone();

    let ucm = webkit6::UserContentManager::new();

    // Register the message-handler names BEFORE adding the user
    // scripts. The shims access `webkit.messageHandlers.fdConsole`
    // etc. at load time; without these registrations the access
    // throws a TypeError and the shim aborts BEFORE it can override
    // `window.alert/confirm/prompt`, falling back to native dialogs.
    install_ucm_handlers(&ucm, &view_id_cell);

    for script in super::userscripts::standard_scripts() {
      ucm.add_script(&script);
    }

    let web_view = webkit6::WebView::builder()
      .network_session(&network_session)
      .user_content_manager(&ucm)
      .build();

    // Native script-dialog suppression. The `dialog.js` shim overrides
    // `window.alert/confirm/prompt` at document-start so almost every
    // page-side call routes through `fdDialog`. But a script that
    // *captures* `window.confirm` before document-start (vanishingly
    // rare; data: URLs with inline `<script>` could in principle) or
    // any early-load corner case would otherwise pop a native GTK
    // dialog. Suppress unconditionally — return `true` from
    // `script-dialog` tells webkit6 the host handled it.
    web_view.connect_script_dialog(|_, _dialog| {
      // TODO(phase-2e): inspect dialog kind and route through
      // `fdDialog` so the host observes dialogs from early-load JS.
      // For now suppression alone is enough to keep tests headless.
      true
    });

    // Suppress the native file chooser. Stock `WKWebView` on macOS
    // can't intercept the open-panel either (see `LIMITATIONS.md`),
    // so we just decline the request — matching that envelope and
    // keeping the host headless. Tests that need to set file inputs
    // do so via `Op::SetFileInput` (JS `DataTransfer` injection), not
    // via clicking the input.
    web_view.connect_run_file_chooser(|_, request| {
      // FileChooserRequest has inherent methods, not via an Ext trait.
      request.cancel();
      true
    });

    // Force the WebView's widget size BEFORE the window presents.
    // Under minimized windows the GTK surface gets no allocation, so
    // `getBoundingClientRect` collapses to (0,0,0,0) and
    // `elementFromPoint(x,y)` always returns <body>. Pinning the
    // widget minimum size with `set_size_request` gives webkit6's
    // renderer a real layout viewport regardless of window state.
    WidgetExt::set_size_request(&web_view, 1280, 800);

    // Window keeps the WebView's GdkSurface alive and triggers
    // realization — webkit6 only fires `LoadEvent::Finished` once
    // the WebView's surface is allocated. We `present()` to realize.
    // We do NOT `minimize()`: under Wayland/X11 minimize suspends the
    // surface, which collapses `getBoundingClientRect` back to zeros
    // and breaks every coordinate-driven action (check, hover, etc).
    // For truly headless runs the host should be wrapped with
    // `xvfb-run -a`; on a developer desktop the window briefly
    // appears but stays out of the way (no decorations, no focus).
    let window = gtk4::Window::builder()
      .default_width(1280)
      .default_height(800)
      .decorated(false)
      .child(&web_view)
      .build();
    window.present();

    let nav_waiters: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));
    let committed_uri: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // load-changed signal:
    //   * `Started`: emit a synthetic `Rep::NetRequestEvent` with
    //     `resource_type = "Document"` so the parent's
    //     `drain_network_events` resets the `InjectedScriptManager`
    //     latch — `window.__fd` gets thrown away on every navigation
    //     and must be re-injected. Mirrors macOS host's
    //     `decidePolicyForNavigationAction:` REP_NET_EVENT path.
    //   * `Finished`: drain all parked `WaitNav` req_ids and reply Ok.
    {
      use std::sync::atomic::{AtomicU64, Ordering};
      static NAV_SEQ: AtomicU64 = AtomicU64::new(0);
      let nav_waiters = nav_waiters.clone();
      let committed_uri = committed_uri.clone();
      web_view.connect_load_changed(move |wv, event| match event {
        webkit6::LoadEvent::Started => {
          let url = wv.uri().map_or_else(String::new, |u| u.to_string());
          let id = format!("nav{}", NAV_SEQ.fetch_add(1, Ordering::Relaxed));
          writer::network_request_event(&id, "GET", &url, "Document");
        },
        webkit6::LoadEvent::Committed => {
          // Update the stashed URI to the post-redirect URL when
          // webkit6 actually surfaces one. For data:/about:srcdoc
          // loads it stays about:blank — the eager stash from
          // `op_navigate` already holds the right value, so only
          // overwrite when we get something non-empty AND not
          // about:blank (avoid clobbering the data: URL with the
          // about:blank webkit returns).
          if let Some(u) = wv.uri() {
            let s = u.to_string();
            if !s.is_empty() && s != "about:blank" {
              *committed_uri.borrow_mut() = Some(s);
            }
          }
        },
        webkit6::LoadEvent::Finished => {
          let mut waiters = nav_waiters.borrow_mut();
          for rid in waiters.drain(..) {
            writer::ok(rid);
          }
        },
        _ => {},
      });
    }

    if !url.is_empty() {
      web_view.load_uri(&url);
    }

    let entry = ViewEntry {
      web_view,
      window,
      nav_waiters,
      committed_uri,
      init_scripts: Vec::new(),
      ucm,
    };
    let id = reg.insert(entry);
    // Now that the registry has assigned a view_id, plug it into the
    // shared cell so the `fdConsole` (and friends) closures emit it
    // on every streamed message.
    view_id_cell.set(id);
    id
  });

  writer::view_created(req_id, result);
}

// ─── Navigate ──────────────────────────────────────────────────────────────

fn op_navigate(req_id: u32, payload: &[u8]) {
  let mut off = 0;
  let url = str_decode(payload, &mut off);
  let view_id = read_u64(payload, &mut off);
  // Optional referer string. macOS host accepts it as the last field;
  // not all callers send it — `str_decode` returns "" on truncation.
  let referer = str_decode(payload, &mut off);

  with_view(req_id, view_id, |entry| {
    // Stash the requested URI eagerly. webkit6's `WebView::uri()`
    // doesn't surface data:/about:srcdoc URLs (returns about:blank
    // for them at all load phases), so the load-changed handler
    // can't recover it. The caller's URL is the source of truth.
    *entry.committed_uri.borrow_mut() = Some(url.clone());
    if referer.is_empty() {
      entry.web_view.load_uri(&url);
    } else {
      let req = webkit6::URIRequest::new(&url);
      // webkit6 0.6.1 has no setter for the whole `http_headers` map —
      // only a getter that returns an existing `Soup::MessageHeaders`.
      // Append the Referer to the existing headers (creates it if the
      // URIRequest came pre-built with one; otherwise the header
      // never lands and Referer is silently lost — same gap noted in
      // docs/webkit-linux-port.md §5).
      if let Some(headers) = req.http_headers() {
        headers.append("Referer", &referer);
      }
      entry.web_view.load_request(&req);
    }
    writer::ok(req_id);
  });
}

// ─── Reload / GoBack / GoForward ──────────────────────────────────────────

fn op_reload(req_id: u32, payload: &[u8]) {
  let mut off = 0;
  let view_id = read_u64(payload, &mut off);
  with_view(req_id, view_id, |entry| {
    entry.web_view.reload();
    writer::ok(req_id);
  });
}

fn op_go_back(req_id: u32, payload: &[u8]) {
  let mut off = 0;
  let view_id = read_u64(payload, &mut off);
  with_view(req_id, view_id, |entry| {
    if entry.web_view.can_go_back() {
      entry.web_view.go_back();
      writer::ok(req_id);
    } else {
      writer::error(req_id, "no back history");
    }
  });
}

fn op_go_forward(req_id: u32, payload: &[u8]) {
  let mut off = 0;
  let view_id = read_u64(payload, &mut off);
  with_view(req_id, view_id, |entry| {
    if entry.web_view.can_go_forward() {
      entry.web_view.go_forward();
      writer::ok(req_id);
    } else {
      writer::error(req_id, "no forward history");
    }
  });
}

// ─── LoadHtml ──────────────────────────────────────────────────────────────

fn op_load_html(req_id: u32, payload: &[u8]) {
  // host.m wire: `u64 vid + str html + str base`.
  let mut off = 0;
  let view_id = read_u64(payload, &mut off);
  let html = str_decode(payload, &mut off);
  let base_uri = str_decode(payload, &mut off);
  with_view(req_id, view_id, |entry| {
    let base = if base_uri.is_empty() {
      None
    } else {
      Some(base_uri.as_str())
    };
    entry.web_view.load_html(&html, base);
    writer::ok(req_id);
  });
}

// ─── SetUserAgent ──────────────────────────────────────────────────────────

fn op_set_user_agent(req_id: u32, payload: &[u8]) {
  let mut off = 0;
  let ua = str_decode(payload, &mut off);
  let view_id = read_u64(payload, &mut off);
  with_view(req_id, view_id, |entry| {
    // Disambiguate vs `WidgetExt::settings` which also lives in scope
    // via the prelude. The webkit6 accessor returns `Option<Settings>`;
    // an attached view always has one but the binding is conservative.
    let Some(settings) = <webkit6::WebView as webkit6::prelude::WebViewExt>::settings(&entry.web_view) else {
      writer::error(req_id, "no Settings on view");
      return;
    };
    settings.set_user_agent(Some(&ua));
    writer::ok(req_id);
  });
}

// ─── AddInitScript ─────────────────────────────────────────────────────────

fn op_add_init_script(req_id: u32, payload: &[u8]) {
  // host.m wire: `u64 vid + str source`.
  let mut off = 0;
  let view_id = read_u64(payload, &mut off);
  let source = str_decode(payload, &mut off);

  REGISTRY.with(|reg| {
    let mut reg = reg.borrow_mut();
    let Some(entry) = reg.get_mut(view_id) else {
      writer::error(req_id, &format!("no such view {view_id}"));
      return;
    };
    let script = webkit6::UserScript::new(
      &source,
      webkit6::UserContentInjectedFrames::AllFrames,
      webkit6::UserScriptInjectionTime::Start,
      &[],
      &[],
    );
    entry.ucm.add_script(&script);
    entry.init_scripts.push(source);
    writer::ok(req_id);
  });
}

// ─── WaitNav ──────────────────────────────────────────────────────────────

fn op_wait_nav(req_id: u32, payload: &[u8]) {
  let mut off = 0;
  let view_id = read_u64(payload, &mut off);
  with_view(req_id, view_id, |entry| {
    // ALWAYS park — even when `is_loading()` returns false. webkit6's
    // `load_uri()` is async: it returns immediately and the load
    // starts on a subsequent main-loop iteration. Parent's `goto`
    // pattern is `Navigate → WaitNav`, with WaitNav typically arriving
    // before `is_loading()` flips to true. If we replied Ok on the
    // "not loading" branch we'd race and `WebView::uri()` /
    // `WebView::title()` would still be empty on the next GetUrl.
    // The `LoadEvent::Finished` signal handler in `op_create_view`
    // drains all parked waiters. Parent's 30s `send` timeout is the
    // fallback if a nav is never actually started.
    entry.nav_waiters.borrow_mut().push(req_id);
  });
}

// ─── SetViewport ──────────────────────────────────────────────────────────

fn op_set_viewport(req_id: u32, payload: &[u8]) {
  // host.m wire: `f64 width + f64 height + f64 dpr + u64 vid`.
  let mut off = 0;
  let width = read_f64(payload, &mut off);
  let height = read_f64(payload, &mut off);
  let _dpr = read_f64(payload, &mut off);
  let view_id = read_u64(payload, &mut off);
  with_view(req_id, view_id, |entry| {
    // f64 → i32 — viewport dimensions never exceed i32::MAX in practice.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let w = width as i32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let h = height as i32;
    // UFCS to pick GtkWindow's `set_default_size` over `WidgetExt`'s.
    <gtk4::Window as gtk4::prelude::GtkWindowExt>::set_default_size(&entry.window, w, h);
    // GTK4 won't actually resize a child without re-realizing, but
    // the WebView still picks up the new logical size via its
    // GtkWidget allocation. For headless under xvfb this is a no-op
    // visually — but `innerWidth`/`innerHeight` on the JS side
    // reflect the new bounds because GTK plumbing.
    writer::ok(req_id);
  });
}

// ─── SetLocale / SetTimezone ───────────────────────────────────────────────

fn install_init_shim(req_id: u32, view_id: u64, shim_source: &str) {
  REGISTRY.with(|reg| {
    let mut reg = reg.borrow_mut();
    let Some(entry) = reg.get_mut(view_id) else {
      writer::error(req_id, &format!("no such view {view_id}"));
      return;
    };
    let script = webkit6::UserScript::new(
      shim_source,
      webkit6::UserContentInjectedFrames::AllFrames,
      webkit6::UserScriptInjectionTime::Start,
      &[],
      &[],
    );
    entry.ucm.add_script(&script);
    // Also evaluate immediately for the current page (matches the
    // macOS host behaviour of `evaluateJavaScript`-ing each
    // UserScript source against the live document on view rebind).
    entry
      .web_view
      .evaluate_javascript(shim_source, None, None, None::<&gio::Cancellable>, |_| {});
    writer::ok(req_id);
  });
}

fn op_set_locale(req_id: u32, payload: &[u8]) {
  // host.m wire: `u64 vid + str locale`.
  let mut off = 0;
  let view_id = read_u64(payload, &mut off);
  let locale = str_decode(payload, &mut off);
  // Override navigator.language and Intl.* lookups. Matches the macOS
  // host's JS-side approach (no native API for per-view locale in
  // either WKWebView or WebKitGTK 6.0).
  let locale_json = serde_json::to_string(&locale).unwrap_or_else(|_| String::from("\"en-US\""));
  let shim = format!(
    "(function(){{\
       const L = {locale_json};\
       try {{ Object.defineProperty(navigator, 'language', {{ get: () => L, configurable: true }}); }} catch (e) {{}}\
       try {{ Object.defineProperty(navigator, 'languages', {{ get: () => [L], configurable: true }}); }} catch (e) {{}}\
     }})();"
  );
  install_init_shim(req_id, view_id, &shim);
}

fn op_set_timezone(req_id: u32, payload: &[u8]) {
  // host.m wire: `u64 vid + str tz`.
  let mut off = 0;
  let view_id = read_u64(payload, &mut off);
  let tz = str_decode(payload, &mut off);
  let tz_json = serde_json::to_string(&tz).unwrap_or_else(|_| String::from("\"UTC\""));
  // Patch Intl.DateTimeFormat.resolvedOptions to advertise the
  // requested zone. Same envelope as the macOS host.
  let shim = format!(
    "(function(){{\
       const TZ = {tz_json};\
       try {{\
         const OrigDTF = Intl.DateTimeFormat;\
         function Patched(...args) {{\
           if (args.length === 0) {{ args = [undefined, {{ timeZone: TZ }}]; }}\
           else if (args.length === 1) {{ args = [args[0], {{ timeZone: TZ }}]; }}\
           else if (!args[1] || args[1].timeZone === undefined) {{ args[1] = Object.assign({{ timeZone: TZ }}, args[1] || {{}}); }}\
           return new OrigDTF(...args);\
         }}\
         Patched.prototype = OrigDTF.prototype;\
         Intl.DateTimeFormat = Patched;\
       }} catch (e) {{}}\
     }})();"
  );
  install_init_shim(req_id, view_id, &shim);
}

// ─── EmulateMedia ──────────────────────────────────────────────────────────

/// For each field, emit a JS statement that mutates the persistent
/// `window.__fdMedia` state map:
///   action 2 (Set):       __fdMedia.key = value;
///   action 1 (Disabled):  __fdMedia.key = null;
///   action 0 (Unchanged): no statement (preserve any prior value).
/// The matchMedia override then reads __fdMedia each query so cross-
/// call merging works correctly (null disable preserves siblings).
fn emulate_media_field_to_stmt(key: &str, action_value: &(u8, String)) -> String {
  match action_value.0 {
    2 => {
      let v = serde_json::to_string(&action_value.1).unwrap_or_else(|_| String::from("\"\""));
      format!("window.__fdMedia.{key} = {v};")
    },
    // Disabled: store explicit `null` (not `delete`) so the shim
    // intercepts the matchMedia call and returns the no-preference
    // default rather than falling through to the system value
    // (under WebKitGTK on a dark-themed desktop the fallthrough
    // returns dark=true, which Playwright's semantic for `null`
    // means "no preference" → false).
    1 => format!("window.__fdMedia.{key} = null;"),
    _ => String::new(),
  }
}

fn op_emulate_media(req_id: u32, payload: &[u8]) {
  // host.m wire: `u64 vid + 5 × (u8 action + str value)`. Action codes:
  //   0 = Unchanged (host leaves the override alone for this field)
  //   1 = Disabled (clear any prior override)
  //   2 = Set (apply value)
  // Field order: color_scheme, reduced_motion, forced_colors, media, contrast.
  let mut off = 0;
  let view_id = read_u64(payload, &mut off);
  let read_field = |o: &mut usize| -> (u8, String) {
    let action = read_u8(payload, o);
    let value = str_decode(payload, o);
    (action, value)
  };
  let mut off2 = off;
  let color_scheme = read_field(&mut off2);
  let reduced_motion = read_field(&mut off2);
  let forced_colors = read_field(&mut off2);
  let media = read_field(&mut off2);
  let contrast = read_field(&mut off2);

  let mut state_updates = String::new();
  state_updates.push_str(&emulate_media_field_to_stmt("colorScheme", &color_scheme));
  state_updates.push_str(&emulate_media_field_to_stmt("reducedMotion", &reduced_motion));
  state_updates.push_str(&emulate_media_field_to_stmt("forcedColors", &forced_colors));
  state_updates.push_str(&emulate_media_field_to_stmt("media", &media));
  state_updates.push_str(&emulate_media_field_to_stmt("contrast", &contrast));

  with_view(req_id, view_id, |entry| {
    // webkit6 0.6.1 has no native `set_color_scheme` API (it lives on
    // a newer WebKitGTK trait we don't have here). Route EVERYTHING —
    // color-scheme, media, forced-colors, contrast, reduced-motion —
    // through the JS `matchMedia` shim. Same envelope as the macOS
    // host's JS path for the non-native fields.
    let shim = format!(
      "(function(){{\
         window.__fdMedia = window.__fdMedia || {{}};\
         {state_updates}\
         if (!window.__fdMediaInstalled) {{\
           window.__fdMediaInstalled = true;\
           const origMM = window.matchMedia.bind(window);\
           window.matchMedia = function (q) {{\
             const O = window.__fdMedia || {{}};\
             const ql = String(q).toLowerCase();\
             function mk(m) {{ return {{ matches: m, media: q, addEventListener:()=>{{}}, removeEventListener:()=>{{}}, addListener:()=>{{}}, removeListener:()=>{{}} }}; }}\
             // For each preference: if the key is PRESENT (set or
             // explicit null), the shim intercepts. null means no
             // preference — return false for any specific query.
             if ('colorScheme' in O && ql.indexOf('prefers-color-scheme') >= 0) return mk(O.colorScheme ? ql.indexOf(O.colorScheme) >= 0 : false);\
             if ('media' in O && ql.indexOf('print') >= 0) return mk(O.media === 'print');\
             if ('media' in O && ql.indexOf('screen') >= 0) return mk(O.media !== 'print');\
             if ('forcedColors' in O && ql.indexOf('forced-colors') >= 0) return mk(O.forcedColors ? (O.forcedColors === 'active' ? ql.indexOf('active') >= 0 : ql.indexOf('none') >= 0) : false);\
             if ('contrast' in O && ql.indexOf('prefers-contrast') >= 0) return mk(O.contrast ? ql.indexOf(O.contrast) >= 0 : false);\
             if ('reducedMotion' in O && ql.indexOf('prefers-reduced-motion') >= 0) return mk(O.reducedMotion ? ql.indexOf(O.reducedMotion) >= 0 : false);\
             return origMM(q);\
           }};\
         }}\
       }})();"
    );
    // await JS so the test's subsequent matchMedia call sees the
    // updated state — fire-and-forget races with the next op.
    let web_view = entry.web_view.clone();
    evaluate_then_ok(web_view, shim, req_id);
  });
}

// ─── Evaluate (already had this in 2b — reuse) ────────────────────────────

fn op_evaluate(req_id: u32, payload: &[u8]) {
  let mut off = 0;
  let expr = str_decode(payload, &mut off);
  let view_id = read_u64(payload, &mut off);

  with_webview(req_id, view_id, move |web_view| {
    let main_ctx = glib::MainContext::default();
    main_ctx.spawn_local(async move {
      let args = glib::VariantDict::new(None);
      args.insert("__fd_expr", expr.as_str());
      let args_variant = args.end();
      match web_view
        .call_async_javascript_function_future(js::EVAL_BODY, Some(&args_variant), None, None::<&str>)
        .await
      {
        Ok(v) if v.is_null() => writer::value_raw_json(req_id, "null"),
        Ok(v) => {
          let s = v.to_str();
          writer::value_raw_json(req_id, s.as_str());
        },
        Err(e) => writer::error(req_id, &format!("evaluate: {e}")),
      }
    });
  });
}

// ─── ReleaseRef ────────────────────────────────────────────────────────────

fn op_release_ref(req_id: u32, payload: &[u8]) {
  let mut off = 0;
  let handle_ref = read_u64(payload, &mut off);
  let view_id = read_u64(payload, &mut off);

  with_webview(req_id, view_id, move |web_view| {
    let main_ctx = glib::MainContext::default();
    main_ctx.spawn_local(async move {
      let args = glib::VariantDict::new(None);
      // JS code uses `__fd_ref_id`; arg name must match.
      // handle_ref is u64 — glib::Variant accepts u64 directly.
      args.insert("__fd_ref_id", handle_ref);
      let args_variant = args.end();
      match web_view
        .call_async_javascript_function_future(js::RELEASE_REF, Some(&args_variant), None, None::<&str>)
        .await
      {
        Ok(_) => writer::ok(req_id),
        Err(e) => writer::error(req_id, &format!("release_ref: {e}")),
      }
    });
  });
}

// ─── AccessibilityTree ─────────────────────────────────────────────────────

fn op_accessibility_tree(req_id: u32, payload: &[u8]) {
  // host.m wire: `u64 vid + i32 depth` (depth currently ignored — the
  // shared JS shim doesn't honor it yet; same behaviour as macOS host
  // when AX tree returns the full DOM walk).
  let mut off = 0;
  let view_id = read_u64(payload, &mut off);
  let _depth = read_i32(payload, &mut off);

  with_webview(req_id, view_id, move |web_view| {
    let main_ctx = glib::MainContext::default();
    main_ctx.spawn_local(async move {
      // The shared `AX_TREE` shim is a plain expression (not a
      // function body), so we route through `evaluate_javascript`
      // and JSON-stringify on the page side already.
      match web_view.evaluate_javascript_future(js::AX_TREE, None, None).await {
        Ok(v) if v.is_null() => writer::value_raw_json(req_id, "[]"),
        Ok(v) => {
          let s = v.to_str();
          // The shim already returns a JSON string, but
          // `JSCValue::to_str` returns the JS string value — for a
          // JS string result, that's the JSON text directly.
          writer::value_raw_json(req_id, s.as_str());
        },
        Err(e) => writer::error(req_id, &format!("accessibility_tree: {e}")),
      }
    });
  });
}

// ─── Cookies ───────────────────────────────────────────────────────────────

fn op_get_cookies(req_id: u32, payload: &[u8]) {
  // host.m wire: `u64 vid`. macOS reads ALL cookies for the data store;
  // we do the same via a wildcard URL on the cookie manager.
  let mut off = 0;
  let _view_id = read_u64(payload, &mut off);

  let session = REGISTRY.with(|reg| reg.borrow().peek_network_session().cloned());
  let Some(session) = session else {
    writer::value_raw_json(req_id, "[]");
    return;
  };
  let Some(cm) = session.cookie_manager() else {
    writer::value_raw_json(req_id, "[]");
    return;
  };

  let main_ctx = glib::MainContext::default();
  main_ctx.spawn_local(async move {
    // `cookies_future(url)` filters to cookies matching `url`. macOS's
    // `WKHTTPCookieStore.getAllCookies:` returns EVERYTHING regardless
    // of URL. webkit6's equivalent is `all_cookies_future`.
    match cm.all_cookies_future().await {
      Ok(mut cookies) => {
        // `soup::Cookie`'s getters take `&mut self` — iterate by mut.
        let mut arr: Vec<serde_json::Value> = Vec::with_capacity(cookies.len());
        for c in &mut cookies {
          let mut obj = serde_json::Map::new();
          obj.insert("name".into(), c.name().unwrap_or_default().as_str().into());
          obj.insert("value".into(), c.value().unwrap_or_default().as_str().into());
          obj.insert("domain".into(), c.domain().unwrap_or_default().as_str().into());
          obj.insert("path".into(), c.path().unwrap_or_default().as_str().into());
          obj.insert("secure".into(), c.is_secure().into());
          obj.insert("httpOnly".into(), c.is_http_only().into());
          arr.push(serde_json::Value::Object(obj));
        }
        let json = serde_json::to_string(&serde_json::Value::Array(arr)).unwrap_or_else(|_| String::from("[]"));
        writer::value_raw_json(req_id, &json);
      },
      Err(e) => writer::error(req_id, &format!("get_cookies: {e}")),
    }
  });
}

fn op_set_cookie(req_id: u32, payload: &[u8]) {
  // host.m wire:
  //   `u64 vid + str name + str value + str domain + str path +
  //    u8 secure + u8 httpOnly + f64 expires + str sameSite`
  let mut off = 0;
  let _view_id = read_u64(payload, &mut off);
  let name = str_decode(payload, &mut off);
  let value = str_decode(payload, &mut off);
  let domain = str_decode(payload, &mut off);
  let path_str = str_decode(payload, &mut off);
  let secure = read_u8(payload, &mut off) != 0;
  let http_only = read_u8(payload, &mut off) != 0;
  let _expires = read_f64(payload, &mut off);
  let _same_site = str_decode(payload, &mut off);

  let session = REGISTRY.with(|reg| reg.borrow().peek_network_session().cloned());
  let Some(session) = session else {
    writer::error(req_id, "no network session yet (create a view first)");
    return;
  };
  let Some(cm) = session.cookie_manager() else {
    writer::error(req_id, "no cookie manager");
    return;
  };

  let path_for_cookie = if path_str.is_empty() {
    String::from("/")
  } else {
    path_str
  };
  let mut cookie = webkit6::soup::Cookie::new(&name, &value, &domain, &path_for_cookie, -1);
  cookie.set_secure(secure);
  cookie.set_http_only(http_only);

  let main_ctx = glib::MainContext::default();
  main_ctx.spawn_local(async move {
    match cm.add_cookie_future(&cookie).await {
      Ok(()) => writer::ok(req_id),
      Err(e) => writer::error(req_id, &format!("set_cookie: {e}")),
    }
  });
}

fn op_delete_cookie(req_id: u32, payload: &[u8]) {
  // host.m wire: `u64 vid + str name + str domain`.
  let mut off = 0;
  let _view_id = read_u64(payload, &mut off);
  let name = str_decode(payload, &mut off);
  let domain = str_decode(payload, &mut off);

  let session = REGISTRY.with(|reg| reg.borrow().peek_network_session().cloned());
  let Some(session) = session else {
    writer::ok(req_id);
    return;
  };
  let Some(cm) = session.cookie_manager() else {
    writer::ok(req_id);
    return;
  };

  let main_ctx = glib::MainContext::default();
  main_ctx.spawn_local(async move {
    // Fetch all cookies (across all domains), find matching name (+
    // domain if specified), delete each. Matches macOS host's
    // `[NSHTTPCookie deleteCookie:]` semantics.
    match cm.all_cookies_future().await {
      Ok(mut cookies) => {
        for c in &mut cookies {
          let c_name = c.name().map(|g| g.as_str().to_string()).unwrap_or_default();
          let c_domain = c.domain().map(|g| g.as_str().to_string()).unwrap_or_default();
          let name_match = c_name == name;
          let domain_match = domain.is_empty() || c_domain == domain || c_domain.trim_start_matches('.') == domain;
          if name_match && domain_match {
            let _ = cm.delete_cookie_future(c).await;
          }
        }
        writer::ok(req_id);
      },
      Err(e) => writer::error(req_id, &format!("delete_cookie: {e}")),
    }
  });
}

fn op_clear_cookies(req_id: u32, _payload: &[u8]) {
  let session = REGISTRY.with(|reg| reg.borrow().peek_network_session().cloned());
  let Some(session) = session else {
    writer::ok(req_id);
    return;
  };
  let Some(wdm) = session.website_data_manager() else {
    writer::ok(req_id);
    return;
  };

  // `WebsiteDataManager::clear` in webkit6 0.6.1 is callback-based
  // (no `clear_future`). Reply directly from the callback — runs on
  // the GTK main thread on completion, which is where the writer
  // thread-local lives, so `writer::ok` is safe.
  wdm.clear(
    webkit6::WebsiteDataTypes::COOKIES,
    glib::TimeSpan::from_seconds(0),
    None::<&gio::Cancellable>,
    move |res| match res {
      Ok(()) => writer::ok(req_id),
      Err(e) => writer::error(req_id, &format!("clear_cookies: {e}")),
    },
  );
}

// ─── Screenshot ────────────────────────────────────────────────────────────

fn op_screenshot(req_id: u32, payload: &[u8]) {
  // host.m wire: `u8 format(0=png,1=jpeg,2=webp) + u8 quality + u64 vid`.
  // We currently emit PNG regardless (Texture::save_to_png_bytes); the
  // `format` and `quality` bytes are read for layout-correctness but
  // ignored — same end result as macOS when only PNG is requested.
  let mut off = 0;
  let _format = read_u8(payload, &mut off);
  let _quality = read_u8(payload, &mut off);
  let view_id = read_u64(payload, &mut off);

  with_webview(req_id, view_id, move |web_view| {
    let main_ctx = glib::MainContext::default();
    main_ctx.spawn_local(async move {
      // FullDocument vs Visible — the parent doesn't pass a fullPage
      // flag on this Op, so we default to Visible (matches the macOS
      // host's `WKSnapshotConfiguration` default).
      let region = webkit6::SnapshotRegion::Visible;
      match web_view
        .snapshot_future(region, webkit6::SnapshotOptions::empty())
        .await
      {
        Ok(texture) => {
          // `gdk::Texture::save_to_png_bytes` returns `glib::Bytes`,
          // a refcounted slice view of the encoded PNG buffer.
          use webkit6::gdk::prelude::TextureExt;
          let bytes = texture.save_to_png_bytes();
          match write_shm(&bytes) {
            Ok(payload) => writer::write_shm_screenshot(req_id, &payload),
            Err(e) => writer::error(req_id, &format!("shm: {e}")),
          }
        },
        Err(e) => writer::error(req_id, &format!("snapshot: {e}")),
      }
    });
  });
}

/// Allocate a POSIX shared-memory segment, copy `bytes` into it, and
/// build the `Rep::ShmScreenshot` payload `(u32 nameLen + name + u32 pngLen)`.
/// Parent unlinks after read; see `decode_shm_screenshot` in
/// `crates/ferridriver/src/backend/webkit/ipc.rs`.
fn write_shm(bytes: &[u8]) -> Result<Vec<u8>, String> {
  use std::ffi::CString;
  let name = format!("/ferridriver-shot-{}-{}", std::process::id(), unique_seq());
  let c_name = CString::new(name.as_bytes()).map_err(|e| format!("CString: {e}"))?;
  let len = bytes.len();

  // SAFETY: All libc calls are POSIX shared memory primitives. We
  // create with O_CREAT|O_EXCL so the kernel rejects collisions; we
  // ftruncate to the exact length we'll write; we mmap RW for the
  // memcpy; we munmap on success/failure. The caller (parent) is
  // responsible for `shm_unlink` after reading the bytes.
  #[allow(unsafe_code)]
  unsafe {
    let fd = libc::shm_open(c_name.as_ptr(), libc::O_RDWR | libc::O_CREAT | libc::O_EXCL, 0o600);
    if fd < 0 {
      return Err(format!("shm_open: errno={}", *libc::__errno_location()));
    }
    let len_off = libc::off_t::try_from(len).map_err(|e| format!("ftruncate len overflow: {e}"))?;
    if libc::ftruncate(fd, len_off) != 0 {
      let e = *libc::__errno_location();
      libc::close(fd);
      libc::shm_unlink(c_name.as_ptr());
      return Err(format!("ftruncate: errno={e}"));
    }
    let map = libc::mmap(
      std::ptr::null_mut(),
      len,
      libc::PROT_READ | libc::PROT_WRITE,
      libc::MAP_SHARED,
      fd,
      0,
    );
    libc::close(fd);
    if map == libc::MAP_FAILED {
      libc::shm_unlink(c_name.as_ptr());
      return Err("mmap MAP_FAILED".into());
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), map.cast::<u8>(), len);
    libc::munmap(map, len);
  }

  // Build the payload bytes — same shape as the macOS host.
  let mut out = Vec::with_capacity(8 + name.len());
  let name_len = u32::try_from(name.len()).map_err(|e| format!("name too long: {e}"))?;
  let png_len = u32::try_from(len).map_err(|e| format!("png too large: {e}"))?;
  out.extend_from_slice(&name_len.to_le_bytes());
  out.extend_from_slice(name.as_bytes());
  out.extend_from_slice(&png_len.to_le_bytes());
  Ok(out)
}

/// Monotonic-ish counter to disambiguate concurrent shm names in the
/// same host process.
fn unique_seq() -> u64 {
  use std::sync::atomic::{AtomicU64, Ordering};
  static SEQ: AtomicU64 = AtomicU64::new(1);
  SEQ.fetch_add(1, Ordering::Relaxed)
}

// ─── SetFileInput ──────────────────────────────────────────────────────────
//
// Synthesises a JS `DataTransfer` carrying the file bytes and assigns
// `input.files`. Mirrors the macOS host's approach exactly. Path
// resolution happens host-side (this process) so the page-side JS
// never touches the filesystem — same security envelope as macOS.

fn op_set_file_input(req_id: u32, payload: &[u8]) {
  // host.m wire: `str selector + str filePath + u64 vid` — SINGLE
  // file per call. Multiple files = multiple SetFileInput frames.
  let mut off = 0;
  let selector = str_decode(payload, &mut off);
  let path = str_decode(payload, &mut off);
  let view_id = read_u64(payload, &mut off);

  let mut files: Vec<(String, Vec<u8>, String)> = Vec::with_capacity(1);
  match std::fs::read(&path) {
    Ok(bytes) => {
      let name = std::path::Path::new(&path)
        .file_name()
        .map_or_else(|| path.clone(), |f| f.to_string_lossy().to_string());
      let mime = mime_for_ext(&path);
      files.push((name, bytes, mime));
    },
    Err(e) => {
      writer::error(req_id, &format!("set_file_input: read {path}: {e}"));
      return;
    },
  }

  let sel_json = serde_json::to_string(&selector).unwrap_or_else(|_| String::from("\"\""));
  let files_json = serde_json::to_string(
    &files
      .iter()
      .map(|(name, bytes, mime)| {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        serde_json::json!({ "name": name, "type": mime, "data": b64 })
      })
      .collect::<Vec<_>>(),
  )
  .unwrap_or_else(|_| String::from("[]"));

  let shim = format!(
    "(function(){{\
       const el = document.querySelector({sel_json});\
       if (!el) throw new Error('no input matches selector');\
       const files = {files_json};\
       const dt = new DataTransfer();\
       // APPEND mode: include any files already on the input so calls
       // accumulate (parent's `set_file_input` clears via `el.value=''`
       // before sending the FIRST file, then sends each remaining file
       // expecting append semantics).
       const existing = el.files;\
       if (existing) {{\
         for (let i = 0; i < existing.length; i++) {{\
           dt.items.add(existing[i]);\
         }}\
       }}\
       for (const f of files) {{\
         const bin = atob(f.data);\
         const arr = new Uint8Array(bin.length);\
         for (let i = 0; i < bin.length; i++) arr[i] = bin.charCodeAt(i);\
         dt.items.add(new File([arr], f.name, {{ type: f.type }}));\
       }}\
       el.files = dt.files;\
       el.dispatchEvent(new Event('input', {{ bubbles: true }}));\
       el.dispatchEvent(new Event('change', {{ bubbles: true }}));\
     }})();"
  );

  with_view(req_id, view_id, |entry| {
    // SetFileInput: await JS to ensure files are set before parent
    // dispatches next op (e.g. test reads input.files[N].name).
    let web_view = entry.web_view.clone();
    evaluate_then_ok(web_view, shim, req_id);
  });
}

fn mime_for_ext(path: &str) -> String {
  let ext = std::path::Path::new(path)
    .extension()
    .and_then(|e| e.to_str())
    .map(str::to_ascii_lowercase)
    .unwrap_or_default();
  match ext.as_str() {
    "txt" => "text/plain",
    "html" | "htm" => "text/html",
    "json" => "application/json",
    "pdf" => "application/pdf",
    "png" => "image/png",
    "jpg" | "jpeg" => "image/jpeg",
    "gif" => "image/gif",
    "svg" => "image/svg+xml",
    "csv" => "text/csv",
    "xml" => "application/xml",
    "zip" => "application/zip",
    _ => "application/octet-stream",
  }
  .into()
}

// ─── Input synthesis (Click / MouseEvent / Type / Keys) ────────────────────
//
// JS-dispatched events on webkit6 — GTK4 has no public API to construct
// `GdkEvent`s, and webkit6 has no `WebView::send_event` accessor.
// `event.isTrusted` is therefore `false` (see `docs/webkit-linux-port.md
// §5a`). We compensate by emulating the browser's default actions in
// JS: focus on mousedown, click-after-mouseup pairing, dblclick on
// click_count >= 2, checkbox/radio `.checked` toggle, contextmenu on
// right-button-up.

/// Embedded JS that lazily defines `window.__fdInput` — a tiny helper
/// object with `mouseEvent(...)`, `typeText(...)`, `keyEvent(...)`.
/// We install it on every input op (idempotent) so the `WebView` always
/// has it regardless of navigation/document-replacement timing. The
/// macOS host does this work in `NSEvent` space; on Linux we
/// DOM-dispatch.
const FD_INPUT_INSTALL: &str = "
(function () {
  if (window.__fdInput) return;
  // Persistent state across input ops. Modifier bitmask mirrors the
  // CDP scheme: 1=Alt, 2=Control, 4=Meta, 8=Shift.
  const state = { modifiers: 0, lastDownEl: null };
  const MOD_BITS = { Alt: 1, Control: 2, Meta: 4, Shift: 8, AltGraph: 1, Super: 4 };
  function modStateFlags() {
    return {
      altKey: (state.modifiers & 1) !== 0,
      ctrlKey: (state.modifiers & 2) !== 0,
      metaKey: (state.modifiers & 4) !== 0,
      shiftKey: (state.modifiers & 8) !== 0,
    };
  }
  function commonInit(x, y, button, buttons, clickCount) {
    return Object.assign({
      bubbles: true, cancelable: true, composed: true, view: window,
      clientX: x, clientY: y, screenX: x, screenY: y,
      button, buttons, detail: clickCount,
    }, modStateFlags());
  }
  function keyInit(key) {
    return Object.assign({
      key, code: keyCodeFor(key),
      bubbles: true, cancelable: true, composed: true, view: window,
    }, modStateFlags());
  }
  function elementAt(x, y) {
    return document.elementFromPoint(x, y) || document.body || document.documentElement;
  }
  function isCheckable(el) {
    return el && el.tagName === 'INPUT' && (el.type === 'checkbox' || el.type === 'radio');
  }
  // When the elementFromPoint at click coords lands on body/html
  // (happens under headless GTK4 if layout coords don't exactly match),
  // walk the elementsFromPoint stack AND fall back to active/focused
  // input — covers the common 'just clicked the focused checkbox' case.
  function findCheckableNear(x, y) {
    if (typeof document.elementsFromPoint === 'function') {
      const stack = document.elementsFromPoint(x, y);
      for (const e of stack) {
        if (isCheckable(e)) return e;
      }
    }
    if (isCheckable(document.activeElement)) return document.activeElement;
    return null;
  }
  function isEditable(el) {
    return el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable);
  }
  function focusIfPossible(el) {
    if (!el) return;
    if (typeof el.focus === 'function') {
      try { el.focus({ preventScroll: true }); } catch (e) {}
    }
  }
  function insertAtCursor(el, text) {
    if (!el) return;
    if (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') {
      const start = typeof el.selectionStart === 'number' ? el.selectionStart : (el.value || '').length;
      const end = typeof el.selectionEnd === 'number' ? el.selectionEnd : start;
      el.value = (el.value || '').slice(0, start) + text + (el.value || '').slice(end);
      const newPos = start + text.length;
      try { el.setSelectionRange(newPos, newPos); } catch (e) {}
      el.dispatchEvent(new InputEvent('input', Object.assign({ bubbles: true, data: text, inputType: 'insertText' }, modStateFlags())));
    } else if (el.isContentEditable) {
      try { document.execCommand('insertText', false, text); } catch (e) {}
      el.dispatchEvent(new InputEvent('input', Object.assign({ bubbles: true, data: text, inputType: 'insertText' }, modStateFlags())));
    }
  }
  function deletePrevChar(el) {
    if (!el) return;
    if (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') {
      const start = typeof el.selectionStart === 'number' ? el.selectionStart : (el.value || '').length;
      const end = typeof el.selectionEnd === 'number' ? el.selectionEnd : start;
      if (start === end && start > 0) {
        el.value = (el.value || '').slice(0, start - 1) + (el.value || '').slice(end);
        const newPos = start - 1;
        try { el.setSelectionRange(newPos, newPos); } catch (e) {}
      } else if (start !== end) {
        el.value = (el.value || '').slice(0, start) + (el.value || '').slice(end);
        try { el.setSelectionRange(start, start); } catch (e) {}
      }
      el.dispatchEvent(new InputEvent('input', Object.assign({ bubbles: true, inputType: 'deleteContentBackward' }, modStateFlags())));
    } else if (el.isContentEditable) {
      try { document.execCommand('delete'); } catch (e) {}
      el.dispatchEvent(new InputEvent('input', Object.assign({ bubbles: true, inputType: 'deleteContentBackward' }, modStateFlags())));
    }
  }
  function applyKeyDefault(key, el) {
    if (!el || !isEditable(el)) return;
    if (key && key.length === 1) {
      insertAtCursor(el, key);
    } else if (key === 'Enter') {
      // textarea + contenteditable: newline. input: form submit (we
      // emulate by dispatching a submit event on the form if present).
      if (el.tagName === 'TEXTAREA' || el.isContentEditable) {
        insertAtCursor(el, '\\n');
      } else if (el.tagName === 'INPUT' && el.form) {
        // Form submit emulation — tests check for the submit event.
        try { el.form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); } catch (e) {}
      }
    } else if (key === 'Backspace') {
      deletePrevChar(el);
    } else if (key === 'Tab') {
      // Tab focus shift — simplest emulation: find next focusable.
      const focusable = Array.from(document.querySelectorAll(
        'input, button, select, textarea, a[href], [tabindex]:not([tabindex=\"-1\"])'
      ));
      const idx = focusable.indexOf(el);
      const next = focusable[(idx + 1) % focusable.length] || el;
      focusIfPossible(next);
    }
  }
  window.__fdInput = {
    mouseEvent(type, x, y, button, buttons, clickCount) {
      const el = elementAt(x, y);
      const init = commonInit(x, y, button, buttons, clickCount);
      if (type === 'mousedown') {
        try { el.dispatchEvent(new PointerEvent('pointerdown', Object.assign({}, init, { pointerType: 'mouse' }))); } catch (e) {}
        el.dispatchEvent(new MouseEvent('mousedown', init));
        state.lastDownEl = el;
        focusIfPossible(el);
      } else if (type === 'mouseup') {
        try { el.dispatchEvent(new PointerEvent('pointerup', Object.assign({}, init, { pointerType: 'mouse' }))); } catch (e) {}
        el.dispatchEvent(new MouseEvent('mouseup', init));
        // Use the mousedown target for click/dblclick/contextmenu/
        // default-action dispatch. Real browsers do this: once a button
        // is pressed on element X, the click event targets X regardless
        // of where mouseup landed (unless the mouse strictly moved off
        // X first). Falling back to elementFromPoint at mouseup time is
        // unreliable under headless GTK4 where layout can lag.
        let target = state.lastDownEl || el;
        // If lastDownEl drifted to body/html under headless layout
        // collapse, try to recover the actual click target from the
        // elementsFromPoint stack or focused input.
        if (!isCheckable(target)) {
          const recovered = findCheckableNear(x, y);
          if (recovered) target = recovered;
        }
        if (button === 0) {
          // Snapshot pre-click state for checkable elements so we can
          // detect whether webkit's synthetic-click default action
          // toggled it (some webkit builds do toggle on
          // element.dispatchEvent(MouseEvent('click')), others don't).
          // Without this snapshot, double-toggling lands the element
          // back at its original state — exactly the symptom that made
          // test_script_check_uncheck flaky.
          const checkable = isCheckable(target) && !target.disabled;
          const wasChecked = checkable ? !!target.checked : false;
          // Dispatch synthetic click event WITH modifiers so onclick
          // handlers see shift/ctrl/etc. (native HTMLElement.click()
          // would lose modifier state — see test_script_click_options).
          // `dispatchEvent` returns `false` when a listener called
          // `preventDefault()` — required so a `preventDefault` checkbox
          // listener actually blocks the toggle (mirrors the browser
          // default-action contract; without this, our manual toggle
          // below would override the preventDefault, breaking
          // test_script_check_behavior step 2).
          const clickNotPrevented = target.dispatchEvent(new MouseEvent('click', init));
          if (clickCount >= 2) {
            target.dispatchEvent(new MouseEvent('dblclick', Object.assign({}, init, { detail: 2 })));
          }
          // Default action emulation — only toggle when the synthetic
          // click did NOT already flip the state AND was not
          // prevented (idempotent across browser engines that
          // disagree on whether untrusted clicks toggle checkboxes).
          if (checkable && clickNotPrevented) {
            if (target.type === 'checkbox') {
              if (target.checked === wasChecked) {
                target.checked = !wasChecked;
              }
            } else {
              // Radio: select target iff it isn't already the selected
              // member of the group. dispatching click on an unchecked
              // radio in some engines moves selection natively; only
              // force-set when it didn't.
              if (!target.checked) {
                const siblings = document.querySelectorAll(
                  'input[type=radio][name=' + JSON.stringify(target.name) + ']'
                );
                siblings.forEach(function (s) { s.checked = (s === target); });
              }
            }
            target.dispatchEvent(new Event('input', { bubbles: true }));
            target.dispatchEvent(new Event('change', { bubbles: true }));
          }
          // <label for=X>: clicking label = clicking the labeled control.
          if (target.tagName === 'LABEL' && target.control) {
            target.control.click();
          }
        }
        if (button === 2) {
          target.dispatchEvent(new MouseEvent('contextmenu', init));
        }
      } else if (type === 'mousemove') {
        try { el.dispatchEvent(new PointerEvent('pointermove', Object.assign({}, init, { pointerType: 'mouse' }))); } catch (e) {}
        el.dispatchEvent(new MouseEvent('mousemove', init));
      } else if (type === 'wheel') {
        el.dispatchEvent(new WheelEvent('wheel', Object.assign({}, init, { deltaX: 0, deltaY: 0 })));
      }
    },
    keyEvent(kind, key) {
      const el = document.activeElement || document.body || document.documentElement;
      // Track modifier state BEFORE dispatching so the event itself
      // carries the right shiftKey/ctrlKey/etc bits.
      if (kind === 'keydown' && MOD_BITS[key]) state.modifiers |= MOD_BITS[key];
      const init = keyInit(key);
      el.dispatchEvent(new KeyboardEvent(kind, init));
      if (kind === 'keydown') {
        // Default action on keydown for non-modifier keys.
        if (!MOD_BITS[key]) applyKeyDefault(key, el);
      }
      if (kind === 'keyup' && MOD_BITS[key]) state.modifiers &= ~MOD_BITS[key];
    },
    typeText(text) {
      const el = document.activeElement || document.body || document.documentElement;
      for (const ch of text) {
        const init = keyInit(ch);
        el.dispatchEvent(new KeyboardEvent('keydown', init));
        el.dispatchEvent(new KeyboardEvent('keypress', init));
        insertAtCursor(el, ch);
        el.dispatchEvent(new KeyboardEvent('keyup', init));
      }
      if (el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA')) {
        el.dispatchEvent(new Event('change', { bubbles: true }));
      }
    },
  };
  function keyCodeFor(k) {
    if (k && k.length === 1) {
      const c = k.toUpperCase();
      if (c >= 'A' && c <= 'Z') return 'Key' + c;
      if (c >= '0' && c <= '9') return 'Digit' + c;
    }
    return k || '';
  }
})();
";

/// Run `source` and reply Ok only after the JS finishes. Fire-and-forget
/// `evaluate_javascript` returns control to the host BEFORE the `WebKit`
/// web-process actually executes the script — so any subsequent Op
/// (poll for state change, etc.) runs against stale DOM state. For
/// state-changing input ops we must `evaluate_javascript_future` and
/// await before sending `Rep::Ok`.
fn evaluate_then_ok(web_view: webkit6::WebView, source: String, req_id: u32) {
  glib::MainContext::default().spawn_local(async move {
    match web_view.evaluate_javascript_future(&source, None, None).await {
      Ok(_) => writer::ok(req_id),
      Err(e) => writer::error(req_id, &format!("evaluate: {e}")),
    }
  });
}

fn op_click(req_id: u32, payload: &[u8]) {
  // host.m wire: `f64 x + f64 y + u64 vid`. Left-button single click.
  // Parent's `click_at_opts` actually drives clicks through MouseEvent;
  // we keep this for protocol compatibility with host.m.
  let mut off = 0;
  let x = read_f64(payload, &mut off);
  let y = read_f64(payload, &mut off);
  let view_id = read_u64(payload, &mut off);
  let combined = format!(
    "{FD_INPUT_INSTALL}window.__fdInput.mouseEvent('mousedown',{x},{y},0,1,1);window.__fdInput.mouseEvent('mouseup',{x},{y},0,0,1);"
  );
  with_webview(req_id, view_id, move |web_view| {
    evaluate_then_ok(web_view, combined, req_id);
  });
}

fn op_mouse_event(req_id: u32, payload: &[u8]) {
  // host.m wire:
  //   `u8 type (0=move,1=down,2=up,3=wheel) +
  //    u8 button (0=left,1=right,2=middle) +
  //    u32 click_count + f64 x + f64 y + u64 vid`
  let mut off = 0;
  let mouse_type = read_u8(payload, &mut off);
  let button = read_u8(payload, &mut off);
  let click_count = read_u32(payload, &mut off).max(1);
  let x = read_f64(payload, &mut off);
  let y = read_f64(payload, &mut off);
  let view_id = read_u64(payload, &mut off);

  // DOM button value: 0=left, 1=middle (not right), 2=right.
  // host.m wire uses button 0=left, 1=right, 2=middle. Translate.
  let dom_button = match button {
    1 => 2, // right
    2 => 1, // middle
    _ => 0, // left
  };
  // buttons bitmask: 1=left, 2=right, 4=middle.
  let buttons = match mouse_type {
    1 => match button {
      1 => 2,
      2 => 4,
      _ => 1,
    },
    _ => 0, // mouseup/move/wheel = no held buttons by default
  };
  let js_type = match mouse_type {
    1 => "mousedown",
    2 => "mouseup",
    3 => "wheel",
    _ => "mousemove",
  };

  let combined =
    format!("{FD_INPUT_INSTALL}window.__fdInput.mouseEvent('{js_type}',{x},{y},{dom_button},{buttons},{click_count});");
  with_webview(req_id, view_id, move |web_view| {
    evaluate_then_ok(web_view, combined, req_id);
  });
}

fn op_type(req_id: u32, payload: &[u8]) {
  let mut off = 0;
  let text = str_decode(payload, &mut off);
  let view_id = read_u64(payload, &mut off);
  let text_json = serde_json::to_string(&text).unwrap_or_else(|_| String::from("\"\""));
  let combined = format!("{FD_INPUT_INSTALL}window.__fdInput.typeText({text_json});");
  with_webview(req_id, view_id, move |web_view| {
    evaluate_then_ok(web_view, combined, req_id);
  });
}

fn op_press_key(req_id: u32, payload: &[u8]) {
  let mut off = 0;
  let key = str_decode(payload, &mut off);
  let view_id = read_u64(payload, &mut off);
  let key_json = serde_json::to_string(&key).unwrap_or_else(|_| String::from("\"\""));
  let combined = format!(
    "{FD_INPUT_INSTALL}window.__fdInput.keyEvent('keydown',{key_json});window.__fdInput.keyEvent('keyup',{key_json});"
  );
  with_webview(req_id, view_id, move |web_view| {
    evaluate_then_ok(web_view, combined, req_id);
  });
}

fn op_key_down(req_id: u32, payload: &[u8]) {
  let mut off = 0;
  let key = str_decode(payload, &mut off);
  let view_id = read_u64(payload, &mut off);
  let key_json = serde_json::to_string(&key).unwrap_or_else(|_| String::from("\"\""));
  let combined = format!("{FD_INPUT_INSTALL}window.__fdInput.keyEvent('keydown',{key_json});");
  with_webview(req_id, view_id, move |web_view| {
    evaluate_then_ok(web_view, combined, req_id);
  });
}

fn op_key_up(req_id: u32, payload: &[u8]) {
  let mut off = 0;
  let key = str_decode(payload, &mut off);
  let view_id = read_u64(payload, &mut off);
  let key_json = serde_json::to_string(&key).unwrap_or_else(|_| String::from("\"\""));
  let combined = format!("{FD_INPUT_INSTALL}window.__fdInput.keyEvent('keyup',{key_json});");
  with_webview(req_id, view_id, move |web_view| {
    evaluate_then_ok(web_view, combined, req_id);
  });
}

// ─── RouteRequest reply path ───────────────────────────────────────────────
//
// The shared `network.js` shim posts to `webkit.messageHandlers.fdRoute`
// expecting a reply. The reply path is plumbed in
// `crates/ferridriver-webkit-host/src/linux/route.rs` (Phase 2e —
// `connect_script_message_with_reply_received` wiring). For now the
// parent's `Op::RouteRequest` reply lands here as a no-op so the parent
// doesn't hang; routes don't fire on Linux until Phase 2e adds the
// `fdRoute` signal handler.

fn op_route_request_reply(req_id: u32, _payload: &[u8]) {
  writer::unsupported(req_id, "RouteRequest reply not yet wired (Phase 2e)");
}

// ─── GetUrl / GetTitle / Close / Shutdown / ListViews / Version ────────────

fn op_get_url(req_id: u32, payload: &[u8]) {
  let mut off = 0;
  let view_id = read_u64(payload, &mut off);
  with_view(req_id, view_id, |entry| {
    // Prefer the URI captured at `LoadEvent::Committed` — that's the
    // last load's actually-loaded URL (works for data:/about:srcdoc
    // unlike `WebView::uri()`). Fall back to `main_resource().uri()`
    // then `WebView::uri()`.
    let uri = entry
      .committed_uri
      .borrow()
      .clone()
      .or_else(|| {
        entry
          .web_view
          .main_resource()
          .and_then(|r| r.uri())
          .map(|g| g.to_string())
      })
      .or_else(|| entry.web_view.uri().map(|g| g.to_string()))
      .unwrap_or_default();
    writer::value_string(req_id, &uri);
  });
}

fn op_get_title(req_id: u32, payload: &[u8]) {
  let mut off = 0;
  let view_id = read_u64(payload, &mut off);
  with_view(req_id, view_id, |entry| {
    let title = entry.web_view.title().map(|g| g.to_string()).unwrap_or_default();
    writer::value_string(req_id, &title);
  });
}

fn op_close(req_id: u32, payload: &[u8]) {
  let mut off = 0;
  let view_id = read_u64(payload, &mut off);
  REGISTRY.with(|reg| {
    if reg.borrow_mut().remove(view_id).is_some() {
      writer::ok(req_id);
    } else {
      writer::error(req_id, &format!("no such view {view_id}"));
    }
  });
}

fn op_shutdown(_req_id: u32) {
  REGISTRY.with(|reg| {
    let ids = reg.borrow().ids();
    let mut reg = reg.borrow_mut();
    for id in ids {
      reg.remove(id);
    }
  });
  super::MAIN_LOOP.with(|ml| {
    if let Some(ml) = ml.borrow().as_ref() {
      ml.quit();
    }
  });
}

fn op_list_views(req_id: u32) {
  REGISTRY.with(|reg| {
    let ids = reg.borrow().ids();
    writer::view_list(req_id, &ids);
  });
}

fn op_get_webkit_version(req_id: u32) {
  writer::value_string(req_id, &super::webkit_version_string());
}
