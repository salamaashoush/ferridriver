//! Synchronous `toThrow` matcher (Jest's `expect(fn).toThrow(...)`).
//!
//! The QuickJS binding invokes the user function in try/catch and
//! constructs the [`ThrownError`] (or `None`) before delegating here —
//! this module does not own any JS invocation, only the matching logic.

use std::panic::Location;

use regex::Regex;
use serde_json::Value;

use crate::asymmetric::json_short;
use crate::{AssertionFailure, CallerLocation};

/// Captured outcome of invoking a function expected to throw.
#[derive(Debug, Clone, Default)]
pub struct ThrownError {
  /// The error message (`error.message` on a JS `Error`, or the
  /// stringification of a thrown primitive).
  pub message: String,
  /// `error.constructor.name` from the JS side.
  pub class_name: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ThrowMatcher {
  Any,
  Substring(String),
  Regex(Regex),
  ClassName(String),
  /// Match against `{ message?, name? }`.
  Object(Value),
}

pub struct ExpectFn {
  caught: Option<ThrownError>,
  is_not: bool,
  is_soft: bool,
  message: Option<String>,
}

#[must_use]
pub fn expect_fn(caught: Option<ThrownError>) -> ExpectFn {
  ExpectFn {
    caught,
    is_not: false,
    is_soft: false,
    message: None,
  }
}

impl ExpectFn {
  #[must_use]
  pub fn not(mut self) -> Self {
    self.is_not = !self.is_not;
    self
  }

  #[must_use]
  pub fn soft(mut self) -> Self {
    self.is_soft = true;
    self
  }

  #[must_use]
  pub fn with_message(mut self, message: impl Into<String>) -> Self {
    self.message = Some(message.into());
    self
  }

  pub fn is_soft(&self) -> bool {
    self.is_soft
  }

  #[track_caller]
  pub fn to_throw(&self, matcher: Option<&ThrowMatcher>) -> Result<(), AssertionFailure> {
    let location = Location::caller();
    let pass = match (&self.caught, matcher) {
      (None, _) => false,
      (Some(_), None) => true,
      (Some(err), Some(ThrowMatcher::Any)) => !err.message.is_empty() || err.class_name.is_some(),
      (Some(err), Some(ThrowMatcher::Substring(s))) => err.message.contains(s.as_str()),
      (Some(err), Some(ThrowMatcher::Regex(re))) => re.is_match(&err.message),
      (Some(err), Some(ThrowMatcher::ClassName(name))) => {
        err.class_name.as_deref() == Some(name.as_str()) || err.message.contains(name.as_str())
      },
      (Some(err), Some(ThrowMatcher::Object(subset))) => {
        if let Some(obj) = subset.as_object() {
          let mut ok = true;
          if let Some(Value::String(msg_expected)) = obj.get("message") {
            ok &= err.message.contains(msg_expected.as_str());
          }
          if let Some(Value::String(name_expected)) = obj.get("name") {
            ok &= err.class_name.as_deref() == Some(name_expected.as_str());
          }
          ok
        } else {
          false
        }
      },
    };
    let pass = if self.is_not { !pass } else { pass };
    if pass {
      return Ok(());
    }
    let expected_desc = match matcher {
      None | Some(ThrowMatcher::Any) => "function to throw".to_string(),
      Some(ThrowMatcher::Substring(s)) => format!("throw containing {s:?}"),
      Some(ThrowMatcher::Regex(re)) => format!("throw matching /{}/", re.as_str()),
      Some(ThrowMatcher::ClassName(n)) => format!("throw of {n}"),
      Some(ThrowMatcher::Object(o)) => format!("throw matching {}", json_short(o)),
    };
    let received_desc = match &self.caught {
      None => "function returned without throwing".to_string(),
      Some(err) => match &err.class_name {
        Some(n) => format!("{n}: {}", err.message),
        None => err.message.clone(),
      },
    };
    let not = if self.is_not { ".not" } else { "" };
    let prefix = self.message.as_ref().map(|m| format!("{m}: ")).unwrap_or_default();
    let title = format!("{prefix}expect(fn){not}.toThrow() failed");
    let body = format!("Expected: {expected_desc}\nReceived: {received_desc}");
    Err(AssertionFailure::new(title, Some(body)).with_location(CallerLocation::from_std(location)))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn err(r: Result<(), AssertionFailure>) {
    assert!(r.is_err(), "expected err");
  }

  #[test]
  fn to_throw_substring_and_classname() {
    let caught = Some(ThrownError {
      message: "boom: out of range".into(),
      class_name: Some("RangeError".into()),
    });
    expect_fn(caught.clone())
      .to_throw(Some(&ThrowMatcher::Substring("out of range".into())))
      .unwrap();
    err(expect_fn(caught.clone()).to_throw(Some(&ThrowMatcher::Substring("nope".into()))));
    expect_fn(caught.clone())
      .to_throw(Some(&ThrowMatcher::ClassName("RangeError".into())))
      .unwrap();
    expect_fn(caught).to_throw(None).unwrap();
    err(expect_fn(None).to_throw(None));
  }

  #[test]
  fn not_inverts_throw() {
    err(
      expect_fn(Some(ThrownError {
        message: "boom".into(),
        class_name: None,
      }))
      .not()
      .to_throw(None),
    );
    expect_fn(None).not().to_throw(None).unwrap();
  }
}
