//! W3C `WebDriver` document, script execution, and capture endpoints.

use serde::Serialize;
use serde_json::Value;

use crate::{Command, HttpMethod};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct GetPageSource;

impl Command for GetPageSource {
  const HTTP_METHOD: HttpMethod = HttpMethod::Get;
  type Response = String;

  fn path(&self) -> Vec<String> {
    vec!["source".into()]
  }
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct TakeScreenshot;

impl Command for TakeScreenshot {
  const HTTP_METHOD: HttpMethod = HttpMethod::Get;
  type Response = String;

  fn path(&self) -> Vec<String> {
    vec!["screenshot".into()]
  }
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecuteSync<'a> {
  pub script: &'a str,
  pub args: &'a [Value],
}

impl Command for ExecuteSync<'_> {
  const HTTP_METHOD: HttpMethod = HttpMethod::Post;
  type Response = Value;

  fn path(&self) -> Vec<String> {
    vec!["execute".into(), "sync".into()]
  }
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecuteAsync<'a> {
  pub script: &'a str,
  pub args: &'a [Value],
}

impl Command for ExecuteAsync<'_> {
  const HTTP_METHOD: HttpMethod = HttpMethod::Post;
  type Response = Value;

  fn path(&self) -> Vec<String> {
    vec!["execute".into(), "async".into()]
  }
}
