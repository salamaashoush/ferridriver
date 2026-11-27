//! `BiDi` `network` module.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Command, EmptyResponse};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InterceptPhase {
  BeforeRequestSent,
  ResponseStarted,
  AuthRequired,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddIntercept<'a> {
  pub phases: &'a [InterceptPhase],
  #[serde(skip_serializing_if = "Option::is_none")]
  pub contexts: Option<&'a [&'a str]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub url_patterns: Option<&'a [Value]>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddInterceptResponse {
  pub intercept: String,
}

impl Command for AddIntercept<'_> {
  const METHOD: &'static str = "network.addIntercept";
  type Response = AddInterceptResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveIntercept<'a> {
  pub intercept: &'a str,
}

impl Command for RemoveIntercept<'_> {
  const METHOD: &'static str = "network.removeIntercept";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddDataCollector<'a> {
  pub data_types: &'a [&'a str],
  pub max_encoded_data_size: u64,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub collector_type: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub contexts: Option<&'a [&'a str]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub user_contexts: Option<&'a [&'a str]>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddDataCollectorResponse {
  pub collector: String,
}

impl Command for AddDataCollector<'_> {
  const METHOD: &'static str = "network.addDataCollector";
  type Response = AddDataCollectorResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetData<'a> {
  pub data_type: &'a str,
  pub request: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub collector: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub disown: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetDataResponse {
  pub bytes: Value,
}

impl Command for GetData<'_> {
  const METHOD: &'static str = "network.getData";
  type Response = GetDataResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinueRequest<'a> {
  pub request: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub body: Option<Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub headers: Option<&'a [Value]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub method: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub url: Option<&'a str>,
}

impl Command for ContinueRequest<'_> {
  const METHOD: &'static str = "network.continueRequest";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailRequest<'a> {
  pub request: &'a str,
}

impl Command for FailRequest<'_> {
  const METHOD: &'static str = "network.failRequest";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvideResponse<'a> {
  pub request: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub body: Option<Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub cookies: Option<&'a [Value]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub headers: Option<&'a [Value]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub reason_phrase: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub status_code: Option<u16>,
}

impl Command for ProvideResponse<'_> {
  const METHOD: &'static str = "network.provideResponse";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCacheBehavior<'a> {
  pub cache_behavior: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub contexts: Option<&'a [&'a str]>,
}

impl Command for SetCacheBehavior<'_> {
  const METHOD: &'static str = "network.setCacheBehavior";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetExtraHeaders<'a> {
  pub headers: &'a [Value],
  #[serde(skip_serializing_if = "Option::is_none")]
  pub contexts: Option<&'a [&'a str]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub user_contexts: Option<&'a [&'a str]>,
}

impl Command for SetExtraHeaders<'_> {
  const METHOD: &'static str = "network.setExtraHeaders";
  type Response = EmptyResponse;
}
