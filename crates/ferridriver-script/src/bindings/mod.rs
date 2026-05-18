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
pub mod artifacts;
pub mod bdd;
pub mod browser;
pub mod browser_type;
pub mod console_message;
pub mod context;
pub mod convert;
pub mod dialog;
pub mod download;
pub mod element_handle;
pub mod file_chooser;
pub mod frame;
pub mod frame_locator;
pub mod js_handle;
pub mod keyboard;
pub mod locator;
pub mod mouse;
pub mod network;
pub mod page;
pub mod plugins;
pub mod video;
pub mod web_error;
pub mod webapi;

pub use api_request::{APIRequestContextJs, APIResponseJs};
pub use artifacts::ArtifactsJs;
pub use bdd::{
  CollectedRegistry, ScenarioWorld, StepOutcome, collect_registry, evaluate_module, install_bdd, invoke_hook,
  invoke_step, reset_world, set_scenario_world,
};
pub use browser::BrowserJs;
pub use browser_type::{BrowserTypeJs, install_browser_type};
pub use console_message::ConsoleMessageJs;
pub use context::BrowserContextJs;
pub use dialog::DialogJs;
pub use download::DownloadJs;
pub use element_handle::ElementHandleJs;
pub use file_chooser::FileChooserJs;
pub use frame::FrameJs;
pub use frame_locator::FrameLocatorJs;
pub use js_handle::JSHandleJs;
pub use keyboard::KeyboardJs;
pub use locator::LocatorJs;
pub use mouse::MouseJs;
pub use network::{RequestJs, ResponseJs, RouteJs, WebSocketJs};
pub use page::PageJs;
pub use plugins::{PluginBinding, PluginCommandsJs, PluginToolBinding, compile_plugin_bytecode, install_plugins};
pub use video::VideoJs;
pub use web_error::WebErrorJs;

use rquickjs::{AsyncContext, Ctx, class::Class};
use std::sync::Arc;

/// Register every class prototype scripts can encounter so rquickjs knows how
/// to build instances when a method returns one (e.g. `APIResponse` from
/// `request.get()` or `Locator` from `page.locator()`).
///
/// Prototype registration is idempotent and session-stable: callers
/// invoke this ONCE at `Session::create`, not per `execute`. The
/// per-call `install_*` helpers below only build the live instance.
pub fn define_classes(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  let g = ctx.globals();
  Class::<PageJs>::define(&g)?;
  Class::<FrameJs>::define(&g)?;
  Class::<LocatorJs>::define(&g)?;
  Class::<BrowserContextJs>::define(&g)?;
  Class::<BrowserJs>::define(&g)?;
  Class::<APIRequestContextJs>::define(&g)?;
  Class::<APIResponseJs>::define(&g)?;
  Class::<KeyboardJs>::define(&g)?;
  Class::<MouseJs>::define(&g)?;
  Class::<ArtifactsJs>::define(&g)?;
  Class::<JSHandleJs>::define(&g)?;
  Class::<ElementHandleJs>::define(&g)?;
  Class::<RequestJs>::define(&g)?;
  Class::<ResponseJs>::define(&g)?;
  Class::<RouteJs>::define(&g)?;
  Class::<WebSocketJs>::define(&g)?;
  Class::<DialogJs>::define(&g)?;
  Class::<FileChooserJs>::define(&g)?;
  Class::<DownloadJs>::define(&g)?;
  Class::<ConsoleMessageJs>::define(&g)?;
  Class::<WebErrorJs>::define(&g)?;
  Class::<VideoJs>::define(&g)?;
  Class::<BrowserTypeJs>::define(&g)?;
  Class::<FrameLocatorJs>::define(&g)?;
  Class::<crate::bindings::page::TouchscreenJs>::define(&g)?;
  Ok(())
}

/// Install the `page` global when a page is available on the run context.
///
/// `async_ctx` is the `AsyncContext` driving the script — `PageJs`
/// captures a clone so `page.route(matcher, fn)` can dispatch the JS
/// callback back into the same context from a backend route handler
/// (which runs on a separate tokio task, outside the script's
/// `async_with` block).
///
/// Scripts that do not need browser interaction can run with
/// `RunContext.page = None` and simply have no `page` binding.
pub fn install_page(ctx: &Ctx<'_>, page: Arc<ferridriver::Page>, async_ctx: AsyncContext) -> rquickjs::Result<()> {
  install_page_on(ctx, &ctx.globals(), page, async_ctx)
}

