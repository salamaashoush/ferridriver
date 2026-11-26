//! `WebKit` `Console` domain.

use serde::Serialize;

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Enable {}

impl Command for Enable {
  const METHOD: &'static str = "Console.enable";
  type Response = EmptyResponse;
}
