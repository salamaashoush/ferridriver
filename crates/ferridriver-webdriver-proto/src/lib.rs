//! Strongly typed W3C Classic `WebDriver` HTTP protocol commands and responses.
//!
//! Mirrors the W3C `WebDriver` Level 2 specification
//! (<https://www.w3.org/TR/webdriver2/>). Each command implements
//! [`Command`] with its HTTP method, relative session path segments, JSON
//! body serialization, and typed response payload.

use serde::Serialize;
use serde::de::DeserializeOwned;

pub mod cookies;
pub mod document;
pub mod element;
pub mod navigation;
pub mod session;
pub mod window;

/// HTTP verb used by a Classic `WebDriver` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
  Get,
  Post,
  Delete,
}

impl HttpMethod {
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Get => "GET",
      Self::Post => "POST",
      Self::Delete => "DELETE",
    }
  }
}

/// A strongly typed W3C Classic `WebDriver` command relative to `/session/{id}`.
pub trait Command: Serialize {
  /// HTTP method (`GET`, `POST`, or `DELETE`).
  const HTTP_METHOD: HttpMethod;
  /// Typed response decoded from the W3C `{ "value": ... }` wrapper.
  type Response: DeserializeOwned;

  /// Relative path segments appended to `/session/{id}` (e.g. `["title"]` or
  /// `["element", id, "click"]`).
  fn path(&self) -> Vec<String>;

  /// Optional JSON request body; `None` for `GET` and `DELETE` endpoints.
  ///
  /// # Errors
  ///
  /// Returns a serialization error if the command cannot be encoded as JSON.
  fn body(&self) -> Result<Option<serde_json::Value>, serde_json::Error> {
    match Self::HTTP_METHOD {
      HttpMethod::Post => Ok(Some(serde_json::to_value(self)?)),
      HttpMethod::Get | HttpMethod::Delete => Ok(None),
    }
  }
}

/// Placeholder response for `WebDriver` commands whose `"value"` is `null` or
/// ignored by the caller.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EmptyResponse;

impl<'de> serde::Deserialize<'de> for EmptyResponse {
  fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    let _ = serde::de::IgnoredAny::deserialize(deserializer)?;
    Ok(Self)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn commands_expose_w3c_paths_methods_and_bodies() {
    let nav = navigation::NavigateTo {
      url: "https://example.test/",
    };
    assert_eq!(navigation::NavigateTo::HTTP_METHOD, HttpMethod::Post);
    assert_eq!(nav.path(), vec!["url"]);
    assert_eq!(
      nav.body().unwrap(),
      Some(serde_json::json!({ "url": "https://example.test/" }))
    );

    let title = navigation::GetTitle;
    assert_eq!(navigation::GetTitle::HTTP_METHOD, HttpMethod::Get);
    assert_eq!(title.path(), vec!["title"]);
    assert_eq!(title.body().unwrap(), None);

    let click = element::Click { element_id: "el-1" };
    assert_eq!(click.path(), vec!["element", "el-1", "click"]);
    assert_eq!(click.body().unwrap(), Some(serde_json::json!({})));

    let keys = element::SendKeys {
      element_id: "el-1",
      text: "hello",
    };
    assert_eq!(keys.path(), vec!["element", "el-1", "value"]);
    assert_eq!(keys.body().unwrap(), Some(serde_json::json!({ "text": "hello" })));

    let empty: EmptyResponse = serde_json::from_value(serde_json::Value::Null).unwrap();
    assert_eq!(empty, EmptyResponse);
    let empty_obj: EmptyResponse = serde_json::from_value(serde_json::json!({"ignored": 1})).unwrap();
    assert_eq!(empty_obj, EmptyResponse);
  }
}
