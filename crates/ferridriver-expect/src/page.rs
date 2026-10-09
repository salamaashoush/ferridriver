//! Page web-first matchers — url + title only. Snapshot/screenshot/
//! aria matchers live in `ferridriver-test` because they need the
//! test-runner's snapshot directory + image pipeline.

use std::borrow::Borrow;

use ferridriver::Page;
use ferridriver::expect::ExpectParams;

use crate::AssertionFailure;
use crate::builder::Expect;
use crate::value::StringOrRegex;
use crate::web::{Printed, Receiver, WebAssert, expected_text};

impl<P: Borrow<Page>> Expect<'_, P> {
  async fn page_text(
    &self,
    method: &'static str,
    expression: &str,
    expected: StringOrRegex,
    substring: bool,
    normalize: bool,
    ignore_case: Option<bool>,
  ) -> Result<(), AssertionFailure> {
    let mut params = ExpectParams::new(expression, self.is_not);
    params.expected_text = Some(vec![expected_text(&expected, substring, normalize, ignore_case)]);
    WebAssert {
      method,
      receiver: Receiver::Page(self.subject.borrow()),
      params,
      printed: Printed::Text { expected, substring },
      timeout: self.timeout,
      is_soft: self.is_soft,
    }
    .run()
    .await
  }

  /// Playwright: `toHaveTitle(titleOrRegExp, options?)`. The title is
  /// compared with its whitespace normalized.
  pub async fn to_have_title(&self, expected: impl Into<StringOrRegex>) -> Result<(), AssertionFailure> {
    self
      .page_text("toHaveTitle", "to.have.title", expected.into(), false, true, None)
      .await
  }

  /// ferridriver-only: the title contains `expected`.
  pub async fn to_contain_title(&self, expected: &str) -> Result<(), AssertionFailure> {
    self
      .page_text("toContainTitle", "to.have.title", expected.into(), true, true, None)
      .await
  }

  pub async fn to_have_url(&self, expected: impl Into<StringOrRegex>) -> Result<(), AssertionFailure> {
    self.to_have_url_with(expected, false).await
  }

  /// Playwright: `toHaveURL(urlOrRegExp, options?: { ignoreCase?, timeout? })`.
  pub async fn to_have_url_with(
    &self,
    expected: impl Into<StringOrRegex>,
    ignore_case: bool,
  ) -> Result<(), AssertionFailure> {
    self
      .page_text(
        "toHaveURL",
        "to.have.url",
        expected.into(),
        false,
        false,
        ignore_case.then_some(true),
      )
      .await
  }

  /// ferridriver-only: the URL contains `expected`.
  pub async fn to_contain_url(&self, expected: &str) -> Result<(), AssertionFailure> {
    self
      .page_text("toContainURL", "to.have.url", expected.into(), true, false, None)
      .await
  }
}
