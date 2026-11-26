//! W3C `WebDriver` cookie endpoints.

use serde::Serialize;
use serde_json::Value;

use crate::{Command, EmptyResponse, HttpMethod};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct GetAllCookies;

impl Command for GetAllCookies {
  const HTTP_METHOD: HttpMethod = HttpMethod::Get;
  type Response = Vec<Value>;

  fn path(&self) -> Vec<String> {
    vec!["cookie".into()]
  }
}

#[derive(Debug, Clone, Serialize)]
pub struct AddCookie<'a> {
  pub cookie: &'a Value,
}

impl Command for AddCookie<'_> {
  const HTTP_METHOD: HttpMethod = HttpMethod::Post;
  type Response = EmptyResponse;

  fn path(&self) -> Vec<String> {
    vec!["cookie".into()]
  }
}
