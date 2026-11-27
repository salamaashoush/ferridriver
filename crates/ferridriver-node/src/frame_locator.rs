//! `FrameLocator` -- NAPI binding for `ferridriver::locator::FrameLocator`.
//!
//! Mirrors Playwright's `FrameLocator` interface
//! (`/tmp/playwright/packages/playwright-core/types/types.d.ts` —
//! search `interface FrameLocator`). A `FrameLocator` is the
//! `<iframe>` analogue of `Locator`: every getter scoped through a
//! `FrameLocator` resolves inside the iframe's document.

use crate::locator::Locator;
use crate::types::FilterOptions;
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

crate::macros::napi_locator_queries!(FrameLocator, required_frame_locator);

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

  /// Playwright: `frameLocator.owner(): Locator` — returns the
  /// `<iframe>` element this locator is scoped through.
  #[napi]
  pub fn owner(&self) -> Locator {
    Locator::wrap(self.inner.owner())
  }

  /// Playwright: `frameLocator.first(): FrameLocator`.
  #[napi]
  pub fn first(&self) -> napi::Result<FrameLocator> {
    self
      .inner
      .first()
      .map(FrameLocator::wrap)
      .map_err(crate::error::to_napi)
  }

  /// Playwright: `frameLocator.last(): FrameLocator`.
  #[napi]
  pub fn last(&self) -> napi::Result<FrameLocator> {
    self.inner.last().map(FrameLocator::wrap).map_err(crate::error::to_napi)
  }

  /// Playwright: `frameLocator.nth(index): FrameLocator`.
  #[napi]
  pub fn nth(&self, index: i32) -> napi::Result<FrameLocator> {
    self
      .inner
      .nth(index)
      .map(FrameLocator::wrap)
      .map_err(crate::error::to_napi)
  }
}
