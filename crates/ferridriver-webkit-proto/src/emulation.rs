//! `WebKit` `Emulation` domain.

use serde::Serialize;

use crate::{Command, EmptyResponse};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetJavaScriptEnabled {
  pub value: bool,
}

impl Command for SetJavaScriptEnabled {
  const METHOD: &'static str = "Emulation.setJavaScriptEnabled";
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
