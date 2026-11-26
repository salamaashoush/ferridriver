//! `WebKit` `Worker` domain.

use serde::Serialize;

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Enable {}

impl Command for Enable {
  const METHOD: &'static str = "Worker.enable";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageToWorker<'a> {
  pub worker_id: &'a str,
  pub message: &'a str,
}

impl Command for SendMessageToWorker<'_> {
  const METHOD: &'static str = "Worker.sendMessageToWorker";
  type Response = EmptyResponse;
}
