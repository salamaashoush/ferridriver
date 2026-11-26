//! CDP `Network`, `Fetch`, `Storage`, `Security`, `Accessibility`, `Tracing`, `HeapProfiler`, `Performance` domains.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Enable {}

impl Command for Enable {
  const METHOD: &'static str = "Network.enable";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Disable {}

impl Command for Disable {
  const METHOD: &'static str = "Network.disable";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetExtraHttpHeaders<'a> {
  pub headers: &'a Value,
}

impl Command for SetExtraHttpHeaders<'_> {
  const METHOD: &'static str = "Network.setExtraHTTPHeaders";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCacheDisabled {
  pub cache_disabled: bool,
}

impl Command for SetCacheDisabled {
  const METHOD: &'static str = "Network.setCacheDisabled";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetBypassServiceWorker {
  pub bypass: bool,
}

impl Command for SetBypassServiceWorker {
  const METHOD: &'static str = "Network.setBypassServiceWorker";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmulateNetworkConditions {
  pub offline: bool,
  pub latency: f64,
  pub download_throughput: f64,
  pub upload_throughput: f64,
}

impl Command for EmulateNetworkConditions {
  const METHOD: &'static str = "Network.emulateNetworkConditions";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct ClearBrowserCache {}

impl Command for ClearBrowserCache {
  const METHOD: &'static str = "Network.clearBrowserCache";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct ClearBrowserCookies {}

impl Command for ClearBrowserCookies {
  const METHOD: &'static str = "Network.clearBrowserCookies";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetResponseBody<'a> {
  pub request_id: &'a str,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetResponseBodyResponse {
  #[serde(default)]
  pub body: String,
  #[serde(default)]
  pub base64_encoded: bool,
}

impl Command for GetResponseBody<'_> {
  const METHOD: &'static str = "Network.getResponseBody";
  type Response = GetResponseBodyResponse;
}
