use ferridriver::backend::webdriver::browser::WebDriverBrowser;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let browser = match std::env::var("FERRIDRIVER_WEBDRIVER_ENDPOINT") {
    Ok(endpoint) => WebDriverBrowser::connect(&endpoint, "safari", None, None, Some(30_000)).await?,
    Err(std::env::VarError::NotPresent) => {
      WebDriverBrowser::launch_safari(&rustc_hash::FxHashMap::default(), Some(30_000)).await?
    },
    Err(error) => return Err(error.into()),
  };
  let result = probe(&browser).await;
  let cleanup = browser.close().await;
  match (result, cleanup) {
    (Ok(report), Ok(())) => println!("{report}"),
    (Err(error), Ok(())) => return Err(error),
    (Ok(_), Err(error)) => return Err(error.into()),
    (Err(error), Err(cleanup)) => return Err(format!("{error}; session cleanup failed: {cleanup}").into()),
  }
  Ok(())
}

async fn probe(browser: &WebDriverBrowser) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
  let page = browser
    .pages()
    .await?
    .into_iter()
    .next()
    .ok_or("Safari did not open a window")?;
  page.set_content(r#"<title>Safari Classic</title><label>Name<input id="name"></label><button onclick="window.trusted=event.isTrusted">Save</button><iframe srcdoc="<button onclick='document.body.dataset.clicked=event.isTrusted'>Frame action</button>"></iframe>"#).await?;
  page.find_element("#name").await?.type_str("sashoush").await?;
  page.find_element("text=Save").await?.click().await?;
  if page.evaluate("document.querySelector('#name').value").await? != Some(json!("sashoush")) {
    return Err("WebDriver typing did not change the input".into());
  }
  if page.evaluate("window.trusted").await? != Some(json!(true)) {
    return Err("WebDriver click was not trusted".into());
  }
  let frame = page.child_frame("iframe").await?;
  frame.find_element("text=Frame action").await?.click().await?;
  if frame.evaluate("document.body.dataset.clicked").await? != Some(json!("true")) {
    return Err("WebDriver did not click inside the frame".into());
  }
  if page.evaluate("document.title").await? != Some(json!("Safari Classic")) {
    return Err("WebDriver failed to restore the parent frame".into());
  }
  let screenshot = page.screenshot_png().await?;
  if !screenshot.starts_with(b"\x89PNG\r\n\x1a\n") {
    return Err("WebDriver screenshot was not PNG".into());
  }
  let tab = browser.new_page("about:blank").await?;
  tab.set_content("<title>Second window</title>").await?;
  if tab.title().await?.as_deref() != Some("Second window") || page.title().await?.as_deref() != Some("Safari Classic")
  {
    return Err("WebDriver mixed window targets".into());
  }
  let enumerated = browser.pages().await?;
  if enumerated.len() != 2 {
    return Err("WebDriver did not enumerate both windows".into());
  }
  tab.close().await?;
  if enumerated.iter().filter(|page| page.is_closed()).count() != 1 || page.is_closed() {
    return Err("WebDriver did not share the closed window state across page handles".into());
  }
  if !tab.title().await.is_err_and(|error| error.is_target_closed_error()) {
    return Err("WebDriver allowed a command on the closed window".into());
  }
  if browser.pages().await?.len() != 1 {
    return Err("WebDriver retained a closed window in page enumeration".into());
  }
  Ok(json!({
    "browserVersion":browser.version(),
    "transport":"webdriver-classic",
    "typedValue":"sashoush",
    "trustedClick":true,
    "frameClick":true,
    "windowSelection":true,
    "sharedWindowLifecycle":true,
    "screenshotBytes":screenshot.len(),
  }))
}
