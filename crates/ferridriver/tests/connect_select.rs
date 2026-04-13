#![allow(clippy::too_many_lines)]
//! Test that connect + page select doesn't hang.
//! Launches its own headless Chrome, connects via `CdpRaw`, and tests page ops.

use ferridriver::backend::cdp::{CdpBrowser, ws::WsTransport};
use ferridriver::state::detect_chromium;
use std::io::BufRead;
use std::time::Instant;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_and_select_pages() {
  let t = Instant::now();

  let chrome_path = detect_chromium();

  // Launch Chrome with port=0 and parse the actual port from stderr
  eprintln!("[test] launching Chrome...");
  let mut child = std::process::Command::new(&chrome_path)
    .args([
      "--headless=new",
      "--remote-debugging-port=0",
      "--no-first-run",
      "--no-default-browser-check",
      "--disable-gpu",
      "--no-sandbox",
      "--temp-profile",
      "about:blank",
    ])
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .expect("failed to launch Chrome");

  // Read stderr to find the WS URL
  let stderr = child.stderr.take().unwrap();
  let reader = std::io::BufReader::new(stderr);
  let mut ws_url = String::new();
  for line in reader.lines() {
    let line = line.unwrap();
    eprintln!("[chrome stderr] {line}");
    if line.contains("DevTools listening on ") {
      ws_url = line.split("DevTools listening on ").nth(1).unwrap().to_string();
      break;
    }
  }
  assert!(!ws_url.is_empty(), "didn't find DevTools WS URL");
  eprintln!("[test] {:?} WS URL: {ws_url}", t.elapsed());

  // --- Connect ---
  eprintln!("[test] {:?} CdpBrowser::<WsTransport>::connect()...", t.elapsed());
  let browser = CdpBrowser::<WsTransport>::connect(&ws_url)
    .await
    .expect("connect failed");
  eprintln!("[test] {:?} connected!", t.elapsed());

  // --- Create extra pages via CDP to simulate multiple tabs ---
  eprintln!("[test] {:?} creating extra pages...", t.elapsed());
  let _ = browser
    .new_page("data:text/html,<title>Page2</title><body>Hello2</body>", None, None)
    .await;
  eprintln!("[test] {:?} created extra page", t.elapsed());

  // --- List pages ---
  eprintln!("[test] {:?} browser.pages()...", t.elapsed());
  let pages = browser.pages().await.expect("pages() failed");
  eprintln!("[test] {:?} got {} pages", t.elapsed(), pages.len());

  // --- Test ops on each page ---
  for (i, page) in pages.iter().enumerate().take(2) {
    eprintln!("\n[test] {:?} === Page {i} ===", t.elapsed());

    let t1 = Instant::now();
    match tokio::time::timeout(std::time::Duration::from_secs(3), page.url()).await {
      Ok(Ok(Some(url))) => eprintln!("[test]   url = {url} ({:?})", t1.elapsed()),
      _ => {},
    }

    let t2 = Instant::now();
    match tokio::time::timeout(std::time::Duration::from_secs(3), page.title()).await {
      Ok(Ok(Some(title))) => eprintln!("[test]   title = {title} ({:?})", t2.elapsed()),
      _ => {},
    }
  }

  // --- Now test: disconnect and reconnect (simulates MCP connect flow) ---
  drop(browser);
  eprintln!("\n[test] {:?} === RECONNECT (simulates MCP connect) ===", t.elapsed());
  let t7 = Instant::now();
  let browser2 = CdpBrowser::<WsTransport>::connect(&ws_url)
    .await
    .expect("reconnect failed");
  eprintln!("[test] {:?} reconnected ({:?})", t.elapsed(), t7.elapsed());

  let t8 = Instant::now();
  let pages2 = browser2.pages().await.expect("pages() failed");
  eprintln!(
    "[test] {:?} got {} pages ({:?})",
    t.elapsed(),
    pages2.len(),
    t8.elapsed()
  );

  eprintln!("\n[test] {:?} done, killing Chrome", t.elapsed());
  let _ = child.kill();
  let _ = child.wait();
}
