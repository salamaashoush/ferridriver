//! Auto-retrying Locator assertions matching Playwright's full LocatorAssertions API.

use ferridriver::Locator;

use super::{poll_until, Expect, MatchError, StringOrRegex};
use crate::model::TestFailure;

impl Expect<'_, Locator> {
  // ── Visibility / State ──

  /// Assert the locator is visible.
  pub async fn to_be_visible(&self) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || async move {
      let visible = locator.is_visible().await.unwrap_or(false);
      check_bool(visible, is_not, "to be visible")
    })
    .await
  }

  /// Assert the locator is hidden.
  pub async fn to_be_hidden(&self) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || async move {
      let hidden = locator.is_hidden().await.unwrap_or(true);
      check_bool(hidden, is_not, "to be hidden")
    })
    .await
  }

  /// Assert the locator is enabled.
  pub async fn to_be_enabled(&self) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || async move {
      let enabled = locator.is_enabled().await.unwrap_or(false);
      check_bool(enabled, is_not, "to be enabled")
    })
    .await
  }

  /// Assert the locator is disabled.
  pub async fn to_be_disabled(&self) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || async move {
      let disabled = locator.is_disabled().await.unwrap_or(false);
      check_bool(disabled, is_not, "to be disabled")
    })
    .await
  }

  /// Assert the locator is checked.
  pub async fn to_be_checked(&self) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || async move {
      let checked = locator.is_checked().await.unwrap_or(false);
      check_bool(checked, is_not, "to be checked")
    })
    .await
  }

  /// Assert the locator is editable.
  pub async fn to_be_editable(&self) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || async move {
      let editable = locator.is_editable().await.unwrap_or(false);
      check_bool(editable, is_not, "to be editable")
    })
    .await
  }

  /// Assert the locator is attached to the DOM.
  pub async fn to_be_attached(&self) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || async move {
      let attached = locator.is_attached().await.unwrap_or(false);
      check_bool(attached, is_not, "to be attached")
    })
    .await
  }

  /// Assert the locator is empty (no text content).
  pub async fn to_be_empty(&self) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || async move {
      let text = locator.text_content().await.unwrap_or(None).unwrap_or_default();
      let empty = text.trim().is_empty();
      if empty == is_not {
        Err(MatchError::new(format!(
          "expected element {}to be empty, got \"{text}\"",
          if is_not { "not " } else { "" }
        )))
      } else {
        Ok(())
      }
    })
    .await
  }

  /// Assert the locator is focused.
  pub async fn to_be_focused(&self) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || async move {
      let focused = locator
        .evaluate("document.activeElement === el")
        .await
        .unwrap_or(None)
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
      check_bool(focused, is_not, "to be focused")
    })
    .await
  }

  /// Assert the locator is in the viewport.
  pub async fn to_be_in_viewport(&self) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || async move {
      let in_viewport = locator
        .evaluate(
          "(function() { var r = el.getBoundingClientRect(); \
           return r.top < window.innerHeight && r.bottom > 0 && \
           r.left < window.innerWidth && r.right > 0; })()",
        )
        .await
        .unwrap_or(None)
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
      check_bool(in_viewport, is_not, "to be in viewport")
    })
    .await
  }

  // ── Text / Value ──

  /// Assert the locator's text content matches exactly.
  pub async fn to_have_text(&self, expected: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
    let expected = expected.into();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || {
      let expected = expected.clone();
      async move {
        let actual = locator.text_content().await.unwrap_or(None).unwrap_or_default();
        check_text_match(&expected, actual.trim(), is_not, "text")
      }
    })
    .await
  }

  /// Assert the locator's text contains the expected substring.
  pub async fn to_contain_text(&self, expected: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
    let expected = expected.into();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || {
      let expected = expected.clone();
      async move {
        let actual = locator.text_content().await.unwrap_or(None).unwrap_or_default();
        let matches = match &expected {
          StringOrRegex::String(s) => actual.contains(s.as_str()),
          StringOrRegex::Regex(re) => re.is_match(&actual),
        };
        if matches == is_not {
          Err(MatchError::new(format!(
            "expected text {}to contain {}\nreceived: \"{actual}\"",
            if is_not { "not " } else { "" },
            expected.description()
          )))
        } else {
          Ok(())
        }
      }
    })
    .await
  }

  /// Assert the locator's input value.
  pub async fn to_have_value(&self, expected: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
    let expected = expected.into();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || {
      let expected = expected.clone();
      async move {
        let actual = locator.input_value().await.unwrap_or_default();
        check_text_match(&expected, &actual, is_not, "value")
      }
    })
    .await
  }

  /// Assert multiple select values (multi-select elements).
  pub async fn to_have_values(&self, expected: &[impl AsRef<str>]) -> Result<(), TestFailure> {
    let expected: Vec<String> = expected.iter().map(|s| s.as_ref().to_string()).collect();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || {
      let expected = expected.clone();
      async move {
        let actual = locator
          .evaluate("Array.from(el.selectedOptions).map(function(o) { return o.value; })")
          .await
          .unwrap_or(None)
          .and_then(|v| {
            v.as_array().map(|arr| {
              arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect::<Vec<_>>()
            })
          })
          .unwrap_or_default();
        let matches = actual == expected;
        if matches == is_not {
          Err(MatchError::new(format!(
            "expected values {}{expected:?}\nreceived: {actual:?}",
            if is_not { "not " } else { "" },
          )))
        } else {
          Ok(())
        }
      }
    })
    .await
  }

  // ── Attributes ──

  /// Assert the locator has an attribute with the expected value.
  pub async fn to_have_attribute(
    &self,
    name: &str,
    value: impl Into<StringOrRegex>,
  ) -> Result<(), TestFailure> {
    let expected = value.into();
    let locator = self.subject;
    let is_not = self.is_not;
    let attr_name = name.to_string();
    poll_until(self.timeout, || {
      let expected = expected.clone();
      let attr_name = attr_name.clone();
      async move {
        let actual = locator
          .get_attribute(&attr_name)
          .await
          .unwrap_or(None)
          .unwrap_or_default();
        check_text_match(&expected, &actual, is_not, &format!("attribute \"{attr_name}\""))
      }
    })
    .await
  }

  /// Assert the locator has the expected CSS class (exact match on class attribute).
  pub async fn to_have_class(&self, expected: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
    let expected = expected.into();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || {
      let expected = expected.clone();
      async move {
        let actual = locator.get_attribute("class").await.unwrap_or(None).unwrap_or_default();
        check_text_match(&expected, &actual, is_not, "class")
      }
    })
    .await
  }

  /// Assert the locator's class list contains the expected class name.
  pub async fn to_contain_class(&self, expected: &str) -> Result<(), TestFailure> {
    let expected = expected.to_string();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || {
      let expected = expected.clone();
      async move {
        let class_attr = locator.get_attribute("class").await.unwrap_or(None).unwrap_or_default();
        let classes: Vec<&str> = class_attr.split_whitespace().collect();
        let contains = classes.iter().any(|c| *c == expected);
        if contains == is_not {
          Err(MatchError::new(format!(
            "expected class list {}to contain \"{expected}\"\nreceived: \"{class_attr}\"",
            if is_not { "not " } else { "" },
          )))
        } else {
          Ok(())
        }
      }
    })
    .await
  }

  /// Assert the locator has the expected CSS property value.
  pub async fn to_have_css(
    &self,
    property: &str,
    value: impl Into<StringOrRegex>,
  ) -> Result<(), TestFailure> {
    let expected = value.into();
    let locator = self.subject;
    let is_not = self.is_not;
    let prop = property.to_string();
    poll_until(self.timeout, || {
      let expected = expected.clone();
      let prop = prop.clone();
      async move {
        let js = format!(
          "window.getComputedStyle(el).getPropertyValue('{}')",
          prop.replace('\'', "\\'")
        );
        let actual = locator
          .evaluate(&js)
          .await
          .unwrap_or(None)
          .and_then(|v| v.as_str().map(String::from))
          .unwrap_or_default();
        check_text_match(&expected, &actual, is_not, &format!("CSS \"{prop}\""))
      }
    })
    .await
  }

  /// Assert the locator has the expected id.
  pub async fn to_have_id(&self, expected: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
    self.to_have_attribute("id", expected).await
  }

  /// Assert the locator has the expected ARIA role.
  pub async fn to_have_role(&self, expected: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
    let expected = expected.into();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || {
      let expected = expected.clone();
      async move {
        let actual = locator
          .evaluate("el.getAttribute('role') || el.tagName.toLowerCase()")
          .await
          .unwrap_or(None)
          .and_then(|v| v.as_str().map(String::from))
          .unwrap_or_default();
        check_text_match(&expected, &actual, is_not, "role")
      }
    })
    .await
  }

  /// Assert the locator has the expected accessible name.
  pub async fn to_have_accessible_name(&self, expected: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
    let expected = expected.into();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || {
      let expected = expected.clone();
      async move {
        let actual = locator
          .evaluate(
            "(function() { \
              var label = el.getAttribute('aria-label') || \
                (el.getAttribute('aria-labelledby') ? \
                  (document.getElementById(el.getAttribute('aria-labelledby')) || {}).textContent : null) || \
                (el.labels && el.labels[0] ? el.labels[0].textContent : null) || ''; \
              return label.trim(); \
            })()",
          )
          .await
          .unwrap_or(None)
          .and_then(|v| v.as_str().map(String::from))
          .unwrap_or_default();
        check_text_match(&expected, &actual, is_not, "accessible name")
      }
    })
    .await
  }

  /// Assert the locator has the expected accessible description.
  pub async fn to_have_accessible_description(
    &self,
    expected: impl Into<StringOrRegex>,
  ) -> Result<(), TestFailure> {
    let expected = expected.into();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || {
      let expected = expected.clone();
      async move {
        let actual = locator
          .evaluate(
            "(function() { \
              var desc = el.getAttribute('aria-description') || \
                (el.getAttribute('aria-describedby') ? \
                  (document.getElementById(el.getAttribute('aria-describedby')) || {}).textContent : null) || ''; \
              return desc.trim(); \
            })()",
          )
          .await
          .unwrap_or(None)
          .and_then(|v| v.as_str().map(String::from))
          .unwrap_or_default();
        check_text_match(&expected, &actual, is_not, "accessible description")
      }
    })
    .await
  }

  /// Assert the locator has a JS property with the expected value.
  pub async fn to_have_js_property(
    &self,
    name: &str,
    value: serde_json::Value,
  ) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    let prop_name = name.to_string();
    poll_until(self.timeout, || {
      let prop_name = prop_name.clone();
      let expected = value.clone();
      async move {
        let js = format!(
          "JSON.stringify(el['{}'])",
          prop_name.replace('\'', "\\'")
        );
        let actual = locator
          .evaluate(&js)
          .await
          .unwrap_or(None)
          .and_then(|v| v.as_str().and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()))
          .unwrap_or(serde_json::Value::Null);
        let matches = actual == expected;
        if matches == is_not {
          Err(MatchError::new(format!(
            "expected JS property \"{prop_name}\" {}{expected}\nreceived: {actual}",
            if is_not { "not " } else { "" },
          )))
        } else {
          Ok(())
        }
      }
    })
    .await
  }

  // ── Count ──

  /// Assert the number of elements matching the locator.
  pub async fn to_have_count(&self, expected: usize) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || async move {
      let actual = locator.count().await.unwrap_or(0);
      let matches = actual == expected;
      if matches == is_not {
        Err(MatchError::new(format!(
          "expected count {}{expected}\nreceived: {actual}",
          if is_not { "not " } else { "" },
        )))
      } else {
        Ok(())
      }
    })
    .await
  }
}

// ── Helpers ──

fn check_bool(actual: bool, is_not: bool, description: &str) -> Result<(), MatchError> {
  if actual == is_not {
    Err(MatchError::new(format!(
      "expected element {}{description}",
      if is_not { "not " } else { "" }
    )))
  } else {
    Ok(())
  }
}

fn check_text_match(
  expected: &StringOrRegex,
  actual: &str,
  is_not: bool,
  label: &str,
) -> Result<(), MatchError> {
  let matches = expected.matches(actual);
  if matches == is_not {
    Err(
      MatchError::new(format!(
        "expected {label} {}{}\nreceived: \"{actual}\"",
        if is_not { "not " } else { "" },
        expected.description()
      ))
      .with_diff(format!(
        "- expected: {}\n+ received: \"{actual}\"",
        expected.description()
      )),
    )
  } else {
    Ok(())
  }
}
