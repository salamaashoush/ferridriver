//! W3C `WebDriver` window and frame endpoints.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Command, EmptyResponse, HttpMethod};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct GetWindowHandle;

impl Command for GetWindowHandle {
  const HTTP_METHOD: HttpMethod = HttpMethod::Get;
  type Response = String;

  fn path(&self) -> Vec<String> {
    vec!["window".into()]
  }
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct GetWindowHandles;

impl Command for GetWindowHandles {
  const HTTP_METHOD: HttpMethod = HttpMethod::Get;
  type Response = Vec<String>;

  fn path(&self) -> Vec<String> {
    vec!["window".into(), "handles".into()]
  }
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct CloseWindow;

impl Command for CloseWindow {
  const HTTP_METHOD: HttpMethod = HttpMethod::Delete;
  type Response = Vec<String>;

  fn path(&self) -> Vec<String> {
    vec!["window".into()]
  }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SwitchToWindow<'a> {
  pub handle: &'a str,
}

impl Command for SwitchToWindow<'_> {
  const HTTP_METHOD: HttpMethod = HttpMethod::Post;
  type Response = EmptyResponse;

  fn path(&self) -> Vec<String> {
    vec!["window".into()]
  }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct NewWindow<'a> {
  #[serde(rename = "type")]
  pub kind: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewWindowResponse {
  pub handle: String,
  #[serde(default, rename = "type")]
  pub kind: String,
}

impl Command for NewWindow<'_> {
  const HTTP_METHOD: HttpMethod = HttpMethod::Post;
  type Response = NewWindowResponse;

  fn path(&self) -> Vec<String> {
    vec!["window".into(), "new".into()]
  }
}

#[derive(Debug, Clone, Serialize)]
pub struct SwitchToFrame {
  pub id: Value,
}

impl Command for SwitchToFrame {
  const HTTP_METHOD: HttpMethod = HttpMethod::Post;
  type Response = EmptyResponse;

  fn path(&self) -> Vec<String> {
    vec!["frame".into()]
  }
}
