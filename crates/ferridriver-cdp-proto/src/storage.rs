//! CDP `Storage` domain.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetCookies<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub browser_context_id: Option<&'a str>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetCookiesResponse {
  #[serde(default)]
  pub cookies: Vec<Value>,
}

impl Command for GetCookies<'_> {
  const METHOD: &'static str = "Storage.getCookies";
  type Response = GetCookiesResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCookies<'a> {
  pub cookies: &'a [Value],
  #[serde(skip_serializing_if = "Option::is_none")]
  pub browser_context_id: Option<&'a str>,
}

impl Command for SetCookies<'_> {
  const METHOD: &'static str = "Storage.setCookies";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearCookies<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub browser_context_id: Option<&'a str>,
}

impl Command for ClearCookies<'_> {
  const METHOD: &'static str = "Storage.clearCookies";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearDataForOrigin<'a> {
  pub origin: &'a str,
  pub storage_types: &'a str,
}

impl Command for ClearDataForOrigin<'_> {
  const METHOD: &'static str = "Storage.clearDataForOrigin";
  type Response = EmptyResponse;
}
