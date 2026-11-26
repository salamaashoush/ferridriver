//! `BiDi` `emulation` module.

use serde::Serialize;
use serde_json::Value;

use crate::{Command, EmptyResponse};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetLocaleOverride<'a> {
  pub locale: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub contexts: Option<&'a [&'a str]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub user_contexts: Option<&'a [&'a str]>,
}

impl Command for SetLocaleOverride<'_> {
  const METHOD: &'static str = "emulation.setLocaleOverride";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetTimezoneOverride<'a> {
  pub timezone: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub contexts: Option<&'a [&'a str]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub user_contexts: Option<&'a [&'a str]>,
}

impl Command for SetTimezoneOverride<'_> {
  const METHOD: &'static str = "emulation.setTimezoneOverride";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetGeolocationOverride<'a> {
  pub coordinates: Option<Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub contexts: Option<&'a [&'a str]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub user_contexts: Option<&'a [&'a str]>,
}

impl Command for SetGeolocationOverride<'_> {
  const METHOD: &'static str = "emulation.setGeolocationOverride";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetUserAgentOverride<'a> {
  pub user_agent: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub contexts: Option<&'a [&'a str]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub user_contexts: Option<&'a [&'a str]>,
}

impl Command for SetUserAgentOverride<'_> {
  const METHOD: &'static str = "emulation.setUserAgentOverride";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetScriptingEnabled<'a> {
  pub enabled: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub contexts: Option<&'a [&'a str]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub user_contexts: Option<&'a [&'a str]>,
}

impl Command for SetScriptingEnabled<'_> {
  const METHOD: &'static str = "emulation.setScriptingEnabled";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetNetworkConditions<'a> {
  pub network_conditions: Option<Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub contexts: Option<&'a [&'a str]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub user_contexts: Option<&'a [&'a str]>,
}

impl Command for SetNetworkConditions<'_> {
  const METHOD: &'static str = "emulation.setNetworkConditions";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetMediaFeaturesOverride<'a> {
  pub media_features: Option<Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub contexts: Option<&'a [&'a str]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub user_contexts: Option<&'a [&'a str]>,
}

impl Command for SetMediaFeaturesOverride<'_> {
  const METHOD: &'static str = "emulation.setMediaFeaturesOverride";
  type Response = EmptyResponse;
}
