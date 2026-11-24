//! `FrameLocatorJs` -- `QuickJS` wrapper for
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
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when `selectorOrLocator` is not a selector string, a `Locator` or a
  /// `{ selector }` object, or when `options` is not an object of the documented shape.
  #[qjs(rename = "locator")]
  pub fn locator<'js>(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: Ctx<'js>,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs extracts JS arguments by value; a `&T` parameter would mean a borrowed class instance"
    )]
    selector_or_locator: rquickjs::Value<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<LocatorJs> {
    let (like, filter) = crate::bindings::locator::lower_locator_args(&ctx, &selector_or_locator, options, true)?;
    Ok(LocatorJs::new(match filter {
      Some(f) => self.inner.locator_with(like, &f),
      None => self.inner.locator(like),
    }))
  }

  /// Playwright's `frameLocator.getByRole(role, options?)`.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when `options.name` or `options.description` is neither a string nor a
  /// `RegExp`.
  #[qjs(rename = "getByRole")]
  pub fn get_by_role(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs extracts JS arguments by value; a `&T` parameter would mean a borrowed class instance"
    )]
    role: String,
    options: Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let opts = parse_role_options(options)?;
    Ok(LocatorJs::new(
      self.inner.get_by_role(role.as_str()).options(opts).into_locator(),
    ))
  }

  /// Playwright's `frameLocator.getByText(text, options?)`.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when `text` is neither a string nor a `RegExp`.
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

  /// Playwright's `frameLocator.getByLabel(text, options?)`.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when `text` is neither a string nor a `RegExp`.
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

  /// Playwright's `frameLocator.getByPlaceholder(text, options?)`.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when `text` is neither a string nor a `RegExp`.
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

  /// Playwright's `frameLocator.getByAltText(text, options?)`.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when `text` is neither a string nor a `RegExp`.
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

  /// Playwright's `frameLocator.getByTitle(text, options?)`.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when `text` is neither a string nor a `RegExp`.
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

  /// Playwright's `frameLocator.getByTestId(testId)`.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when `testId` is neither a string nor a `RegExp`.
  #[qjs(rename = "getByTestId")]
  pub fn get_by_test_id(&self, test_id: rquickjs::Value<'_>) -> rquickjs::Result<LocatorJs> {
    let t = string_or_regex_from_js(test_id)?;
    Ok(LocatorJs::new(self.inner.get_by_test_id(t)))
  }

  #[qjs(rename = "owner")]
  #[must_use]
  pub fn owner(&self) -> LocatorJs {
    LocatorJs::new(self.inner.owner())
  }

  #[qjs(rename = "frameLocator")]
  #[must_use]
  pub fn frame_locator(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs extracts JS arguments by value; a `&T` parameter would mean a borrowed class instance"
    )]
    selector: String,
  ) -> FrameLocatorJs {
    FrameLocatorJs::new(self.inner.frame_locator(selector.as_str()))
  }

  /// Playwright's `frameLocator.first()`.
  ///
  /// # Errors
  ///
  /// Throws a `FerriError` on `frameLocator()`, which names no iframe to pick from.
  #[qjs(rename = "first")]
  pub fn first(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: Ctx<'_>,
  ) -> rquickjs::Result<FrameLocatorJs> {
    self.inner.first().map(FrameLocatorJs::new).into_js_with(&ctx)
  }

  /// Playwright's `frameLocator.last()`.
  ///
  /// # Errors
  ///
  /// Throws a `FerriError` on `frameLocator()`, which names no iframe to pick from.
  #[qjs(rename = "last")]
  pub fn last(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: Ctx<'_>,
  ) -> rquickjs::Result<FrameLocatorJs> {
    self.inner.last().map(FrameLocatorJs::new).into_js_with(&ctx)
  }

  /// Playwright's `frameLocator.nth(index)`.
  ///
  /// # Errors
  ///
  /// Throws a `FerriError` on `frameLocator()`, which names no iframe to pick from.
  #[qjs(rename = "nth")]
  pub fn nth(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: Ctx<'_>,
    index: i32,
  ) -> rquickjs::Result<FrameLocatorJs> {
    self.inner.nth(index).map(FrameLocatorJs::new).into_js_with(&ctx)
  }
}
