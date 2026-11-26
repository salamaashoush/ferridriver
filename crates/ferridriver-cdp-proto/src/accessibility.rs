//! CDP `Accessibility` domain.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Enable {}

impl Command for Enable {
  const METHOD: &'static str = "Accessibility.enable";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetFullAxTree<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub depth: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub frame_id: Option<&'a str>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AxValue {
  #[serde(default, rename = "type")]
  pub value_type: String,
  #[serde(default)]
  pub value: Option<Value>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AxProperty {
  #[serde(default)]
  pub name: String,
  #[serde(default)]
  pub value: AxValue,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AxNode {
  #[serde(default)]
  pub node_id: String,
  #[serde(default)]
  pub parent_id: Option<String>,
  #[serde(default)]
  pub ignored: bool,
  #[serde(default)]
  pub role: Option<AxValue>,
  #[serde(default)]
  pub name: Option<AxValue>,
  #[serde(default)]
  pub value: Option<AxValue>,
  #[serde(default)]
  pub description: Option<AxValue>,
  #[serde(default)]
  pub properties: Option<Vec<AxProperty>>,
  #[serde(default)]
  pub child_ids: Option<Vec<String>>,
  #[serde(default, rename = "backendDOMNodeId")]
  pub backend_dom_node_id: Option<i64>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetFullAxTreeResponse {
  #[serde(default)]
  pub nodes: Vec<AxNode>,
}

impl Command for GetFullAxTree<'_> {
  const METHOD: &'static str = "Accessibility.getFullAXTree";
  type Response = GetFullAxTreeResponse;
}
