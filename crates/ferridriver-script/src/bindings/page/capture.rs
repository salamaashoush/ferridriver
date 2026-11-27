//! Capture, audit, snapshot, tracing, and developer-tool helpers for `PageJs`.

use rquickjs::class::Class;
use rquickjs::function::Opt;
use rquickjs::{Ctx, IntoJs, Object, Value};

use super::PageJs;
use super::options::{JsAccessibilityOptions, JsPageQualityOptions, parse_pdf_options, parse_screenshot_options};
use crate::bindings::convert::{FerriResultCtxExt, i64_to_f64, json_to_js, serde_from_js, serde_to_js};

impl PageJs {
  pub(super) async fn check_accessibility_impl<'js>(
    &self,
    ctx: &Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let parsed: JsAccessibilityOptions = match options.into_inner() {
      Some(value) if !value.is_undefined() && !value.is_null() => serde_from_js(ctx, value)?,
      _ => JsAccessibilityOptions::default(),
    };
    let report = self
      .inner
      .check_accessibility()
      .options(ferridriver::accessibility::AccessibilityOptions {
        include: parsed.include.unwrap_or_default(),
        exclude: parsed.exclude.unwrap_or_default(),
        tags: parsed.tags.unwrap_or_default(),
      })
      .await
      .into_js_with(ctx)?;
    serde_to_js(ctx, &report)
  }

  pub(super) async fn developer_tools_impl<'js>(&self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let groups = self.inner.developer_tools().await.into_js_with(ctx)?;
    serde_to_js(ctx, &groups)
  }

  pub(super) async fn execute_developer_tool_impl<'js>(
    &self,
    ctx: &Ctx<'js>,
    name: &str,
    params: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let parsed: Option<serde_json::Value> = match params.into_inner() {
      Some(value) if !value.is_undefined() && !value.is_null() => Some(serde_from_js(ctx, value)?),
      _ => None,
    };
    let result = self
      .inner
      .execute_developer_tool(name, parsed)
      .await
      .into_js_with(ctx)?;
    json_to_js(ctx, &result)
  }

  pub(super) async fn take_heap_snapshot_impl<'js>(&self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let snapshot = self.inner.take_heap_snapshot().await.into_js_with(ctx)?;
    let instance = Class::instance(ctx.clone(), crate::bindings::heap::HeapSnapshotJs::new(snapshot))?;
    instance.into_js(ctx)
  }

  pub(super) async fn check_page_quality_impl<'js>(
    &self,
    ctx: &Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let parsed: JsPageQualityOptions = match options.into_inner() {
      Some(value) if !value.is_undefined() && !value.is_null() => serde_from_js(ctx, value)?,
      _ => JsPageQualityOptions::default(),
    };
    let report = self
      .inner
      .check_page_quality()
      .options(ferridriver::audits::PageQualityOptions {
        only: parsed.only.unwrap_or_default(),
      })
      .await
      .into_js_with(ctx)?;
    serde_to_js(ctx, &report)
  }

  pub(super) async fn screenshot_impl<'js>(
    &self,
    ctx: &Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Vec<u8>> {
    let opts = parse_screenshot_options(ctx, options)?;
    self.inner.screenshot().options(opts).await.into_js_with(ctx)
  }

  pub(super) async fn pdf_impl<'js>(&self, ctx: &Ctx<'js>, options: Opt<Value<'js>>) -> rquickjs::Result<Vec<u8>> {
    let opts = parse_pdf_options(ctx, options)?;
    self.inner.pdf().options(opts).await.into_js_with(ctx)
  }

  pub(super) async fn start_tracing_impl<'js>(
    &self,
    ctx: &Ctx<'js>,
    categories: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    let categories = match categories.into_inner() {
      Some(value) if !value.is_undefined() && !value.is_null() => Some(serde_from_js::<Vec<String>>(ctx, value)?),
      _ => None,
    };
    self.inner.start_tracing(categories.as_deref()).await.into_js_with(ctx)
  }

  pub(super) async fn stop_tracing_impl<'js>(&self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let events = self.inner.stop_tracing().await.into_js_with(ctx)?;
    serde_to_js(ctx, &events)
  }

  pub(super) async fn metrics_impl<'js>(&self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let metrics = self.inner.metrics().await.into_js_with(ctx)?;
    serde_to_js(ctx, &metrics)
  }

  pub(super) async fn snapshot_for_ai_impl<'js>(
    &self,
    ctx: &Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let core_opts = match options.0 {
      None => ferridriver::snapshot::SnapshotOptions::default(),
      Some(v) if v.is_undefined() || v.is_null() => ferridriver::snapshot::SnapshotOptions::default(),
      Some(v) => {
        #[derive(serde::Deserialize, Default)]
        #[serde(rename_all = "camelCase", default)]
        struct JsSnap {
          depth: Option<i32>,
          track: Option<String>,
        }
        let parsed: JsSnap = serde_from_js(ctx, v)?;
        ferridriver::snapshot::SnapshotOptions {
          depth: parsed.depth,
          track: parsed.track,
        }
      },
    };
    let snap = self
      .inner
      .snapshot_for_ai()
      .options(core_opts)
      .await
      .into_js_with(ctx)?;
    let obj = Object::new(ctx.clone())?;
    obj.set("full", snap.full)?;
    if let Some(inc) = snap.incremental {
      obj.set("incremental", inc)?;
    }
    let ref_map = Object::new(ctx.clone())?;
    for (k, v) in snap.ref_map {
      ref_map.set(k, i64_to_f64(v))?;
    }
    obj.set("refMap", ref_map)?;
    obj.into_js(ctx)
  }

  pub(super) async fn aria_snapshot_impl<'js>(
    &self,
    ctx: &Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<String> {
    let core_opts = crate::bindings::locator::aria_snapshot_options(ctx, options)?;
    self.inner.aria_snapshot().options(core_opts).await.into_js_with(ctx)
  }

  pub(super) async fn aria_snapshot_json_impl<'js>(
    &self,
    ctx: &Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let core_opts = crate::bindings::locator::aria_snapshot_options(ctx, options)?;
    let json = self
      .inner
      .aria_snapshot_json()
      .options(core_opts)
      .await
      .into_js_with(ctx)?;
    json_to_js(ctx, &serde_json::Value::Array(json))
  }
}
