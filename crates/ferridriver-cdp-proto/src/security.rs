//! CDP `Security` domain.

use serde::Serialize;

use crate::{Command, EmptyResponse};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetIgnoreCertificateErrors {
  pub ignore: bool,
}

impl Command for SetIgnoreCertificateErrors {
  const METHOD: &'static str = "Security.setIgnoreCertificateErrors";
  type Response = EmptyResponse;
}
