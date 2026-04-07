//! BDD JUnit XML reporter.

use std::path::PathBuf;

use crate::model::{StepCategory, StepStatus, TestOutcome, TestStatus, TestStep};
use crate::reporter::{Reporter, ReporterEvent};

pub struct BddJunitReporter {
  output_path: PathBuf,
  results: Vec<TestOutcome>,
  total_duration_ms: u64,
}

impl BddJunitReporter {
  pub fn new(output_path: PathBuf) -> Self {
    Self {
      output_path,
      results: Vec::new(),
      total_duration_ms: 0,
    }
  }
}

#[async_trait::async_trait]
impl Reporter for BddJunitReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    match event {
      ReporterEvent::TestFinished { outcome, .. } => {
        self.results.push(outcome.clone());
      }
      ReporterEvent::RunFinished { duration, .. } => {
        self.total_duration_ms = duration.as_millis() as u64;
      }
      _ => {}
    }
  }

  async fn finalize(&mut self) -> Result<(), String> {
    use std::fmt::Write;

    let mut suites: rustc_hash::FxHashMap<String, Vec<&TestOutcome>> = rustc_hash::FxHashMap::default();
    for result in &self.results {
      let suite = result.test_id.suite.clone().unwrap_or_else(|| result.test_id.file.clone());
      suites.entry(suite).or_default().push(result);
    }

    let failures = self
      .results
      .iter()
      .filter(|r| matches!(r.status, TestStatus::Failed | TestStatus::TimedOut))
      .count();
    let skipped = self.results.iter().filter(|r| r.status == TestStatus::Skipped).count();

    let mut xml = String::new();
    writeln!(xml, r#"<?xml version="1.0" encoding="UTF-8"?>"#).ok();
    writeln!(
      xml,
      r#"<testsuites tests="{}" failures="{}" skipped="{}" time="{:.3}">"#,
      self.results.len(),
      failures,
      skipped,
      self.total_duration_ms as f64 / 1000.0
    )
    .ok();

    for (suite_name, tests) in &suites {
      let sf = tests
        .iter()
        .filter(|t| matches!(t.status, TestStatus::Failed | TestStatus::TimedOut))
        .count();
      let st: f64 = tests.iter().map(|t| t.duration.as_secs_f64()).sum();

      writeln!(
        xml,
        r#"  <testsuite name="{}" tests="{}" failures="{sf}" time="{st:.3}">"#,
        escape_xml(suite_name),
        tests.len()
      )
      .ok();

      for test in tests {
        let time = test.duration.as_secs_f64();
        writeln!(
          xml,
          r#"    <testcase name="{}" classname="{}" time="{time:.3}">"#,
          escape_xml(&test.test_id.name),
          escape_xml(suite_name)
        )
        .ok();

        match test.status {
          TestStatus::Failed | TestStatus::TimedOut => {
            let msg = test.error.as_ref().map(|e| escape_xml(&e.message)).unwrap_or_default();
            writeln!(xml, r#"      <failure message="{msg}">{msg}</failure>"#).ok();
          }
          TestStatus::Skipped => {
            writeln!(xml, r#"      <skipped />"#).ok();
          }
          _ => {}
        }

        let user_steps: Vec<&TestStep> = test.steps.iter().filter(|s| s.category == StepCategory::TestStep).collect();
        if !user_steps.is_empty() {
          let mut lines = String::new();
          format_steps(&user_steps, &mut lines, 0);
          writeln!(xml, r#"      <system-out><![CDATA[{lines}]]></system-out>"#).ok();
        }

        writeln!(xml, r#"    </testcase>"#).ok();
      }
      writeln!(xml, r#"  </testsuite>"#).ok();
    }

    writeln!(xml, r#"</testsuites>"#).ok();

    if let Some(parent) = self.output_path.parent() {
      let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&self.output_path, xml).map_err(|e| format!("write {}: {e}", self.output_path.display()))?;
    Ok(())
  }
}

fn format_steps(steps: &[&TestStep], out: &mut String, indent: usize) {
  use std::fmt::Write;
  let pad = "  ".repeat(indent);
  for step in steps {
    let icon = match step.status {
      StepStatus::Passed => "v",
      StepStatus::Failed => "x",
      StepStatus::Skipped => "-",
      StepStatus::Pending => "P",
    };
    let _ = writeln!(out, "{pad}{icon} {} ({}ms)", step.title, step.duration.as_millis());
    if let Some(err) = &step.error {
      let _ = writeln!(out, "{pad}  Error: {err}");
    }
    let nested: Vec<&TestStep> = step.steps.iter().filter(|s| s.category == StepCategory::TestStep).collect();
    if !nested.is_empty() {
      format_steps(&nested, out, indent + 1);
    }
  }
}

fn escape_xml(s: &str) -> String {
  s.replace('&', "&amp;")
    .replace('<', "&lt;")
    .replace('>', "&gt;")
    .replace('"', "&quot;")
    .replace('\'', "&apos;")
}
