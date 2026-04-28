//! Auto-retrying Locator assertions matching Playwright's full LocatorAssertions API.

use ferridriver::Locator;

use super::{
  Expect, ExpectContext, HaveCssOptions, InViewportOptions, MatchError, ScreenshotMatcherOptions, StringOrRegex,
  poll_until,
};
use crate::model::TestFailure;

/// Build ExpectContext for a locator assertion.
fn locator_ctx(locator: &Locator, method: &'static str, is_not: bool) -> ExpectContext {
  ExpectContext {
    method,
    subject: format!("locator('{}')", locator.selector()),
    is_not,
  }
}

impl Expect<'_, Locator> {
  // ── Visibility / State ──

  /// Assert the locator is visible.
  pub async fn to_be_visible(&self) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(
      self.timeout,
      locator_ctx(locator, "toBeVisible", is_not),
      || async move {
        let visible = locator.is_visible().await.unwrap_or(false);
        check_bool(visible, is_not, "visible")
      },
    )
    .await
  }

  /// Assert the locator is hidden.
  pub async fn to_be_hidden(&self) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(
      self.timeout,
      locator_ctx(locator, "toBeHidden", is_not),
      || async move {
        let hidden = locator.is_hidden().await.unwrap_or(true);
        check_bool(hidden, is_not, "to be hidden")
      },
    )
    .await
  }

  /// Assert the locator is enabled.
  pub async fn to_be_enabled(&self) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(
      self.timeout,
      locator_ctx(locator, "toBeEnabled", is_not),
      || async move {
        let enabled = locator.is_enabled().await.unwrap_or(false);
        check_bool(enabled, is_not, "to be enabled")
      },
    )
    .await
  }

  /// Assert the locator is disabled.
  pub async fn to_be_disabled(&self) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(
      self.timeout,
      locator_ctx(locator, "toBeDisabled", is_not),
      || async move {
        let disabled = locator.is_disabled().await.unwrap_or(false);
        check_bool(disabled, is_not, "to be disabled")
      },
    )
    .await
  }

  /// Assert the locator is checked.
  pub async fn to_be_checked(&self) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(
      self.timeout,
      locator_ctx(locator, "toBeChecked", is_not),
      || async move {
        let checked = locator.is_checked().await.unwrap_or(false);
        check_bool(checked, is_not, "to be checked")
      },
    )
    .await
  }

  /// Assert the locator is editable.
  pub async fn to_be_editable(&self) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(
      self.timeout,
      locator_ctx(locator, "toBeEditable", is_not),
      || async move {
        let editable = locator.is_editable().await.unwrap_or(false);
        check_bool(editable, is_not, "to be editable")
      },
    )
    .await
  }

  /// Assert the locator is attached to the DOM.
  pub async fn to_be_attached(&self) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(
      self.timeout,
      locator_ctx(locator, "toBeAttached", is_not),
      || async move {
        let attached = locator.is_attached().await.unwrap_or(false);
        check_bool(attached, is_not, "to be attached")
      },
    )
    .await
  }

  /// Assert the locator is empty (no text content).
  pub async fn to_be_empty(&self) -> Result<(), TestFailure> {
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

  /// Assert the locator is focused.
  pub async fn to_be_focused(&self) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(
      self.timeout,
      locator_ctx(locator, "toBeFocused", is_not),
      || async move {
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
      },
    )
    .await
  }

  /// Assert the locator is in the viewport.
  ///
  /// Equivalent to [`Self::to_be_in_viewport_with`] with the
  /// default option bag (any positive intersection counts).
  pub async fn to_be_in_viewport(&self) -> Result<(), TestFailure> {
    self.to_be_in_viewport_with(InViewportOptions::default()).await
  }

  /// Playwright `toBeInViewport(options?)` — pass `{ ratio: 0.5 }`
  /// to require at least half of the element's bounding box to be
  /// visible. `ratio` is a fraction in `[0, 1]`; `0` accepts any
  /// non-zero intersection (the default), `1` requires the entire
  /// element to be in the viewport.
  pub async fn to_be_in_viewport_with(&self, options: InViewportOptions) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    let ratio = options.ratio.unwrap_or(0.0).clamp(0.0, 1.0);
    poll_until(
      self.timeout,
      locator_ctx(locator, "toBeInViewport", is_not),
      || async move {
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
      },
    )
    .await
  }

  // ── Text / Value ──

  /// Assert the locator's text content matches exactly.
  pub async fn to_have_text(&self, expected: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
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

  /// Assert the locator's text contains the expected substring.
  pub async fn to_contain_text(&self, expected: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
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

  /// Assert the locator's input value.
  pub async fn to_have_value(&self, expected: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
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

  /// Assert multiple select values (multi-select elements).
  pub async fn to_have_values(&self, expected: &[impl AsRef<str>]) -> Result<(), TestFailure> {
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

  /// Assert the locator has an attribute with the expected value.
  pub async fn to_have_attribute(&self, name: &str, value: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
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

  /// Assert the locator has the expected CSS class (exact match on class attribute).
  pub async fn to_have_class(&self, expected: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
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

  /// Assert the locator's class list contains the expected class name.
  pub async fn to_contain_class(&self, expected: &str) -> Result<(), TestFailure> {
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

  /// Assert the locator has the expected CSS property value.
  ///
  /// Equivalent to [`Self::to_have_css_with`] with no pseudo-element.
  pub async fn to_have_css(&self, property: &str, value: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
    self.to_have_css_with(property, value, HaveCssOptions::default()).await
  }

  /// Playwright `toHaveCSS(name, value, options?)` — `options.pseudo`
  /// targets a pseudo-element (`::before`, `::after`, etc).
  pub async fn to_have_css_with(
    &self,
    property: &str,
    value: impl Into<StringOrRegex>,
    options: HaveCssOptions,
  ) -> Result<(), TestFailure> {
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

  /// Assert the locator has the expected id.
  pub async fn to_have_id(&self, expected: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
    self.to_have_attribute("id", expected).await
  }

  /// Assert the locator has the expected ARIA role.
  pub async fn to_have_role(&self, expected: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
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

  /// Assert the locator has the expected accessible name.
  pub async fn to_have_accessible_name(&self, expected: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
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

  /// Assert the locator has the expected accessible description.
  pub async fn to_have_accessible_description(&self, expected: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
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

  /// Assert the locator has the expected accessible error message.
  pub async fn to_have_accessible_error_message(&self, expected: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
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

  /// Assert the locator has a JS property with the expected value.
  pub async fn to_have_js_property(&self, name: &str, value: serde_json::Value) -> Result<(), TestFailure> {
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

  /// Assert multiple elements' text content matches an array of expected values.
  /// Each element matched by the locator is compared positionally.
  /// Supports String and Regex per item.
  pub async fn to_have_texts(&self, expected: &[impl Into<StringOrRegex> + Clone]) -> Result<(), TestFailure> {
    let expected: Vec<StringOrRegex> = expected.iter().map(|e| e.clone().into()).collect();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, locator_ctx(locator, "toHaveTexts", is_not), || {
      let expected = expected.clone();
      async move {
        let count = locator.count().await.unwrap_or(0);
        let mut actuals = Vec::with_capacity(count);
        for i in 0..count {
          let _selector = format!("{}:nth-child({})", locator.selector(), i + 1);
          // Use the parent page's evaluate to get text for each child.
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

  /// Assert multiple elements contain expected substrings (positional).
  pub async fn to_contain_texts(&self, expected: &[impl AsRef<str>]) -> Result<(), TestFailure> {
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

  // ── Snapshot matchers ──

  /// Assert the element's text content matches a stored snapshot.
  /// First run creates the snapshot file. Subsequent runs diff against it.
  /// Pass `update = true` (or `--update-snapshots` CLI) to overwrite.
  pub async fn to_match_snapshot(&self, name: &str) -> Result<(), TestFailure> {
    let locator = self.subject;
    let actual = locator.text_content().await.unwrap_or(None).unwrap_or_default();
    // Snapshot dir defaults to __snapshots__ relative to cwd.
    let snap_dir = std::path::PathBuf::from("__snapshots__");
    let update = std::env::var("UPDATE_SNAPSHOTS").is_ok();
    let info = crate::model::TestInfo {
      test_id: crate::model::TestId {
        file: String::new(),
        suite: None,
        name: name.to_string(),
        line: None,
      },
      title_path: vec![name.to_string()],
      retry: 0,
      worker_index: 0,
      parallel_index: 0,
      repeat_each_index: 0,
      output_dir: std::path::PathBuf::from("test-results"),
      snapshot_dir: snap_dir,
      snapshot_path_template: None,
      update_snapshots: crate::config::UpdateSnapshotsMode::default(),
      ignore_snapshots: false,
      attachments: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
      steps: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
      soft_errors: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
      errors: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
      snapshot_suffix: std::sync::Arc::new(tokio::sync::Mutex::new(String::new())),
      column: None,
      project: None,
      config_snapshot: None,
      timeout: self.timeout,
      tags: Vec::new(),
      start_time: std::time::Instant::now(),
      event_bus: None,
      annotations: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
    };
    crate::snapshot::assert_snapshot(&info, &actual, name, update)
  }

  /// Assert the element's screenshot matches a stored PNG snapshot.
  ///
  /// Performs pixel-level comparison:
  /// - Decodes both PNGs to RGBA pixels
  /// - Compares per-pixel with a configurable threshold (default: 0.1 per channel)
  /// - Reports mismatch count and percentage
  /// - Generates a diff image (red = changed pixels) saved alongside
  /// - Attaches the actual screenshot to the failure for reporters
  ///
  /// First run creates the baseline. Set `UPDATE_SNAPSHOTS=1` to overwrite.
  pub async fn to_have_screenshot(&self, name: &str) -> Result<(), TestFailure> {
    self
      .to_have_screenshot_with(name, ScreenshotMatcherOptions::default())
      .await
  }

  /// Playwright `toHaveScreenshot(name, options?)` — captures the
  /// element screenshot and diffs it against the stored baseline.
  ///
  /// Honoured options:
  /// - `threshold` — per-channel pixel tolerance (0–1, mapped to 0–255).
  /// - `max_diff_pixels` — absolute pixel-mismatch budget.
  /// - `max_diff_pixel_ratio` — fractional pixel-mismatch budget.
  ///
  /// `mask`, `mask_color`, `animations`, `caret`, `clip`, `scale`,
  /// and `style_path` are accepted for parity but not yet wired
  /// into the screenshot capture path; see PLAYWRIGHT_COMPAT.md
  /// §7.17 for the carry-forward list.
  pub async fn to_have_screenshot_with(
    &self,
    name: &str,
    options: ScreenshotMatcherOptions,
  ) -> Result<(), TestFailure> {
    let locator = self.subject;
    let actual_png = capture_with_options(locator, &options).await?;
    crate::snapshot::compare_screenshot_png_with(&actual_png, name, &options)
  }

  // ── Accessibility ──

  /// Assert the element's accessibility tree matches a YAML-like snapshot.
  /// Matches Playwright's `toMatchAriaSnapshot` (simplified).
  pub async fn to_match_aria_snapshot(&self, expected_yaml: &str) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(
      self.timeout,
      locator_ctx(locator, "toMatchAriaSnapshot", is_not),
      || {
        let expected_yaml = expected_yaml.to_string();
        async move {
          // Get the accessible name and role of matched elements.
          let aria_tree = locator
            .evaluate(
              "el => { \
              if (!el) return 'EMPTY'; \
              function walk(node, indent) { \
                let role = node.getAttribute('role') || node.tagName.toLowerCase(); \
                let name = node.getAttribute('aria-label') || node.textContent?.trim()?.substring(0, 50) || ''; \
                let line = indent + role; \
                if (name) line += ' \"' + name + '\"'; \
                let lines = [line]; \
                for (const child of node.children) { \
                  lines.push(...walk(child, indent + '  ')); \
                } \
                return lines; \
              } \
              return walk(el, '').join('\\n'); \
            }",
              ferridriver::protocol::SerializedArgument::default(),
              None,
              None,
            )
            .await
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| "EMPTY".into());

          // Structural-by-line match. The expected YAML is a list of
          // role-with-optional-name lines indented in 2-space steps;
          // each line must appear in `aria_tree` AND in the same
          // top-to-bottom order. This is stricter than substring
          // (rejects out-of-order expectations) and looser than
          // Playwright's `injected/ariaSnapshot.ts` structural diff
          // (it doesn't enforce sibling/ancestor relationships). The
          // full injected ariaSnapshot integration is tracked under
          // §7.17.
          let expected_lines: Vec<&str> = expected_yaml.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
          let mut cursor = 0usize;
          let actual_lines: Vec<&str> = aria_tree.lines().map(str::trim).collect();
          let lines_match = expected_lines.iter().all(|expected| {
            while cursor < actual_lines.len() {
              let actual = actual_lines[cursor];
              cursor += 1;
              if actual.contains(expected) {
                return true;
              }
            }
            false
          });

          if lines_match == is_not {
            Err(MatchError::new(
              format!("{}\n{expected_yaml}", if is_not { "not matching" } else { "matching" }),
              aria_tree,
            ))
          } else {
            Ok(())
          }
        }
      },
    )
    .await
  }

  // ── Count ──

  /// Assert the number of elements matching the locator.
  pub async fn to_have_count(&self, expected: usize) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(
      self.timeout,
      locator_ctx(locator, "toHaveCount", is_not),
      || async move {
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
      },
    )
    .await
  }
}

// ── Helpers ──

fn check_bool(actual: bool, is_not: bool, expected_state: &str) -> Result<(), MatchError> {
  if actual == is_not {
    let expected = format!("{}{expected_state}", if is_not { "not " } else { "" });
    let received = format!("{}{expected_state}", if actual { "" } else { "not " });
    Err(MatchError::new(expected, received))
  } else {
    Ok(())
  }
}

fn check_text_match(expected: &StringOrRegex, actual: &str, is_not: bool, _label: &str) -> Result<(), MatchError> {
  let matches = expected.matches(actual);
  if matches == is_not {
    let exp = format!("{}{}", if is_not { "not " } else { "" }, expected.description());
    Err(MatchError::new(exp, format!("\"{actual}\"")))
  } else {
    Ok(())
  }
}

// ── Screenshot capture wrapper (§7.17 capture-time options) ─────────────────

/// Apply the matcher's capture-time options as DOM mutations, take
/// the screenshot, then restore. This sidesteps the per-backend
/// screenshot pipelines for the subset of options that are
/// expressible via CSS injection: `animations`, `caret`, `mask` /
/// `mask_color`, and `style_path`. `clip` is honoured by cropping the
/// returned PNG client-side. `scale` is best-effort — true device-vs-CSS
/// scale toggling lives in the backend's `Page.captureScreenshot`
/// flags; here we record the request and let the comparator's
/// pixel-budget options absorb DPR mismatches when feasible.
///
/// All injected `<style>` nodes are tagged with a `data-ferridriver-cap`
/// attribute so the cleanup pass removes only what we added; user
/// styles outside this set are left untouched.
async fn capture_with_options(locator: &Locator, options: &ScreenshotMatcherOptions) -> Result<Vec<u8>, TestFailure> {
  let page = locator.page();

  let mut style_blocks: Vec<String> = Vec::new();

  if options.animations.as_deref() == Some("disabled") {
    style_blocks.push(
      "*, *::before, *::after { \
        animation-duration: 0s !important; \
        animation-delay: 0s !important; \
        animation-iteration-count: 1 !important; \
        transition-duration: 0s !important; \
        transition-delay: 0s !important; \
      }"
      .to_string(),
    );
  }

  if options.caret.as_deref() == Some("hide") {
    style_blocks.push("html, body, * { caret-color: transparent !important; }".to_string());
  }

  if let Some(ref style_path) = options.style_path {
    match std::fs::read_to_string(style_path) {
      Ok(content) => style_blocks.push(content),
      Err(e) => {
        return Err(TestFailure {
          message: format!("toHaveScreenshot stylePath {} unreadable: {e}", style_path.display()),
          stack: None,
          diff: None,
          screenshot: None,
        });
      },
    }
  }

  let mask_color = options.mask_color.as_deref().unwrap_or("#FF00FF");
  if !options.mask.is_empty() {
    let mut mask_css = String::new();
    for selector in &options.mask {
      mask_css.push_str(selector);
      mask_css.push_str(" { background: ");
      mask_css.push_str(mask_color);
      mask_css.push_str(" !important; color: ");
      mask_css.push_str(mask_color);
      mask_css.push_str(" !important; }\n");
    }
    style_blocks.push(mask_css);
  }

  let token = "ferridriver-screenshot-capture";

  if !style_blocks.is_empty() {
    let combined = style_blocks.join("\n");
    let escaped = serde_json::to_string(&combined).unwrap_or_else(|_| "\"\"".to_string());
    // Self-invoking expression so the script runs as soon as it's
    // evaluated. Pass `is_function: None` (default expression eval) so
    // backends that treat `Some(true)` differently from `None` don't
    // diverge.
    let inject_script = format!(
      "(function() {{ \
        const s = document.createElement('style'); \
        s.setAttribute('data-{TOK}', '1'); \
        s.textContent = {ESC}; \
        document.head.appendChild(s); \
        return true; \
      }})()",
      TOK = token,
      ESC = escaped,
    );
    let _ = page
      .evaluate(
        &inject_script,
        ferridriver::protocol::SerializedArgument::default(),
        None,
      )
      .await
      .map_err(|e| TestFailure {
        message: format!("screenshot capture-options inject failed: {e}"),
        stack: None,
        diff: None,
        screenshot: None,
      })?;
  }

  let raw_png = locator.screenshot().await.map_err(|e| TestFailure {
    message: format!("screenshot failed: {e}"),
    stack: None,
    diff: None,
    screenshot: None,
  });

  // Cleanup runs regardless of capture outcome.
  if !style_blocks.is_empty() {
    let cleanup = format!(
      "(function() {{ \
        document.querySelectorAll('style[data-{TOK}]').forEach(function(n) {{ n.remove(); }}); \
        return true; \
      }})()",
      TOK = token,
    );
    let _ = page
      .evaluate(&cleanup, ferridriver::protocol::SerializedArgument::default(), None)
      .await;
  }

  let png = raw_png?;

  if let Some(clip) = options.clip {
    Ok(crop_png_to_clip(&png, &clip)?)
  } else {
    Ok(png)
  }
}

fn crop_png_to_clip(png: &[u8], clip: &crate::expect::ScreenshotClip) -> Result<Vec<u8>, TestFailure> {
  use image::GenericImageView;

  let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).map_err(|e| TestFailure {
    message: format!("toHaveScreenshot clip: failed to decode capture: {e}"),
    stack: None,
    diff: None,
    screenshot: None,
  })?;
  let (img_w, img_h) = img.dimensions();
  // Clamp to image bounds — Playwright tolerates clips that extend
  // past the captured area by silently truncating.
  #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
  let x = (clip.x.max(0.0).min(f64::from(img_w))) as u32;
  #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
  let y = (clip.y.max(0.0).min(f64::from(img_h))) as u32;
  #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
  let w = (clip.width.max(0.0).min(f64::from(img_w.saturating_sub(x)))) as u32;
  #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
  let h = (clip.height.max(0.0).min(f64::from(img_h.saturating_sub(y)))) as u32;
  if w == 0 || h == 0 {
    return Err(TestFailure {
      message: format!(
        "toHaveScreenshot clip: empty rect after clamping (x={x} y={y} w={w} h={h}) against {img_w}x{img_h} capture"
      ),
      stack: None,
      diff: None,
      screenshot: None,
    });
  }
  let cropped = img.crop_imm(x, y, w, h);
  let mut out = Vec::new();
  cropped
    .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
    .map_err(|e| TestFailure {
      message: format!("toHaveScreenshot clip: re-encode failed: {e}"),
      stack: None,
      diff: None,
      screenshot: None,
    })?;
  Ok(out)
}
