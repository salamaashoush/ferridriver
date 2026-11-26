//! `WebKit` `Console` domain.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Enable {}

impl Command for Enable {
  const METHOD: &'static str = "Console.enable";
  type Response = EmptyResponse;
}

/// Payload inside `Console.messageAdded`.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleMessagePayload {
  #[serde(default)]
  pub source: String,
  #[serde(default)]
  pub level: String,
  #[serde(default)]
  pub text: String,
  #[serde(default, rename = "type")]
  pub kind: Option<String>,
  #[serde(default)]
  pub url: Option<String>,
  #[serde(default)]
  pub line: Option<i64>,
  #[serde(default)]
  pub column: Option<i64>,
  #[serde(default)]
  pub parameters: Vec<Value>,
  #[serde(default)]
  pub stack_trace: Option<Value>,
}

/// `Console.messageAdded` event payload.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageAddedEvent {
  #[serde(default)]
  pub message: ConsoleMessagePayload,
}
