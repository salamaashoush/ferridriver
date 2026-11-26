//! `BiDi` `script` module.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Command, EmptyResponse};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(untagged)]
pub enum Target<'a> {
  Context {
    context: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    sandbox: Option<&'a str>,
  },
  Realm {
    realm: &'a str,
  },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Evaluate<'a> {
  pub expression: &'a str,
  pub target: Target<'a>,
  pub await_promise: bool,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub result_ownership: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub serialization_options: Option<Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub user_activation: Option<bool>,
}

impl Command for Evaluate<'_> {
  const METHOD: &'static str = "script.evaluate";
  type Response = Value;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallFunction<'a> {
  pub function_declaration: &'a str,
  pub target: Target<'a>,
  pub await_promise: bool,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub arguments: Option<&'a [Value]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub this: Option<Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub result_ownership: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub serialization_options: Option<Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub user_activation: Option<bool>,
}

impl Command for CallFunction<'_> {
  const METHOD: &'static str = "script.callFunction";
  type Response = Value;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddPreloadScript<'a> {
  pub function_declaration: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub arguments: Option<&'a [Value]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub contexts: Option<&'a [&'a str]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub user_contexts: Option<&'a [&'a str]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub sandbox: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddPreloadScriptResponse {
  pub script: String,
}

impl Command for AddPreloadScript<'_> {
  const METHOD: &'static str = "script.addPreloadScript";
  type Response = AddPreloadScriptResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovePreloadScript<'a> {
  pub script: &'a str,
}

impl Command for RemovePreloadScript<'_> {
  const METHOD: &'static str = "script.removePreloadScript";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Disown<'a> {
  pub handles: &'a [&'a str],
  pub target: Target<'a>,
}

impl Command for Disown<'_> {
  const METHOD: &'static str = "script.disown";
  type Response = EmptyResponse;
}

/// Source context/realm inside `script.message`.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSource {
  #[serde(default)]
  pub realm: String,
  #[serde(default)]
  pub context: Option<String>,
}

/// `script.message` event payload.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageEvent {
  #[serde(default)]
  pub channel: String,
  #[serde(default)]
  pub data: Value,
  #[serde(default)]
  pub source: MessageSource,
}
