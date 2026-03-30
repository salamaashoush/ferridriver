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
