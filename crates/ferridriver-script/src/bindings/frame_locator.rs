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

crate::bindings::macros::qjs_impl! {
  #[expect(
    clippy::needless_pass_by_value,
    reason = "rquickjs hands methods their Ctx and JS arguments by value"
  )]
  impl FrameLocatorJs {
    @mixin locator_queries(required_frame_locator);

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

    #[qjs(rename = "owner")]
    #[must_use]
    pub fn owner(&self) -> LocatorJs {
      LocatorJs::new(self.inner.owner())
    }

    /// Playwright's `frameLocator.first()`.
    ///
    /// # Errors
    ///
    /// Throws a `FerriError` on `frameLocator()`, which names no iframe to pick from.
    #[qjs(rename = "first")]
    pub fn first(&self, ctx: Ctx<'_>) -> rquickjs::Result<FrameLocatorJs> {
      self.inner.first().map(FrameLocatorJs::new).into_js_with(&ctx)
    }

    /// Playwright's `frameLocator.last()`.
    ///
    /// # Errors
    ///
    /// Throws a `FerriError` on `frameLocator()`, which names no iframe to pick from.
    #[qjs(rename = "last")]
    pub fn last(&self, ctx: Ctx<'_>) -> rquickjs::Result<FrameLocatorJs> {
      self.inner.last().map(FrameLocatorJs::new).into_js_with(&ctx)
    }

    /// Playwright's `frameLocator.nth(index)`.
    ///
    /// # Errors
    ///
    /// Throws a `FerriError` on `frameLocator()`, which names no iframe to pick from.
    #[qjs(rename = "nth")]
    pub fn nth(&self, ctx: Ctx<'_>, index: i32) -> rquickjs::Result<FrameLocatorJs> {
      self.inner.nth(index).map(FrameLocatorJs::new).into_js_with(&ctx)
    }
  }
}
