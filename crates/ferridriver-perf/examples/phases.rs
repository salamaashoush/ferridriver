//! Where the time in an analysis goes, phase by phase.
//!
//! `examples/bench.rs` says how long a trace takes; this says which part
//! of it to go and look at. Every handler walks the same event list, so
//! a number here that stands out is one handler doing per-event work
//! rather than the iteration being slow.
//!
//! `cargo run --release -p ferridriver-perf --example phases -- trace.json [iters]`

use std::time::Instant;

use ferridriver_perf::handlers::{meta::Meta, page_load::PageLoadMetrics, page_signals, paint, renderer};

/// Time `$body` over `$iters` runs and report the mean, keeping the last
/// result so the optimiser cannot delete the work.
macro_rules! phase {
  ($name:expr, $iters:expr, $body:expr) => {{
    let started = Instant::now();
    let mut out = None;
    for _ in 0..$iters {
      out = Some(std::hint::black_box($body));
    }
    println!("{:32} {:?}", $name, started.elapsed() / $iters);
    out
  }};
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let Some(path) = std::env::args().nth(1) else {
    eprintln!("usage: phases <trace.json> [iters]");
    return Ok(());
  };
  let iters: u32 = std::env::args()
    .nth(2)
    .and_then(|s| s.parse().ok())
    .unwrap_or(20)
    .max(1);
  let bytes = std::fs::read(path)?;
  let events = ferridriver_perf::event::parse(&bytes)?;
  println!("{} bytes, {} events, {iters} iterations\n", bytes.len(), events.len());

  let meta = phase!("meta", iters, Meta::from_events(&events)).unwrap_or_default();
  let requests = phase!(
    "network",
    iters,
    ferridriver_perf::handlers::network::from_events(&events)
  )
  .unwrap_or_default();
  let metrics = phase!("page_load", iters, PageLoadMetrics::from_events(&events, &meta)).unwrap_or_default();
  let origin = meta.time_origin();
  phase!(
    "paint",
    iters,
    paint::LargestPaint::from_events(&events, &meta, &requests, origin)
  );
  phase!(
    "page_signals",
    iters,
    page_signals::PageSignals::from_events(&events, &meta)
  );
  phase!("renderer", iters, renderer::Renderer::from_events(&events));
  phase!(
    "interactions",
    iters,
    ferridriver_perf::handlers::interactions::from_events(&events)
  );
  phase!("painted_images", iters, paint::painted_images(&events));
  phase!(
    "layout_shift_culprits",
    iters,
    page_signals::layout_shift_culprits(&events)
  );
  phase!(
    "scripts",
    iters,
    ferridriver_perf::handlers::scripts::from_events(&events)
  );

  let at = |ms: Option<f64>| ms.map(|ms| origin + ferridriver_perf::units::ms_to_micros(ms));
  let (fcp, lcp) = (at(metrics.first_contentful_paint), at(metrics.largest_contentful_paint));
  phase!(
    "lantern graph + baselines",
    iters,
    ferridriver_perf::lantern::Context::build(&requests, &meta.main_frame_url, &events, fcp, lcp)
  );

  println!();
  phase!("whole analysis", iters, ferridriver_perf::analyze(&events));
  Ok(())
}
