//! `testDebug` — the controls a session gets when it was published by a
//! stopped test.
//!
//! Stepping is a script call, not a protocol verb. The session wire carries
//! exactly one verb (`run`), and the whole reason it does is that a verb
//! table always lags the API it fronts; adding `resume` / `step-over` to
//! the wire to drive a pause would start rebuilding the table this design
//! deleted. A binding costs nothing on the wire and composes with
//! everything else a script can do:
//!
//! ```js
//! const { test, action } = await testDebug.info();
//! console.log(`about to run ${action.title} at ${action.location}`);
//! await page.screenshot();          // look before it happens
//! await testDebug.stepOver();       // run just that call, stop again
//! await testDebug.pauseAt('checkout.spec.ts:42');
//! await testDebug.resume();         // let the test finish
//! ```
//!
//! The global is absent unless the session came from a stopped test, so a
//! script can feature-detect with `typeof testDebug !== 'undefined'`.

use std::sync::Arc;

use rquickjs::{Ctx, Function, Object, Result as QjsResult};

/// The API call a stopped run is sitting in front of.
#[derive(Debug, Clone)]
pub struct PendingAction {
  /// Display title (`page.goto`, `locator.click`).
  pub title: String,
  /// `file:line` the call was written at, when one was captured.
  pub location: Option<String>,
}

/// The stopped run, from the session's side.
///
/// Implemented by whoever owns the pause (the CLI's `--debug` hook); this
/// crate only exposes it to JS.
pub trait TestDebugControl: std::fmt::Debug + Send + Sync + 'static {
  /// Let the run continue to the end. Idempotent: a second call on an
  /// already-running test does nothing, because a script that calls it
  /// twice is not an error worth failing a debugging session over.
  fn resume(&self);

  /// Run the call it is stopped at, then stop before the next one.
  fn step_over(&self);

  /// Continue until a call written at `location` (`file:line`, or a
  /// suffix of one) is about to run, and stop there.
  fn pause_at(&self, location: &str);

  /// Whether the run is stopped right now.
  fn paused(&self) -> bool;

  /// Whether the run has been let go for good.
  fn resumed(&self) -> bool;

  /// What is stopped: the test's full name.
  fn test(&self) -> String;

  /// Where the test is, as `file:line`, when discovery recorded one.
  fn location(&self) -> Option<String>;

  /// Why it stopped, when it stopped at a failure.
  fn error(&self) -> Option<String>;

  /// The call it is stopped in front of, if it is stopped at one.
  fn pending(&self) -> Option<PendingAction>;
}

/// Install `testDebug` into `ctx`.
///
/// # Errors
///
/// Propagates an `rquickjs` failure building the object.
pub fn install<'js>(ctx: &Ctx<'js>, control: Arc<dyn TestDebugControl>) -> QjsResult<()> {
  let obj = Object::new(ctx.clone())?;

  {
    let c = Arc::clone(&control);
    obj.set(
      "resume",
      Function::new(ctx.clone(), move || {
        c.resume();
      })?,
    )?;
  }
  {
    let c = Arc::clone(&control);
    obj.set(
      "stepOver",
      Function::new(ctx.clone(), move || {
        c.step_over();
      })?,
    )?;
  }
  {
    let c = Arc::clone(&control);
    obj.set(
      "pauseAt",
      Function::new(ctx.clone(), move |location: String| {
        c.pause_at(&location);
      })?,
    )?;
  }
  {
    let c = Arc::clone(&control);
    obj.set("resumed", Function::new(ctx.clone(), move || c.resumed())?)?;
  }
  {
    let c = Arc::clone(&control);
    obj.set("paused", Function::new(ctx.clone(), move || c.paused())?)?;
  }
  {
    let c = Arc::clone(&control);
    obj.set(
      "info",
      // The context comes in as a parameter rather than being captured: a
      // native closure holding a `Ctx` is a GC edge the collector cannot
      // trace, and it aborts `JS_FreeRuntime` at teardown
      // ("list_empty(&rt->gc_obj_list)"). Nothing captured here touches JS.
      Function::new(ctx.clone(), move |ctx: Ctx<'js>| -> QjsResult<Object<'js>> {
        let info = Object::new(ctx.clone())?;
        info.set("test", c.test())?;
        info.set("location", c.location())?;
        info.set("error", c.error())?;
        info.set("paused", c.paused())?;
        info.set("resumed", c.resumed())?;
        match c.pending() {
          Some(pending) => {
            let action = Object::new(ctx)?;
            action.set("title", pending.title)?;
            action.set("location", pending.location)?;
            info.set("action", action)?;
          },
          None => info.set("action", rquickjs::Value::new_null(ctx))?,
        }
        QjsResult::Ok(info)
      })?,
    )?;
  }

  ctx.globals().set("testDebug", obj)?;
  crate::bindings::runtime::mirror_global(ctx, "testDebug")?;
  Ok(())
}
