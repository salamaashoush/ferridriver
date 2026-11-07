//! The core's waits listen from the call. Each wait is created, the event
//! it waits for is caused and over, and only then is the wait awaited: a
//! wait that listened from its first poll would miss the event and time
//! out. The script and Node bindings arm these same waits in the
//! synchronous part of their calls, so this is the behaviour both inherit.

use std::io::{Read as _, Write as _};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use ferridriver::Page;
use ferridriver::events::{ContextEvent, PageEvent};
use ferridriver::options::LaunchOptions;
use ferridriver::url_matcher::UrlMatcher;

const WAIT: u64 = 3000;

/// Serves a page, `/echo` and a `/download` attachment, a thread per
/// connection and every reply closing it.
fn serve() -> anyhow::Result<String> {
  let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
  let origin = format!("http://{}", listener.local_addr()?);
  std::thread::spawn(move || {
    for stream in listener.incoming().flatten() {
      std::thread::spawn(move || {
        let mut stream = stream;
        let mut request = [0_u8; 4096];
        let read = stream.read(&mut request).unwrap_or(0);
        let head = String::from_utf8_lossy(&request[..read]);
        let path = head.split_whitespace().nth(1).unwrap_or("/");
        let (kind, extra, body) = match path.split('?').next().unwrap_or("/") {
          "/echo" => ("text/plain", "", "echo"),
          "/download" => (
            "text/plain",
            "Content-Disposition: attachment; filename=\"greeting.txt\"\r\n",
            "hello",
          ),
          _ => (
            "text/html",
            "",
            "<!doctype html><title>Waits</title><a id=dl href=/download>dl</a>",
          ),
        };
        let _ = write!(
          stream,
          "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
          body.len()
        );
      });
    }
  });
  Ok(origin)
}

async fn launch(browser: ferridriver::BrowserType) -> anyhow::Result<ferridriver::Browser> {
  Ok(
    browser
      .launch(LaunchOptions {
        headless: Some(true),
        ..Default::default()
      })
      .await?,
  )
}

/// Runs `script` in the page and waits for what it returns.
async fn run(page: &Arc<Page>, script: &str) -> anyhow::Result<()> {
  page.inner().evaluate(script).await?;
  Ok(())
}

/// Records `surface` as having missed its event when `outcome` failed.
fn check(missed: &mut Vec<String>, surface: &str, outcome: anyhow::Result<()>) {
  if let Err(error) = outcome {
    missed.push(format!("{surface}: {error:#}"));
  }
}

async fn waits_listen_from_the_call(browser: ferridriver::Browser) -> anyhow::Result<()> {
  let origin = serve()?;
  let page = browser.new_page().await?;
  page.goto(&format!("{origin}/")).await?;
  let mut missed = Box::pin(event_waits(&page, &origin)).await?;
  missed.extend(Box::pin(claims_and_contexts(&browser, &page)).await?);
  browser.close().await?;
  ensure!(
    missed.is_empty(),
    "waits that missed their event:\n{}",
    missed.join("\n")
  );
  Ok(())
}

/// The page's event, network and navigation waits.
async fn event_waits(page: &Arc<Page>, origin: &str) -> anyhow::Result<Vec<String>> {
  let mut missed = Vec::new();

  let console = page.wait_for_event("console", Some(WAIT));
  run(page, "console.log('logged')").await?;
  check(
    &mut missed,
    "Page::wait_for_event",
    async {
      ensure!(matches!(console.await?, PageEvent::Console(message) if message.text() == "logged"));
      Ok(())
    }
    .await,
  );

  let request = page.wait_for_request(UrlMatcher::glob("**/echo?request")?, Some(WAIT));
  let response = page.wait_for_response(UrlMatcher::glob("**/echo?response")?, Some(WAIT));
  run(
    page,
    "Promise.all([fetch('/echo?request'), fetch('/echo?response')]).then(() => null)",
  )
  .await?;
  check(
    &mut missed,
    "Page::wait_for_request",
    async {
      ensure!(request.await?.url().ends_with("?request"));
      Ok(())
    }
    .await,
  );
  check(
    &mut missed,
    "Page::wait_for_response",
    async {
      ensure!(response.await?.status() == 200);
      Ok(())
    }
    .await,
  );

  let loaded = page.expect_navigation(Some(WAIT));
  let navigation = page
    .wait_for_navigation()
    .options(ferridriver::options::WaitForNavigationOptions {
      timeout: Some(WAIT),
      ..Default::default()
    });
  page.goto(&format!("{origin}/?next")).await?;
  check(&mut missed, "Page::expect_navigation", loaded.await.map_err(Into::into));
  check(
    &mut missed,
    "Frame::wait_for_navigation",
    async {
      ensure!(navigation.await?.is_some());
      Ok(())
    }
    .await,
  );
  Ok(missed)
}

