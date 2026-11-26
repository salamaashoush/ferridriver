//! `WebKit` `Target` domain.

use serde::{Deserialize, Serialize};

use crate::{Command, EmptyResponse};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resume<'a> {
  pub target_id: &'a str,
}

impl Command for Resume<'_> {
  const METHOD: &'static str = "Target.resume";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageToTarget<'a> {
  pub target_id: &'a str,
  pub message: &'a str,
}

impl Command for SendMessageToTarget<'_> {
  const METHOD: &'static str = "Target.sendMessageToTarget";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetInfo {
  #[serde(default)]
  pub target_id: String,
  #[serde(default, rename = "type")]
  pub target_type: String,
  #[serde(default)]
  pub is_provisional: bool,
  #[serde(default)]
  pub is_paused: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetCreatedEvent {
  pub target_info: TargetInfo,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DidCommitProvisionalTargetEvent {
  pub old_target_id: String,
  pub new_target_id: String,
}
