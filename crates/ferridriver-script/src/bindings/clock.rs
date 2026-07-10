//! `ClockJs`: QuickJS binding for `context.clock` / `page.clock`.
//!
//! Mirrors Playwright's `Clock` (`client/clock.ts`): `install`,
//! `fastForward`, `pauseAt`, `resume`, `runFor`, `setFixedTime`,
//! `setSystemTime`. Time arguments accept `number | string | Date`;
//! tick arguments accept `number | string` (`"mm:ss"` grammar parsed by
//! the core).

use std::sync::Arc;

use ferridriver::clock::{ClockTicks, ClockTime};
use ferridriver::context::ContextRef;
use rquickjs::function::Opt;
use rquickjs::{Ctx, JsLifetime, Value, class::Trace};

use crate::bindings::convert::FerriResultCtxExt;

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Clock")]
pub struct ClockJs {
  #[qjs(skip_trace)]
  inner: Arc<ContextRef>,
}

impl ClockJs {
  #[must_use]
  pub fn new(inner: Arc<ContextRef>) -> Self {
    Self { inner }
  }
}

/// Lower a JS `number | string | Date` into a core [`ClockTime`].
fn time_from_js<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<ClockTime> {
  if let Some(n) = value.as_number() {
    return Ok(ClockTime::Millis(n));
  }
  if let Some(s) = value.as_string() {
    return Ok(ClockTime::Text(s.to_string()?));
  }
  if let Some(obj) = value.as_object() {
    if let Ok(get_time) = obj.get::<_, rquickjs::Function<'js>>("getTime") {
      let ms: f64 = get_time.call((rquickjs::function::This(obj.clone()),))?;
      if ms.is_finite() {
        return Ok(ClockTime::Millis(ms));
      }
      return Err(crate::bindings::convert::throw_named(
        ctx,
        "Error",
        "Invalid date".to_string(),
      ));
    }
  }
  Err(crate::bindings::convert::throw_named(
    ctx,
    "TypeError",
    "time: expected number, string, or Date".to_string(),
  ))
}

/// Lower a JS `number | string` into a core [`ClockTicks`].
fn ticks_from_js<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<ClockTicks> {
  if let Some(n) = value.as_number() {
    return Ok(ClockTicks::Millis(n));
  }
  if let Some(s) = value.as_string() {
    return Ok(ClockTicks::Text(s.to_string()?));
  }
  Err(crate::bindings::convert::throw_named(
    ctx,
    "TypeError",
    "ticks: expected number or string".to_string(),
  ))
}

#[rquickjs::methods]
impl ClockJs {
  /// Playwright: `clock.install(options?: { time? })`.
  #[qjs(rename = "install")]
  pub async fn install<'js>(&self, ctx: Ctx<'js>, options: Opt<Value<'js>>) -> rquickjs::Result<()> {
    let time = match options.0.and_then(rquickjs::Value::into_object) {
      Some(obj) => {
        let t: Value<'js> = obj.get("time")?;
        if t.is_undefined() || t.is_null() {
          None
        } else {
          Some(time_from_js(&ctx, t)?)
        }
      },
      None => None,
    };
    self.inner.clock().install(time).await.into_js_with(&ctx)
  }

  /// Playwright: `clock.fastForward(ticks)`.
  #[qjs(rename = "fastForward")]
  pub async fn fast_forward<'js>(&self, ctx: Ctx<'js>, ticks: Value<'js>) -> rquickjs::Result<()> {
    let ticks = ticks_from_js(&ctx, ticks)?;
    self.inner.clock().fast_forward(ticks).await.into_js_with(&ctx)
  }

  /// Playwright: `clock.pauseAt(time)`.
  #[qjs(rename = "pauseAt")]
  pub async fn pause_at<'js>(&self, ctx: Ctx<'js>, time: Value<'js>) -> rquickjs::Result<()> {
    let time = time_from_js(&ctx, time)?;
    self.inner.clock().pause_at(time).await.into_js_with(&ctx)
  }

  /// Playwright: `clock.resume()`.
  #[qjs(rename = "resume")]
  pub async fn resume(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    self.inner.clock().resume().await.into_js_with(&ctx)
  }

  /// Playwright: `clock.runFor(ticks)`.
  #[qjs(rename = "runFor")]
  pub async fn run_for<'js>(&self, ctx: Ctx<'js>, ticks: Value<'js>) -> rquickjs::Result<()> {
    let ticks = ticks_from_js(&ctx, ticks)?;
    self.inner.clock().run_for(ticks).await.into_js_with(&ctx)
  }

  /// Playwright: `clock.setFixedTime(time)`.
  #[qjs(rename = "setFixedTime")]
  pub async fn set_fixed_time<'js>(&self, ctx: Ctx<'js>, time: Value<'js>) -> rquickjs::Result<()> {
    let time = time_from_js(&ctx, time)?;
    self.inner.clock().set_fixed_time(time).await.into_js_with(&ctx)
  }

  /// Playwright: `clock.setSystemTime(time)`.
  #[qjs(rename = "setSystemTime")]
  pub async fn set_system_time<'js>(&self, ctx: Ctx<'js>, time: Value<'js>) -> rquickjs::Result<()> {
    let time = time_from_js(&ctx, time)?;
    self.inner.clock().set_system_time(time).await.into_js_with(&ctx)
  }
}
