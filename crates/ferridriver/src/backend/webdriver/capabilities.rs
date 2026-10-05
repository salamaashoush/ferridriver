use super::page::WebDriverPage;
use crate::error::{FerriError, Result};

pub(crate) fn uses_xcuitest(capabilities: &serde_json::Value) -> bool {
  capabilities["platformName"]
    .as_str()
    .is_some_and(|value| value.eq_ignore_ascii_case("ios"))
    && capabilities
      .get("appium:automationName")
      .or_else(|| capabilities.get("automationName"))
      .or_else(|| capabilities.pointer("/appium:options/automationName"))
      .and_then(serde_json::Value::as_str)
      .is_some_and(|value| value.eq_ignore_ascii_case("xcuitest"))
}

impl WebDriverPage {
  pub(crate) fn supports_window_resize(&self) -> bool {
    self.capabilities["setWindowRect"] != false && !uses_xcuitest(&self.capabilities)
  }
}

macro_rules! unavailable {
  ($name:ident($($arg:ident: $ty:ty),*) -> $output:ty, $reason:literal) => {
    pub fn $name(&self, $($arg: $ty),*) -> impl std::future::Future<Output = Result<$output>> + Send {
      std::future::ready(Err(FerriError::unsupported($reason)))
    }
  };
}

impl WebDriverPage {
  unavailable!(request_gc() -> (), "Classic WebDriver does not expose JavaScript garbage collection");
  unavailable!(emulate_media(_options: &crate::options::EmulateMediaOptions) -> (), "Classic WebDriver does not expose media emulation");
  unavailable!(set_extra_http_headers(_headers: &rustc_hash::FxHashMap<String, String>) -> (), "Classic WebDriver does not expose request header overrides");
  unavailable!(set_http_credentials(_credentials: Option<crate::options::HttpCredentials>) -> (), "Classic WebDriver does not expose HTTP authentication interception");
  unavailable!(reset_permissions() -> (), "Classic WebDriver does not expose browser permission overrides");
  unavailable!(start_tracing(_categories: Option<&[String]>) -> (), "Classic WebDriver does not expose engine tracing");
  unavailable!(stop_tracing() -> Vec<serde_json::Value>, "Classic WebDriver does not expose engine tracing");
  unavailable!(metrics() -> Vec<crate::backend::MetricData>, "Classic WebDriver does not expose engine performance metrics");
  unavailable!(route(_route: crate::route::RegisteredRoute) -> (), "Classic WebDriver does not expose network interception");
  unavailable!(unroute(_matcher: &crate::url_matcher::UrlMatcher, _scope: crate::route::RouteScope, _handler_id: Option<usize>) -> (), "Classic WebDriver does not expose network interception");
  unavailable!(unroute_all(_behavior: crate::options::UnrouteBehavior, _scope: Option<crate::route::RouteScope>) -> (), "Classic WebDriver does not expose network interception");
  unavailable!(expose_binding(_name: &str, _binding: crate::events::ExposedBinding) -> (), "Classic WebDriver does not expose browser-to-host script channels");
  unavailable!(remove_exposed_function(_name: &str) -> (), "Classic WebDriver does not expose browser-to-host script channels");
  unavailable!(add_init_script(_source: &str) -> String, "Classic WebDriver cannot register scripts before document execution");
  unavailable!(remove_init_script(_identifier: &str) -> (), "Classic WebDriver cannot register scripts before document execution");
  unavailable!(stop_screencast() -> (), "Classic WebDriver does not expose a screencast stream");
}
