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
  let _ = browser
    .new_page("data:text/html,<title>Page3</title><body>Hello3</body>", None, None)
    .await;
  eprintln!("[test] {:?} created extra pages", t.elapsed());

  // --- List pages ---
  eprintln!("[test] {:?} browser.pages()...", t.elapsed());
  let pages = browser.pages().await.expect("pages() failed");
  eprintln!("[test] {:?} got {} pages", t.elapsed(), pages.len());

  // --- Test ops on each page ---
  for (i, page) in pages.iter().enumerate() {
    eprintln!("\n[test] {:?} === Page {i} ===", t.elapsed());

    let t1 = Instant::now();
    eprintln!("[test]   url()...");
    match tokio::time::timeout(std::time::Duration::from_secs(3), page.url()).await {
      Ok(Ok(Some(url))) => eprintln!("[test]   url = {url} ({:?})", t1.elapsed()),
      Ok(Ok(None)) => eprintln!("[test]   url = None ({:?})", t1.elapsed()),
      Ok(Err(e)) => eprintln!("[test]   url ERROR: {e} ({:?})", t1.elapsed()),
      Err(_) => eprintln!("[test]   url TIMED OUT ({:?})", t1.elapsed()),
    }

    let t2 = Instant::now();
    eprintln!("[test]   title()...");
    match tokio::time::timeout(std::time::Duration::from_secs(3), page.title()).await {
      Ok(Ok(Some(title))) => eprintln!("[test]   title = {title} ({:?})", t2.elapsed()),
      Ok(Ok(None)) => eprintln!("[test]   title = None ({:?})", t2.elapsed()),
      Ok(Err(e)) => eprintln!("[test]   title ERROR: {e} ({:?})", t2.elapsed()),
      Err(_) => eprintln!("[test]   title TIMED OUT ({:?})", t2.elapsed()),
    }

    let t3 = Instant::now();
    eprintln!("[test]   accessibility_tree_with_depth(-1)...");
    match tokio::time::timeout(
      std::time::Duration::from_secs(5),
      page.accessibility_tree_with_depth(-1),
    )
    .await
    {
      Ok(Ok(tree)) => eprintln!("[test]   a11y = {} nodes ({:?})", tree.len(), t3.elapsed()),
      Ok(Err(e)) => eprintln!("[test]   a11y ERROR: {e} ({:?})", t3.elapsed()),
      Err(_) => eprintln!("[test]   a11y TIMED OUT ({:?})", t3.elapsed()),
    }
  }

  // --- Now test: disconnect and reconnect (simulates MCP connect flow) ---
  // Create a chrome:// page to simulate what a real user has
  eprintln!("\n[test] {:?} creating chrome:// page...", t.elapsed());
  let t6 = Instant::now();
  let chrome_page = browser.new_page("chrome://version", None, None).await;
  eprintln!(
    "[test] {:?} chrome:// page created: {:?} ({:?})",
    t.elapsed(),
    chrome_page.is_ok(),
    t6.elapsed()
  );

  // Now simulate what state.rs connect_to_url does:
  // Drop the browser and reconnect fresh, which will try to attach to ALL pages
  // including the chrome:// one.
  drop(browser);
  eprintln!("\n[test] {:?} === RECONNECT (simulates MCP connect) ===", t.elapsed());
  let t7 = Instant::now();
  let browser2 = CdpBrowser::<WsTransport>::connect(&ws_url)
    .await
    .expect("reconnect failed");
  eprintln!("[test] {:?} reconnected ({:?})", t.elapsed(), t7.elapsed());

  let t8 = Instant::now();
  eprintln!("[test] {:?} browser.pages()...", t.elapsed());
  let pages2 = browser2.pages().await.expect("pages() failed");
  eprintln!(
    "[test] {:?} got {} pages ({:?})",
    t.elapsed(),
    pages2.len(),
    t8.elapsed()
  );

  for (i, page) in pages2.iter().enumerate() {
    eprintln!("\n[test] {:?} === Reconnected Page {i} ===", t.elapsed());
    let t9 = Instant::now();
    match tokio::time::timeout(std::time::Duration::from_secs(3), page.url()).await {
      Ok(Ok(Some(url))) => eprintln!("[test]   url = {url} ({:?})", t9.elapsed()),
      Ok(Ok(None)) => eprintln!("[test]   url = None ({:?})", t9.elapsed()),
      Ok(Err(e)) => eprintln!("[test]   url ERROR: {e} ({:?})", t9.elapsed()),
      Err(_) => eprintln!("[test]   url TIMED OUT ({:?})", t9.elapsed()),
    }

    let t10 = Instant::now();
    match tokio::time::timeout(
      std::time::Duration::from_secs(5),
      page.accessibility_tree_with_depth(-1),
    )
    .await
    {
      Ok(Ok(tree)) => eprintln!("[test]   a11y = {} nodes ({:?})", tree.len(), t10.elapsed()),
      Ok(Err(e)) => eprintln!("[test]   a11y ERROR: {e} ({:?})", t10.elapsed()),
      Err(_) => eprintln!("[test]   a11y TIMED OUT ({:?})", t10.elapsed()),
    }
  }

  eprintln!("\n[test] {:?} done, killing Chrome", t.elapsed());
  let _ = child.kill();
  let _ = child.wait();
}

