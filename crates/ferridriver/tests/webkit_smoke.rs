#![allow(clippy::expect_used, clippy::unwrap_used, clippy::large_futures)]
//! End-to-end smoke test for the Playwright `WebKit` backend.
//!
//! Skipped unless a Playwright `WebKit` binary is reachable via
//! `FERRIDRIVER_WEBKIT` or the standard Playwright cache. Exercises
//! launch -> context -> page -> navigate -> evaluate -> close through
//! the high-level `WebKitBrowser` / `AnyPage` surface.

use ferridriver::backend::NavLifecycle;
use ferridriver::backend::webkit::{LaunchConfig, WebKitBrowser, locate_binary};

fn binary_available() -> bool {
  locate_binary().is_ok()
}

/// Minimal thread-per-connection HTTP server serving a flat page for
/// every path. `Connection: close` so `WebKit`'s speculative
/// preconnections never starve the accept loop.
fn spawn_html_server() -> u16 {
  use std::io::{BufRead as _, BufReader, Write as _};
  let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind html server");
  let port = listener.local_addr().expect("addr").port();
  std::thread::spawn(move || {
    while let Ok((mut stream, _)) = listener.accept() {
      std::thread::spawn(move || {
        let mut reader = BufReader::new(match stream.try_clone() {
          Ok(s) => s,
          Err(_) => return,
        });
        loop {
          let mut line = String::new();
          if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
          }
          if line == "\r\n" || line == "\n" {
            break;
          }
        }
        let body = "<!doctype html><body>probe</body>";
        let resp = format!(
          "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
          body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
      });
    }
  });
  port
}

#[tokio::test]
async fn webkit_launch_navigate_evaluate() {
  let _ = tracing_subscriber::fmt()
    .with_env_filter(
      tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "ferridriver=info".into()),
    )
    .with_writer(std::io::stderr)
    .try_init();

  if !binary_available() {
    eprintln!("skipping: no Playwright WebKit binary found (set FERRIDRIVER_WEBKIT)");
    return;
  }

  let config = LaunchConfig {
    headless: true,
    ..LaunchConfig::default()
  };
  let mut browser = match WebKitBrowser::launch(&config).await {
    Ok(b) => b,
    Err(e) => {
      eprintln!("skipping: launch failed ({e}); is libicu74 missing? install AUR `icu74` on Arch");
      return;
    },
  };
  eprintln!("PW WebKit version: {}", browser.version());

  let context_id = browser.new_context(None).await.expect("new_context");
  let page = browser
    .new_page("data:text/html,<h1>hello</h1>", Some(&context_id), None)
    .await
    .expect("new_page");

  let _ = page
    .goto("data:text/html,<h1>hello</h1>", NavLifecycle::Load, 30_000, None)
    .await;

  let value = page.evaluate("1 + 1").await.expect("evaluate");
  assert_eq!(value, Some(serde_json::json!(2)), "evaluate returned: {value:?}");

  let html = page
    .evaluate("document.documentElement.outerHTML")
    .await
    .expect("evaluate outerHTML");
  assert!(
    html
      .as_ref()
      .and_then(|v| v.as_str())
      .is_some_and(|s| s.contains("hello")),
    "outerHTML should include the navigated content: {html:?}"
  );

  page
    .close_page(ferridriver::options::PageCloseOptions::default())
    .await
    .expect("close page");
  browser.close().await.expect("close browser");
}

