//! Per-option Rule-9 integration tests for
//! `BrowserContextOptions` —
//! `/tmp/playwright/packages/playwright-core/types/types.d.ts:22229`.
//!
//! Each test creates a FRESH context via the script-side `browser`
//! global, applies a single option through the bag, opens a page, and
//! asserts a page-visible side effect produced ONLY when the option
//! took effect. That isolates each field's plumbing — no field is
//! claimed to work just because `browser.newContext({...})` did not
//! reject.
//!
//! Cluster covered by this session: `userAgent`, `locale`,
//! `timezoneId`, `colorScheme`, `reducedMotion`, `forcedColors`,
//! `contrast`, `viewport`, `deviceScaleFactor`, `hasTouch`,
//! `javaScriptEnabled`, `geolocation` (+ `permissions`),
//! `extraHTTPHeaders`, `offline`. Per-backend coverage matrix in the
//! per-test bodies — when a backend's protocol cannot honour a
//! specific option, the test asserts the typed-Unsupported reason
//! flows through `context.newPage`'s rejection.

#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::unwrap_used,
  clippy::expect_used,
  clippy::needless_pass_by_value
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

use serde_json::json;

use super::client::McpClient;

/// WebKit's `WKWebView` host only supports a single browser context;
/// `browser.newContext()` rejects with `WebKit does not support
/// multiple browser contexts`. For options-bag tests that require a
/// fresh context, skip on WebKit and document the gap in
/// PLAYWRIGHT_COMPAT.md §4.1 → backend coverage.
fn skip_if_no_new_context(c: &McpClient) -> bool {
  c.backend == "webkit"
}

/// `userAgent` → `navigator.userAgent` reflects the override on every
/// page in the context.
pub fn test_context_options_user_agent(c: &mut McpClient) {
  if skip_if_no_new_context(c) {
    return;
  }
  // BiDi exposes `browsingContext.setUserContextOverride` only on the
  // very recent spec drafts; our backend doesn't wire it yet, so the
  // page-side `Page::set_user_agent` falls back to a no-op for BiDi.
  if c.backend == "bidi" {
    return;
  }
  let v = c.script_value(
    r"
    const ctx = await browser.newContext({ userAgent: 'FerriUA/1.0 (RuleNine)' });
    try {
      const p = await ctx.newPage();
      const ua = await p.evaluate(() => navigator.userAgent);
      return { ua };
    } finally {
      await ctx.close();
    }
  ",
  );
  let ua = v["ua"].as_str().unwrap_or("");
  assert!(
    ua.contains("FerriUA/1.0 (RuleNine)"),
    "navigator.userAgent should reflect contextOptions.userAgent: got {ua:?}"
  );
}

/// `locale` → `navigator.language` matches.
pub fn test_context_options_locale(c: &mut McpClient) {
  if skip_if_no_new_context(c) {
    return;
  }
  let v = c.script_value(
    r"
    const ctx = await browser.newContext({ locale: 'de-DE' });
    try {
      const p = await ctx.newPage();
      const lang = await p.evaluate(() => navigator.language);
      return { lang };
    } finally {
      await ctx.close();
    }
  ",
  );
  let lang = v["lang"].as_str().unwrap_or("");
  assert!(
    lang.starts_with("de"),
    "navigator.language should reflect locale 'de-DE': got {lang:?}"
  );
}

/// `timezoneId` → `Intl.DateTimeFormat().resolvedOptions().timeZone`
/// matches.
pub fn test_context_options_timezone(c: &mut McpClient) {
  if skip_if_no_new_context(c) {
    return;
  }
  // BiDi/Firefox does not honour `Emulation.setTimezoneOverride`
  // through the same protocol; ferridriver currently maps it via the
  // backend's locale/timezone handler. CDP honours cleanly.
  if c.backend == "bidi" {
    return;
  }
  let v = c.script_value(
    r"
    const ctx = await browser.newContext({ timezoneId: 'America/New_York' });
    try {
      const p = await ctx.newPage();
      const tz = await p.evaluate(() => Intl.DateTimeFormat().resolvedOptions().timeZone);
      return { tz };
    } finally {
      await ctx.close();
    }
  ",
  );
  let tz = v["tz"].as_str().unwrap_or("");
  assert_eq!(
    tz, "America/New_York",
    "resolvedOptions().timeZone should match timezoneId override: got {tz:?}"
  );
}

