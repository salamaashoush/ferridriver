//! `TracingJs`: QuickJS binding for `context.tracing`.
//!
//! Mirrors Playwright's `Tracing` (`client/tracing.ts`). HAR recording
//! (`startHar` / `stopHar`) is fully wired; the trace `.zip` recorder
//! (`start` / `stop` / `startChunk` / `stopChunk`) surfaces the core's
//! typed Unsupported error.

use std::sync::Arc;

use ferridriver::ContextRef;
use ferridriver::tracing::{HarContentPolicy, HarMode, StartHarOptions};
use rquickjs::function::Opt;
use rquickjs::{Ctx, JsLifetime, Value, class::Trace};

use crate::bindings::convert::FerriResultCtxExt;

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Tracing")]
pub struct TracingJs {
  #[qjs(skip_trace)]
  ctx: Arc<ContextRef>,
}

impl TracingJs {
  #[must_use]
  pub fn new(ctx: Arc<ContextRef>) -> Self {
    Self { ctx }
  }
}

#[rquickjs::methods]
impl TracingJs {
  /// Playwright: `tracing.startHar(path, { content?, mode?, urlFilter?, resourcesDir? })`.
  /// A `.zip` path packs `har.har` plus attached bodies; default content
  /// policy is `attach` for `.zip`, `embed` otherwise.
  #[qjs(rename = "startHar")]
  pub async fn start_har<'js>(&self, ctx: Ctx<'js>, path: String, options: Opt<Value<'js>>) -> rquickjs::Result<()> {
    let opts = parse_start_har_options(&ctx, options)?;
    self.ctx.tracing().start_har(path, opts).await.into_js_with(&ctx)
  }

  /// Playwright: `tracing.stopHar()`.
  #[qjs(rename = "stopHar")]
  pub async fn stop_har(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    self.ctx.tracing().stop_har().await.into_js_with(&ctx)
  }

  /// Playwright: `tracing.start(options?)`. Not implemented (trace .zip).
  #[qjs(rename = "start")]
  pub async fn start(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    self.ctx.tracing().start().await.into_js_with(&ctx)
  }

  /// Playwright: `tracing.startChunk(options?)`. Not implemented.
  #[qjs(rename = "startChunk")]
  pub async fn start_chunk(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    self.ctx.tracing().start_chunk().await.into_js_with(&ctx)
  }

  /// Playwright: `tracing.stopChunk(options?)`. Not implemented.
  #[qjs(rename = "stopChunk")]
  pub async fn stop_chunk(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    self.ctx.tracing().stop_chunk().await.into_js_with(&ctx)
  }

  /// Playwright: `tracing.stop(options?)`. Not implemented.
  #[qjs(rename = "stop")]
  pub async fn stop(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    self.ctx.tracing().stop().await.into_js_with(&ctx)
  }
}

fn parse_start_har_options<'js>(ctx: &Ctx<'js>, options: Opt<Value<'js>>) -> rquickjs::Result<StartHarOptions> {
  let Some(obj) = options
    .0
    .filter(|v| !v.is_undefined() && !v.is_null())
    .and_then(rquickjs::Value::into_object)
  else {
    return Ok(StartHarOptions::default());
  };
  let content = match obj.get::<_, Option<String>>("content")?.as_deref() {
    Some("embed") => Some(HarContentPolicy::Embed),
    Some("attach") => Some(HarContentPolicy::Attach),
    Some("omit") => Some(HarContentPolicy::Omit),
    _ => None,
  };
  let mode = match obj.get::<_, Option<String>>("mode")?.as_deref() {
    Some("full") => Some(HarMode::Full),
    Some("minimal") => Some(HarMode::Minimal),
    _ => None,
  };
  let url_filter = match obj.get::<_, Option<Value<'js>>>("urlFilter")? {
    Some(v) if !v.is_undefined() && !v.is_null() => Some(crate::bindings::page::options::url_value_to_matcher(ctx, v)?),
    _ => None,
  };
  let resources_dir = obj
    .get::<_, Option<String>>("resourcesDir")?
    .map(std::path::PathBuf::from);
  Ok(StartHarOptions {
    content,
    mode,
    url_filter,
    resources_dir,
  })
}
