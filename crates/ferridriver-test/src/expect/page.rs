//! Auto-retrying Page assertions.

use ferridriver::Page;

use super::{poll_until, Expect, MatchError, StringOrRegex};
use crate::model::TestFailure;

impl Expect<'_, Page> {
  /// Assert the page title matches the expected value.
  pub async fn to_have_title(&self, expected: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
    let expected = expected.into();
    let page = self.subject;
    let is_not = self.is_not;

    poll_until(self.timeout, || {
      let expected = expected.clone();
      async move {
        let actual = page.title().await.map_err(|e| MatchError::new(e.to_string()))?;
        let matches = expected.matches(&actual);
        if matches == is_not {
          Err(
            MatchError::new(format!(
              "expected title {}{}\nreceived: \"{actual}\"",
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
    })
    .await
  }

  /// Assert the page title contains the expected substring (auto-retry).
  pub async fn to_contain_title(&self, expected: &str) -> Result<(), TestFailure> {
    let expected = expected.to_string();
    let page = self.subject;
    let is_not = self.is_not;

    poll_until(self.timeout, || {
      let expected = expected.clone();
      async move {
        let actual = page.title().await.map_err(|e| MatchError::new(e.to_string()))?;
        let contains = actual.contains(&expected);
        if contains == is_not {
          Err(
            MatchError::new(format!(
              "expected title {}to contain \"{expected}\"\nreceived: \"{actual}\"",
              if is_not { "not " } else { "" },
            ))
            .with_diff(format!(
              "- expected to contain: \"{expected}\"\n+ received: \"{actual}\""
            )),
          )
        } else {
          Ok(())
        }
      }
    })
    .await
  }

  /// Assert the page URL contains the expected substring (auto-retry).
  pub async fn to_contain_url(&self, expected: &str) -> Result<(), TestFailure> {
    let expected = expected.to_string();
    let page = self.subject;
    let is_not = self.is_not;

    poll_until(self.timeout, || {
      let expected = expected.clone();
      async move {
        let actual = page.url().await.map_err(|e| MatchError::new(e.to_string()))?;
        let contains = actual.contains(&expected);
        if contains == is_not {
          Err(
            MatchError::new(format!(
              "expected URL {}to contain \"{expected}\"\nreceived: \"{actual}\"",
              if is_not { "not " } else { "" },
            ))
            .with_diff(format!(
              "- expected to contain: \"{expected}\"\n+ received: \"{actual}\""
            )),
          )
        } else {
          Ok(())
        }
      }
    })
    .await
  }

  /// Assert the page screenshot matches a stored PNG snapshot.
  ///
  /// Uses the same pixel-level comparison as `Locator::to_have_screenshot()`.
  /// Pass `UPDATE_SNAPSHOTS=1` env var to update baseline images.
  pub async fn to_have_screenshot(&self, name: &str) -> Result<(), TestFailure> {
    // Take page screenshot, then delegate to the shared snapshot comparison.
    let page = self.subject;
    let actual_png = page
      .screenshot(ferridriver::options::ScreenshotOptions::default())
      .await
      .map_err(|e| TestFailure {
        message: format!("page screenshot failed: {e}"),
        stack: None,
        diff: None,
        screenshot: None,
      })?;

    crate::snapshot::compare_screenshot_png(&actual_png, name)
  }

  /// Assert the page accessibility tree matches the expected ARIA snapshot.
  pub async fn to_match_aria_snapshot(&self, expected: &str) -> Result<(), TestFailure> {
    let page = self.subject;
    let is_not = self.is_not;
    let expected = expected.to_string();

    poll_until(self.timeout, || {
      let expected = expected.clone();
      async move {
        let snapshot = page
          .snapshot_for_ai(ferridriver::snapshot::SnapshotOptions {
            depth: None,
            track: None,
          })
          .await
          .map_err(|e| MatchError::new(format!("aria snapshot failed: {e}")))?;

        let contains = snapshot.full.contains(&expected);
        if contains == is_not {
          Err(
            MatchError::new(format!(
              "expected ARIA snapshot {}to contain \"{expected}\"",
              if is_not { "not " } else { "" },
            ))
            .with_diff(format!(
              "- expected to contain: \"{expected}\"\n+ received:\n{}",
              &snapshot.full[..snapshot.full.len().min(500)]
            )),
          )
        } else {
          Ok(())
        }
      }
    })
    .await
  }

  /// Assert the page URL matches the expected value.
  pub async fn to_have_url(&self, expected: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
    let expected = expected.into();
    let page = self.subject;
    let is_not = self.is_not;

    poll_until(self.timeout, || {
      let expected = expected.clone();
      async move {
        let actual = page.url().await.map_err(|e| MatchError::new(e.to_string()))?;
        let matches = expected.matches(&actual);
        if matches == is_not {
          Err(
            MatchError::new(format!(
              "expected URL {}{}\nreceived: \"{actual}\"",
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
    })
    .await
  }
}
