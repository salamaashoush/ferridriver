//! `Clock` — NAPI binding for `context.clock` / `page.clock`.
//!
//! Mirrors Playwright's `Clock` (`client/clock.ts` / `types.d.ts:20004`):
//! `install`, `fastForward`, `pauseAt`, `resume`, `runFor`,
//! `setFixedTime`, `setSystemTime`. Time arguments accept
//! `number | string | Date`; tick arguments accept `number | string`.

use napi::Result;
use napi::bindgen_prelude::{Either, Either3};
use napi_derive::napi;

use crate::error::IntoNapi;
use crate::types::JsDateLike;
use ferridriver::clock::{ClockTicks, ClockTime};

type TimeArg = Either3<f64, String, JsDateLike>;
type TicksArg = Either<f64, String>;

fn lower_time(time: TimeArg) -> ClockTime {
  match time {
    Either3::A(ms) => ClockTime::Millis(ms),
    Either3::B(text) => ClockTime::Text(text),
    Either3::C(date) => ClockTime::Millis(date.time_ms),
  }
}

fn lower_ticks(ticks: TicksArg) -> ClockTicks {
  match ticks {
    Either::A(ms) => ClockTicks::Millis(ms),
    Either::B(text) => ClockTicks::Text(text),
  }
}

/// Options bag for `clock.install`.
#[napi(object)]
pub struct ClockInstallOptions {
  /// Initial fake time (default: current system time).
  #[napi(ts_type = "number | string | Date")]
  pub time: Option<TimeArg>,
}

/// Fake-time controller for a browser context (Playwright `Clock`).
#[napi]
pub struct Clock {
  inner: ferridriver::ContextRef,
}

impl Clock {
  pub(crate) fn wrap(inner: ferridriver::ContextRef) -> Self {
    Self { inner }
  }
}

#[napi]
impl Clock {
  /// Playwright: `clock.install(options?: { time? })`.
  #[napi(ts_args_type = "options?: { time?: number | string | Date }")]
  pub async fn install(&self, options: Option<ClockInstallOptions>) -> Result<()> {
    let time = options.and_then(|o| o.time).map(lower_time);
    self.inner.clock().install(time).await.into_napi()
  }

  /// Playwright: `clock.fastForward(ticks)`.
  #[napi(ts_args_type = "ticks: number | string")]
  pub async fn fast_forward(&self, ticks: TicksArg) -> Result<()> {
    self.inner.clock().fast_forward(lower_ticks(ticks)).await.into_napi()
  }

  /// Playwright: `clock.pauseAt(time)`.
  #[napi(ts_args_type = "time: number | string | Date")]
  pub async fn pause_at(&self, time: TimeArg) -> Result<()> {
    self.inner.clock().pause_at(lower_time(time)).await.into_napi()
  }

  /// Playwright: `clock.resume()`.
  #[napi]
  pub async fn resume(&self) -> Result<()> {
    self.inner.clock().resume().await.into_napi()
  }

  /// Playwright: `clock.runFor(ticks)`.
  #[napi(ts_args_type = "ticks: number | string")]
  pub async fn run_for(&self, ticks: TicksArg) -> Result<()> {
    self.inner.clock().run_for(lower_ticks(ticks)).await.into_napi()
  }

  /// Playwright: `clock.setFixedTime(time)`.
  #[napi(ts_args_type = "time: number | string | Date")]
  pub async fn set_fixed_time(&self, time: TimeArg) -> Result<()> {
    self.inner.clock().set_fixed_time(lower_time(time)).await.into_napi()
  }

  /// Playwright: `clock.setSystemTime(time)`.
  #[napi(ts_args_type = "time: number | string | Date")]
  pub async fn set_system_time(&self, time: TimeArg) -> Result<()> {
    self.inner.clock().set_system_time(lower_time(time)).await.into_napi()
  }
}
