//! JUnit XML reporter for CI integration.

use std::path::PathBuf;
use std::time::Duration;

use crate::model::TestOutcome;
use crate::reporter::{Reporter, ReporterEvent};

pub struct JUnitReporter {
  output_path: PathBuf,
  results: Vec<TestOutcome>,
  total_duration: Duration,
}

impl JUnitReporter {
  pub fn new(output_path: PathBuf) -> Self {
    Self {
      output_path,
      results: Vec::new(),
      total_duration: Duration::ZERO,
    }
  }
}

#[async_trait::async_trait]
impl Reporter for JUnitReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    match event {
      ReporterEvent::TestFinished { outcome, .. } => {
        self.results.push(outcome.clone());
      }
      ReporterEvent::RunFinished { duration, .. } => {
        self.total_duration = *duration;
      }
      _ => {}
    }
  }

  async fn finalize(&mut self) -> Result<(), String> {
    use std::fmt::Write;

    let failures = self
      .results
      .iter()
      .filter(|r| {
        matches!(
          r.status,
          crate::model::TestStatus::Failed | crate::model::TestStatus::TimedOut
        )
      })
      .count();
    let skipped = self
      .results
      .iter()
      .filter(|r| r.status == crate::model::TestStatus::Skipped)
      .count();

    let mut xml = String::new();
    writeln!(xml, r#"<?xml version="1.0" encoding="UTF-8"?>"#).ok();
    writeln!(
      xml,
      r#"<testsuites tests="{}" failures="{}" skipped="{}" time="{:.3}">"#,
      self.results.len(),
      failures,
      skipped,
      self.total_duration.as_secs_f64()
    )
    .ok();

    // Group by file.
    let mut by_file: rustc_hash::FxHashMap<&str, Vec<&TestOutcome>> = rustc_hash::FxHashMap::default();
    for result in &self.results {
      by_file.entry(&result.test_id.file).or_default().push(result);
    }

    for (file, tests) in &by_file {
      let suite_failures = tests
        .iter()
        .filter(|t| {
          matches!(
            t.status,
            crate::model::TestStatus::Failed | crate::model::TestStatus::TimedOut
          )
        })
        .count();
      let suite_time: f64 = tests.iter().map(|t| t.duration.as_secs_f64()).sum();

      writeln!(
        xml,
        r#"  <testsuite name="{file}" tests="{}" failures="{suite_failures}" time="{suite_time:.3}">"#,
        tests.len()
      )
      .ok();

      for test in tests {
        let name = xml_escape(&test.test_id.name);
        let time = test.duration.as_secs_f64();

        writeln!(xml, r#"    <testcase name="{name}" time="{time:.3}">"#).ok();

        match test.status {
          crate::model::TestStatus::Failed | crate::model::TestStatus::TimedOut => {
            let msg = test
              .error
              .as_ref()
              .map(|e| xml_escape(&e.message))
              .unwrap_or_default();
            writeln!(xml, r#"      <failure message="{msg}" />"#).ok();
          }
          crate::model::TestStatus::Skipped => {
            writeln!(xml, r#"      <skipped />"#).ok();
          }
          _ => {}
        }

        writeln!(xml, r#"    </testcase>"#).ok();
      }

      writeln!(xml, r#"  </testsuite>"#).ok();
    }

    writeln!(xml, r#"</testsuites>"#).ok();

    if let Some(parent) = self.output_path.parent() {
      std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&self.output_path, xml)
      .map_err(|e| format!("cannot write {}: {e}", self.output_path.display()))?;

    tracing::info!("JUnit report written to {}", self.output_path.display());
    Ok(())
  }
}

fn xml_escape(s: &str) -> String {
  s.replace('&', "&amp;")
    .replace('<', "&lt;")
    .replace('>', "&gt;")
    .replace('"', "&quot;")
    .replace('\'', "&apos;")
}
