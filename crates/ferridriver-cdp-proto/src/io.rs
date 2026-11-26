//! CDP `IO` domain (streaming blobs, traces, and native screen recordings).

use serde::{Deserialize, Serialize};

use crate::{Command, EmptyResponse};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Read<'a> {
  pub handle: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub offset: Option<u64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub size: Option<u64>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadResponse {
  #[serde(default)]
  pub base64_encoded: bool,
  #[serde(default)]
  pub data: String,
  #[serde(default)]
  pub eof: bool,
}

impl Command for Read<'_> {
  const METHOD: &'static str = "IO.read";
  type Response = ReadResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Close<'a> {
  pub handle: &'a str,
}

impl Command for Close<'_> {
  const METHOD: &'static str = "IO.close";
  type Response = EmptyResponse;
}
