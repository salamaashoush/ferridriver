//! `WebKit` `Screencast` domain.

use serde::Serialize;

use crate::{Command, EmptyResponse};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartScreencast {
  pub quality: u8,
  pub width: u32,
  pub height: u32,
}

impl Command for StartScreencast {
  const METHOD: &'static str = "Screencast.startScreencast";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct StopScreencast {}

impl Command for StopScreencast {
  const METHOD: &'static str = "Screencast.stopScreencast";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreencastFrameAck {
  pub generation: i64,
}

impl Command for ScreencastFrameAck {
  const METHOD: &'static str = "Screencast.screencastFrameAck";
  type Response = EmptyResponse;
}