/// Install the `page` binding onto an arbitrary target object.
///
/// This is the single implementation; scripting passes `ctx.globals()`
/// (so bare `page.goto(...)` keeps working) and the BDD layer passes a
/// per-scenario World object (so cucumber `this.page` resolves to that
/// scenario's fixtures). One binding, two install targets — no
/// duplicate `PageJs` wiring.
pub fn install_page_on<'js>(
  ctx: &Ctx<'js>,
  target: &rquickjs::Object<'js>,
  page: Arc<ferridriver::Page>,
  async_ctx: AsyncContext,
) -> rquickjs::Result<()> {
  let js_page = Class::instance(ctx.clone(), PageJs::new_with_async_ctx(page, async_ctx))?;
  target.set("page", js_page)?;
  // Per-page route handler registry (`Map<id, fn>`) used by
  // `page.route(matcher, fn)` to look up callbacks from cross-task
  // dispatch. Always lives on `globalThis` (route dispatch re-enters
  // the context and looks it up there) regardless of the binding
  // target. Idempotent `||=`: never wipes an existing registry.
  ctx.eval::<(), _>(b"globalThis.__fdRoutes ||= new Map(); globalThis.__fdRoutePreds ||= new Map();".as_slice())?;
  Ok(())
}

/// Install the `context` global (cookies, storage, permissions, route, etc.).
pub fn install_browser_context(ctx: &Ctx<'_>, bcx: Arc<ferridriver::context::ContextRef>) -> rquickjs::Result<()> {
  install_browser_context_on(ctx, &ctx.globals(), bcx)
}

/// `context` binding onto an arbitrary target (see [`install_page_on`]).
pub fn install_browser_context_on<'js>(
  ctx: &Ctx<'js>,
  target: &rquickjs::Object<'js>,
  bcx: Arc<ferridriver::context::ContextRef>,
) -> rquickjs::Result<()> {
  let js_bcx = Class::instance(ctx.clone(), BrowserContextJs::new(bcx))?;
  target.set("context", js_bcx)?;
  Ok(())
}

/// Install the `browser` global — exposes `browser.newContext(options?)`
/// so scripts can construct fresh contexts with the full Playwright
/// [`ferridriver::options::BrowserContextOptions`] bag. Rule-9 tests
/// for §4.1 consume this entry point.
pub fn install_browser(ctx: &Ctx<'_>, browser: Arc<ferridriver::Browser>) -> rquickjs::Result<()> {
  install_browser_on(ctx, &ctx.globals(), browser)
}

/// `browser` binding onto an arbitrary target (see [`install_page_on`]).
pub fn install_browser_on<'js>(
  ctx: &Ctx<'js>,
  target: &rquickjs::Object<'js>,
  browser: Arc<ferridriver::Browser>,
) -> rquickjs::Result<()> {
  let js_browser = Class::instance(ctx.clone(), BrowserJs::new(browser))?;
  target.set("browser", js_browser)?;
  Ok(())
}

/// Install the `request` global (runner-side HTTP via APIRequestContext).
pub fn install_request(ctx: &Ctx<'_>, req: Arc<ferridriver::api_request::APIRequestContext>) -> rquickjs::Result<()> {
  install_request_on(ctx, &ctx.globals(), req)
}

/// `request` binding onto an arbitrary target (see [`install_page_on`]).
pub fn install_request_on<'js>(
  ctx: &Ctx<'js>,
  target: &rquickjs::Object<'js>,
  req: Arc<ferridriver::api_request::APIRequestContext>,
) -> rquickjs::Result<()> {
  let js_req = Class::instance(ctx.clone(), APIRequestContextJs::new(req))?;
  target.set("request", js_req)?;
  Ok(())
}

/// Install the `artifacts` global — a dedicated sandboxed directory for
/// script outputs (screenshots, PDFs, traces, downloaded bodies).
pub fn install_artifacts(ctx: &Ctx<'_>, sandbox: Arc<crate::fs::PathSandbox>) -> rquickjs::Result<()> {
  let js_art = Class::instance(ctx.clone(), ArtifactsJs::new(sandbox))?;
  ctx.globals().set("artifacts", js_art)?;
  Ok(())
}
