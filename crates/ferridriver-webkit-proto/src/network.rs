//! `WebKit` `Network` domain.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Enable {}

impl Command for Enable {
  const METHOD: &'static str = "Network.enable";
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
pub struct SetResourceCachingDisabled {
  pub disabled: bool,
}

impl Command for SetResourceCachingDisabled {
  const METHOD: &'static str = "Network.setResourceCachingDisabled";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetEmulateOfflineState {
  pub offline: bool,
}

impl Command for SetEmulateOfflineState {
  const METHOD: &'static str = "Network.setEmulateOfflineState";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetInterceptionEnabled {
  pub enabled: bool,
}

impl Command for SetInterceptionEnabled {
  const METHOD: &'static str = "Network.setInterceptionEnabled";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddInterception<'a> {
  pub url: &'a str,
  pub stage: &'a str,
}

impl Command for AddInterception<'_> {
  const METHOD: &'static str = "Network.addInterception";
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
