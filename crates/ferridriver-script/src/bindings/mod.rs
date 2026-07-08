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

pub mod abort;
pub mod artifacts;
pub mod bdd;
pub mod blob;
pub mod browser;
pub mod browser_type;
pub mod console_message;
pub mod context;
pub mod convert;
pub mod crypto;
pub mod dialog;
pub mod disposable;
pub mod download;
pub mod element_handle;
pub mod expect;
pub mod fetch;
pub mod file_chooser;
pub mod form_data;
pub mod frame;
pub mod frame_locator;
pub mod http_client;
pub mod js_handle;
pub mod keyboard;
pub mod locator;
pub mod mouse;
pub mod native_modules;
pub mod network;
pub mod node_compat;
pub mod page;
pub mod plugins;
pub mod process;
pub mod runtime;
pub mod sidecars;
pub mod streams;
pub mod timers;
pub mod tracing;
pub mod url_search_params;
pub mod video;
pub mod web_error;
pub mod web_socket_route;
pub mod web_storage;
pub mod webapi;

pub use artifacts::ArtifactsJs;
pub use bdd::{
  CollectedAllow, CollectedRegistry, CollectedTool, HookArg, JsArg, ScenarioWorld, ScriptAttachment, StepOutcome,
  collect_registry, drain_attachments, install_bdd, invoke_hook, invoke_step, reset_world, set_scenario_world,
  tools_len, tools_snapshot,
};
pub use browser::BrowserJs;
pub use browser_type::{BrowserTypeJs, install_browser_type};
pub use console_message::ConsoleMessageJs;
pub use context::BrowserContextJs;
pub use dialog::DialogJs;
pub use disposable::DisposableJs;
pub use download::DownloadJs;
pub use element_handle::ElementHandleJs;
pub use file_chooser::FileChooserJs;
pub use frame::FrameJs;
pub use frame_locator::FrameLocatorJs;
pub use http_client::{HttpClientJs, HttpResponseJs};
pub use js_handle::JSHandleJs;
pub use keyboard::KeyboardJs;
pub use locator::LocatorJs;
pub use mouse::MouseJs;
pub use network::{RequestJs, ResponseJs, RouteJs, WebSocketJs};
pub use page::PageJs;
pub use plugins::{PluginBinding, PluginCommandsJs, install_plugins, invoke_tool_by_name};
pub use sidecars::{SidecarJs, SidecarsJs, install_sidecars};
pub use video::VideoJs;
pub use web_error::WebErrorJs;

use rquickjs::{Ctx, class::Class};
use std::sync::Arc;

/// Register every class prototype scripts can encounter so rquickjs knows how
/// to build instances when a method returns one (e.g. `HttpResponse` from
/// `request.get()` or `Locator` from `page.locator()`).
///
/// Prototype registration is idempotent and session-stable: callers
/// invoke this ONCE at `Session::create`, not per `execute`. The
/// per-call `install_*` helpers below only build the live instance.
pub fn define_classes<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<()> {
  let g = ctx.globals();
  Class::<PageJs>::define(&g)?;
  Class::<FrameJs>::define(&g)?;
  Class::<LocatorJs>::define(&g)?;
  Class::<BrowserContextJs>::define(&g)?;
  Class::<BrowserJs>::define(&g)?;
  Class::<HttpClientJs>::define(&g)?;
  Class::<HttpResponseJs>::define(&g)?;
  Class::<KeyboardJs>::define(&g)?;
  Class::<MouseJs>::define(&g)?;
  Class::<ArtifactsJs>::define(&g)?;
  Class::<JSHandleJs>::define(&g)?;
  Class::<ElementHandleJs>::define(&g)?;
  // Playwright page-network `Request`/`Response` are NOT globalised
  // (Playwright itself never puts them on globalThis — they are only
  // ever return values; `Class::instance` registers their prototype
  // lazily, so `page.on('response', r => r.status())` still works). The
  // bare `Request`/`Response` globals belong to the WHATWG fetch
  // classes below.
  Class::<RouteJs>::define(&g)?;
  Class::<WebSocketJs>::define(&g)?;
  Class::<DialogJs>::define(&g)?;
  Class::<FileChooserJs>::define(&g)?;
  Class::<DownloadJs>::define(&g)?;
  Class::<DisposableJs>::define(&g)?;
  Class::<ConsoleMessageJs>::define(&g)?;
  Class::<WebErrorJs>::define(&g)?;
  Class::<VideoJs>::define(&g)?;
  Class::<BrowserTypeJs>::define(&g)?;
  Class::<FrameLocatorJs>::define(&g)?;
  Class::<crate::bindings::page::TouchscreenJs>::define(&g)?;
  Class::<crate::bindings::fetch::HeadersJs>::define(&g)?;
  Class::<crate::bindings::fetch::FetchResponseJs>::define(&g)?;
  Class::<crate::bindings::fetch::FetchRequestJs>::define(&g)?;
  Class::<crate::bindings::abort::AbortControllerJs<'js>>::define(&g)?;
  Class::<crate::bindings::abort::AbortSignalJs<'js>>::define(&g)?;
  Class::<crate::bindings::streams::ReadableStreamJs>::define(&g)?;
  Class::<crate::bindings::streams::ReadableStreamDefaultReaderJs>::define(&g)?;
  Class::<crate::bindings::streams::ReadableStreamDefaultControllerJs>::define(&g)?;
  Class::<crate::bindings::blob::BlobJs>::define(&g)?;
  Class::<crate::bindings::form_data::FormDataJs>::define(&g)?;
  Ok(())
}