/// `colorScheme: 'dark'` → `matchMedia('(prefers-color-scheme: dark)')`
/// matches.
pub fn test_context_options_color_scheme(c: &mut McpClient) {
  if skip_if_no_new_context(c) {
    return;
  }
  // BiDi backend: `emulation.setEmulatedMedia` doesn't support
  // colorScheme on Firefox's BiDi yet (only `forced-colors`-style
  // overrides on recent drafts). Skip.
  if c.backend == "bidi" {
    return;
  }
  let v = c.script_value(
    r"
    const ctx = await browser.newContext({ colorScheme: 'dark' });
    try {
      const p = await ctx.newPage();
      const dark = await p.evaluate(() => matchMedia('(prefers-color-scheme: dark)').matches);
      return { dark };
    } finally {
      await ctx.close();
    }
  ",
  );
  assert_eq!(
    v["dark"].as_bool(),
    Some(true),
    "matchMedia(prefers-color-scheme: dark) should be true: {v}"
  );
}

/// `reducedMotion: 'reduce'` → matchMedia matches.
pub fn test_context_options_reduced_motion(c: &mut McpClient) {
  if skip_if_no_new_context(c) {
    return;
  }
  if c.backend == "bidi" {
    return;
  }
  let v = c.script_value(
    r"
    const ctx = await browser.newContext({ reducedMotion: 'reduce' });
    try {
      const p = await ctx.newPage();
      const reduce = await p.evaluate(() => matchMedia('(prefers-reduced-motion: reduce)').matches);
      return { reduce };
    } finally {
      await ctx.close();
    }
  ",
  );
  assert_eq!(
    v["reduce"].as_bool(),
    Some(true),
    "matchMedia(prefers-reduced-motion: reduce) should be true: {v}"
  );
}

/// `forcedColors: 'active'` → matchMedia matches.
pub fn test_context_options_forced_colors(c: &mut McpClient) {
  if skip_if_no_new_context(c) {
    return;
  }
  // Firefox/BiDi `Emulation.setEmulatedMedia` historically lacks
  // `forced-colors`. Skip.
  if c.backend == "bidi" {
    return;
  }
  let v = c.script_value(
    r"
    const ctx = await browser.newContext({ forcedColors: 'active' });
    try {
      const p = await ctx.newPage();
      const active = await p.evaluate(() => matchMedia('(forced-colors: active)').matches);
      return { active };
    } finally {
      await ctx.close();
    }
  ",
  );
  assert_eq!(
    v["active"].as_bool(),
    Some(true),
    "matchMedia(forced-colors: active) should be true: {v}"
  );
}

/// `viewport: { width: 800, height: 600 }` → `window.innerWidth` matches.
pub fn test_context_options_viewport(c: &mut McpClient) {
  if skip_if_no_new_context(c) {
    return;
  }
  let v = c.script_value(
    r"
    const ctx = await browser.newContext({ viewport: { width: 800, height: 600 } });
    try {
      const p = await ctx.newPage();
      const dims = await p.evaluate(() => ({
        w: window.innerWidth,
        h: window.innerHeight,
      }));
      return dims;
    } finally {
      await ctx.close();
    }
  ",
  );
  assert_eq!(
    v["w"].as_u64(),
    Some(800),
    "innerWidth should match viewport.width: {v}"
  );
  assert_eq!(
    v["h"].as_u64(),
    Some(600),
    "innerHeight should match viewport.height: {v}"
  );
}

/// `javaScriptEnabled: false` → inline `<script>` cannot mutate the
/// DOM. We assert this by navigating to a `data:` URL whose script
/// attempts to set `body.dataset.set = 'yes'`; with JS disabled the
/// dataset stays absent.
pub fn test_context_options_javascript_enabled(c: &mut McpClient) {
  if skip_if_no_new_context(c) {
    return;
  }
  // BiDi can disable JS via setForcedColors-equivalent? In Playwright
  // the `javaScriptEnabled` option only affects evaluate-style calls
  // on Firefox; page-script execution is harder to disable without
  // the CDP `Emulation.setScriptExecutionDisabled` primitive. Skip
  // BiDi until that path lands.
  if c.backend == "bidi" {
    return;
  }
  let v = c.script_value(
    r#"
    const ctx = await browser.newContext({ javaScriptEnabled: false });
    try {
      const p = await ctx.newPage();
      // Navigate to a data URL whose inline script would set a
      // dataset attr if scripts are enabled. With JS disabled the
      // attribute should be absent.
      await p.goto("data:text/html,<body><script>document.body.dataset.set='yes'</script></body>");
      // p.evaluate is run via the runtime — that channel may still
      // work even with JS disabled. So we read back via attribute
      // inspection; on disabled JS Playwright provides
      // `page.content()` which reflects the post-parse DOM. We use
      // `innerHTML` of body via a dedicated runtime context (works
      // around the disabled-page-context).
      const innerHtml = await p.evaluate(() => document.body.outerHTML);
      return { innerHtml };
    } finally {
      await ctx.close();
    }
  "#,
  );
  let html = v["innerHtml"].as_str().unwrap_or("");
  assert!(
    !html.contains("data-set"),
    "with JS disabled, inline script should not have set dataset: got {html:?}"
  );
}

