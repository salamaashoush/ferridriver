//! CDP `Tracing` and `IO` domains.

use serde::{Deserialize, Serialize};

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Start<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub categories: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub transfer_mode: Option<&'a str>,
}

impl Command for Start<'_> {
  const METHOD: &'static str = "Tracing.start";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct End {}

impl Command for End {
  const METHOD: &'static str = "Tracing.end";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IoRead<'a> {
  pub handle: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub offset: Option<u64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub size: Option<u64>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IoReadResponse {
  #[serde(default)]
  pub base64_encoded: bool,
  #[serde(default)]
  pub data: String,
  #[serde(default)]
  pub eof: bool,
}

impl Command for IoRead<'_> {
  const METHOD: &'static str = "IO.read";
  type Response = IoReadResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IoClose<'a> {
  pub handle: &'a str,
}

impl Command for IoClose<'_> {
  const METHOD: &'static str = "IO.close";
  type Response = EmptyResponse;
}
