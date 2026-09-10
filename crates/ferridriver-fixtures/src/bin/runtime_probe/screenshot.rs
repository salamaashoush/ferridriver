use std::path::Path;

use anyhow::Result;
use ferridriver::options::LaunchOptions;
use ferridriver::protocol::SerializedArgument;
use ferridriver_test::ct::server::ComponentServer;
use ferridriver_test::expect::{LocatorSnapshotMatchers, expect};
use serde_json::{Value, json};

pub async fn run(root: &Path, name: &str, expression: Option<&str>) -> Result<Value> {
  let server = ComponentServer::start(root).await?;
  let result = async {
    let browser = ferridriver::chromium()
      .launch(LaunchOptions {
        headless: Some(true),
        ..Default::default()
      })
      .await?;
    let result = async {
      let page = browser.new_page_with_url(&server.url()).await?;
      if let Some(expression) = expression {
        page.evaluate(expression, SerializedArgument::default(), None).await?;
      }
      let result = expect(&page.locator("#box")).to_have_screenshot(name).await;
      Ok::<_, anyhow::Error>(match result {
        Ok(()) => json!({ "matched": true }),
        Err(error) => {
          json!({ "matched": false, "message": error.message, "hasScreenshot": error.screenshot.is_some() })
        },
      })
    }
    .await;
    let closed = browser.close().await;
    let result = result?;
    closed?;
    Ok::<_, anyhow::Error>(result)
  }
  .await;
  server.stop().await;
  result
}
