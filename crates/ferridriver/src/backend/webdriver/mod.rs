mod android;
mod api;
pub mod browser;
mod capabilities;
mod capture;
mod context;
pub mod element;
mod evaluation;
pub(crate) mod launcher;
mod negotiation;
#[cfg(test)]
mod negotiation_tests;
pub mod page;
pub mod session;

use crate::error::{FerriError, Result};

pub(crate) use android::connect_android_chrome;
pub use negotiation::WebDriverProtocol;
pub(crate) use negotiation::connect;

pub(crate) fn uses_android_chrome(capabilities: Option<&serde_json::Value>) -> bool {
  let Some(caps) = capabilities else {
    return false;
  };
  if ["appium:app", "appium:appPackage", "appium:bundleId"]
    .iter()
    .any(|key| caps.get(key).is_some())
    || ["app", "appPackage", "bundleId"].iter().any(|key| {
      caps
        .get("appium:options")
        .and_then(|options| options.get(key))
        .is_some()
    })
  {
    return false;
  }
  caps
    .get("platformName")
    .and_then(serde_json::Value::as_str)
    .is_some_and(|v| v.eq_ignore_ascii_case("android"))
    && caps
      .pointer("/appium:options/automationName")
      .or_else(|| caps.get("appium:automationName"))
      .and_then(serde_json::Value::as_str)
      .is_some_and(|v| v.eq_ignore_ascii_case("uiautomator2"))
    && caps
      .get("browserName")
      .and_then(serde_json::Value::as_str)
      .is_none_or(|v| v.eq_ignore_ascii_case("chrome"))
}

pub(crate) fn http_client(headers: Option<&rustc_hash::FxHashMap<String, String>>) -> Result<reqwest::Client> {
  let mut client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
  if let Some(headers) = headers {
    let mut request_headers = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
      let name = reqwest::header::HeaderName::try_from(name)
        .map_err(|e| FerriError::invalid_argument("headers", format!("invalid header name '{name}': {e}")))?;
      let value = reqwest::header::HeaderValue::try_from(value)
        .map_err(|e| FerriError::invalid_argument("headers", format!("invalid value for '{name}': {e}")))?;
      request_headers.insert(name, value);
    }
    client = client.default_headers(request_headers);
  }
  client
    .build()
    .map_err(|e| FerriError::backend(format!("WebDriver HTTP client setup failed: {e}")))
}

pub(crate) fn same_origin(endpoint: &str, websocket: &str) -> bool {
  let (Ok(endpoint), Ok(mut websocket)) = (reqwest::Url::parse(endpoint), reqwest::Url::parse(websocket)) else {
    return false;
  };
  let scheme = match websocket.scheme() {
    "ws" => "http",
    "wss" => "https",
    _ => return false,
  };
  if websocket.set_scheme(scheme).is_err() {
    return false;
  }
  endpoint.origin() == websocket.origin()
}

pub(crate) fn session_url(endpoint: &str) -> Result<reqwest::Url> {
  let mut url = reqwest::Url::parse(endpoint)
    .map_err(|e| FerriError::invalid_argument("endpoint", format!("invalid WebDriver endpoint: {e}")))?;
  if !matches!(url.scheme(), "http" | "https") {
    return Err(FerriError::invalid_argument(
      "endpoint",
      "WebDriver requires an HTTP or HTTPS endpoint",
    ));
  }
  let mut path = url.path().trim_end_matches('/').to_string();
  if !path.ends_with("/session") {
    path.push_str("/session");
  }
  url.set_path(&path);
  url.set_fragment(None);
  Ok(url)
}
