//! Locator web-first matchers — single source of truth shared by the
//! test runner (`ferridriver-test`) and the QuickJS binding
//! (`ferridriver-script`). Screenshot / aria-snapshot / value-snapshot
//! matchers stay in `ferridriver-test` because they pull in image and
//! aria-YAML infrastructure that does not belong in this lightweight
//! crate.

use ferridriver::Locator;

use crate::AssertionFailure;
use crate::builder::{Expect, HaveCssOptions, InViewportOptions};
use crate::poll::{ExpectContext, MatchError, poll_until};
use crate::value::StringOrRegex;

fn locator_ctx(locator: &Locator, method: &'static str, is_not: bool) -> ExpectContext {
  ExpectContext {
    method,
    subject: format!("locator('{}')", locator.selector()),
    is_not,
  }
}

pub fn check_bool(actual: bool, is_not: bool, expected_state: &str) -> Result<(), MatchError> {
  if actual == is_not {
    let expected = format!("{}{expected_state}", if is_not { "not " } else { "" });
    let received = format!("{}{expected_state}", if actual { "" } else { "not " });
    Err(MatchError::new(expected, received))
  } else {
    Ok(())
  }
}

pub fn check_text_match(
  expected: &StringOrRegex,
  actual: &str,
  is_not: bool,
  _label: &str,
) -> Result<(), MatchError> {
  let matches = expected.matches(actual);
  if matches == is_not {
    let exp = format!("{}{}", if is_not { "not " } else { "" }, expected.description());
    Err(MatchError::new(exp, format!("\"{actual}\"")))
  } else {
    Ok(())
  }
}

