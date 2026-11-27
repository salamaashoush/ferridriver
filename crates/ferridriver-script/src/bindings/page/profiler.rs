//! Profiler, coverage, screencast, locator handler, and touchscreen helpers for
//! `PageJs`.

use std::sync::Arc;

use rquickjs::function::Opt;
use rquickjs::{Ctx, Function, Value};

use super::PageJs;
use super::callbacks::{PageCallbacks, SavedCallback, with_page_callbacks};
use crate::bindings::convert::{FerriResultCtxExt, serde_from_js, throw_named};
use crate::bindings::locator::LocatorJs;

impl PageJs {
  pub(super) async fn start_js_coverage_impl<'js>(
    &self,
    ctx: &Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let options = match options.into_inner() {
      Some(value) if !value.is_undefined() && !value.is_null() => serde_from_js(ctx, value)?,
      _ => serde_json::json!({}),
    };
    let context = self
      .inner
      .context()
      .ok_or_else(|| ferridriver::FerriError::unsupported("JavaScript coverage requires a browser context"))
      .into_js_with(ctx)?;
    let mut coverage = self.coverage_session.lock().await;
    if coverage.is_some() {
      return Err(throw_named(
        ctx,
        "Error",
        "page.startJSCoverage: JavaScript coverage is already started",
      ));
    }
    let session = context.new_cdp_session(&self.inner).await.into_js_with(ctx)?;
    session
      .send("Profiler.enable", serde_json::json!({}))
      .await
      .into_js_with(ctx)?;
    let result = session
      .send(
        "Profiler.startPreciseCoverage",
        serde_json::json!({
          "callCount": options.get("callCount").and_then(serde_json::Value::as_bool).unwrap_or(false),
          "detailed": options.get("detailed").and_then(serde_json::Value::as_bool).unwrap_or(true),
          "allowTriggeredUpdates": options
            .get("allowTriggeredUpdates")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        }),
      )
      .await
      .into_js_with(ctx)?;
    *coverage = Some(session);
    crate::bindings::convert::json_to_js(ctx, &result)
  }

  pub(super) async fn take_js_coverage_impl<'js>(&self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let coverage = self.coverage_session.lock().await;
    let Some(session) = coverage.as_ref() else {
      return Err(throw_named(
        ctx,
        "Error",
        "page.takeJSCoverage: JavaScript coverage has not been started",
      ));
    };
    let result = session
      .send("Profiler.takePreciseCoverage", serde_json::json!({}))
      .await
      .into_js_with(ctx)?;
    crate::bindings::convert::json_to_js(ctx, &result)
  }

  pub(super) async fn stop_js_coverage_impl<'js>(&self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let mut coverage = self.coverage_session.lock().await;
    let Some(session) = coverage.take() else {
      return Err(throw_named(
        ctx,
        "Error",
        "page.stopJSCoverage: JavaScript coverage has not been started",
      ));
    };
    let result = session
      .send("Profiler.stopPreciseCoverage", serde_json::json!({}))
      .await
      .into_js_with(ctx)?;
    session
      .send("Profiler.disable", serde_json::json!({}))
      .await
      .into_js_with(ctx)?;
    crate::bindings::convert::json_to_js(ctx, &result)
  }

  pub(super) async fn start_cpu_profile_impl<'js>(&self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let context = self
      .inner
      .context()
      .ok_or_else(|| ferridriver::FerriError::unsupported("CPU profiling requires a browser context"))
      .into_js_with(ctx)?;
    let mut profiler = self.cpu_profile_session.lock().await;
    if profiler.is_some() {
      return Err(throw_named(
        ctx,
        "Error",
        "page.startCPUProfile: CPU profiling is already started",
      ));
    }
    let session = context.new_cdp_session(&self.inner).await.into_js_with(ctx)?;
    session
      .send("Profiler.enable", serde_json::json!({}))
      .await
      .into_js_with(ctx)?;
    let result = session
      .send("Profiler.start", serde_json::json!({}))
      .await
      .into_js_with(ctx)?;
    *profiler = Some(session);
    crate::bindings::convert::json_to_js(ctx, &result)
  }

  pub(super) async fn stop_cpu_profile_impl<'js>(&self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let mut profiler = self.cpu_profile_session.lock().await;
    let Some(session) = profiler.take() else {
      return Err(throw_named(
        ctx,
        "Error",
        "page.stopCPUProfile: CPU profiling has not been started",
      ));
    };
    let result = session
      .send("Profiler.stop", serde_json::json!({}))
      .await
      .into_js_with(ctx)?;
    session
      .send("Profiler.disable", serde_json::json!({}))
      .await
      .into_js_with(ctx)?;
    crate::bindings::convert::json_to_js(ctx, &result)
  }

  pub(super) fn add_locator_handler_impl<'js>(
    &self,
    ctx: &Ctx<'js>,
    locator: &rquickjs::Class<'js, LocatorJs>,
    handler: Function<'js>,
    options: &Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    let mut times: Option<u32> = None;
    let mut no_wait_after = false;
    if let Some(bag) = options.0.as_ref().and_then(Value::as_object) {
      if let Ok(Some(t)) = bag.get::<_, Option<f64>>("times") {
        times = Some(crate::bindings::convert::f64_to_u32(t));
      }
      if let Ok(Some(n)) = bag.get::<_, Option<bool>>("noWaitAfter") {
        no_wait_after = n;
      }
    }
    let vm = self.vm.clone().ok_or_else(|| {
      throw_named(
        ctx,
        "Error",
        "page.addLocatorHandler requires the script engine's VM handle (install_page)",
      )
    })?;
    let id = with_page_callbacks(ctx, PageCallbacks::next_route_id)?;
    let saved = SavedCallback::save(ctx, handler);
    with_page_callbacks(ctx, |r| r.insert_locator_handler(id, saved))?;

    let core_locator = locator.borrow().inner_ref().clone();
    let rust_handler: ferridriver::locator_handler::LocatorHandlerFn = Arc::new(move |loc| {
      let vm = vm.clone();
      Box::pin(async move {
        let (tx, rx) = tokio::sync::oneshot::channel::<ferridriver::error::Result<()>>();
        tokio::spawn(async move {
          use rquickjs::class::Class;
          let result: Result<rquickjs::Result<()>, crate::error::ScriptError> = crate::vm_with!(vm => |ctx| {
            let f = with_page_callbacks(&ctx, |r| r.get_locator_handler(id))?.ok_or_else(|| {
              throw_named(&ctx, "Error", "page.addLocatorHandler: locator handler gone")
            })?;
            let loc_class = Class::instance(ctx.clone(), LocatorJs::new(loc))?;
            let _: Value<'_> = f.call_bracketed_async(&ctx, (loc_class,)).await?;
            Ok(())
          })
          .await;
          let mapped = match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(ferridriver::error::FerriError::evaluation(format!(
              "locator handler failed: {e}"
            ))),
            Err(e) => Err(ferridriver::error::FerriError::evaluation(format!(
              "locator handler dispatch failed: {}",
              e.message
            ))),
          };
          let _ = tx.send(mapped);
        });
        match rx.await {
          Ok(r) => r,
          Err(_) => Err(ferridriver::error::FerriError::evaluation(
            "locator handler dispatch task dropped",
          )),
        }
      })
    });

    self
      .inner
      .add_locator_handler(&core_locator, rust_handler, times, no_wait_after)
      .into_js_with(ctx)?;
    self
      .locator_handler_ids
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .entry(core_locator.selector().to_string())
      .or_default()
      .push(id);
    Ok(())
  }

  pub(super) fn remove_locator_handler_impl<'js>(
    &self,
    ctx: &Ctx<'js>,
    locator: &rquickjs::Class<'js, LocatorJs>,
  ) -> rquickjs::Result<()> {
    let core_locator = locator.borrow().inner_ref().clone();
    self.inner.remove_locator_handler(&core_locator);
    let ids = self
      .locator_handler_ids
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .remove(core_locator.selector())
      .unwrap_or_default();
    with_page_callbacks(ctx, |r| {
      for id in ids {
        r.remove_locator_handler(id);
      }
    })?;
    Ok(())
  }

  pub(super) fn start_screencast_impl<'js>(
    &self,
    ctx: Ctx<'js>,
    quality: u8,
    max_width: u32,
    max_height: u32,
    callback: Function<'js>,
  ) -> rquickjs::Result<rquickjs::promise::Promised<impl std::future::Future<Output = rquickjs::Result<()>> + 'js>> {
    let saved = SavedCallback::save(&ctx, callback);
    with_page_callbacks(&ctx, |r| r.screencast = Some(saved))?;
    let inner = Arc::clone(&self.inner);
    Ok(rquickjs::promise::Promised::from(async move {
      let mut rx = inner
        .start_screencast(quality, max_width, max_height)
        .await
        .into_js_with(&ctx)?;
      let pump_ctx = ctx.clone();
      ctx.spawn(async move {
        while let Some((mut bytes, mut ts)) = rx.recv().await {
          while let Ok((b, t)) = rx.try_recv() {
            bytes = b;
            ts = t;
          }
          let Ok(Some(saved)) = with_page_callbacks(&pump_ctx, |r| r.screencast.clone()) else {
            break;
          };
          let Ok(f) = saved.restore(&pump_ctx) else { break };
          let deliver = || -> rquickjs::Result<()> {
            let payload = rquickjs::Object::new(pump_ctx.clone())?;
            let buf = rquickjs::TypedArray::<u8>::new(pump_ctx.clone(), bytes)?;
            payload.set("frame", buf)?;
            payload.set("timestamp", ts)?;
            let _: Value<'_> = f.call((payload,))?;
            Ok(())
          };
          let _ = deliver();
        }
      });
      Ok(())
    }))
  }
}

/// Playwright `Touchscreen`. Construct via `page.touchscreen`.
#[derive(rquickjs::JsLifetime, rquickjs::class::Trace)]
#[rquickjs::class(rename = "Touchscreen")]
pub struct TouchscreenJs {
  #[qjs(skip_trace)]
  pub(super) page: Arc<ferridriver::Page>,
}

#[rquickjs::methods]
impl TouchscreenJs {
  /// Playwright: `touchscreen.tap(x, y)`.
  ///
  /// # Errors
  ///
  /// Rejects with the core failure (`TimeoutError`, `TargetClosedError` or `FerriError`) when the tap fails.
  #[qjs(rename = "tap")]
  pub async fn tap(&self, ctx: Ctx<'_>, x: f64, y: f64) -> rquickjs::Result<()> {
    self.page.touchscreen().tap(x, y).await.into_js_with(&ctx)
  }
}
