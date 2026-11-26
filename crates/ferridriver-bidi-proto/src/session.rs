//! `BiDi` `session` module.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Status {}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusResponse {
  #[serde(default)]
  pub ready: bool,
  #[serde(default)]
  pub message: String,
}

impl Command for Status {
  const METHOD: &'static str = "session.status";
  type Response = StatusResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct New {
  pub capabilities: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewResponse {
  pub session_id: String,
  #[serde(default)]
  pub capabilities: Value,
}

impl Command for New {
  const METHOD: &'static str = "session.new";
  type Response = NewResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Subscribe<'a> {
  pub events: &'a [&'a str],
  #[serde(skip_serializing_if = "Option::is_none")]
  pub contexts: Option<&'a [&'a str]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub user_contexts: Option<&'a [&'a str]>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeResponse {
  #[serde(default)]
  pub subscription: Option<String>,
}

impl Command for Subscribe<'_> {
  const METHOD: &'static str = "session.subscribe";
  type Response = SubscribeResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Unsubscribe<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub events: Option<&'a [&'a str]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub subscriptions: Option<&'a [&'a str]>,
}

impl Command for Unsubscribe<'_> {
  const METHOD: &'static str = "session.unsubscribe";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct End {}

impl Command for End {
  const METHOD: &'static str = "session.end";
  type Response = EmptyResponse;
}
