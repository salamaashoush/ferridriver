//! A raw `WebKit` Inspector Protocol session reaches the page's live main
//! target: its commands are answered there, its layer tree is readable, its
//! events stream in wire order, and a cross-process navigation moves it to
//! the new target.

use std::io::{Read as _, Write as _};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use ferridriver::options::LaunchOptions;
use ferridriver::webkit_session::{WebKitEvents, WebKitSession};
use serde_json::{Value, json};

/// Only stops a hung test; the work takes a fraction of it.
const HUNG: Duration = Duration::from_secs(120);

const PAGE: &str = "<!doctype html><title>Layers</title>\
  <div id=box style=\"will-change: transform; width: 40px; height: 40px; background: red\">box</div>\
  <p id=text>text</p>";

/// Serves [`PAGE`] at every path, a thread per connection and every reply
/// closing it. Returns the port.
fn serve() -> anyhow::Result<u16> {
  let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
  let port = listener.local_addr()?.port();
  std::thread::spawn(move || {
    for stream in listener.incoming().flatten() {
      std::thread::spawn(move || {
        let mut stream = stream;
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request);
        let _ = write!(
          stream,
          "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{PAGE}",
          PAGE.len()
        );
      });
    }
  });
  Ok(port)
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

async fn evaluated(session: &WebKitSession, expression: &str) -> anyhow::Result<Value> {
  let reply = session
    .send(
      "Runtime.evaluate",
      json!({ "expression": expression, "returnByValue": true }),
    )
    .await?;
  reply
    .pointer("/result/value")
    .cloned()
    .with_context(|| format!("Runtime.evaluate answered {reply}"))
}

async fn node(session: &WebKitSession, root: i64, selector: &str) -> anyhow::Result<i64> {
  session
    .send("DOM.querySelector", json!({ "nodeId": root, "selector": selector }))
    .await?
    .get("nodeId")
    .and_then(Value::as_i64)
    .with_context(|| format!("{selector} has no node"))
}

/// The next event called `method`, skipping others.
async fn next_named(events: &mut WebKitEvents, method: &str) -> anyhow::Result<Value> {
  tokio::time::timeout(HUNG, async {
    while let Some(event) = events.next().await {
      if event.method == method {
        return Ok(event.params);
      }
    }
    anyhow::bail!("the target went away before {method}")
  })
  .await
  .with_context(|| format!("no {method} within the hung-test limit"))?
}

