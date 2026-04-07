//! Usage reporter: tracks step definition usage statistics.

use std::time::Duration;

use rustc_hash::FxHashMap;

use crate::reporter::{Reporter, ReporterEvent};

pub struct UsageReporter {
  /// Map from step expression -> (call_count, total_duration).
  stats: FxHashMap<String, (usize, Duration)>,
}

impl UsageReporter {
  pub fn new() -> Self {
    Self {
      stats: FxHashMap::default(),
    }
  }

  fn format_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
      format!("{ms}ms")
    } else {
      format!("{:.1}s", d.as_secs_f64())
    }
  }
}

impl Default for UsageReporter {
  fn default() -> Self {
    Self::new()
  }
}

#[async_trait::async_trait]
impl Reporter for UsageReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    if let ReporterEvent::StepFinished(ev) = event {
      if !ev.category.is_visible() {
        return;
      }

      let expression = ev
        .metadata
        .as_ref()
        .and_then(|m| m.get("bdd_text"))
        .and_then(|v| v.as_str())
        .map_or_else(|| ev.title.clone(), |s| s.to_string());

      let entry = self.stats.entry(expression).or_insert((0, Duration::ZERO));
      entry.0 += 1;
      entry.1 += ev.duration;
    }
  }

  async fn finalize(&mut self) -> Result<(), String> {
    if self.stats.is_empty() {
      return Ok(());
    }

    let mut entries: Vec<_> = self.stats.drain().collect();
    entries.sort_by(|a, b| b.1.1.cmp(&a.1.1));

    println!();
    println!("  Step Usage Statistics:");
    println!(
      "    {:<50} {:>5}   {:>8}   {:>8}",
      "Expression", "Count", "Total", "Avg"
    );

    for (expression, (count, total)) in &entries {
      let avg = if *count > 0 {
        *total / (*count as u32)
      } else {
        Duration::ZERO
      };
      println!(
        "    {:<50} {:>5}   {:>8}   {:>8}",
        expression,
        count,
        Self::format_duration(*total),
        Self::format_duration(avg),
      );
    }
    println!();

    Ok(())
  }
}
