//! Page web-first matchers — url + title only. Snapshot/screenshot/
//! aria matchers live in `ferridriver-test` because they need the
//! test-runner's snapshot directory + image pipeline.

use std::borrow::Borrow;

use ferridriver::Page;

use std::future::Future;
use std::time::Duration;

use crate::AssertionFailure;
use crate::builder::Expect;
use crate::poll::{ExpectContext, MatchError, poll_traced};
use crate::value::StringOrRegex;

fn page_ctx(method: &'static str, is_not: bool) -> ExpectContext {
  ExpectContext {
    method,
    subject: "page".into(),
    is_not,
  }
}

/// [`poll_traced`] on the page's context — the shared funnel for every
/// page matcher below.
async fn poll_page<F, Fut>(
  page: &Page,
  timeout: Duration,
  method: &'static str,
  is_not: bool,
  check: F,
) -> Result<(), AssertionFailure>
where
  F: FnMut() -> Fut,
  Fut: Future<Output = Result<(), MatchError>>,
{
  let params = serde_json::json!({
    "isNot": is_not,
    "timeout": u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
  });
  poll_traced(Some(page), params, timeout, page_ctx(method, is_not), check).await
}

impl<P: Borrow<Page>> Expect<'_, P> {
  pub async fn to_have_title(&self, expected: impl Into<StringOrRegex>) -> Result<(), AssertionFailure> {
    let expected = expected.into();
    let page: &Page = self.subject.borrow();
    let is_not = self.is_not;
    poll_page(page, self.timeout, "toHaveTitle", is_not, || {
      let expected = expected.clone();
      async move {
        let actual = page
          .title()
          .await
          .map_err(|e| MatchError::new("(title)", e.to_string()))?;
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

  pub async fn to_contain_title(&self, expected: &str) -> Result<(), AssertionFailure> {
    let expected = expected.to_string();
    let page: &Page = self.subject.borrow();
    let is_not = self.is_not;
    poll_page(page, self.timeout, "toContainTitle", is_not, || {
      let expected = expected.clone();
      async move {
        let actual = page
          .title()
          .await
          .map_err(|e| MatchError::new("(title)", e.to_string()))?;
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

  pub async fn to_have_url(&self, expected: impl Into<StringOrRegex>) -> Result<(), AssertionFailure> {
    let expected = expected.into();
    let page: &Page = self.subject.borrow();
    let is_not = self.is_not;
    poll_page(page, self.timeout, "toHaveURL", is_not, || {
      let expected = expected.clone();
      async move {
        let actual = page.url();
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

  pub async fn to_contain_url(&self, expected: &str) -> Result<(), AssertionFailure> {
    let expected = expected.to_string();
    let page: &Page = self.subject.borrow();
    let is_not = self.is_not;
    poll_page(page, self.timeout, "toContainURL", is_not, || {
      let expected = expected.clone();
      async move {
        let actual = page.url();
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
}
