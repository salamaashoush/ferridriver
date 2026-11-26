//! CDP `DOM` domain.

use serde::{Deserialize, Serialize};

use crate::runtime::RemoteObject;
use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Enable {}

impl Command for Enable {
  const METHOD: &'static str = "DOM.enable";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetDocument {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub depth: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub pierce: Option<bool>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
  #[serde(default, rename = "nodeId")]
  pub id: i64,
  #[serde(default)]
  pub backend_node_id: i64,
  #[serde(default, rename = "nodeType")]
  pub kind: i64,
  #[serde(default, rename = "nodeName")]
  pub name: String,
  #[serde(default)]
  pub frame_id: Option<String>,
  #[serde(default)]
  pub content_document: Option<Box<Node>>,
  #[serde(default)]
  pub attributes: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetDocumentResponse {
  pub root: Node,
}

impl Command for GetDocument {
  const METHOD: &'static str = "DOM.getDocument";
  type Response = GetDocumentResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DescribeNode<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub node_id: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub backend_node_id: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub object_id: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub depth: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub pierce: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DescribeNodeResponse {
  pub node: Node,
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
  pub object_group: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub execution_context_id: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveNodeResponse {
  pub object: RemoteObject,
}

impl Command for ResolveNode<'_> {
  const METHOD: &'static str = "DOM.resolveNode";
  type Response = ResolveNodeResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestNode<'a> {
  pub object_id: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestNodeResponse {
  pub node_id: i64,
}

impl Command for RequestNode<'_> {
  const METHOD: &'static str = "DOM.requestNode";
  type Response = RequestNodeResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct GetBoxModel<'a> {
  #[serde(rename = "nodeId", skip_serializing_if = "Option::is_none")]
  pub node: Option<i64>,
  #[serde(rename = "backendNodeId", skip_serializing_if = "Option::is_none")]
  pub backend_node: Option<i64>,
  #[serde(rename = "objectId", skip_serializing_if = "Option::is_none")]
  pub object: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoxModel {
  pub content: Vec<f64>,
  pub padding: Vec<f64>,
  pub border: Vec<f64>,
  pub margin: Vec<f64>,
  pub width: i64,
  pub height: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetBoxModelResponse {
  pub model: BoxModel,
}

impl Command for GetBoxModel<'_> {
  const METHOD: &'static str = "DOM.getBoxModel";
  type Response = GetBoxModelResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct GetContentQuads<'a> {
  #[serde(rename = "nodeId", skip_serializing_if = "Option::is_none")]
  pub node: Option<i64>,
  #[serde(rename = "backendNodeId", skip_serializing_if = "Option::is_none")]
  pub backend_node: Option<i64>,
  #[serde(rename = "objectId", skip_serializing_if = "Option::is_none")]
  pub object: Option<&'a str>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetContentQuadsResponse {
  #[serde(default)]
  pub quads: Vec<Vec<f64>>,
}

impl Command for GetContentQuads<'_> {
  const METHOD: &'static str = "DOM.getContentQuads";
  type Response = GetContentQuadsResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetFrameOwner<'a> {
  pub frame_id: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetFrameOwnerResponse {
  pub backend_node_id: i64,
  #[serde(default)]
  pub node_id: Option<i64>,
}

impl Command for GetFrameOwner<'_> {
  const METHOD: &'static str = "DOM.getFrameOwner";
  type Response = GetFrameOwnerResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetFileInputFiles<'a> {
  pub files: &'a [String],
  #[serde(skip_serializing_if = "Option::is_none")]
  pub node_id: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub backend_node_id: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub object_id: Option<&'a str>,
}

impl Command for SetFileInputFiles<'_> {
  const METHOD: &'static str = "DOM.setFileInputFiles";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Focus<'a> {
  #[serde(rename = "nodeId", skip_serializing_if = "Option::is_none")]
  pub node: Option<i64>,
  #[serde(rename = "backendNodeId", skip_serializing_if = "Option::is_none")]
  pub backend_node: Option<i64>,
  #[serde(rename = "objectId", skip_serializing_if = "Option::is_none")]
  pub object: Option<&'a str>,
}

impl Command for Focus<'_> {
  const METHOD: &'static str = "DOM.focus";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct ScrollIntoViewIfNeeded<'a> {
  #[serde(rename = "nodeId", skip_serializing_if = "Option::is_none")]
  pub node: Option<i64>,
  #[serde(rename = "backendNodeId", skip_serializing_if = "Option::is_none")]
  pub backend_node: Option<i64>,
  #[serde(rename = "objectId", skip_serializing_if = "Option::is_none")]
  pub object: Option<&'a str>,
}

impl Command for ScrollIntoViewIfNeeded<'_> {
  const METHOD: &'static str = "DOM.scrollIntoViewIfNeeded";
  type Response = EmptyResponse;
}
