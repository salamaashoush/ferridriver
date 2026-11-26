//! W3C `WebDriver` element interaction endpoints.

use serde::Serialize;

use crate::{Command, EmptyResponse, HttpMethod};

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Click<'a> {
  #[serde(skip)]
  pub element_id: &'a str,
}

impl Command for Click<'_> {
  const HTTP_METHOD: HttpMethod = HttpMethod::Post;
  type Response = EmptyResponse;

  fn path(&self) -> Vec<String> {
    vec!["element".into(), self.element_id.into(), "click".into()]
  }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Clear<'a> {
  #[serde(skip)]
  pub element_id: &'a str,
}

impl Command for Clear<'_> {
  const HTTP_METHOD: HttpMethod = HttpMethod::Post;
  type Response = EmptyResponse;

  fn path(&self) -> Vec<String> {
    vec!["element".into(), self.element_id.into(), "clear".into()]
  }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SendKeys<'a> {
  #[serde(skip)]
  pub element_id: &'a str,
  pub text: &'a str,
}

impl Command for SendKeys<'_> {
  const HTTP_METHOD: HttpMethod = HttpMethod::Post;
  type Response = EmptyResponse;

  fn path(&self) -> Vec<String> {
    vec!["element".into(), self.element_id.into(), "value".into()]
  }
}