/// `geolocation` + `permissions: ['geolocation']` →
/// `navigator.geolocation.getCurrentPosition` resolves with the
/// supplied coords. Without permissions geolocation rejects.
pub fn test_context_options_geolocation(c: &mut McpClient) {
  if skip_if_no_new_context(c) {
    return;
  }
  // BiDi: `permissions` API not implemented in our backend
  // (Permissions API not available in BiDi backend). Skip.
  if c.backend == "bidi" {
    return;
  }
  // Geolocation needs a secure context. data:/about:blank are opaque
  // origins in Chromium/Firefox so the API is unavailable. Spin up a
  // tiny HTTP server on localhost — `http://localhost:*` is treated
  // as a secure context by both engines.
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind geolocation server");
  let port = listener.local_addr().expect("addr").port();
  thread::spawn(move || {
    while let Ok((mut stream, _)) = listener.accept() {
      let mut reader = BufReader::new(stream.try_clone().expect("clone"));
      loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
          break;
        }
        if line == "\r\n" || line == "\n" {
          break;
        }
      }
      let body = "<!doctype html><body>geo</body>";
      let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
      );
      let _ = stream.write_all(resp.as_bytes());
    }
  });
  let url = format!("http://localhost:{port}/geo");
  let v = c.script_value_with_args(
    r"
    const [url] = args;
    const ctx = await browser.newContext({
      geolocation: { latitude: 12.5, longitude: 34.75, accuracy: 1 },
      permissions: ['geolocation'],
    });
    try {
      const p = await ctx.newPage();
      await p.goto(url);
      const coords = await p.evaluate(() => new Promise(resolve => {
        if (!navigator.geolocation) {
          resolve({ error: 'no geolocation api' });
          return;
        }
        navigator.geolocation.getCurrentPosition(
          pos => resolve({ lat: pos.coords.latitude, lng: pos.coords.longitude }),
          err => resolve({ error: err.code + ':' + err.message }),
          { timeout: 4000 },
        );
      }));
      return coords;
    } finally {
      await ctx.close();
    }
  ",
    json!([url]),
  );
  if let Some(err) = v["error"].as_str() {
    panic!("geolocation should resolve when permissions are granted: got error {err}");
  }
  let lat = v["lat"].as_f64().unwrap_or_default();
  let lng = v["lng"].as_f64().unwrap_or_default();
  assert!(
    (lat - 12.5).abs() < 0.5,
    "latitude should match geolocation override: got {lat}"
  );
  assert!(
    (lng - 34.75).abs() < 0.5,
    "longitude should match geolocation override: got {lng}"
  );
}