/// Install the `page` global when a page is available on the run context.
///
/// `vm` is the session's VM-loop handle — `PageJs` captures a clone so
/// `page.route(matcher, fn)` can dispatch the JS callback back into the
/// VM from a backend route handler (which runs on a separate tokio
/// task, outside the VM event loop).
///
/// Scripts that do not need browser interaction can run with
/// `RunContext.page = None` and simply have no `page` binding.
pub fn install_page(ctx: &Ctx<'_>, page: Arc<ferridriver::Page>, vm: crate::vm::VmHandle) -> rquickjs::Result<()> {
  install_page_on(ctx, &ctx.globals(), page, vm)?;
  crate::bindings::runtime::mirror_global(ctx, "page")
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
  vm: crate::vm::VmHandle,
) -> rquickjs::Result<()> {
  let js_page = Class::instance(ctx.clone(), PageJs::new_with_vm(page, vm))?;
  target.set("page", js_page)?;
  // Native page-callbacks registry (context userdata): route handlers,
  // exposeFunction callbacks, screencast — all cross-task dispatched.
  // Idempotent; independent of the binding target.
  page::ensure_page_callbacks(ctx);
  Ok(())
}

/// Install the `context` global (cookies, storage, permissions, route, etc.).
pub fn install_browser_context(ctx: &Ctx<'_>, bcx: Arc<ferridriver::context::ContextRef>) -> rquickjs::Result<()> {
  install_browser_context_on(ctx, &ctx.globals(), bcx)?;
  crate::bindings::runtime::mirror_global(ctx, "context")
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
  install_browser_on(ctx, &ctx.globals(), browser)?;
  crate::bindings::runtime::mirror_global(ctx, "browser")
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

/// Install the `request` global (runner-side HTTP via HttpClient).
pub fn install_request(ctx: &Ctx<'_>, req: Arc<ferridriver::http_client::HttpClient>) -> rquickjs::Result<()> {
  install_request_on(ctx, &ctx.globals(), req)?;
  crate::bindings::runtime::mirror_global(ctx, "request")
}

/// `request` binding onto an arbitrary target (see [`install_page_on`]).
pub fn install_request_on<'js>(
  ctx: &Ctx<'js>,
  target: &rquickjs::Object<'js>,
  req: Arc<ferridriver::http_client::HttpClient>,
) -> rquickjs::Result<()> {
  let js_req = Class::instance(ctx.clone(), HttpClientJs::new(req))?;
  target.set("request", js_req)?;
  Ok(())
}

/// Install the `artifacts` global — a dedicated sandboxed directory for
/// script outputs (screenshots, PDFs, traces, downloaded bodies).
pub fn install_artifacts(ctx: &Ctx<'_>, sandbox: Arc<crate::fs::PathSandbox>) -> rquickjs::Result<()> {
  let js_art = Class::instance(ctx.clone(), ArtifactsJs::new(sandbox))?;
  ctx.globals().set("artifacts", js_art)?;
  crate::bindings::runtime::mirror_global(ctx, "artifacts")?;
  Ok(())
}
