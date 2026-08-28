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

  // The MCP server hands over already-parsed values rather than bytes,
  // so that path is timed too.
  let values: Vec<serde_json::Value> = serde_json::from_slice::<serde_json::Value>(&bytes)
    .ok()
    .and_then(|v| match v {
      serde_json::Value::Array(a) => Some(a),
      serde_json::Value::Object(o) => o.get("traceEvents").and_then(|t| t.as_array()).cloned(),
      _ => None,
    })
    .unwrap_or_default();
  let t = std::time::Instant::now();
  for _ in 0..iters {
    std::hint::black_box(ferridriver_perf::analyze_values(&values));
  }
  let from_values = t.elapsed() / iters;

  println!("parse:   {parse:?}");
  println!("from values (MCP path): {from_values:?}");
  println!("analyze: {analyze:?}");
  println!("total:   {:?}", parse + analyze);
  Ok(())
}