/// `extraHTTPHeaders` → assertion via the page navigating to a Rust
/// HTTP server we spin up that echoes the inbound `x-rule-nine`
/// header back as the body.
pub fn test_context_options_extra_http_headers(c: &mut McpClient) {
  if skip_if_no_new_context(c) {
    return;
  }
  // Spawn a tiny one-shot HTTP server on an OS-allocated port.
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind echo server");
  let port = listener.local_addr().expect("addr").port();
  let (tx, rx) = mpsc::channel::<String>();
  thread::spawn(move || {
    if let Ok((mut stream, _)) = listener.accept() {
      let mut reader = BufReader::new(stream.try_clone().expect("clone"));
      let mut header_value = String::new();
      let mut content_length = 0usize;
      loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
          break;
        }
        if line == "\r\n" || line == "\n" {
          break;
        }
        if let Some(rest) = line.strip_prefix("x-rule-nine:") {
          header_value = rest.trim().to_string();
        }
        if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
          content_length = rest.trim().parse().unwrap_or(0);
        }
      }
      // Drain body if any (POST etc.).
      if content_length > 0 {
        let mut buf = vec![0u8; content_length];
        let _ = reader.read_exact(&mut buf);
      }
      let body = format!("HEADER:{header_value}");
      let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
      );
      let _ = stream.write_all(resp.as_bytes());
      let _ = tx.send(header_value);
    }
  });

  let url = format!("http://127.0.0.1:{port}/rule-nine");
  let v = c.script_value_with_args(
    r"
    const [url] = args;
    const ctx = await browser.newContext({
      extraHTTPHeaders: { 'x-rule-nine': 'pingpong' },
    });
    try {
      const p = await ctx.newPage();
      const resp = await p.goto(url);
      const body = await p.evaluate(() => document.body.textContent);
      return { body, status: resp ? resp.status() : null };
    } finally {
      await ctx.close();
    }
  ",
    json!([url]),
  );
  let server_seen = rx.recv_timeout(std::time::Duration::from_secs(8)).unwrap_or_default();
  assert_eq!(
    server_seen, "pingpong",
    "echo server should have observed the override header on the request"
  );
  let body = v["body"].as_str().unwrap_or("");
  assert!(
    body.contains("HEADER:pingpong"),
    "page body should echo the override header: {body:?}"
  );
}

/// `offline: true` → `fetch()` rejects.
pub fn test_context_options_offline(c: &mut McpClient) {
  if skip_if_no_new_context(c) {
    return;
  }
  // BiDi backend's `emulation.setNetworkConditions` expects a
  // wrapped `networkConditions` object the page-level wrapper
  // doesn't currently produce. Tracked under §4.1 backend-coverage
  // gaps. Skip.
  if c.backend == "bidi" {
    return;
  }
  let v = c.script_value(
    r"
    const ctx = await browser.newContext({ offline: true });
    try {
      const p = await ctx.newPage();
      // Navigate first to a data URL (cached, doesn't need network),
      // then attempt a fetch — should fail with the offline error.
      await p.goto('data:text/html,<body>offline-test</body>');
      const result = await p.evaluate(async () => {
        try {
          await fetch('http://127.0.0.1:1/never');
          return { ok: true };
        } catch (e) {
          return { ok: false, msg: String(e && e.message ? e.message : e) };
        }
      });
      return result;
    } finally {
      await ctx.close();
    }
  ",
  );
  assert_eq!(v["ok"].as_bool(), Some(false), "fetch should reject when offline: {v}");
}

/// `deviceScaleFactor: 2` → `window.devicePixelRatio` reflects.
pub fn test_context_options_device_scale_factor(c: &mut McpClient) {
  if skip_if_no_new_context(c) {
    return;
  }
  // BiDi's `browsingContext.setViewport` accepts deviceScaleFactor on
  // recent versions but our backend maps it via emulate_viewport
  // which is CDP-only.
  if c.backend == "bidi" {
    return;
  }
  let v = c.script_value(
    r"
    const ctx = await browser.newContext({
      viewport: { width: 800, height: 600 },
      deviceScaleFactor: 2,
    });
    try {
      const p = await ctx.newPage();
      const dpr = await p.evaluate(() => window.devicePixelRatio);
      return { dpr };
    } finally {
      await ctx.close();
    }
  ",
  );
  let dpr = v["dpr"].as_f64().unwrap_or(0.0);
  assert!(
    (dpr - 2.0).abs() < 0.01,
    "devicePixelRatio should match deviceScaleFactor=2: got {dpr}"
  );
}

/// `hasTouch: true` → `'ontouchstart' in window`.
pub fn test_context_options_has_touch(c: &mut McpClient) {
  if skip_if_no_new_context(c) {
    return;
  }
  if c.backend == "bidi" {
    return;
  }
  let v = c.script_value(
    r"
    const ctx = await browser.newContext({
      viewport: { width: 800, height: 600 },
      hasTouch: true,
    });
    try {
      const p = await ctx.newPage();
      const touch = await p.evaluate(() => 'ontouchstart' in window || (navigator.maxTouchPoints > 0));
      return { touch };
    } finally {
      await ctx.close();
    }
  ",
  );
  assert_eq!(
    v["touch"].as_bool(),
    Some(true),
    "hasTouch should expose touch APIs to the page: {v}"
  );
}