#[tokio::test]
async fn webkit_dynamic_locale_applies_to_live_page() {
  if !binary_available() {
    eprintln!("skipping: no Playwright WebKit binary found (set FERRIDRIVER_WEBKIT)");
    return;
  }
  let config = LaunchConfig {
    headless: true,
    ..LaunchConfig::default()
  };
  let mut browser = match WebKitBrowser::launch(&config).await {
    Ok(b) => b,
    Err(e) => {
      eprintln!("skipping: launch failed ({e})");
      return;
    },
  };

  // Wire semantics under test (all verified against the Playwright
  // WebKit build): `Playwright.setLanguages` after context creation
  // reaches (a) the live page's next cross-site navigation (the swap
  // lands in a freshly spawned web process) and (b) any page created
  // afterwards. It can NOT reach an existing web process in place —
  // languages are latched at process spawn — which is why creation-time
  // options (the worker/`@use` path) are the canonical way to set
  // locale.
  let context_id = browser.new_context(None).await.expect("new_context");
  let page = browser
    .new_page("data:text/html,<h1>a</h1>", Some(&context_id), None)
    .await
    .expect("new_page");
  let before = page.evaluate("navigator.language").await.expect("evaluate before");
  assert_eq!(before, Some(serde_json::json!("en-US")), "default locale: {before:?}");

  let opts = ferridriver::options::BrowserContextOptions {
    locale: Some("de-DE".to_string()),
    ..Default::default()
  };
  page.apply_context_options(&opts).await.expect("apply locale");

  let port = spawn_html_server();
  let _ = page
    .goto(&format!("http://127.0.0.1:{port}/"), NavLifecycle::Load, 30_000, None)
    .await;
  let after_nav = page
    .evaluate("navigator.language")
    .await
    .expect("evaluate after http nav");
  assert_eq!(
    after_nav,
    Some(serde_json::json!("de-DE")),
    "locale set after context creation must be live on the page's next cross-site navigation"
  );

  let page2 = browser
    .new_page("data:text/html,<h1>c</h1>", Some(&context_id), None)
    .await
    .expect("new_page 2");
  let fresh = page2.evaluate("navigator.language").await.expect("evaluate fresh page");
  assert_eq!(
    fresh,
    Some(serde_json::json!("de-DE")),
    "pages created after the locale change must observe it"
  );

  browser.close().await.expect("close browser");
}

#[tokio::test]
async fn webkit_mobile_emulation_matches_playwrights_surface() {
  // Without a viewport meta a mobile page lays out at WebKit's 980px desktop
  // fallback -- on a real iPhone too -- so the fixture opts in like a real app.
  const MOBILE_PAGE: &str = "data:text/html,<meta name=\"viewport\" content=\"width=device-width\"><h1>m</h1>";

  if !binary_available() {
    eprintln!("skipping: no Playwright WebKit binary found (set FERRIDRIVER_WEBKIT)");
    return;
  }
  let config = LaunchConfig {
    headless: true,
    ..LaunchConfig::default()
  };
  let mut browser = match WebKitBrowser::launch(&config).await {
    Ok(b) => b,
    Err(e) => {
      eprintln!("skipping: launch failed ({e})");
      return;
    },
  };

  // The Playwright WebKit build ships without `window.safari`,
  // `window.GestureEvent` and `PublicKeyCredential`, and lays a mobile
  // viewport out at the 980px desktop fallback unless `fixedLayout` is set.
  // Playwright papers over both; a page driven through ferridriver has to see
  // the same surface or Safari feature detection takes the wrong branch.
  let viewport = ferridriver::options::ViewportConfig {
    width: 390,
    height: 844,
    device_scale_factor: 3.0,
    is_mobile: true,
    has_touch: true,
    is_landscape: false,
  };

  let context_id = browser.new_context(None).await.expect("new_context");
  let page = browser
    .new_page(MOBILE_PAGE, Some(&context_id), Some(&viewport))
    .await
    .expect("new_page");

  let _ = page.goto(MOBILE_PAGE, NavLifecycle::Load, 30_000, None).await;

  let probe = page
    .evaluate(
      "JSON.stringify({ gesture: typeof GestureEvent, safari: typeof window.safari, \
       pkc: typeof window.PublicKeyCredential, width: innerWidth, dpr: devicePixelRatio, \
       touch: 'ontouchstart' in window })",
    )
    .await
    .expect("evaluate probe");
  let probe: serde_json::Value = serde_json::from_str(
    probe
      .as_ref()
      .and_then(|v| v.as_str())
      .expect("probe returns a JSON string"),
  )
  .expect("probe parses");

  assert_eq!(probe["gesture"], "function", "GestureEvent shim missing: {probe}");
  assert_eq!(probe["safari"], "object", "window.safari shim missing: {probe}");
  assert_eq!(probe["pkc"], "object", "PublicKeyCredential shim missing: {probe}");
  assert_eq!(
    probe["width"], 390,
    "mobile viewport did not drive layout width: {probe}"
  );
  assert_eq!(probe["dpr"], 3.0, "device scale factor not applied: {probe}");
  assert_eq!(probe["touch"], true, "touch events not enabled: {probe}");

  page
    .close_page(ferridriver::options::PageCloseOptions::default())
    .await
    .expect("close page");
  browser.close().await.expect("close browser");
}

