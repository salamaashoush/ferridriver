//! W3C `WebDriver` session-level endpoints (timeouts, Appium context switch).

use serde::Serialize;

use crate::{Command, EmptyResponse, HttpMethod};

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SetScriptTimeout {
  pub script: Option<u64>,
}

impl Command for SetScriptTimeout {
  const HTTP_METHOD: HttpMethod = HttpMethod::Post;
  type Response = EmptyResponse;

  fn path(&self) -> Vec<String> {
    vec!["timeouts".into()]
  }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SwitchContext<'a> {
  pub name: &'a str,
}

impl Command for SwitchContext<'_> {
  const HTTP_METHOD: HttpMethod = HttpMethod::Post;
  type Response = EmptyResponse;

  fn path(&self) -> Vec<String> {
    vec!["context".into()]
  }
}
