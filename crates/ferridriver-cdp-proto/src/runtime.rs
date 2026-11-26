//! CDP `Runtime` domain.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Enable {}

impl Command for Enable {
  const METHOD: &'static str = "Runtime.enable";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Disable {}

impl Command for Disable {
  const METHOD: &'static str = "Runtime.disable";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct RunIfWaitingForDebugger {}

impl Command for RunIfWaitingForDebugger {
  const METHOD: &'static str = "Runtime.runIfWaitingForDebugger";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteObject {
  #[serde(default, rename = "type")]
  pub object_type: String,
  #[serde(default)]
  pub subtype: Option<String>,
  #[serde(default)]
  pub class_name: Option<String>,
  #[serde(default)]
  pub value: Option<Value>,
  #[serde(default)]
  pub unserializable_value: Option<String>,
  #[serde(default)]
  pub description: Option<String>,
  #[serde(default)]
  pub object_id: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExceptionDetails {
  #[serde(default)]
  pub text: String,
  #[serde(default)]
  pub line_number: i64,
  #[serde(default)]
  pub column_number: i64,
  #[serde(default)]
  pub url: Option<String>,
  #[serde(default)]
  pub exception: Option<RemoteObject>,
  #[serde(default)]
  pub stack_trace: Option<Value>,
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
  pub user_gesture: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub silent: Option<bool>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluateResponse {
  #[serde(default)]
  pub result: RemoteObject,
  #[serde(default)]
  pub exception_details: Option<ExceptionDetails>,
}

impl Command for Evaluate<'_> {
  const METHOD: &'static str = "Runtime.evaluate";
  type Response = EvaluateResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallArgument {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub value: Option<Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub unserializable_value: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub object_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallFunctionOn<'a> {
  pub function_declaration: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub object_id: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub execution_context_id: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub arguments: Option<&'a [CallArgument]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub return_by_value: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub await_promise: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub user_gesture: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub silent: Option<bool>,
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
  #[serde(skip_serializing_if = "Option::is_none")]
  pub execution_context_name: Option<&'a str>,
}

impl Command for AddBinding<'_> {
  const METHOD: &'static str = "Runtime.addBinding";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveBinding<'a> {
  pub name: &'a str,
}

impl Command for RemoveBinding<'_> {
  const METHOD: &'static str = "Runtime.removeBinding";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetProperties<'a> {
  pub object_id: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub own_properties: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PropertyDescriptor {
  pub name: String,
  #[serde(default)]
  pub value: Option<RemoteObject>,
  #[serde(default)]
  pub enumerable: bool,
  #[serde(default)]
  pub is_own: Option<bool>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetPropertiesResponse {
  #[serde(default)]
  pub result: Vec<PropertyDescriptor>,
}

impl Command for GetProperties<'_> {
  const METHOD: &'static str = "Runtime.getProperties";
  type Response = GetPropertiesResponse;
}

/// `Runtime.executionContextCreated` event payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionContextAuxData {
  #[serde(default)]
  pub frame_id: Option<String>,
  #[serde(default)]
  pub is_default: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionContextDescription {
  pub id: i64,
  #[serde(default)]
  pub origin: String,
  #[serde(default)]
  pub name: String,
  #[serde(default)]
  pub aux_data: Option<ExecutionContextAuxData>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionContextCreatedEvent {
  pub context: ExecutionContextDescription,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionContextDestroyedEvent {
  pub execution_context_id: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BindingCalledEvent {
  pub name: String,
  pub payload: String,
  pub execution_context_id: i64,
}

/// `Runtime.consoleAPICalled` event payload.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleApiCalledEvent {
  #[serde(default, rename = "type")]
  pub kind: String,
  #[serde(default)]
  pub args: Vec<Value>,
  #[serde(default)]
  pub execution_context_id: i64,
  #[serde(default)]
  pub timestamp: f64,
  #[serde(default)]
  pub stack_trace: Option<Value>,
}

/// `Runtime.exceptionThrown` event payload.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExceptionThrownEvent {
  #[serde(default)]
  pub timestamp: f64,
  #[serde(default)]
  pub exception_details: Value,
}
