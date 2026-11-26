//! CDP `WebMCP` domain.

use serde::Serialize;

use crate::{Command, EmptyResponse};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelInvocation<'a> {
  pub invocation_id: &'a str,
}

impl Command for CancelInvocation<'_> {
  const METHOD: &'static str = "WebMCP.cancelInvocation";
  type Response = EmptyResponse;
}
