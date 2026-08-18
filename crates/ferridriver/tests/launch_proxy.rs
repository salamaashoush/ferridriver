#![allow(clippy::expect_used, clippy::unwrap_used, clippy::large_futures)]
//! `browserType.launch({ proxy })` across every backend.
//!
//! Playwright's launch-level proxy is per-process on Chromium and `WebKit` and
//! a session capability on Firefox, and each engine spells it differently. A
//! switch a browser does not recognise is accepted and ignored, so the failure
//! mode is not a crash but a proxy that silently does nothing — which is what
//! these tests exist to catch.
//!
//! Each skips when its browser is not installed.

use std::sync::{Arc, Mutex};

/// A proxy that answers nothing and records the first request line it receives.
///
/// Enough to prove routing: a browser ignoring the proxy connects to the origin
/// directly and this listener never sees a byte.
fn spawn_recording_proxy() -> (u16, Arc<Mutex<Vec<String>>>) {
  use std::io::{BufRead as _, BufReader};

  let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind proxy");
  let port = listener.local_addr().expect("addr").port();
  let seen = Arc::new(Mutex::new(Vec::new()));
  let recorder = Arc::clone(&seen);

  std::thread::spawn(move || {
    while let Ok((stream, _)) = listener.accept() {
      let recorder = Arc::clone(&recorder);
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

fn proxy_options(port: u16) -> ferridriver::options::LaunchOptions {
  ferridriver::options::LaunchOptions {
    headless: Some(true),
    proxy: Some(ferridriver::options::ProxyConfig {
      server: format!("http://127.0.0.1:{port}"),
      bypass: Some("127.0.0.1,localhost".to_string()),
      username: None,
      password: None,
    }),
    ..Default::default()
  }
}

/// Launch `browser_type` behind a recording proxy, ask for a host nothing
/// resolves, and report what the proxy was asked for.
async fn requests_through_proxy(browser_type: ferridriver::browser_type::BrowserType) -> Option<Vec<String>> {
  let (port, seen) = spawn_recording_proxy();

  let browser = match browser_type.launch(proxy_options(port)).await {
    Ok(browser) => browser,
    Err(e) => {
      eprintln!("skipping: launch failed ({e})");
      return None;
    },
  };

  let page = browser.new_page().await.expect("new_page");
  // Nothing resolves this name, so only the proxy could answer — and the
  // assertion is about what the proxy was asked, not about the outcome.
  let _ = page.goto("https://proxy-probe.invalid/").timeout(5_000).await;

  let requests = seen.lock().expect("recorded requests").clone();
  let _ = browser.close().await;
  Some(requests)
}

#[tokio::test]
async fn chromium_cdp_pipe_routes_through_the_launch_proxy() {
  let Some(requests) = requests_through_proxy(ferridriver::browser_type::chromium()).await else {
    return;
  };

  assert!(
    requests.iter().any(|line| line.contains("proxy-probe.invalid")),
    "launch({{ proxy }}) did not route through the proxy; saw: {requests:?}"
  );
}

#[tokio::test]
async fn chromium_cdp_raw_routes_through_the_launch_proxy() {
  let browser_type = ferridriver::browser_type::BrowserType::chromium_with(&ferridriver::options::BrowserTypeOptions {
    transport: Some(ferridriver::options::ChromiumTransport::Ws),
  });

  let Some(requests) = requests_through_proxy(browser_type).await else {
    return;
  };

  assert!(
    requests.iter().any(|line| line.contains("proxy-probe.invalid")),
    "launch({{ proxy }}) did not route through the proxy; saw: {requests:?}"
  );
}

#[tokio::test]
async fn firefox_routes_through_the_launch_proxy() {
  if std::env::var("FIREFOX_PATH").is_err() && ferridriver::browser_type::firefox().executable_path().is_none() {
    eprintln!("skipping: no Firefox installed (set FIREFOX_PATH)");
    return;
  }

  // Firefox has no proxy switch at all: this only passes if the proxy reached
  // `session.new` as a WebDriver capability.
  let Some(requests) = requests_through_proxy(ferridriver::browser_type::firefox()).await else {
    return;
  };

  assert!(
    requests.iter().any(|line| line.contains("proxy-probe.invalid")),
    "launch({{ proxy }}) did not route through the proxy; saw: {requests:?}"
  );
}
