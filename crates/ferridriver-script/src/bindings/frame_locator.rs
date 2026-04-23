//! `FrameLocatorJs` -- QuickJS wrapper for
//! [`ferridriver::locator::FrameLocator`].
//!
//! Mirrors Playwright's `FrameLocator` interface
//! (`/tmp/playwright/packages/playwright-core/types/types.d.ts` —
//! search `interface FrameLocator`). Construct via `page.frameLocator`,
//! `frame.frameLocator`, `locator.contentFrame`, or
//! `locator.frameLocator`.

use ferridriver::locator::FrameLocator;
use rquickjs::function::Opt;
use rquickjs::{JsLifetime, class::Trace};

use super::locator::LocatorJs;
use super::page::{parse_role_options, parse_text_options, string_or_regex_from_js};

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "FrameLocator")]
pub struct FrameLocatorJs {
  #[qjs(skip_trace)]
  pub(crate) inner: FrameLocator,
}

impl FrameLocatorJs {
  #[must_use]
  pub fn new(inner: FrameLocator) -> Self {
    Self { inner }
  }
}

#[rquickjs::methods]
impl FrameLocatorJs {
  #[qjs(rename = "locator")]
  pub fn locator(&self, selector: String) -> LocatorJs {
    LocatorJs::new(self.inner.locator(&selector, None))
  }

  #[qjs(rename = "getByRole")]
  pub fn get_by_role(&self, role: String, options: Opt<rquickjs::Value<'_>>) -> rquickjs::Result<LocatorJs> {
    let opts = parse_role_options(options)?;
    Ok(LocatorJs::new(self.inner.get_by_role(&role, &opts)))
  }

  #[qjs(rename = "getByText")]
  pub fn get_by_text(
    &self,
    text: rquickjs::Value<'_>,
    options: Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = string_or_regex_from_js(text)?;
    let opts = parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_text(&t, &opts)))
  }

  #[qjs(rename = "getByLabel")]
  pub fn get_by_label(
    &self,
    text: rquickjs::Value<'_>,
    options: Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = string_or_regex_from_js(text)?;
    let opts = parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_label(&t, &opts)))
  }

  #[qjs(rename = "getByPlaceholder")]
  pub fn get_by_placeholder(
    &self,
    text: rquickjs::Value<'_>,
    options: Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = string_or_regex_from_js(text)?;
    let opts = parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_placeholder(&t, &opts)))
  }

  #[qjs(rename = "getByAltText")]
  pub fn get_by_alt_text(
    &self,
    text: rquickjs::Value<'_>,
    options: Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = string_or_regex_from_js(text)?;
    let opts = parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_alt_text(&t, &opts)))
  }

  #[qjs(rename = "getByTitle")]
  pub fn get_by_title(
    &self,
    text: rquickjs::Value<'_>,
    options: Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = string_or_regex_from_js(text)?;
    let opts = parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_title(&t, &opts)))
  }

  #[qjs(rename = "getByTestId")]
  pub fn get_by_test_id(&self, test_id: rquickjs::Value<'_>) -> rquickjs::Result<LocatorJs> {
    let t = string_or_regex_from_js(test_id)?;
    Ok(LocatorJs::new(self.inner.get_by_test_id(&t)))
  }

  #[qjs(rename = "owner")]
  pub fn owner(&self) -> LocatorJs {
    LocatorJs::new(self.inner.owner())
  }

  #[qjs(rename = "frameLocator")]
  pub fn frame_locator(&self, selector: String) -> FrameLocatorJs {
    FrameLocatorJs::new(self.inner.frame_locator(&selector))
  }

  #[qjs(rename = "first")]
  pub fn first(&self) -> FrameLocatorJs {
    FrameLocatorJs::new(self.inner.first())
  }

  #[qjs(rename = "last")]
  pub fn last(&self) -> FrameLocatorJs {
    FrameLocatorJs::new(self.inner.last())
  }

  #[qjs(rename = "nth")]
  pub fn nth(&self, index: i32) -> FrameLocatorJs {
    FrameLocatorJs::new(self.inner.nth(index))
  }
}