impl Expect<'_, Locator> {
  // ── Visibility / State ──

  pub async fn to_be_visible(&self) -> Result<(), AssertionFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, locator_ctx(locator, "toBeVisible", is_not), || async move {
      let visible = locator.is_visible().await.unwrap_or(false);
      check_bool(visible, is_not, "visible")
    })
    .await
  }

  pub async fn to_be_hidden(&self) -> Result<(), AssertionFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, locator_ctx(locator, "toBeHidden", is_not), || async move {
      let hidden = locator.is_hidden().await.unwrap_or(true);
      check_bool(hidden, is_not, "to be hidden")
    })
    .await
  }

  pub async fn to_be_enabled(&self) -> Result<(), AssertionFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, locator_ctx(locator, "toBeEnabled", is_not), || async move {
      let enabled = locator.is_enabled().await.unwrap_or(false);
      check_bool(enabled, is_not, "to be enabled")
    })
    .await
  }

  pub async fn to_be_disabled(&self) -> Result<(), AssertionFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, locator_ctx(locator, "toBeDisabled", is_not), || async move {
      let disabled = locator.is_disabled().await.unwrap_or(false);
      check_bool(disabled, is_not, "to be disabled")
    })
    .await
  }

  pub async fn to_be_checked(&self) -> Result<(), AssertionFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, locator_ctx(locator, "toBeChecked", is_not), || async move {
      let checked = locator.is_checked().await.unwrap_or(false);
      check_bool(checked, is_not, "to be checked")
    })
    .await
  }

  pub async fn to_be_editable(&self) -> Result<(), AssertionFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, locator_ctx(locator, "toBeEditable", is_not), || async move {
      let editable = locator.is_editable().await.unwrap_or(false);
      check_bool(editable, is_not, "to be editable")
    })
    .await
  }

  pub async fn to_be_attached(&self) -> Result<(), AssertionFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, locator_ctx(locator, "toBeAttached", is_not), || async move {
      let attached = locator.is_attached().await.unwrap_or(false);
      check_bool(attached, is_not, "to be attached")
    })
    .await
  }

  pub async fn to_be_empty(&self) -> Result<(), AssertionFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, locator_ctx(locator, "toBeEmpty", is_not), || async move {
      let text = locator.text_content().await.unwrap_or(None).unwrap_or_default();
      let empty = text.trim().is_empty();
      if empty == is_not {
        Err(MatchError::new(
          format!("{}empty", if is_not { "not " } else { "" }),
          format!("\"{}\"", text.trim()),
        ))
      } else {
        Ok(())
      }
    })
    .await
  }

  pub async fn to_be_focused(&self) -> Result<(), AssertionFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, locator_ctx(locator, "toBeFocused", is_not), || async move {
      let focused = locator
        .evaluate(
          "el => document.activeElement === el",
          ferridriver::protocol::SerializedArgument::default(),
          None,
          None,
        )
        .await
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
      check_bool(focused, is_not, "to be focused")
    })
    .await
  }

  pub async fn to_be_in_viewport(&self) -> Result<(), AssertionFailure> {
    self.to_be_in_viewport_with(InViewportOptions::default()).await
  }

  pub async fn to_be_in_viewport_with(&self, options: InViewportOptions) -> Result<(), AssertionFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    let ratio = options.ratio.unwrap_or(0.0).clamp(0.0, 1.0);
    poll_until(self.timeout, locator_ctx(locator, "toBeInViewport", is_not), || async move {
      let js = format!(
        "el => {{ var r = el.getBoundingClientRect(); \
         if (r.width === 0 || r.height === 0) return false; \
         var iw = window.innerWidth, ih = window.innerHeight; \
         var visW = Math.max(0, Math.min(r.right, iw) - Math.max(r.left, 0)); \
         var visH = Math.max(0, Math.min(r.bottom, ih) - Math.max(r.top, 0)); \
         var inter = visW * visH; var area = r.width * r.height; \
         if (inter <= 0) return false; \
         return inter / area >= {ratio:.6}; }}"
      );
      let in_viewport = locator
        .evaluate(&js, ferridriver::protocol::SerializedArgument::default(), None, None)
        .await
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
      check_bool(in_viewport, is_not, "to be in viewport")
    })
    .await
  }

  // ── Text / Value ──

  pub async fn to_have_text(&self, expected: impl Into<StringOrRegex>) -> Result<(), AssertionFailure> {
    let expected = expected.into();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, locator_ctx(locator, "toHaveText", is_not), || {
      let expected = expected.clone();
      async move {
        let actual = locator.text_content().await.unwrap_or(None).unwrap_or_default();
        check_text_match(&expected, actual.trim(), is_not, "text")
      }
    })
    .await
  }

  pub async fn to_contain_text(&self, expected: impl Into<StringOrRegex>) -> Result<(), AssertionFailure> {
    let expected = expected.into();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, locator_ctx(locator, "toContainText", is_not), || {
      let expected = expected.clone();
      async move {
        let actual = locator.text_content().await.unwrap_or(None).unwrap_or_default();
        let matches = match &expected {
          StringOrRegex::String(s) => actual.contains(s.as_str()),
          StringOrRegex::Regex(re) => re.is_match(&actual),
        };
        if matches == is_not {
          Err(MatchError::new(
            format!(
              "{}containing {}",
              if is_not { "not " } else { "" },
              expected.description()
            ),
            format!("\"{actual}\""),
          ))
        } else {
          Ok(())
        }
      }
    })
    .await
  }

  pub async fn to_have_value(&self, expected: impl Into<StringOrRegex>) -> Result<(), AssertionFailure> {
    let expected = expected.into();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, locator_ctx(locator, "toHaveValue", is_not), || {
      let expected = expected.clone();
      async move {
        let actual = locator.input_value().await.unwrap_or_default();
        check_text_match(&expected, &actual, is_not, "value")
      }
    })
    .await
  }

  pub async fn to_have_values(&self, expected: &[impl AsRef<str>]) -> Result<(), AssertionFailure> {
    let expected: Vec<String> = expected.iter().map(|s| s.as_ref().to_string()).collect();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, locator_ctx(locator, "toHaveValues", is_not), || {
      let expected = expected.clone();
      async move {
        let actual = locator
          .evaluate(
            "el => Array.from(el.selectedOptions).map(function(o) { return o.value; })",
            ferridriver::protocol::SerializedArgument::default(),
            None,
            None,
          )
          .await
          .ok()
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
          Err(MatchError::new(
            format!("{}{expected:?}", if is_not { "not " } else { "" }),
            format!("{actual:?}"),
          ))
        } else {
          Ok(())
        }
      }
    })
    .await
  }

  // ── Attributes ──

  pub async fn to_have_attribute(
    &self,
    name: &str,
    value: impl Into<StringOrRegex>,
  ) -> Result<(), AssertionFailure> {
    let expected = value.into();
    let locator = self.subject;
    let is_not = self.is_not;
    let attr_name = name.to_string();
    poll_until(self.timeout, locator_ctx(locator, "toHaveAttribute", is_not), || {
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

  pub async fn to_have_attribute_exists(&self, name: &str) -> Result<(), AssertionFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    let attr_name = name.to_string();
    poll_until(self.timeout, locator_ctx(locator, "toHaveAttribute", is_not), || {
      let attr_name = attr_name.clone();
      async move {
        let present = locator.get_attribute(&attr_name).await.unwrap_or(None).is_some();
        if present == is_not {
          Err(MatchError::new(
            format!(
              "{}attribute \"{attr_name}\" to be present",
              if is_not { "not " } else { "" }
            ),
            (if present { "present" } else { "missing" }).to_string(),
          ))
        } else {
          Ok(())
        }
      }
    })
    .await
  }

  pub async fn to_have_class(&self, expected: impl Into<StringOrRegex>) -> Result<(), AssertionFailure> {
    let expected = expected.into();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, locator_ctx(locator, "toHaveClass", is_not), || {
      let expected = expected.clone();
      async move {
        let actual = locator.get_attribute("class").await.unwrap_or(None).unwrap_or_default();
        check_text_match(&expected, &actual, is_not, "class")
      }
    })
    .await
  }

  pub async fn to_contain_class(&self, expected: &str) -> Result<(), AssertionFailure> {
    let expected = expected.to_string();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, locator_ctx(locator, "toContainClass", is_not), || {
      let expected = expected.clone();
      async move {
        let class_attr = locator.get_attribute("class").await.unwrap_or(None).unwrap_or_default();
        let classes: Vec<&str> = class_attr.split_whitespace().collect();
        let contains = classes.iter().any(|c| *c == expected);
        if contains == is_not {
          Err(MatchError::new(
            format!("{}containing class \"{expected}\"", if is_not { "not " } else { "" }),
            format!("\"{class_attr}\""),
          ))
        } else {
          Ok(())
        }
      }
    })
    .await
  }

  pub async fn to_have_css(
    &self,
    property: &str,
    value: impl Into<StringOrRegex>,
  ) -> Result<(), AssertionFailure> {
    self.to_have_css_with(property, value, HaveCssOptions::default()).await
  }

  pub async fn to_have_css_with(
    &self,
    property: &str,
    value: impl Into<StringOrRegex>,
    options: HaveCssOptions,
  ) -> Result<(), AssertionFailure> {
    let expected = value.into();
    let locator = self.subject;
    let is_not = self.is_not;
    let prop = property.to_string();
    let pseudo = options.pseudo.clone();
    poll_until(self.timeout, locator_ctx(locator, "toHaveCSS", is_not), || {
      let expected = expected.clone();
      let prop = prop.clone();
      let pseudo = pseudo.clone();
      async move {
        let pseudo_arg = pseudo
          .as_deref()
          .map(|p| format!(", '{}'", p.replace('\'', "\\'")))
          .unwrap_or_default();
        let js = format!(
          "el => window.getComputedStyle(el{pseudo_arg}).getPropertyValue('{}')",
          prop.replace('\'', "\\'")
        );
        let actual = locator
          .evaluate(&js, ferridriver::protocol::SerializedArgument::default(), None, None)
          .await
          .ok()
          .and_then(|v| v.as_str().map(String::from))
          .unwrap_or_default();
        check_text_match(&expected, &actual, is_not, &format!("CSS \"{prop}\""))
      }
    })
    .await
  }

  pub async fn to_have_id(&self, expected: impl Into<StringOrRegex>) -> Result<(), AssertionFailure> {
    self.to_have_attribute("id", expected).await
  }

  pub async fn to_have_role(&self, expected: impl Into<StringOrRegex>) -> Result<(), AssertionFailure> {
    let expected = expected.into();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, locator_ctx(locator, "toHaveRole", is_not), || {
      let expected = expected.clone();
      async move {
        let actual = locator
          .evaluate(
            "el => el.getAttribute('role') || el.tagName.toLowerCase()",
            ferridriver::protocol::SerializedArgument::default(),
            None,
            None,
          )
          .await
          .ok()
          .and_then(|v| v.as_str().map(String::from))
          .unwrap_or_default();
        check_text_match(&expected, &actual, is_not, "role")
      }
    })
    .await
  }

  pub async fn to_have_accessible_name(&self, expected: impl Into<StringOrRegex>) -> Result<(), AssertionFailure> {
    let expected = expected.into();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(
      self.timeout,
      locator_ctx(locator, "toHaveAccessibleName", is_not),
      || {
        let expected = expected.clone();
        async move {
          let actual = locator
            .evaluate(
              "el => { \
              var label = el.getAttribute('aria-label') || \
                (el.getAttribute('aria-labelledby') ? \
                  (document.getElementById(el.getAttribute('aria-labelledby')) || {}).textContent : null) || \
                (el.labels && el.labels[0] ? el.labels[0].textContent : null) || ''; \
              return label.trim(); \
            }",
              ferridriver::protocol::SerializedArgument::default(),
              None,
              None,
            )
            .await
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
          check_text_match(&expected, &actual, is_not, "accessible name")
        }
      },
    )
    .await
  }

  pub async fn to_have_accessible_description(
    &self,
    expected: impl Into<StringOrRegex>,
  ) -> Result<(), AssertionFailure> {
    let expected = expected.into();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(
      self.timeout,
      locator_ctx(locator, "toHaveAccessibleDescription", is_not),
      || {
        let expected = expected.clone();
        async move {
          let actual = locator
            .evaluate(
              "el => { \
              var desc = el.getAttribute('aria-description') || \
                (el.getAttribute('aria-describedby') ? \
                  (document.getElementById(el.getAttribute('aria-describedby')) || {}).textContent : null) || ''; \
              return desc.trim(); \
            }",
              ferridriver::protocol::SerializedArgument::default(),
              None,
              None,
            )
            .await
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
          check_text_match(&expected, &actual, is_not, "accessible description")
        }
      },
    )
    .await
  }

  pub async fn to_have_accessible_error_message(
    &self,
    expected: impl Into<StringOrRegex>,
  ) -> Result<(), AssertionFailure> {
    let expected = expected.into();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(
      self.timeout,
      locator_ctx(locator, "toHaveAccessibleErrorMessage", is_not),
      || {
        let expected = expected.clone();
        async move {
          let actual = locator
            .evaluate(
              "el => { \
              var errId = el.getAttribute('aria-errormessage'); \
              if (errId) { \
                var errEl = document.getElementById(errId); \
                return errEl ? errEl.textContent.trim() : ''; \
              } \
              return el.validationMessage || ''; \
            }",
              ferridriver::protocol::SerializedArgument::default(),
              None,
              None,
            )
            .await
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
          check_text_match(&expected, &actual, is_not, "accessible error message")
        }
      },
    )
    .await
  }

  pub async fn to_have_js_property(
    &self,
    name: &str,
    value: serde_json::Value,
  ) -> Result<(), AssertionFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    let prop_name = name.to_string();
    poll_until(self.timeout, locator_ctx(locator, "toHaveJSProperty", is_not), || {
      let prop_name = prop_name.clone();
      let expected = value.clone();
      async move {
        let js = format!("el => JSON.stringify(el['{}'])", prop_name.replace('\'', "\\'"));
        let actual = locator
          .evaluate(&js, ferridriver::protocol::SerializedArgument::default(), None, None)
          .await
          .ok()
          .and_then(|v| {
            v.as_str()
              .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
          })
          .unwrap_or(serde_json::Value::Null);
        let matches = actual == expected;
        if matches == is_not {
          Err(MatchError::new(
            format!("{}{expected}", if is_not { "not " } else { "" }),
            format!("{actual}"),
          ))
        } else {
          Ok(())
        }
      }
    })
    .await
  }

  // ── Array text matchers ──

  pub async fn to_have_texts(
    &self,
    expected: &[impl Into<StringOrRegex> + Clone],
  ) -> Result<(), AssertionFailure> {
    let expected: Vec<StringOrRegex> = expected.iter().map(|e| e.clone().into()).collect();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, locator_ctx(locator, "toHaveTexts", is_not), || {
      let expected = expected.clone();
      async move {
        let count = locator.count().await.unwrap_or(0);
        let mut actuals = Vec::with_capacity(count);
        for i in 0..count {
          let text = locator
            .evaluate(
              &format!(
                "() => document.querySelectorAll('{}')[{i}]?.textContent?.trim() || ''",
                locator.selector().replace('\'', "\\'")
              ),
              ferridriver::protocol::SerializedArgument::default(),
              None,
              None,
            )
            .await
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
          actuals.push(text);
        }

        if actuals.len() != expected.len() {
          let matches = false;
          if matches == is_not {
            return Ok(());
          }
          return Err(MatchError::new(
            format!(
              "{} texts: {:?}",
              expected.len(),
              expected.iter().map(|e| e.description()).collect::<Vec<_>>()
            ),
            format!("{} texts: {actuals:?}", actuals.len()),
          ));
        }

        for (i, (exp, act)) in expected.iter().zip(actuals.iter()).enumerate() {
          let matches = exp.matches(act);
          if matches == is_not {
            return Err(MatchError::new(
              format!("{}[{i}] = {}", if is_not { "not " } else { "" }, exp.description()),
              format!("[{i}] = \"{act}\""),
            ));
          }
        }
        Ok(())
      }
    })
    .await
  }

  pub async fn to_contain_texts(&self, expected: &[impl AsRef<str>]) -> Result<(), AssertionFailure> {
    let expected: Vec<String> = expected.iter().map(|s| s.as_ref().to_string()).collect();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, locator_ctx(locator, "toContainTexts", is_not), || {
      let expected = expected.clone();
      async move {
        let count = locator.count().await.unwrap_or(0);
        let mut actuals = Vec::with_capacity(count);
        for i in 0..count {
          let text = locator
            .evaluate(
              &format!(
                "() => document.querySelectorAll('{}')[{i}]?.textContent?.trim() || ''",
                locator.selector().replace('\'', "\\'")
              ),
              ferridriver::protocol::SerializedArgument::default(),
              None,
              None,
            )
            .await
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
          actuals.push(text);
        }

        if actuals.len() != expected.len() {
          if is_not {
            return Ok(());
          }
          return Err(MatchError::new(
            format!("{} texts", expected.len()),
            format!("{} texts", actuals.len()),
          ));
        }

        for (i, (exp, act)) in expected.iter().zip(actuals.iter()).enumerate() {
          let contains = act.contains(exp.as_str());
          if contains == is_not {
            return Err(MatchError::new(
              format!("{}[{i}] containing \"{exp}\"", if is_not { "not " } else { "" }),
              format!("[{i}] = \"{act}\""),
            ));
          }
        }
        Ok(())
      }
    })
    .await
  }

  // ── Count ──

  pub async fn to_have_count(&self, expected: usize) -> Result<(), AssertionFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, locator_ctx(locator, "toHaveCount", is_not), || async move {
      let actual = locator.count().await.unwrap_or(0);
      let matches = actual == expected;
      if matches == is_not {
        Err(MatchError::new(
          format!("{}{expected}", if is_not { "not " } else { "" }),
          format!("{actual}"),
        ))
      } else {
        Ok(())
      }
    })
    .await
  }
}
