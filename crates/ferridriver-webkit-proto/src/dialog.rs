//! `WebKit` `Dialog` domain.

use serde::Serialize;

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Enable {}

impl Command for Enable {
  const METHOD: &'static str = "Dialog.enable";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HandleJavaScriptDialog<'a> {
  pub accept: bool,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub prompt_text: Option<&'a str>,
}

impl Command for HandleJavaScriptDialog<'_> {
  const METHOD: &'static str = "Dialog.handleJavaScriptDialog";
  type Response = EmptyResponse;
}
