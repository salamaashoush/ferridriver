//! CDP `Browser` domain.

use serde::{Deserialize, Serialize};

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct GetVersion {}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetVersionResponse {
  #[serde(default)]
  pub protocol_version: String,
  #[serde(default)]
  pub product: String,
  #[serde(default)]
  pub revision: String,
  #[serde(default)]
  pub user_agent: String,
  #[serde(default)]
  pub js_version: String,
}

impl Command for GetVersion {
  const METHOD: &'static str = "Browser.getVersion";
  type Response = GetVersionResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetWindowForTarget<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub target_id: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bounds {
  #[serde(default)]
  pub left: Option<i64>,
  #[serde(default)]
  pub top: Option<i64>,
  #[serde(default)]
  pub width: Option<i64>,
  #[serde(default)]
  pub height: Option<i64>,
  #[serde(default)]
  pub window_state: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetWindowForTargetResponse {
  pub window_id: i64,
  pub bounds: Bounds,
}

impl Command for GetWindowForTarget<'_> {
  const METHOD: &'static str = "Browser.getWindowForTarget";
  type Response = GetWindowForTargetResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetWindowBoundsParams {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub left: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub top: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub width: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub height: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub window_state: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetWindowBounds {
  pub window_id: i64,
  pub bounds: SetWindowBoundsParams,
}

impl Command for SetWindowBounds {
  const METHOD: &'static str = "Browser.setWindowBounds";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadBehavior {
  Deny,
  Allow,
  AllowAndName,
  Default,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetDownloadBehavior<'a> {
  pub behavior: DownloadBehavior,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub browser_context_id: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub download_path: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub events_enabled: Option<bool>,
}

impl Command for SetDownloadBehavior<'_> {
  const METHOD: &'static str = "Browser.setDownloadBehavior";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelDownload<'a> {
  pub guid: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub browser_context_id: Option<&'a str>,
}

impl Command for CancelDownload<'_> {
  const METHOD: &'static str = "Browser.cancelDownload";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantPermissions<'a> {
  pub permissions: &'a [&'a str],
  #[serde(skip_serializing_if = "Option::is_none")]
  pub origin: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub browser_context_id: Option<&'a str>,
}

impl Command for GrantPermissions<'_> {
  const METHOD: &'static str = "Browser.grantPermissions";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetPermissions<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub browser_context_id: Option<&'a str>,
}

impl Command for ResetPermissions<'_> {
  const METHOD: &'static str = "Browser.resetPermissions";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Close {}

impl Command for Close {
  const METHOD: &'static str = "Browser.close";
  type Response = EmptyResponse;
}

/// `Browser.downloadWillBegin` event payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadWillBeginEvent {
  #[serde(default)]
  pub frame_id: String,
  #[serde(default)]
  pub guid: String,
  #[serde(default)]
  pub url: String,
  #[serde(default)]
  pub suggested_filename: String,
}

/// `Browser.downloadProgress` event payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgressEvent {
  #[serde(default)]
  pub guid: String,
  #[serde(default)]
  pub total_bytes: f64,
  #[serde(default)]
  pub received_bytes: f64,
  #[serde(default)]
  pub state: String,
}
