//! `BiDi` `log` module events.

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackFrame {
  #[serde(default)]
  pub url: String,
  #[serde(default)]
  pub function_name: String,
  #[serde(default)]
  pub line_number: u64,
  #[serde(default)]
  pub column_number: u64,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackTrace {
  #[serde(default)]
  pub call_frames: Vec<StackFrame>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogSource {
  #[serde(default)]
  pub realm: String,
  #[serde(default)]
  pub context: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryAddedEvent {
  #[serde(default, rename = "type")]
  pub entry_type: String,
  #[serde(default)]
  pub level: String,
  #[serde(default)]
  pub source: LogSource,
  #[serde(default)]
  pub text: Option<String>,
  #[serde(default)]
  pub timestamp: u64,
  #[serde(default)]
  pub method: Option<String>,
  #[serde(default)]
  pub args: Option<Vec<Value>>,
  #[serde(default)]
  pub stack_trace: Option<StackTrace>,
}
