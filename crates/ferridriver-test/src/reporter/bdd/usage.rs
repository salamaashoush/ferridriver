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

  /// The tally so far, sorted by expression. Read-only view for callers
  /// that want the numbers without the printed table.
  #[must_use]
  pub fn stats_snapshot(&self) -> Vec<(String, (usize, Duration))> {
    let mut entries: Vec<(String, (usize, Duration))> = self.stats.iter().map(|(k, v)| (k.clone(), *v)).collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
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

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
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

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::time::Duration;

  use super::*;
  use crate::model::{StepCategory, TestId};

  #[tokio::test]
  async fn calls_are_counted_per_step_expression() {
    let mut reporter = UsageReporter::new();
    for _ in 0..3 {
      reporter
        .on_event(&ReporterEvent::StepFinished(Arc::new(
          crate::reporter::StepFinishedEvent {
            test_id: TestId::default(),
            step_id: "s".into(),
            title: "Given a user named Bob".into(),
            category: StepCategory::TestStep,
            duration: Duration::from_millis(10),
            error: None,
            metadata: Some(serde_json::json!({ "bdd_text": "a user named {word}" })),
          },
        )))
        .await;
    }
    let stats = reporter.stats_snapshot();
    assert_eq!(stats.len(), 1, "the expression groups the calls, not the rendered text");
    let (expression, (count, total)) = stats.into_iter().next().expect("one entry");
    assert_eq!(expression, "a user named {word}");
    assert_eq!(count, 3);
    assert_eq!(total, Duration::from_millis(30));
  }

  #[tokio::test]
  async fn a_step_without_bdd_metadata_falls_back_to_its_title() {
    let mut reporter = UsageReporter::new();
    reporter
      .on_event(&ReporterEvent::StepFinished(Arc::new(
        crate::reporter::StepFinishedEvent {
          test_id: TestId::default(),
          step_id: "s".into(),
          title: "plain step".into(),
          category: StepCategory::TestStep,
          duration: Duration::from_millis(5),
          error: None,
          metadata: None,
        },
      )))
      .await;
    assert_eq!(reporter.stats_snapshot()[0].0, "plain step");
  }
}
