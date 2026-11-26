//! `WebKit` `Runtime` domain.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Enable {}

impl Command for Enable {
  const METHOD: &'static str = "Runtime.enable";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Evaluate<'a> {
  pub expression: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub context_id: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub return_by_value: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub await_promise: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub do_not_pause_on_exceptions_and_mute_console: Option<bool>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluateResponse {
  #[serde(default)]
  pub result: Value,
  #[serde(default)]
  pub was_thrown: Option<bool>,
}

impl Command for Evaluate<'_> {
  const METHOD: &'static str = "Runtime.evaluate";
  type Response = EvaluateResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallFunctionOn<'a> {
  pub object_id: &'a str,
  pub function_declaration: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub arguments: Option<&'a [Value]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub return_by_value: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub await_promise: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub do_not_pause_on_exceptions_and_mute_console: Option<bool>,
}

impl Command for CallFunctionOn<'_> {
  const METHOD: &'static str = "Runtime.callFunctionOn";
  type Response = EvaluateResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseObject<'a> {
  pub object_id: &'a str,
}

impl Command for ReleaseObject<'_> {
  const METHOD: &'static str = "Runtime.releaseObject";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddBinding<'a> {
  pub name: &'a str,
}

impl Command for AddBinding<'_> {
  const METHOD: &'static str = "Runtime.addBinding";
  type Response = EmptyResponse;
}