#[tokio::test(flavor = "multi_thread")]
async fn a_webkit_session_reads_the_page_s_target_and_its_layer_tree() -> anyhow::Result<()> {
  let port = serve()?;
  let browser = launch(ferridriver::webkit()).await?;
  let context = browser.new_context().await?;
  let page = context.new_page().await?;
  page.goto(&format!("http://127.0.0.1:{port}/")).await?;
  let session = context.new_webkit_session(&page)?;
  ensure!(evaluated(&session, "document.title").await? == json!("Layers"));

  let document = session.send("DOM.getDocument", json!({})).await?;
  let root = document
    .pointer("/root/nodeId")
    .and_then(Value::as_i64)
    .with_context(|| format!("DOM.getDocument answered {document}"))?;
  let boxed = node(&session, root, "#box").await?;
  let text = node(&session, root, "#text").await?;

  session.send("LayerTree.enable", json!({})).await?;
  let layers = session
    .send("LayerTree.layersForNode", json!({ "nodeId": root }))
    .await?;
  let layers = layers
    .get("layers")
    .and_then(Value::as_array)
    .with_context(|| format!("LayerTree.layersForNode answered {layers}"))?;
  let owned_by = |owner: i64| {
    layers
      .iter()
      .filter(|layer| layer.get("nodeId").and_then(Value::as_i64) == Some(owner))
      .count()
  };
  ensure!(owned_by(boxed) == 1, "the composited box has its own layer: {layers:?}");
  ensure!(owned_by(text) == 0, "plain text has none: {layers:?}");
  ensure!(
    layers
      .iter()
      .all(|layer| layer.get("memory").and_then(Value::as_f64).is_some()),
    "every layer reports its memory: {layers:?}"
  );

  let refused = session.send("LayerTree.noSuchMethod", json!({})).await;
  ensure!(refused.is_err(), "an unknown method is refused: {refused:?}");
  browser.close().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_webkit_session_streams_the_target_s_events_in_wire_order() -> anyhow::Result<()> {
  let port = serve()?;
  let browser = launch(ferridriver::webkit()).await?;
  let context = browser.new_context().await?;
  let page = context.new_page().await?;
  page.goto(&format!("http://127.0.0.1:{port}/")).await?;
  let session = context.new_webkit_session(&page)?;
  session.send("Console.enable", json!({})).await?;
  let mut events = session.events();
  evaluated(&session, "console.log('first'), console.log('second'), 1").await?;
  let first = next_named(&mut events, "Console.messageAdded").await?;
  let second = next_named(&mut events, "Console.messageAdded").await?;
  ensure!(
    first.pointer("/message/text") == Some(&json!("first"))
      && second.pointer("/message/text") == Some(&json!("second")),
    "the messages arrive in the order they were logged: {first} then {second}"
  );
  browser.close().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_webkit_session_follows_the_page_across_a_cross_process_navigation() -> anyhow::Result<()> {
  let port = serve()?;
  let browser = launch(ferridriver::webkit()).await?;
  let context = browser.new_context().await?;
  let page = context.new_page().await?;
  page.goto(&format!("http://127.0.0.1:{port}/")).await?;
  let session = context.new_webkit_session(&page)?;
  let before = session.target_id();
  let mut old_events = session.events();
  page.goto(&format!("http://localhost:{port}/")).await?;
  let after = session.target_id();
  ensure!(before != after, "a cross-site navigation replaced the target {before}");
  ensure!(evaluated(&session, "location.hostname").await? == json!("localhost"));
  let ended = tokio::time::timeout(HUNG, async { while old_events.next().await.is_some() {} }).await;
  ensure!(ended.is_ok(), "the old target's events end once it goes away");
  browser.close().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_page_answers_no_webkit_session_command() -> anyhow::Result<()> {
  let port = serve()?;
  let browser = launch(ferridriver::webkit()).await?;
  let context = browser.new_context().await?;
  let page = context.new_page().await?;
  page.goto(&format!("http://127.0.0.1:{port}/")).await?;
  let session = context.new_webkit_session(&page)?;
  let mut events = session.events();
  page.close().await?;
  let refused = tokio::time::timeout(HUNG, evaluated(&session, "1")).await?;
  ensure!(refused.is_err(), "a closed page answers nothing: {refused:?}");
  ensure!(
    tokio::time::timeout(HUNG, events.next()).await?.is_none(),
    "the events of a page that closes end"
  );
  ensure!(
    tokio::time::timeout(HUNG, session.events().next()).await?.is_none(),
    "a closed page's events have already ended"
  );
  browser.close().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_webkit_session_is_opened_only_through_the_page_s_own_context() -> anyhow::Result<()> {
  let browser = launch(ferridriver::webkit()).await?;
  let context = browser.new_context().await?;
  let other = browser.new_context().await?;
  let page = context.new_page().await?;
  let refused = other
    .new_webkit_session(&page)
    .err()
    .context("another context's page is refused")?;
  ensure!(
    refused
      .to_string()
      .contains("the page belongs to another browser context"),
    "{refused}"
  );
  ensure!(context.new_webkit_session(&page).is_ok());
  browser.close().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_webkit_session_is_refused_on_chromium() -> anyhow::Result<()> {
  let browser = launch(ferridriver::chromium()).await?;
  let context = browser.new_context().await?;
  let page = context.new_page().await?;
  let refused = context
    .new_webkit_session(&page)
    .err()
    .context("Chromium has no WebKit session")?;
  ensure!(
    refused
      .to_string()
      .contains("WebKit session is only available on the webkit backend"),
    "{refused}"
  );
  browser.close().await?;
  Ok(())
}
