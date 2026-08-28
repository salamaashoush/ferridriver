//! Print a saved trace's analysis as JSON, for comparing against other
//! implementations.
//!
//! `cargo run -p ferridriver-perf --example report -- trace.json`

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let path = std::env::args().nth(1).unwrap_or_else(|| "trace.json".into());
  let report = ferridriver_perf::analyze_json(&std::fs::read(path)?)?;
  println!("{}", serde_json::to_string_pretty(&report)?);
  Ok(())
}
