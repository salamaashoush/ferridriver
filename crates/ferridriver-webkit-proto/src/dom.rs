//! `WebKit` `DOM` domain.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Enable {}

impl Command for Enable {
  const METHOD: &'static str = "DOM.enable";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DescribeNode<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub object_id: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub node_id: Option<i64>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DescribeNodeResponse {
  #[serde(default)]
  pub content_frame_id: Option<String>,
  #[serde(default)]
  pub node: Value,
}

impl Command for DescribeNode<'_> {
  const METHOD: &'static str = "DOM.describeNode";
  type Response = DescribeNodeResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveNode<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub node_id: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub backend_node_id: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub frame_id: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub execution_context_id: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub object_group: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveNodeResponse {
  pub object: Value,
}

impl Command for ResolveNode<'_> {
  const METHOD: &'static str = "DOM.resolveNode";
  type Response = ResolveNodeResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetInputFiles<'a> {
  pub object_id: &'a str,
  pub paths: &'a [String],
}

impl Command for SetInputFiles<'_> {
  const METHOD: &'static str = "DOM.setInputFiles";
  type Response = EmptyResponse;
}
