//! CDP `Target` domain.

use serde::{Deserialize, Serialize};

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetInfo {
  #[serde(default)]
  pub target_id: String,
  #[serde(default, rename = "type")]
  pub target_type: String,
  #[serde(default)]
  pub title: String,
  #[serde(default)]
  pub url: String,
  #[serde(default)]
  pub attached: bool,
  #[serde(default)]
  pub opener_id: Option<String>,
  #[serde(default)]
  pub opener_frame_id: Option<String>,
  #[serde(default)]
  pub browser_context_id: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct GetTargets {}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTargetsResponse {
  #[serde(default)]
  pub target_infos: Vec<TargetInfo>,
}

impl Command for GetTargets {
  const METHOD: &'static str = "Target.getTargets";
  type Response = GetTargetsResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTarget<'a> {
  pub url: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub browser_context_id: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub width: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub height: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTargetResponse {
  pub target_id: String,
}

impl Command for CreateTarget<'_> {
  const METHOD: &'static str = "Target.createTarget";
  type Response = CreateTargetResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseTarget<'a> {
  pub target_id: &'a str,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseTargetResponse {
  #[serde(default)]
  pub success: bool,
}

impl Command for CloseTarget<'_> {
  const METHOD: &'static str = "Target.closeTarget";
  type Response = CloseTargetResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivateTarget<'a> {
  pub target_id: &'a str,
}

impl Command for ActivateTarget<'_> {
  const METHOD: &'static str = "Target.activateTarget";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachToTarget<'a> {
  pub target_id: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub flatten: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachToTargetResponse {
  pub session_id: String,
}

impl Command for AttachToTarget<'_> {
  const METHOD: &'static str = "Target.attachToTarget";
  type Response = AttachToTargetResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct AttachToBrowserTarget {}

impl Command for AttachToBrowserTarget {
  const METHOD: &'static str = "Target.attachToBrowserTarget";
  type Response = AttachToTargetResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachFromTarget<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub session_id: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub target_id: Option<&'a str>,
}

impl Command for DetachFromTarget<'_> {
  const METHOD: &'static str = "Target.detachFromTarget";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBrowserContext<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub dispose_on_detach: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub proxy_server: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub proxy_bypass_list: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBrowserContextResponse {
  pub browser_context_id: String,
}

impl Command for CreateBrowserContext<'_> {
  const METHOD: &'static str = "Target.createBrowserContext";
  type Response = CreateBrowserContextResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DisposeBrowserContext<'a> {
  pub browser_context_id: &'a str,
}

impl Command for DisposeBrowserContext<'_> {
  const METHOD: &'static str = "Target.disposeBrowserContext";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct GetBrowserContexts {}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetBrowserContextsResponse {
  #[serde(default)]
  pub browser_context_ids: Vec<String>,
}

impl Command for GetBrowserContexts {
  const METHOD: &'static str = "Target.getBrowserContexts";
  type Response = GetBrowserContextsResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAutoAttach {
  pub auto_attach: bool,
  pub wait_for_debugger_on_start: bool,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub flatten: Option<bool>,
}

impl Command for SetAutoAttach {
  const METHOD: &'static str = "Target.setAutoAttach";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetDiscoverTargets {
  pub discover: bool,
}

impl Command for SetDiscoverTargets {
  const METHOD: &'static str = "Target.setDiscoverTargets";
  type Response = EmptyResponse;
}

/// `Target.attachedToTarget` event payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachedToTargetEvent {
  pub session_id: String,
  pub target_info: TargetInfo,
  #[serde(default)]
  pub waiting_for_debugger: bool,
}

/// `Target.detachedFromTarget` event payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachedFromTargetEvent {
  pub session_id: String,
  #[serde(default)]
  pub target_id: Option<String>,
}

/// `Target.targetCreated` event payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetCreatedEvent {
  pub target_info: TargetInfo,
}

/// `Target.targetDestroyed` event payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetDestroyedEvent {
  pub target_id: String,
}
