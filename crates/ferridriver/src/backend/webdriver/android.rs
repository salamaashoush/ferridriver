use serde_json::{Value, json};

use super::WebDriverProtocol;
use super::browser::WebDriverBrowser;
use super::session::{Command, CreatedSession, Target, WebDriverSession};
use crate::backend::AnyBrowser;
use crate::error::{FerriError, Result};

#[cfg(test)]
#[path = "android_tests.rs"]
mod tests;

pub(crate) async fn connect_android_chrome(
  endpoint: &str,
  capabilities: &Value,
  headers: Option<&rustc_hash::FxHashMap<String, String>>,
  timeout_ms: Option<u64>,
  protocol: WebDriverProtocol,
) -> Result<AnyBrowser> {
  let timeout_ms = timeout_ms.unwrap_or(30_000);
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
  let mut capabilities = capabilities.clone();
  capabilities
    .as_object_mut()
    .ok_or_else(|| FerriError::invalid_argument("capabilities", "expected an object"))?
    .entry("browserName")
    .or_insert_with(|| json!("Chrome"));
  let created = WebDriverSession::create(
    super::http_client(headers)?,
    super::session_url(endpoint)?,
    capabilities,
    timeout_ms,
    headers,
  )
  .await?;
  let owner = created.session.clone();
  let attach = attach(created, endpoint, headers, deadline, timeout_ms, protocol);
  let result = if timeout_ms == 0 {
    attach.await
  } else {
    tokio::time::timeout_at(deadline, attach)
      .await
      .unwrap_or_else(|_| Err(FerriError::timeout("attaching to Appium Chrome", timeout_ms)))
  };
  match result {
    Ok(browser) => Ok(browser),
    Err(error) => {
      if let Err(cleanup) = owner.close().await {
        return Err(FerriError::backend(format!("{error}; cleanup failed: {cleanup}")));
      }
      Err(error)
    },
  }
}

async fn attach(
  mut created: CreatedSession,
  endpoint: &str,
  headers: Option<&rustc_hash::FxHashMap<String, String>>,
  deadline: tokio::time::Instant,
  timeout_ms: u64,
  protocol: WebDriverProtocol,
) -> Result<AnyBrowser> {
  if protocol == WebDriverProtocol::Classic
    && created.capabilities["browserVersion"]
      .as_str()
      .is_some_and(|version| !version.is_empty())
  {
    return WebDriverBrowser::from_created(created, deadline, timeout_ms)
      .await
      .map(AnyBrowser::WebDriver);
  }
  let capabilities = created
    .session
    .execute(
      Target::default(),
      Command::post(
        &["execute", "sync"],
        json!({"script":"mobile: getChromeCapabilities", "args":[]}),
      ),
      timeout_ms,
    )
    .await;
  let capabilities = match capabilities {
    Ok(Value::Object(caps)) => Value::Object(caps),
    Ok(_) => return Err(FerriError::protocol("Appium Chrome capabilities", "expected an object")),
    Err(FerriError::Unsupported(_)) => {
      return WebDriverBrowser::from_created(created, deadline, timeout_ms)
        .await
        .map(AnyBrowser::WebDriver);
    },
    Err(error) => return Err(error),
  };
  if created.capabilities["browserVersion"]
    .as_str()
    .is_none_or(str::is_empty)
    && let Some(version) = capabilities["browserVersion"]
      .as_str()
      .filter(|version| !version.is_empty())
  {
    created.capabilities["browserVersion"] = json!(version);
  }
  if protocol == WebDriverProtocol::Classic {
    return WebDriverBrowser::from_created(created, deadline, timeout_ms)
      .await
      .map(AnyBrowser::WebDriver);
  }
  let discovery = discovery_url(&capabilities)?;
  let remote_loopback = discovery.as_ref().is_some_and(|discovery| {
    discovery.host_str().is_some_and(is_loopback)
      && reqwest::Url::parse(endpoint).is_ok_and(|url| !url.host_str().is_some_and(is_loopback))
  });
  let Some(discovery) = discovery.filter(|_| !remote_loopback) else {
    return WebDriverBrowser::from_created(created, deadline, timeout_ms)
      .await
      .map(AnyBrowser::WebDriver);
  };
  let payload = super::http_client(None)?
    .get(discovery)
    .send()
    .await
    .map_err(|error| FerriError::backend(format!("Chrome discovery failed: {}", error.without_url())))?
    .error_for_status()
    .map_err(|error| FerriError::backend(format!("Chrome discovery failed: {}", error.without_url())))?
    .json::<Value>()
    .await
    .map_err(|error| FerriError::protocol("Chrome discovery", error.without_url().to_string()))?;
  let socket = payload
    .get("webSocketDebuggerUrl")
    .and_then(Value::as_str)
    .ok_or_else(|| FerriError::protocol("Chrome discovery", "response omitted webSocketDebuggerUrl"))?;
  let socket_headers = headers
    .filter(|_| super::same_origin(endpoint, socket))
    .map(|headers| {
      headers
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
    })
    .unwrap_or_default();
  let browser = Box::pin(crate::backend::cdp::CdpBrowser::connect_owned_webdriver(
    socket,
    &socket_headers,
    created.session,
  ))
  .await?;
  Ok(AnyBrowser::CdpWs(browser))
}

fn discovery_url(capabilities: &Value) -> Result<Option<reqwest::Url>> {
  let Some(options) = capabilities.get("goog:chromeOptions") else {
    return Ok(None);
  };
  let options = options
    .as_object()
    .ok_or_else(|| FerriError::protocol("Appium Chrome capabilities", "goog:chromeOptions must be an object"))?;
  let Some(address) = options.get("debuggerAddress") else {
    return Ok(None);
  };
  let address = address.as_str().filter(|address| !address.is_empty()).ok_or_else(|| {
    FerriError::protocol(
      "Appium Chrome capabilities",
      "debuggerAddress must be a non-empty address",
    )
  })?;
  let mut url = reqwest::Url::parse(&format!("http://{address}"))
    .map_err(|_| FerriError::protocol("Appium Chrome capabilities", "invalid debuggerAddress"))?;
  if url.host_str().is_none()
    || !url.username().is_empty()
    || url.password().is_some()
    || url.path() != "/"
    || url.query().is_some()
    || url.fragment().is_some()
  {
    return Err(FerriError::protocol(
      "Appium Chrome capabilities",
      "invalid debuggerAddress",
    ));
  }
  url.set_path("/json/version");
  Ok(Some(url))
}

fn is_loopback(host: &str) -> bool {
  host.eq_ignore_ascii_case("localhost")
    || host
      .trim_matches(['[', ']'])
      .parse::<std::net::IpAddr>()
      .is_ok_and(|ip| ip.is_loopback())
}
