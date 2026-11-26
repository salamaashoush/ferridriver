//! `BiDi` `input` module.

use serde::Serialize;
use serde_json::Value;

use crate::{Command, EmptyResponse};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformActions<'a> {
  pub context: &'a str,
  pub actions: &'a [Value],
}

impl Command for PerformActions<'_> {
  const METHOD: &'static str = "input.performActions";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseActions<'a> {
  pub context: &'a str,
}

impl Command for ReleaseActions<'_> {
  const METHOD: &'static str = "input.releaseActions";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetFiles<'a> {
  pub context: &'a str,
  pub element: Value,
  pub files: &'a [String],
}

impl Command for SetFiles<'_> {
  const METHOD: &'static str = "input.setFiles";
  type Response = EmptyResponse;
}
