//! `BiDi` `browser` module.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateUserContext {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub accept_insecure_certs: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub proxy: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateUserContextResponse {
  pub user_context: String,
}

impl Command for CreateUserContext {
  const METHOD: &'static str = "browser.createUserContext";
  type Response = CreateUserContextResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveUserContext<'a> {
  pub user_context: &'a str,
}

impl Command for RemoveUserContext<'_> {
  const METHOD: &'static str = "browser.removeUserContext";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct GetUserContexts {}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserContextInfo {
  pub user_context: String,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetUserContextsResponse {
  #[serde(default)]
  pub user_contexts: Vec<UserContextInfo>,
}

impl Command for GetUserContexts {
  const METHOD: &'static str = "browser.getUserContexts";
  type Response = GetUserContextsResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DownloadBehavior<'a> {
  Allowed {
    #[serde(rename = "destinationFolder")]
    destination_folder: &'a str,
  },
  Denied {},
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetDownloadBehavior<'a> {
  pub download_behavior: Option<DownloadBehavior<'a>>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub user_contexts: Option<&'a [&'a str]>,
}

impl Command for SetDownloadBehavior<'_> {
  const METHOD: &'static str = "browser.setDownloadBehavior";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Close {}

impl Command for Close {
  const METHOD: &'static str = "browser.close";
  type Response = EmptyResponse;
}