/// Connect to the user's running Chrome (not headless) and test page ops.
/// Run: `cargo test --test connect_select connect_real_chrome -- --nocapture --ignored`
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires user's running Chrome instance"]
async fn connect_real_chrome() {
  use ferridriver::backend::BackendKind;
  use ferridriver::state::{BrowserState, ConnectMode};

  let t = Instant::now();

  let mut state = BrowserState::new(ConnectMode::Launch, BackendKind::CdpRaw);
  eprintln!("[test] {:?} connect_auto...", t.elapsed());
  let page_count = match state.connect_auto("default", "stable", None).await {
    Ok(n) => n,
    Err(e) => {
      eprintln!("Skipping — no running Chrome: {e}");
      return;
    },
  };
  eprintln!("[test] {:?} connected, {page_count} pages", t.elapsed());

  let contexts = state.list_contexts().await;
  for ctx in &contexts {
    eprintln!("  Session '{}': {} pages", ctx.name, ctx.pages.len());
    for (i, pg) in ctx.pages.iter().enumerate() {
      eprintln!("    Page {i}: {} - {}", pg.url, pg.title);
    }
  }

  let total_pages = contexts.first().map_or(0, |c| c.pages.len());
  for idx in 0..total_pages {
    eprintln!("\n[test] {:?} --- select page {idx} ---", t.elapsed());
    if let Err(e) = state.select_page("default", idx) {
      eprintln!("  select_page error: {e}");
      continue;
    }

    let page = match state.active_page("default") {
      Ok(p) => p.clone(),
      Err(e) => {
        eprintln!("  active_page error: {e}");
        continue;
      },
    };

    let t1 = Instant::now();
    eprintln!("[test]   url()...");
    match tokio::time::timeout(std::time::Duration::from_secs(3), page.url()).await {
      Ok(Ok(Some(url))) => eprintln!("[test]   url = {url} ({:?})", t1.elapsed()),
      Ok(Ok(None)) => eprintln!("[test]   url = None ({:?})", t1.elapsed()),
      Ok(Err(e)) => eprintln!("[test]   url ERROR: {e} ({:?})", t1.elapsed()),
      Err(_) => eprintln!("[test]   url TIMED OUT ({:?})", t1.elapsed()),
    }

    let t2 = Instant::now();
    eprintln!("[test]   title()...");
    match tokio::time::timeout(std::time::Duration::from_secs(3), page.title()).await {
      Ok(Ok(Some(title))) => eprintln!("[test]   title = {title} ({:?})", t2.elapsed()),
      Ok(Ok(None)) => eprintln!("[test]   title = None ({:?})", t2.elapsed()),
      Ok(Err(e)) => eprintln!("[test]   title ERROR: {e} ({:?})", t2.elapsed()),
      Err(_) => eprintln!("[test]   title TIMED OUT ({:?})", t2.elapsed()),
    }

    let t3 = Instant::now();
    eprintln!("[test]   accessibility_tree(-1)...");
    match tokio::time::timeout(
      std::time::Duration::from_secs(5),
      page.accessibility_tree_with_depth(-1),
    )
    .await
    {
      Ok(Ok(tree)) => eprintln!("[test]   a11y = {} nodes ({:?})", tree.len(), t3.elapsed()),
      Ok(Err(e)) => eprintln!("[test]   a11y ERROR: {e} ({:?})", t3.elapsed()),
      Err(_) => eprintln!("[test]   a11y TIMED OUT ({:?})", t3.elapsed()),
    }
  }

  eprintln!("\n[test] {:?} done", t.elapsed());
}
