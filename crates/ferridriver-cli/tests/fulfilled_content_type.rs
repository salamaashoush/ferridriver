use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use ferridriver::BrowserType;
use ferridriver::options::LaunchOptions;
use ferridriver::protocol::SerializedArgument;
use ferridriver::route::{FulfillResponse, Route, RouteHandler};
use ferridriver::url_matcher::UrlMatcher;

const HUNG: Duration = Duration::from_secs(120);

const ORIGIN: &str = "http://fulfilled.test";

const DOCUMENT: &str = "<!doctype html><link rel=stylesheet href=/sheet.css><p id=mark>Fulfilled.</p>";

const SHEET: &str = "#mark { color: rgb(1, 2, 3); }";

const READING: &str = "() => [document.contentType, document.getElementById('mark')?.textContent ?? null, document.getElementById('mark') ? getComputedStyle(document.getElementById('mark')).color : null]";

fn serving(document_type: &'static str, sheet_type: &'static str) -> RouteHandler {
  Arc::new(move |route: Route| {
    let sheet = route.request().url.ends_with("/sheet.css");
    route.fulfill(FulfillResponse {
      status: 200,
      headers: Vec::new(),
      body: if sheet { SHEET } else { DOCUMENT }.as_bytes().to_vec(),
      content_type: Some(if sheet { sheet_type } else { document_type }.to_string()),
    });
  })
}

async fn read_as_served(
  browser: BrowserType,
  document_type: &'static str,
  sheet_type: &'static str,
) -> anyhow::Result<()> {
  tokio::time::timeout(HUNG, async {
    let browser = browser
      .launch(LaunchOptions {
        headless: Some(true),
        ..Default::default()
      })
      .await?;
    let context = browser.new_context().await?;
    let page = context.new_page().await?;
    page
      .route(
        UrlMatcher::glob(format!("{ORIGIN}/**"))?,
        serving(document_type, sheet_type),
        None,
      )
      .await?;
    page.goto(&format!("{ORIGIN}/")).await?;
    let read = page
      .evaluate(READING, SerializedArgument::default(), Some(true))
      .await?
      .to_json_like()
      .context("the reading is JSON")?;
    browser.close().await?;
    ensure!(
      read == serde_json::json!(["text/html", "Fulfilled.", "rgb(1, 2, 3)"]),
      "a document fulfilled as {document_type} with a sheet fulfilled as {sheet_type} read {read}"
    );
    Ok(())
  })
  .await
  .context("the fulfilled page hung past the hung-test limit")?
}

async fn a_content_type_with_a_charset_is_parsed_as_its_type(browser: BrowserType) -> anyhow::Result<()> {
  read_as_served(browser, "text/html; charset=utf-8", "text/css; charset=utf-8").await
}

async fn a_bare_content_type_is_parsed_as_itself(browser: BrowserType) -> anyhow::Result<()> {
  read_as_served(browser, "text/html", "text/css").await
}

macro_rules! on_both_engines {
  ($($scenario:ident),* $(,)?) => {
    $(
      mod $scenario {
        #[tokio::test(flavor = "multi_thread")]
        async fn on_cdp_pipe() -> anyhow::Result<()> {
          Box::pin(super::$scenario(ferridriver::chromium())).await
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn on_webkit() -> anyhow::Result<()> {
          Box::pin(super::$scenario(ferridriver::webkit())).await
        }
      }
    )*
  };
}

on_both_engines!(
  a_content_type_with_a_charset_is_parsed_as_its_type,
  a_bare_content_type_is_parsed_as_itself,
);
