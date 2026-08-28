//! Time the analysis over a saved trace.
//!
//! `cargo run --release -p ferridriver-perf --example bench -- trace.json [iters]`

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let path = std::env::args().nth(1).unwrap_or_else(|| "trace.json".into());
  let iters: u32 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(100);
  let bytes = std::fs::read(&path)?;

  // Parse once to report the shape, then time parse and analysis apart:
  // the JSON parse dominates, and conflating them hides that.
  let events = ferridriver_perf::event::parse(&bytes)?;
  println!("{} bytes, {} events", bytes.len(), events.len());

  let t = std::time::Instant::now();
  for _ in 0..iters {
    std::hint::black_box(ferridriver_perf::event::parse(&bytes)?);
  }
  let parse = t.elapsed() / iters;

  let t = std::time::Instant::now();
  for _ in 0..iters {
    std::hint::black_box(ferridriver_perf::analyze(&events));
  }
  let analyze = t.elapsed() / iters;

  println!("parse:   {parse:?}");
  println!("analyze: {analyze:?}");
  println!("total:   {:?}", parse + analyze);
  Ok(())
}
