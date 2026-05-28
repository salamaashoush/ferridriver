#![allow(clippy::expect_used)]
//! Ignored microbench for the QuickJS -> ferridriver evaluate-argument bridge.
//!
//! Run with:
//! cargo test --profile release-fast -p ferridriver-script --test binding_convert_bench -- --ignored --nocapture

use std::time::Instant;

use ferridriver_script::bindings::convert::quickjs_arg_to_serialized;

const ITERS: u32 = 20_000;

#[test]
#[ignore = "perf microbench; run explicitly with --ignored --nocapture"]
fn quickjs_arg_conversion_plain_object() {
  let rt = rquickjs::Runtime::new().expect("runtime");
  let cx = rquickjs::Context::full(&rt).expect("context");
  cx.with(|ctx| {
    let value: rquickjs::Value<'_> = ctx
      .eval(
        "Array.from({ length: 12 }, (_, i) => ({ \
           i, n: i + 0.25, s: 'item-' + i, ok: i % 2 === 0, \
           nested: { a: i, b: [i, i + 1, null, undefined, () => 1] } \
         }))",
      )
      .expect("build value");

    for _ in 0..1_000 {
      let _ = quickjs_arg_to_serialized(&ctx, Some(value.clone())).expect("warmup");
    }

    let start = Instant::now();
    for _ in 0..ITERS {
      let _ = quickjs_arg_to_serialized(&ctx, Some(value.clone())).expect("convert");
    }
    let elapsed = start.elapsed();
    let avg_us = elapsed.as_secs_f64() * 1_000_000.0 / f64::from(ITERS);
    println!(
      "quickjs_arg_conversion_plain_object: total={:.3}ms avg={:.3}us iters={ITERS}",
      elapsed.as_secs_f64() * 1_000.0,
      avg_us
    );
  });
}
