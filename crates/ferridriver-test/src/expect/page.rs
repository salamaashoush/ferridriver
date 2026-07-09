//! Snapshot / screenshot / aria matchers for `Expect<Arc<Page>>`. The
//! url / title matchers live in [`ferridriver_expect::page`].

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use ferridriver::Page;
use ferridriver_expect::{Expect, ExpectContext, MatchError, poll_until as expect_poll_until};

use crate::model::TestFailure;

fn page_ctx(method: &'static str, is_not: bool) -> ExpectContext {
  ExpectContext {
    method,
    subject: "page".into(),
    is_not,
  }
}

async fn poll_until_test<F, Fut>(timeout: Duration, ctx: ExpectContext, check: F) -> Result<(), TestFailure>
where
  F: FnMut() -> Fut,
  Fut: Future<Output = Result<(), MatchError>>,
{
  expect_poll_until(timeout, ctx, check).await.map_err(Into::into)
}

/// Snapshot matchers for `expect(page)`. Import via
/// `use ferridriver_test::expect::PageSnapshotMatchers;`.
#[allow(async_fn_in_trait)]
pub trait PageSnapshotMatchers {
  async fn to_have_screenshot(&self, name: &str) -> Result<(), TestFailure>;
  async fn to_match_aria_snapshot(&self, expected: &str) -> Result<(), TestFailure>;
}

impl PageSnapshotMatchers for Expect<'_, Arc<Page>> {
  async fn to_have_screenshot(&self, name: &str) -> Result<(), TestFailure> {
    let page = self.subject;
    let actual_png = page.screenshot().await.map_err(|e| TestFailure {
      message: format!("page screenshot failed: {e}"),
      stack: None,
      diff: None,
      screenshot: None,
    })?;

    crate::snapshot::compare_screenshot_png(&actual_png, name)
  }

  async fn to_match_aria_snapshot(&self, expected: &str) -> Result<(), TestFailure> {
    let page = self.subject;
    let is_not = self.is_not;
    let expected = expected.to_string();

    poll_until_test(self.timeout, page_ctx("toMatchAriaSnapshot", is_not), || {
      let expected = expected.clone();
      async move {
        let snapshot = page
          .snapshot_for_ai()
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
