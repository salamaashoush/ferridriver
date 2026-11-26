//! W3C `WebDriver` navigation and document-identity endpoints.

use serde::Serialize;

use crate::{Command, EmptyResponse, HttpMethod};

#[derive(Debug, Clone, Copy, Serialize)]
pub struct NavigateTo<'a> {
  pub url: &'a str,
}

impl Command for NavigateTo<'_> {
  const HTTP_METHOD: HttpMethod = HttpMethod::Post;
  type Response = EmptyResponse;

  fn path(&self) -> Vec<String> {
    vec!["url".into()]
  }
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct GetCurrentUrl;

impl Command for GetCurrentUrl {
  const HTTP_METHOD: HttpMethod = HttpMethod::Get;
  type Response = String;

  fn path(&self) -> Vec<String> {
    vec!["url".into()]
  }
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct GetTitle;

impl Command for GetTitle {
  const HTTP_METHOD: HttpMethod = HttpMethod::Get;
  type Response = String;

  fn path(&self) -> Vec<String> {
    vec!["title".into()]
  }
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Back {}

impl Command for Back {
  const HTTP_METHOD: HttpMethod = HttpMethod::Post;
  type Response = EmptyResponse;

  fn path(&self) -> Vec<String> {
    vec!["back".into()]
  }
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Forward {}

impl Command for Forward {
  const HTTP_METHOD: HttpMethod = HttpMethod::Post;
  type Response = EmptyResponse;

  fn path(&self) -> Vec<String> {
    vec!["forward".into()]
  }
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Refresh {}

impl Command for Refresh {
  const HTTP_METHOD: HttpMethod = HttpMethod::Post;
  type Response = EmptyResponse;

  fn path(&self) -> Vec<String> {
    vec!["refresh".into()]
  }
}
