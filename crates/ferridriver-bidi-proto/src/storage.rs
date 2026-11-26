//! `BiDi` `storage`, `emulation`, `permissions`, and `log` modules.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetCookies {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub filter: Option<Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub partition: Option<Value>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetCookiesResponse {
  #[serde(default)]
  pub cookies: Vec<Value>,
}

impl Command for GetCookies {
  const METHOD: &'static str = "storage.getCookies";
  type Response = GetCookiesResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCookie {
  pub cookie: Value,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub partition: Option<Value>,
}

impl Command for SetCookie {
  const METHOD: &'static str = "storage.setCookie";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteCookies {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub filter: Option<Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub partition: Option<Value>,
}

impl Command for DeleteCookies {
  const METHOD: &'static str = "storage.deleteCookies";
  type Response = EmptyResponse;
}
