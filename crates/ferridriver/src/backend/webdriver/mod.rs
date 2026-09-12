pub mod element;
pub mod page;
pub mod session;

use crate::error::{FerriError, Result};

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

pub(crate) async fn connect_android_chrome(
  endpoint: &str,
  capabilities: &serde_json::Value,
  headers: Option<&rustc_hash::FxHashMap<String, String>>,
  timeout_ms: Option<u64>,
) -> Result<super::AnyBrowser> {
  let timeout_ms = timeout_ms.unwrap_or(30_000);
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
  let mut capabilities = capabilities.clone();
  let object = capabilities
    .as_object_mut()
    .ok_or_else(|| FerriError::invalid_argument("capabilities", "expected an object"))?;
  object
    .entry("browserName")
    .or_insert_with(|| serde_json::json!("Chrome"));
  let client = http_client(headers, timeout_ms)?;
  let session_url = super::bidi::browser::webdriver_session_url(endpoint)?;
  let created =
    session::WebDriverSession::create(client.clone(), session_url, capabilities, timeout_ms, headers).await?;
  let owner = created.session;
  let attach = async {
    let caps = owner
      .execute(
        session::Target::default(),
        session::Command::post(
          &["execute", "sync"],
          serde_json::json!({
            "script":"mobile: getChromeCapabilities", "args":[]
          }),
        ),
        timeout_ms,
      )
      .await?;
    let address = caps
      .pointer("/goog:chromeOptions/debuggerAddress")
      .and_then(serde_json::Value::as_str)
      .ok_or_else(|| FerriError::unsupported("Appium did not expose Chrome's debuggerAddress"))?;
    let discovery = reqwest::Url::parse(&format!("http://{address}/json/version"))
      .map_err(|_| FerriError::protocol("Appium Chrome capabilities", "invalid debuggerAddress"))?;
    let endpoint_url =
      reqwest::Url::parse(endpoint).map_err(|_| FerriError::invalid_argument("endpoint", "invalid URL"))?;
    if discovery.host_str().is_some_and(is_loopback) && !endpoint_url.host_str().is_some_and(is_loopback) {
      return Err(FerriError::unsupported(
        "Remote Appium returned a loopback Chrome endpoint; the provider must expose a reachable CDP or BiDi endpoint",
      ));
    }
    let discovery_client = http_client(None, timeout_ms)?;
    let payload = discovery_client
      .get(discovery)
      .send()
      .await
      .map_err(|e| FerriError::backend(format!("Chrome discovery failed: {}", e.without_url())))?
      .error_for_status()
      .map_err(|e| FerriError::backend(format!("Chrome discovery failed: {}", e.without_url())))?
      .json::<serde_json::Value>()
      .await
      .map_err(|e| FerriError::protocol("Chrome discovery", e.without_url().to_string()))?;
    let socket = payload
      .get("webSocketDebuggerUrl")
      .and_then(serde_json::Value::as_str)
      .ok_or_else(|| FerriError::protocol("Chrome discovery", "response omitted webSocketDebuggerUrl"))?;
    let socket_headers = headers
      .filter(|_| same_origin(endpoint, socket))
      .map(|h| h.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
      .unwrap_or_default();
    let browser = Box::pin(super::cdp::CdpBrowser::connect_owned_webdriver(
      socket,
      &socket_headers,
      owner.clone(),
    ))
    .await?;
    Ok(super::AnyBrowser::CdpRaw(browser))
  };
  let result = if timeout_ms == 0 {
    attach.await
  } else {
    tokio::time::timeout_at(deadline, attach)
      .await
      .unwrap_or_else(|_| Err(FerriError::timeout("attaching to Appium Chrome", timeout_ms)))
  };
  if result.is_err()
    && let Err(cleanup) = owner.close().await
  {
    return Err(FerriError::backend(format!(
      "Appium Chrome attachment failed; cleanup failed: {cleanup}"
    )));
  }
  result
}

fn is_loopback(host: &str) -> bool {
  host.eq_ignore_ascii_case("localhost")
    || host
      .trim_matches(['[', ']'])
      .parse::<std::net::IpAddr>()
      .is_ok_and(|ip| ip.is_loopback())
}

pub(crate) fn http_client(
  headers: Option<&rustc_hash::FxHashMap<String, String>>,
  timeout_ms: u64,
) -> Result<reqwest::Client> {
  let mut client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
  if timeout_ms != 0 {
    client = client.timeout(std::time::Duration::from_millis(timeout_ms));
  }
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
