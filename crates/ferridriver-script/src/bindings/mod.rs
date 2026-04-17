//! QuickJS bindings for ferridriver core types.
//!
//! Wrappers live here (one module per core type) and use `rquickjs`'s own
//! `#[class]` / `#[methods]` proc macros to generate the JS FFI. Each wrapper
//! is a thin delegation to the core type.
//!
//! # Drift-detection
//!
//! Because wrappers are hand-authored, new core methods are invisible until
//! a wrapper is added. The `audit` tests (see `tests/audit.rs`) enumerate
//! every `pub` method on the wrapped core types at build time and assert
//! each has a corresponding wrapper (or is explicitly marked `#[skip]`).
//!
//! # Error mapping
//!
//! `ferridriver::FerriError` is converted to `rquickjs::Error` at every
//! binding boundary via [`convert::to_rq_error`]. The resulting JS exception
//! carries the error message and, where applicable, a `name` matching
//! Playwright's convention (`TimeoutError`, `TargetClosedError`).

pub mod api_request;
pub mod context;
pub mod convert;
pub mod locator;
pub mod page;

pub use api_request::{APIRequestContextJs, APIResponseJs};
pub use context::BrowserContextJs;
pub use locator::LocatorJs;
pub use page::PageJs;

use rquickjs::{Ctx, class::Class};
use std::sync::Arc;

/// Register every class prototype scripts can encounter so rquickjs knows how
/// to build instances when a method returns one (e.g. `APIResponse` from
/// `request.get()` or `Locator` from `page.locator()`).
fn define_classes(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  let g = ctx.globals();
  Class::<PageJs>::define(&g)?;
  Class::<LocatorJs>::define(&g)?;
  Class::<BrowserContextJs>::define(&g)?;
  Class::<APIRequestContextJs>::define(&g)?;
  Class::<APIResponseJs>::define(&g)?;
  Ok(())
}

/// Install the `page` global when a page is available on the run context.
///
/// Scripts that do not need browser interaction can run with
/// `RunContext.page = None` and simply have no `page` binding.
pub fn install_page(ctx: &Ctx<'_>, page: Arc<ferridriver::Page>) -> rquickjs::Result<()> {
  define_classes(ctx)?;
  let js_page = Class::instance(ctx.clone(), PageJs::new(page))?;
  ctx.globals().set("page", js_page)?;
  Ok(())
}

/// Install the `context` global (cookies, storage, permissions, route, etc.).
pub fn install_browser_context(ctx: &Ctx<'_>, bcx: Arc<ferridriver::context::ContextRef>) -> rquickjs::Result<()> {
  define_classes(ctx)?;
  let js_bcx = Class::instance(ctx.clone(), BrowserContextJs::new(bcx))?;
  ctx.globals().set("context", js_bcx)?;
  Ok(())
}

/// Install the `request` global (runner-side HTTP via APIRequestContext).
pub fn install_request(ctx: &Ctx<'_>, req: Arc<ferridriver::api_request::APIRequestContext>) -> rquickjs::Result<()> {
  define_classes(ctx)?;
  let js_req = Class::instance(ctx.clone(), APIRequestContextJs::new(req))?;
  ctx.globals().set("request", js_req)?;
  Ok(())
}
