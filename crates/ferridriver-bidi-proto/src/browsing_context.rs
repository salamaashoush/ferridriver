//! `BiDi` `browsingContext` module.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Command, EmptyResponse};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CreateType {
  Tab,
  Window,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Create<'a> {
  #[serde(rename = "type")]
  pub kind: CreateType,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub reference_context: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub background: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub user_context: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateResponse {
  pub context: String,
}

impl Command for Create<'_> {
  const METHOD: &'static str = "browsingContext.create";
  type Response = CreateResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Close<'a> {
  pub context: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub prompt_unload: Option<bool>,
}

impl Command for Close<'_> {
  const METHOD: &'static str = "browsingContext.close";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReadinessState {
  None,
  Interactive,
  Complete,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Navigate<'a> {
  pub context: &'a str,
  pub url: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub wait: Option<ReadinessState>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigateResponse {
  #[serde(default)]
  pub navigation: Option<String>,
  #[serde(default)]
  pub url: String,
}

impl Command for Navigate<'_> {
  const METHOD: &'static str = "browsingContext.navigate";
  type Response = NavigateResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reload<'a> {
  pub context: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub ignore_cache: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub wait: Option<ReadinessState>,
}

impl Command for Reload<'_> {
  const METHOD: &'static str = "browsingContext.reload";
  type Response = NavigateResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraverseHistory<'a> {
  pub context: &'a str,
  pub delta: i64,
}

impl Command for TraverseHistory<'_> {
  const METHOD: &'static str = "browsingContext.traverseHistory";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTree<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub max_depth: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub root: Option<&'a str>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextInfo {
  pub context: String,
  #[serde(default)]
  pub url: String,
  #[serde(default)]
  pub parent: Option<String>,
  #[serde(default)]
  pub user_context: Option<String>,
  #[serde(default)]
  pub original_opener: Option<String>,
  #[serde(default)]
  pub children: Option<Vec<ContextInfo>>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTreeResponse {
  #[serde(default)]
  pub contexts: Vec<ContextInfo>,
}

impl Command for GetTree<'_> {
  const METHOD: &'static str = "browsingContext.getTree";
  type Response = GetTreeResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Viewport {
  pub width: u32,
  pub height: u32,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetViewport<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub context: Option<&'a str>,
  pub viewport: Option<Viewport>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub device_pixel_ratio: Option<f64>,
}

impl Command for SetViewport<'_> {
  const METHOD: &'static str = "browsingContext.setViewport";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Activate<'a> {
  pub context: &'a str,
}

impl Command for Activate<'_> {
  const METHOD: &'static str = "browsingContext.activate";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HandleUserPrompt<'a> {
  pub context: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub accept: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub user_text: Option<&'a str>,
}

impl Command for HandleUserPrompt<'_> {
  const METHOD: &'static str = "browsingContext.handleUserPrompt";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureScreenshot<'a> {
  pub context: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub origin: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub format: Option<Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub clip: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureScreenshotResponse {
  pub data: String,
}

impl Command for CaptureScreenshot<'_> {
  const METHOD: &'static str = "browsingContext.captureScreenshot";
  type Response = CaptureScreenshotResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocateNodes<'a> {
  pub context: &'a str,
  pub locator: Value,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub max_node_count: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub start_nodes: Option<&'a [Value]>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocateNodesResponse {
  #[serde(default)]
  pub nodes: Vec<Value>,
}

impl Command for LocateNodes<'_> {
  const METHOD: &'static str = "browsingContext.locateNodes";
  type Response = LocateNodesResponse;
}

/// `browsingContext.navigationStarted` / `navigationCommitted` / `domContentLoaded` /
/// `load` / `historyUpdated` / `fragmentNavigated` event payload.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigationInfoEvent {
  #[serde(default)]
  pub context: String,
  #[serde(default)]
  pub navigation: Option<String>,
  #[serde(default)]
  pub timestamp: u64,
  #[serde(default)]
  pub url: String,
}

/// `browsingContext.userPromptOpened` event payload.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserPromptOpenedEvent {
  #[serde(default)]
  pub context: String,
  #[serde(default)]
  pub handler: Option<String>,
  #[serde(default)]
  pub message: String,
  #[serde(default, rename = "type")]
  pub prompt_type: String,
  #[serde(default)]
  pub default_value: String,
}

/// `browsingContext.userPromptClosed` event payload.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserPromptClosedEvent {
  #[serde(default)]
  pub context: String,
  #[serde(default)]
  pub accepted: bool,
  #[serde(default)]
  pub user_text: Option<String>,
}

/// `browsingContext.downloadWillBegin` event payload.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadWillBeginEvent {
  #[serde(default)]
  pub context: String,
  #[serde(default)]
  pub navigation: Option<String>,
  #[serde(default)]
  pub url: String,
  #[serde(default)]
  pub suggested_filename: String,
}

/// `browsingContext.downloadEnd` event payload.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadEndEvent {
  #[serde(default)]
  pub context: String,
  #[serde(default)]
  pub navigation: Option<String>,
  #[serde(default)]
  pub status: String,
  #[serde(default)]
  pub filepath: Option<String>,
}
