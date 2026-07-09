//! `FrameLocator` -- NAPI binding for `ferridriver::locator::FrameLocator`.
//!
//! Mirrors Playwright's `FrameLocator` interface
//! (`/tmp/playwright/packages/playwright-core/types/types.d.ts` —
//! search `interface FrameLocator`). A `FrameLocator` is the
//! `<iframe>` analogue of `Locator`: every getter scoped through a
//! `FrameLocator` resolves inside the iframe's document.

use crate::locator::Locator;
use crate::types::{FilterOptions, RoleOptions, TextOptions};
use napi_derive::napi;

/// Lazy locator scoped to the contents of an `<iframe>`. Construct via
/// [`crate::page::Page::frame_locator`],
/// [`crate::frame::Frame::frame_locator`],
/// [`crate::locator::Locator::frame_locator`], or
/// [`crate::locator::Locator::content_frame`].
#[napi]
pub struct FrameLocator {
  inner: ferridriver::locator::FrameLocator,
}

impl FrameLocator {
  pub(crate) fn wrap(inner: ferridriver::locator::FrameLocator) -> Self {
    Self { inner }
  }
}

#[napi]
impl FrameLocator {
  /// Playwright: `frameLocator.locator(selectorOrLocator, options?): Locator`.
  #[napi(ts_args_type = "selectorOrLocator: string | Locator, options?: FilterOptions")]
  pub fn locator(
    &self,
    selector_or_locator: napi::Either<String, crate::types::LocatorRef>,
    options: Option<FilterOptions>,
  ) -> Locator {
    // FrameLocator's `locator` takes a string selector; lower the
    // string-or-Locator union by extracting the underlying selector.
    let selector = match selector_or_locator {
      napi::Either::A(s) => s,
      napi::Either::B(loc) => loc.selector,
    };
    let opts = options.map(ferridriver::options::FilterOptions::from);
    Locator::wrap(match opts {
      Some(f) => self.inner.locator_with(&selector, &f),
      None => self.inner.locator(&selector),
    })
  }

  /// Playwright: `frameLocator.getByRole(role, options?): Locator`.
  #[napi]
  pub fn get_by_role(&self, role: String, options: Option<RoleOptions>) -> Locator {
    let opts = options.map(ferridriver::options::RoleOptions::from);
    Locator::wrap(self.inner.get_by_role(role.as_str()).maybe_options(opts).into_locator())
  }

  #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
  pub fn get_by_text(
    &self,
    text: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<TextOptions>,
  ) -> Locator {
    let opts = options.map(ferridriver::options::TextOptions::from);
    Locator::wrap(
      self
        .inner
        .get_by_text(crate::types::getby_input_to_rust(text))
        .maybe_options(opts)
        .into_locator(),
    )
  }

  #[napi(ts_args_type = "testId: string | RegExp")]
  pub fn get_by_test_id(&self, test_id: napi::Either<String, crate::types::JsRegExpLike>) -> Locator {
    Locator::wrap(self.inner.get_by_test_id(crate::types::getby_input_to_rust(test_id)))
  }

  #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
  pub fn get_by_label(
    &self,
    text: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<TextOptions>,
  ) -> Locator {
    let opts = options.map(ferridriver::options::TextOptions::from);
    Locator::wrap(
      self
        .inner
        .get_by_label(crate::types::getby_input_to_rust(text))
        .maybe_options(opts)
        .into_locator(),
    )
  }

  #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
  pub fn get_by_placeholder(
    &self,
    text: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<TextOptions>,
  ) -> Locator {
    let opts = options.map(ferridriver::options::TextOptions::from);
    Locator::wrap(
      self
        .inner
        .get_by_placeholder(crate::types::getby_input_to_rust(text))
        .maybe_options(opts)
        .into_locator(),
    )
  }

  #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
  pub fn get_by_alt_text(
    &self,
    text: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<TextOptions>,
  ) -> Locator {
    let opts = options.map(ferridriver::options::TextOptions::from);
    Locator::wrap(
      self
        .inner
        .get_by_alt_text(crate::types::getby_input_to_rust(text))
        .maybe_options(opts)
        .into_locator(),
    )
  }

  #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
  pub fn get_by_title(
    &self,
    text: napi::Either<String, crate::types::JsRegExpLike>,
    options: Option<TextOptions>,
  ) -> Locator {
    let opts = options.map(ferridriver::options::TextOptions::from);
    Locator::wrap(
      self
        .inner
        .get_by_title(crate::types::getby_input_to_rust(text))
        .maybe_options(opts)
        .into_locator(),
    )
  }

  /// Playwright: `frameLocator.owner(): Locator` — returns the
  /// `<iframe>` element this locator is scoped through.
  #[napi]
  pub fn owner(&self) -> Locator {
    Locator::wrap(self.inner.owner())
  }

  /// Nested-frame chain: `frameLocator.frameLocator(selector)`.
  #[napi]
  pub fn frame_locator(&self, selector: String) -> FrameLocator {
    FrameLocator::wrap(self.inner.frame_locator(&selector))
  }

  /// Playwright: `frameLocator.first(): FrameLocator`.
  #[napi]
  pub fn first(&self) -> FrameLocator {
    FrameLocator::wrap(self.inner.first())
  }

  /// Playwright: `frameLocator.last(): FrameLocator`.
  #[napi]
  pub fn last(&self) -> FrameLocator {
    FrameLocator::wrap(self.inner.last())
  }

  /// Playwright: `frameLocator.nth(index): FrameLocator`.
  #[napi]
  pub fn nth(&self, index: i32) -> FrameLocator {
    FrameLocator::wrap(self.inner.nth(index))
  }
}
