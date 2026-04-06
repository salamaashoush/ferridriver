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

  /// Assert the locator has the expected accessible error message.
  pub async fn to_have_accessible_error_message(
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
              var errId = el.getAttribute('aria-errormessage'); \
              if (errId) { \
                var errEl = document.getElementById(errId); \
                return errEl ? errEl.textContent.trim() : ''; \
              } \
              return el.validationMessage || ''; \
            })()",
          )
          .await
          .unwrap_or(None)
          .and_then(|v| v.as_str().map(String::from))
          .unwrap_or_default();
        check_text_match(&expected, &actual, is_not, "accessible error message")
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

  // ── Array text matchers ──

  /// Assert multiple elements' text content matches an array of expected values.
  /// Each element matched by the locator is compared positionally.
  /// Supports String and Regex per item.
  pub async fn to_have_texts(&self, expected: &[impl Into<StringOrRegex> + Clone]) -> Result<(), TestFailure> {
    let expected: Vec<StringOrRegex> = expected.iter().map(|e| e.clone().into()).collect();
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || {
      let expected = expected.clone();
      async move {
        let count = locator.count().await.unwrap_or(0);
        let mut actuals = Vec::with_capacity(count);
        for i in 0..count {
          let _selector = format!("{}:nth-child({})", locator.selector(), i + 1);
          // Use the parent page's evaluate to get text for each child.
          let text = locator
            .evaluate(&format!(
              "document.querySelectorAll('{}')[{i}]?.textContent?.trim() || ''",
              locator.selector().replace('\'', "\\'")
            ))
            .await
            .unwrap_or(None)
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
          actuals.push(text);
        }

        if actuals.len() != expected.len() {
          let matches = false;
          if matches == is_not { return Ok(()); }
          return Err(MatchError::new(format!(
            "expected {} texts, got {}\nexpected: {:?}\nreceived: {:?}",
            expected.len(), actuals.len(),
            expected.iter().map(|e| e.description()).collect::<Vec<_>>(),
            actuals,
          )));
        }

        for (i, (exp, act)) in expected.iter().zip(actuals.iter()).enumerate() {
          let matches = exp.matches(act);
          if matches == is_not {
            return Err(MatchError::new(format!(
              "text mismatch at index {i}\n{}expected: {}\nreceived: \"{act}\"",
              if is_not { "not " } else { "" },
              exp.description(),
            )));
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
    poll_until(self.timeout, || {
      let expected = expected.clone();
      async move {
        let count = locator.count().await.unwrap_or(0);
        let mut actuals = Vec::with_capacity(count);
        for i in 0..count {
          let text = locator
            .evaluate(&format!(
              "document.querySelectorAll('{}')[{i}]?.textContent?.trim() || ''",
              locator.selector().replace('\'', "\\'")
            ))
            .await
            .unwrap_or(None)
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
          actuals.push(text);
        }

        if actuals.len() != expected.len() {
          if is_not { return Ok(()); }
          return Err(MatchError::new(format!(
            "expected {} texts, got {}",
            expected.len(), actuals.len(),
          )));
        }

        for (i, (exp, act)) in expected.iter().zip(actuals.iter()).enumerate() {
          let contains = act.contains(exp.as_str());
          if contains == is_not {
            return Err(MatchError::new(format!(
              "text at index {i} {}to contain \"{exp}\"\nreceived: \"{act}\"",
              if is_not { "not expected " } else { "expected " },
            )));
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
    let actual = locator
      .text_content()
      .await
      .unwrap_or(None)
      .unwrap_or_default();
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
      attachments: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
      steps: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
      soft_errors: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
      timeout: self.timeout,
      tags: Vec::new(),
      start_time: std::time::Instant::now(),
      event_bus: None,
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
    let locator = self.subject;
    let actual_png = locator
      .screenshot()
      .await
      .map_err(|e| TestFailure {
        message: format!("screenshot failed: {e}"),
        stack: None, diff: None, screenshot: None,
      })?;

    crate::snapshot::compare_screenshot_png(&actual_png, name)
  }

  // ── Accessibility ──

  /// Assert the element's accessibility tree matches a YAML-like snapshot.
  /// Matches Playwright's `toMatchAriaSnapshot` (simplified).
  pub async fn to_match_aria_snapshot(&self, expected_yaml: &str) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until(self.timeout, || {
      let expected_yaml = expected_yaml.to_string();
      async move {
        // Get the accessible name and role of matched elements.
        let aria_tree = locator
          .evaluate(
            "(() => { \
              const el = document.querySelector(selector); \
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
            })()"
          )
          .await
          .unwrap_or(None)
          .and_then(|v| v.as_str().map(String::from))
          .unwrap_or_else(|| "EMPTY".into());

        // Simple substring matching against the expected YAML.
        let lines_match = expected_yaml.lines().all(|expected_line| {
          let trimmed = expected_line.trim();
          if trimmed.is_empty() { return true; }
          aria_tree.contains(trimmed)
        });

        if lines_match == is_not {
          Err(MatchError::new(format!(
            "aria snapshot {}match\nexpected:\n{expected_yaml}\nreceived:\n{aria_tree}",
            if is_not { "unexpected " } else { "did not " },
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
