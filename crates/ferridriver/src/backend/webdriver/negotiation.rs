use serde_json::{Value, json};

use super::browser::WebDriverBrowser;
use super::session::WebDriverSession;
use crate::backend::{AnyBrowser, BackendKind, bidi::BidiBrowser};
use crate::error::{FerriError, Result};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WebDriverProtocol {
  #[default]
  Auto,
  Classic,
  Bidi,
}

impl WebDriverProtocol {
  #[must_use]
  pub fn from_backend(backend: Option<BackendKind>) -> Self {
    match backend {
      Some(BackendKind::WebDriver) => Self::Classic,
      Some(BackendKind::Bidi) => Self::Bidi,
      _ => Self::Auto,
    }
  }

  fn resolve(self, capabilities: Option<&Value>) -> Result<Self> {
    if capabilities.is_some_and(|value| !value.is_object()) {
      return Err(FerriError::invalid_argument("capabilities", "expected an object"));
    }
    let requested = match capabilities.and_then(|caps| caps.get("webSocketUrl")) {
      None | Some(Value::Null) => return Ok(self),
      Some(Value::Bool(true)) => Self::Bidi,
      Some(Value::Bool(false)) => Self::Classic,
      Some(_) => return Err(FerriError::invalid_argument("webSocketUrl", "expected a boolean")),
    };
    if self != Self::Auto && self != requested {
      return Err(FerriError::invalid_argument(
        "webSocketUrl",
        "conflicts with the selected WebDriver protocol",
      ));
    }
    Ok(requested)
  }
}

pub(crate) async fn connect(
  endpoint: &str,
  browser_name: &str,
  capabilities: Option<&Value>,
  headers: Option<&rustc_hash::FxHashMap<String, String>>,
  timeout_ms: Option<u64>,
  protocol: WebDriverProtocol,
) -> Result<AnyBrowser> {
  let protocol = protocol.resolve(capabilities)?;
  let effective_name = capabilities
    .and_then(|caps| caps.get("browserName"))
    .and_then(Value::as_str)
    .unwrap_or(browser_name);
  if protocol != WebDriverProtocol::Bidi
    && effective_name.eq_ignore_ascii_case("chrome")
    && super::uses_android_chrome(capabilities)
  {
    return super::connect_android_chrome(
      endpoint,
      capabilities.ok_or_else(|| FerriError::invalid_argument("capabilities", "expected Android capabilities"))?,
      headers,
      timeout_ms,
      protocol,
    )
    .await;
  }
  let timeout_ms = timeout_ms.unwrap_or(30_000);
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
  let request_bidi = protocol == WebDriverProtocol::Bidi
    || (protocol == WebDriverProtocol::Auto && !effective_name.eq_ignore_ascii_case("safari"));
  let requested = if request_bidi {
    crate::backend::bidi::browser::webdriver_capabilities(browser_name, capabilities)
  } else {
    let mut requested = json!({"browserName":browser_name,"unhandledPromptBehavior":"ignore"});
    if let Some(extra) = capabilities.and_then(Value::as_object)
      && let Some(target) = requested.as_object_mut()
    {
      target.extend(extra.iter().map(|(key, value)| (key.clone(), value.clone())));
    }
    requested
  };
  let created = WebDriverSession::create(
    super::http_client(headers)?,
    super::session_url(endpoint)?,
    requested,
    timeout_ms,
    headers,
  )
  .await?;
  let owner = created.session.clone();
  let attach = async {
    match created.capabilities.get("webSocketUrl") {
      Some(Value::String(url)) if !url.is_empty() && protocol != WebDriverProtocol::Classic => {
        BidiBrowser::from_created(created, endpoint, headers)
          .await
          .map(AnyBrowser::Bidi)
      },
      None | Some(Value::Null | Value::Bool(false)) if protocol != WebDriverProtocol::Bidi => {
        WebDriverBrowser::from_created(created, deadline, timeout_ms)
          .await
          .map(AnyBrowser::WebDriver)
      },
      Some(_) if protocol == WebDriverProtocol::Classic => {
        WebDriverBrowser::from_created(created, deadline, timeout_ms)
          .await
          .map(AnyBrowser::WebDriver)
      },
      None | Some(Value::Null | Value::Bool(false)) => Err(FerriError::unsupported(
        "WebDriver server created a Classic session without a BiDi webSocketUrl capability",
      )),
      Some(_) => Err(FerriError::protocol(
        "WebDriver capabilities",
        "webSocketUrl must be a non-empty WebSocket URL",
      )),
    }
  };
  let result = if timeout_ms == 0 {
    attach.await
  } else {
    tokio::time::timeout_at(deadline, attach)
      .await
      .unwrap_or_else(|_| Err(FerriError::timeout("connecting WebDriver session", timeout_ms)))
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