/// The waits that claim a dialog or a download, and the context and
/// browser waits.
async fn claims_and_contexts(browser: &ferridriver::Browser, page: &Arc<Page>) -> anyhow::Result<Vec<String>> {
  let mut missed = Vec::new();
  let dialog = page.wait_for_dialog(WAIT);
  run(page, "setTimeout(() => alert('later'), 0)").await?;
  tokio::time::sleep(Duration::from_millis(500)).await;
  check(
    &mut missed,
    "Page::wait_for_dialog",
    async {
      let dialog = dialog.await?;
      ensure!(dialog.message() == "later");
      dialog.dismiss().await?;
      Ok(())
    }
    .await,
  );

  // A first download turns on the browser's download reporting, which
  // takes a round trip; the second is about the claim alone.
  let (first, clicked) = tokio::join!(page.wait_for_download(WAIT), async {
    tokio::time::sleep(Duration::from_millis(200)).await;
    run(page, "document.getElementById('dl').click()").await
  });
  clicked?;
  first?;
  let download = page.wait_for_download(WAIT);
  run(page, "document.getElementById('dl').click()").await?;
  tokio::time::sleep(Duration::from_millis(1000)).await;
  check(
    &mut missed,
    "Page::wait_for_download",
    async {
      ensure!(download.await?.url().ends_with("/download"));
      Ok(())
    }
    .await,
  );

  let context = page.context().context("a page with a context")?.clone();
  let opened = context.wait_for_event("page", WAIT);
  run(page, "void window.open('/?popup')").await?;
  tokio::time::sleep(Duration::from_millis(500)).await;
  check(
    &mut missed,
    "ContextRef::wait_for_event",
    async {
      ensure!(matches!(opened.await?, ContextEvent::Page(_)));
      Ok(())
    }
    .await,
  );

  let created = browser.wait_for_event("context", WAIT);
  let other = browser.new_context().await?;
  check(
    &mut missed,
    "Browser::wait_for_event",
    created.await.map(|_| ()).map_err(Into::into),
  );
  other.close().await?;
  Ok(missed)
}

#[tokio::test(flavor = "multi_thread")]
async fn waits_listen_from_the_call_on_cdp_pipe() -> anyhow::Result<()> {
  Box::pin(waits_listen_from_the_call(launch(ferridriver::chromium()).await?)).await
}

#[tokio::test(flavor = "multi_thread")]
async fn waits_listen_from_the_call_on_cdp_raw() -> anyhow::Result<()> {
  let raw = ferridriver::BrowserType::chromium_with(&ferridriver::options::BrowserTypeOptions {
    transport: Some(ferridriver::options::ChromiumTransport::Ws),
  });
  Box::pin(waits_listen_from_the_call(launch(raw).await?)).await
}

#[tokio::test(flavor = "multi_thread")]
async fn waits_listen_from_the_call_on_webkit() -> anyhow::Result<()> {
  Box::pin(waits_listen_from_the_call(launch(ferridriver::webkit()).await?)).await
}

#[tokio::test(flavor = "multi_thread")]
async fn waits_listen_from_the_call_on_bidi() -> anyhow::Result<()> {
  Box::pin(waits_listen_from_the_call(launch(ferridriver::firefox()).await?)).await
}
