//! `WebKit` `Emulation` domain.

use serde::Serialize;

use crate::{Command, EmptyResponse};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetJavaScriptEnabled {
  pub enabled: bool,
}

impl Command for SetJavaScriptEnabled {
  const METHOD: &'static str = "Emulation.setJavaScriptEnabled";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetActiveAndFocused {
  pub active: bool,
}

impl Command for SetActiveAndFocused {
  const METHOD: &'static str = "Emulation.setActiveAndFocused";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetOrientationOverride {
  pub angle: i32,
}

impl Command for SetOrientationOverride {
  const METHOD: &'static str = "Emulation.setOrientationOverride";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct ResetPermissions {}

impl Command for ResetPermissions {
  const METHOD: &'static str = "Emulation.resetPermissions";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetActive {
  pub value: bool,
}

impl Command for SetActive {
  const METHOD: &'static str = "Emulation.setActive";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetFocus {
  pub value: bool,
}

impl Command for SetFocus {
  const METHOD: &'static str = "Emulation.setFocus";
  type Response = EmptyResponse;
}
