//! `expect.poll(...)` polling assertion binding.

use std::time::Duration;

use ferridriver_expect::{AssertionFailure, POLL_INTERVALS};
use rquickjs::{Class, Ctx, Function, JsLifetime, Object, Persistent, Value, class::Trace, function::Opt};
use serde_json::Value as JsonValue;

use crate::bindings::convert::{json_to_js, serde_from_js};

use super::settled::{install_poll_not_getter, install_poll_settled_refusal, with_generic_poll_matchers};
use super::{assertion_to_rq, capture_stack, custom_message, default_timeout, opts_obj, u64_array_field, u64_field};

// ── ExpectPollJs ─────────────────────────────────────────────────────

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "ExpectPoll")]
pub struct ExpectPollJs {
  #[qjs(skip_trace)]
  generator: Persistent<Function<'static>>,
  #[qjs(skip_trace)]
  timeout: Duration,
  #[qjs(skip_trace)]
  intervals: Vec<u64>,
  pub(super) is_not: bool,
  pub(super) message: Option<String>,
  /// As [`ExpectJs`]'s: where `expect.poll(...)` was called.
  stack: Option<String>,
}

#[rquickjs::methods]
impl ExpectPollJs {
  #[qjs(rename = "withTimeout")]
  #[must_use]
  pub fn with_timeout(&self, timeout_ms: u32) -> ExpectPollJs {
    ExpectPollJs {
      generator: self.generator.clone(),
      timeout: Duration::from_millis(u64::from(timeout_ms)),
      intervals: self.intervals.clone(),
      is_not: self.is_not,
      message: self.message.clone(),
      stack: self.stack.clone(),
    }
  }

  /// What the generic matchers on `expect.poll` read to poll the way the
  /// native ones do.
  ///
  /// # Errors
  ///
  /// When the generator cannot be restored into this context or the object
  /// cannot be built.
  #[qjs(rename = "_pollInfo")]
  pub fn poll_info<'js>(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: Ctx<'js>,
  ) -> rquickjs::Result<Object<'js>> {
    let info = Object::new(ctx.clone())?;
    info.set("generator", self.generator.clone().restore(&ctx)?)?;
    info.set(
      "timeout",
      crate::bindings::convert::u64_to_f64(crate::bindings::convert::duration_ms_to_u64(self.timeout)),
    )?;
    info.set(
      "intervals",
      self
        .intervals
        .iter()
        .map(|&ms| crate::bindings::convert::u64_to_f64(ms))
        .collect::<Vec<_>>(),
    )?;
    info.set("isNot", self.is_not)?;
    Ok(info)
  }

  #[qjs(rename = "_notInner")]
  #[must_use]
  pub fn not_inner(&self) -> ExpectPollJs {
    ExpectPollJs {
      generator: self.generator.clone(),
      timeout: self.timeout,
      intervals: self.intervals.clone(),
      is_not: !self.is_not,
      message: self.message.clone(),
      stack: self.stack.clone(),
    }
  }

  /// Playwright: `toSatisfy`.
  ///
  /// # Errors
  ///
  /// Rejects with an `AssertionError` when `predicate` still fails at the
  /// timeout, or with whatever the generator or `predicate` throws.
  #[qjs(rename = "toSatisfy")]
  pub async fn to_satisfy<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    predicate: Function<'js>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let saved_pred = Persistent::save(&ctx, predicate);
        let generator_fn = self.generator.clone();
        let deadline = tokio::time::Instant::now() + self.timeout;
        let mut interval_idx = 0;
        let is_not = self.is_not;
        let final_dbg: String = loop {
          let actual: rquickjs::Result<JsonValue> = call_generator(&ctx, &generator_fn).await;
          let actual = actual?;
          let dbg = ferridriver_expect::asymmetric::json_short(&actual);
          let pred = saved_pred.clone().restore(&ctx)?;
          let actual_js = json_to_js(&ctx, &actual)?;
          let result: rquickjs::Value<'_> = pred.call((actual_js,))?;
          let passes = result.as_bool().unwrap_or(false);
          let passes = if is_not { !passes } else { passes };
          if passes {
            return Ok(());
          }
          let interval_ms = self
            .intervals
            .get(interval_idx)
            .copied()
            .unwrap_or_else(|| self.intervals.last().copied().unwrap_or(1000));
          interval_idx += 1;
          // Clamp the interval to the remaining budget so the final
          // attempt lands AT the deadline instead of bailing early.
          let now = tokio::time::Instant::now();
          if now >= deadline {
            break dbg;
          }
          let sleep_dur = Duration::from_millis(interval_ms).min(deadline - now);
          tokio::time::sleep(sleep_dur).await;
        };
        let last = final_dbg.as_str();
        Err(assertion_to_rq(
          &ctx,
          AssertionFailure::new(
            format!(
              "expect.poll().toSatisfy() timed out after {}ms; last value was {last}",
              self.timeout.as_millis()
            ),
            None,
          ),
          self.stack.as_deref(),
        ))
      })
      .await
  }
}

pub(super) async fn call_generator(
  ctx: &Ctx<'_>,
  generator_fn: &Persistent<Function<'static>>,
) -> rquickjs::Result<JsonValue> {
  serde_from_js(ctx, call_generator_live(ctx, generator_fn).await?)
}

/// One round of the poll generator, awaited, with the result left as the
/// live JS value the identity matchers need.
pub(super) async fn call_generator_live<'js>(
  ctx: &Ctx<'js>,
  generator_fn: &Persistent<Function<'static>>,
) -> rquickjs::Result<Value<'js>> {
  let f = generator_fn.clone().restore(ctx)?;
  let result: rquickjs::Value<'js> = f.call(())?;
  // Await the result if it's a thenable.
  if let Some(promise) = result.as_promise() {
    promise.clone().into_future::<rquickjs::Value<'js>>().await
  } else {
    Ok(result)
  }
}

/// Playwright: `expect.poll(actual, messageOrOptions?: string |
/// { message?, timeout?, intervals? })`.
pub(super) fn make_poll_fn<'js>(
  ctx: &Ctx<'js>,
  meta: &ferridriver_expect::ExpectMeta<()>,
) -> rquickjs::Result<Function<'js>> {
  let poll_meta = meta.clone();
  Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, generator: Function<'js>, opts: Opt<Value<'js>>| -> rquickjs::Result<Value<'js>> {
      let o = opts_obj(&opts);
      let default_timeout =
        crate::bindings::convert::duration_ms_to_u64(poll_meta.timeout.unwrap_or_else(|| default_timeout(&ctx)));
      let timeout_ms = u64_field(o.as_ref(), "timeout").unwrap_or(default_timeout);
      let intervals = u64_array_field(o.as_ref(), "intervals").unwrap_or_else(|| POLL_INTERVALS.to_vec());
      let saved = Persistent::save(&ctx, generator);
      let inst = ExpectPollJs {
        generator: saved,
        timeout: Duration::from_millis(timeout_ms),
        intervals,
        is_not: false,
        message: custom_message(opts.0.as_ref())?.or_else(|| poll_meta.message.clone()),
        stack: capture_stack(&ctx),
      };
      let class = Class::instance(ctx.clone(), inst)?;
      {
        let val = class.into_value();
        install_poll_not_getter(&ctx, &val)?;
        install_poll_settled_refusal(&ctx, &val)?;
        with_generic_poll_matchers(&ctx, val)
      }
    },
  )
}
