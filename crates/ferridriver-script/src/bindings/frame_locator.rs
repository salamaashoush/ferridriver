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
use rquickjs::{Ctx, JsLifetime, class::Trace};

use crate::bindings::convert::FerriResultCtxExt;

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
  /// Playwright: `frameLocator.locator(selectorOrLocator: string | Locator,
  /// options?: LocatorOptions): Locator`.
  #[qjs(rename = "locator")]
  pub fn locator<'js>(
    &self,
    ctx: Ctx<'js>,
    selector_or_locator: rquickjs::Value<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<LocatorJs> {
    let (like, filter) = crate::bindings::locator::lower_locator_args(&ctx, &selector_or_locator, options, true)?;
    Ok(LocatorJs::new(match filter {
      Some(f) => self.inner.locator_with(like, &f),
      None => self.inner.locator(like),
    }))
  }

  #[qjs(rename = "getByRole")]
  pub fn get_by_role(&self, role: String, options: Opt<rquickjs::Value<'_>>) -> rquickjs::Result<LocatorJs> {
    let opts = parse_role_options(options)?;
    Ok(LocatorJs::new(
      self.inner.get_by_role(role.as_str()).options(opts).into_locator(),
    ))
  }

  #[qjs(rename = "getByText")]
  pub fn get_by_text(
    &self,
    text: rquickjs::Value<'_>,
    options: Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = string_or_regex_from_js(text)?;
    let opts = parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_text(t).options(opts).into_locator()))
  }

  #[qjs(rename = "getByLabel")]
  pub fn get_by_label(
    &self,
    text: rquickjs::Value<'_>,
    options: Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = string_or_regex_from_js(text)?;
    let opts = parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_label(t).options(opts).into_locator()))
  }

  #[qjs(rename = "getByPlaceholder")]
  pub fn get_by_placeholder(
    &self,
    text: rquickjs::Value<'_>,
    options: Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = string_or_regex_from_js(text)?;
    let opts = parse_text_options(options);
    Ok(LocatorJs::new(
      self.inner.get_by_placeholder(t).options(opts).into_locator(),
    ))
  }

  #[qjs(rename = "getByAltText")]
  pub fn get_by_alt_text(
    &self,
    text: rquickjs::Value<'_>,
    options: Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = string_or_regex_from_js(text)?;
    let opts = parse_text_options(options);
    Ok(LocatorJs::new(
      self.inner.get_by_alt_text(t).options(opts).into_locator(),
    ))
  }

  #[qjs(rename = "getByTitle")]
  pub fn get_by_title(
    &self,
    text: rquickjs::Value<'_>,
    options: Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = string_or_regex_from_js(text)?;
    let opts = parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_title(t).options(opts).into_locator()))
  }

  #[qjs(rename = "getByTestId")]
  pub fn get_by_test_id(&self, test_id: rquickjs::Value<'_>) -> rquickjs::Result<LocatorJs> {
    let t = string_or_regex_from_js(test_id)?;
    Ok(LocatorJs::new(self.inner.get_by_test_id(t)))
  }

  #[qjs(rename = "owner")]
  pub fn owner(&self) -> LocatorJs {
    LocatorJs::new(self.inner.owner())
  }

  #[qjs(rename = "frameLocator")]
  pub fn frame_locator(&self, selector: String) -> FrameLocatorJs {
    FrameLocatorJs::new(self.inner.frame_locator(selector.as_str()))
  }

  #[qjs(rename = "first")]
  pub fn first(&self, ctx: Ctx<'_>) -> rquickjs::Result<FrameLocatorJs> {
    self.inner.first().map(FrameLocatorJs::new).into_js_with(&ctx)
  }

  #[qjs(rename = "last")]
  pub fn last(&self, ctx: Ctx<'_>) -> rquickjs::Result<FrameLocatorJs> {
    self.inner.last().map(FrameLocatorJs::new).into_js_with(&ctx)
  }

  #[qjs(rename = "nth")]
  pub fn nth(&self, ctx: Ctx<'_>, index: i32) -> rquickjs::Result<FrameLocatorJs> {
    self.inner.nth(index).map(FrameLocatorJs::new).into_js_with(&ctx)
  }
}
