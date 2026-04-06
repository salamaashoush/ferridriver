//! Auto-retrying Page assertions.

use ferridriver::Page;

use super::{poll_until, Expect, ExpectContext, MatchError, StringOrRegex};
use crate::model::TestFailure;

fn page_ctx(method: &'static str, is_not: bool) -> ExpectContext {
  ExpectContext {
    method,
    subject: "page".into(),
    is_not,
  }
}

impl Expect<'_, Page> {
  /// Assert the page title matches the expected value.
  pub async fn to_have_title(&self, expected: impl Into<StringOrRegex>) -> Result<(), TestFailure> {
    let expected = expected.into();
    let page = self.subject;
    let is_not = self.is_not;

    poll_until(self.timeout, page_ctx("toHaveTitle", is_not), || {
      let expected = expected.clone();
      async move {
        let actual = page.title().await.map_err(|e| MatchError::new("(title)", e.to_string()))?;
        let matches = expected.matches(&actual);
        if matches == is_not {
          Err(MatchError::new(
            format!("{}{}", if is_not { "not " } else { "" }, expected.description()),
            format!("\"{actual}\""),
          ))
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

    poll_until(self.timeout, page_ctx("toContainTitle", is_not), || {
      let expected = expected.clone();
      async move {
        let actual = page.title().await.map_err(|e| MatchError::new("(title)", e.to_string()))?;
        let contains = actual.contains(&expected);
        if contains == is_not {
          Err(MatchError::new(
            format!("{}containing \"{expected}\"", if is_not { "not " } else { "" }),
            format!("\"{actual}\""),
          ))
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

    poll_until(self.timeout, page_ctx("toHaveURL", is_not), || {
      let expected = expected.clone();
      async move {
        let actual = page.url().await.map_err(|e| MatchError::new("(url)", e.to_string()))?;
        let matches = expected.matches(&actual);
        if matches == is_not {
          Err(MatchError::new(
            format!("{}{}", if is_not { "not " } else { "" }, expected.description()),
            format!("\"{actual}\""),
          ))
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

    poll_until(self.timeout, page_ctx("toContainURL", is_not), || {
      let expected = expected.clone();
      async move {
        let actual = page.url().await.map_err(|e| MatchError::new("(url)", e.to_string()))?;
        let contains = actual.contains(&expected);
        if contains == is_not {
          Err(MatchError::new(
            format!("{}containing \"{expected}\"", if is_not { "not " } else { "" }),
            format!("\"{actual}\""),
          ))
        } else {
          Ok(())
        }
      }
    })
    .await
  }

  /// Assert the page screenshot matches a stored PNG snapshot.
  pub async fn to_have_screenshot(&self, name: &str) -> Result<(), TestFailure> {
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

    poll_until(self.timeout, page_ctx("toMatchAriaSnapshot", is_not), || {
      let expected = expected.clone();
      async move {
        let snapshot = page
          .snapshot_for_ai(ferridriver::snapshot::SnapshotOptions {
            depth: None,
            track: None,
          })
          .await
          .map_err(|e| MatchError::new("(aria snapshot)", format!("error: {e}")))?;

        let contains = snapshot.full.contains(&expected);
        if contains == is_not {
          Err(MatchError::new(
            format!("{}\n{expected}", if is_not { "not matching" } else { "matching" }),
            snapshot.full[..snapshot.full.len().min(500)].to_string(),
          ))
        } else {
          Ok(())
        }
      }
    })
    .await
  }
}