#[tokio::test]
async fn webkit_desktop_page_has_no_orientation_surface() {
  if !binary_available() {
    eprintln!("skipping: no Playwright WebKit binary found (set FERRIDRIVER_WEBKIT)");
    return;
  }
  let config = LaunchConfig {
    headless: true,
    ..LaunchConfig::default()
  };
  let mut browser = match WebKitBrowser::launch(&config).await {
    Ok(b) => b,
    Err(e) => {
      eprintln!("skipping: launch failed ({e})");
      return;
    },
  };

  let viewport = ferridriver::options::ViewportConfig {
    width: 1280,
    height: 720,
    device_scale_factor: 1.0,
    is_mobile: false,
    has_touch: false,
    is_landscape: true,
  };

  let context_id = browser.new_context(None).await.expect("new_context");
  let page = browser
    .new_page("data:text/html,<h1>d</h1>", Some(&context_id), Some(&viewport))
    .await
    .expect("new_page");

  let _ = page
    .goto("data:text/html,<h1>d</h1>", NavLifecycle::Load, 30_000, None)
    .await;

  // `window.orientation` on a desktop page tells a sniffing script it is on a
  // phone; Playwright deletes it for non-mobile contexts.
  let orientation = page
    .evaluate("'orientation' in window")
    .await
    .expect("evaluate orientation");
  assert_eq!(
    orientation,
    Some(serde_json::json!(false)),
    "orientation surface left on a desktop page"
  );

  page
    .close_page(ferridriver::options::PageCloseOptions::default())
    .await
    .expect("close page");
  browser.close().await.expect("close browser");
}

/// A proxy that answers nothing and records the first request line it is sent.
///
/// Enough to prove the browser routed through it: a browser ignoring the proxy
/// resolves and connects to the origin directly, and this listener never sees a
/// byte.
fn spawn_recording_proxy() -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
  use std::io::{BufRead as _, BufReader};

  let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind proxy");
  let port = listener.local_addr().expect("addr").port();
  let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
  let recorder = std::sync::Arc::clone(&seen);

  std::thread::spawn(move || {
    while let Ok((stream, _)) = listener.accept() {
      let recorder = std::sync::Arc::clone(&recorder);
      std::thread::spawn(move || {
        let mut line = String::new();
        if BufReader::new(stream).read_line(&mut line).is_ok()
          && let Ok(mut seen) = recorder.lock()
        {
          seen.push(line.trim().to_string());
        }
      });
    }
  });

  (port, seen)
}

#[tokio::test]
async fn webkit_launch_proxy_routes_traffic_like_playwright() {
  if !binary_available() {
    eprintln!("skipping: no Playwright WebKit binary found (set FERRIDRIVER_WEBKIT)");
    return;
  }

  let (proxy_port, seen) = spawn_recording_proxy();

  // The public API, the way a caller writes it in Playwright:
  // `webkit.launch({ proxy: { server, bypass } })`.
  let browser = match ferridriver::browser_type::webkit()
    .launch(ferridriver::options::LaunchOptions {
      headless: Some(true),
      proxy: Some(ferridriver::options::ProxyConfig {
        server: format!("http://127.0.0.1:{proxy_port}"),
        bypass: Some("127.0.0.1,localhost".to_string()),
        username: None,
        password: None,
      }),
      ..Default::default()
    })
    .await
  {
    Ok(b) => b,
    Err(e) => {
      eprintln!("skipping: launch failed ({e})");
      return;
    },
  };

  let page = browser.new_page().await.expect("new_page");
  // Nothing resolves this name, so a reply can only come from the proxy —
  // and the assertion is about what the proxy was asked, not the outcome.
  let _ = page.goto("https://proxy-probe.invalid/").timeout(5_000).await;

  let requests = seen.lock().expect("recorded requests").clone();
  browser.close().await.expect("close browser");

  assert!(
    requests.iter().any(|line| line.contains("proxy-probe.invalid")),
    "launch({{ proxy }}) did not route the page's request through the proxy; saw: {requests:?}"
  );
}
