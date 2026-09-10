use anyhow::Result;
use ferridriver::backend::NavLifecycle;
use ferridriver::backend::webkit::{LaunchConfig, WebKitBrowser};
use ferridriver::options::{BrowserContextOptions, PageCloseOptions, ViewportConfig};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(tag = "scenario", rename_all = "kebab-case")]
pub enum Request {
  Navigation,
  Locale { url: String },
  Mobile,
  Desktop,
}

async fn observe(browser: &WebKitBrowser, request: Request) -> Result<Value> {
  let context = browser.new_context(None).await?;
  let viewport = match &request {
    Request::Mobile => Some(ViewportConfig {
      width: 390,
      height: 844,
      device_scale_factor: 3.0,
      is_mobile: true,
      has_touch: true,
      is_landscape: false,
    }),
    Request::Desktop => Some(ViewportConfig {
      width: 1280,
      height: 720,
      device_scale_factor: 1.0,
      is_mobile: false,
      has_touch: false,
      is_landscape: true,
    }),
    _ => None,
  };
  let url = if matches!(request, Request::Mobile) {
    "data:text/html,<meta name=\"viewport\" content=\"width=device-width\"><h1>m</h1>"
  } else {
    "data:text/html,<h1>hello</h1>"
  };
  let page = Box::pin(browser.new_page(url, Some(&context), viewport.as_ref())).await?;
  page.goto(url, NavLifecycle::Load, 30_000, None).await?;
  let result = match request {
    Request::Navigation => json!({
      "sum": page.evaluate("1 + 1").await?,
      "html": page.evaluate("document.documentElement.outerHTML").await?,
    }),
    Request::Locale { url } => {
      let before = page.evaluate("navigator.language").await?;
      Box::pin(page.apply_context_options(&BrowserContextOptions {
        locale: Some("de-DE".into()),
        ..Default::default()
      }))
      .await?;
      page.goto(&url, NavLifecycle::Load, 30_000, None).await?;
      let after = page.evaluate("navigator.language").await?;
      let fresh = Box::pin(browser.new_page("data:text/html,<h1>c</h1>", Some(&context), None)).await?;
      json!({ "before": before, "after": after, "fresh": fresh.evaluate("navigator.language").await? })
    },
    Request::Mobile => json!({ "probe": page.evaluate(
      "JSON.stringify({ gesture: typeof GestureEvent, safari: typeof window.safari, pkc: typeof window.PublicKeyCredential, width: innerWidth, dpr: devicePixelRatio, touch: 'ontouchstart' in window })"
    ).await? }),
    Request::Desktop => json!({ "orientation": page.evaluate("'orientation' in window").await? }),
  };
  page.close_page(PageCloseOptions::default()).await?;
  Ok(result)
}

pub async fn run(request: Request) -> Result<Value> {
  let mut browser = WebKitBrowser::launch(&LaunchConfig {
    headless: true,
    ..Default::default()
  })
  .await?;
  let result = observe(&browser, request).await;
  let closed = browser.close().await;
  let result = result?;
  closed?;
  Ok(result)
}
