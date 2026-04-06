//! BDD rerun reporter: writes failed scenario locations for re-execution.

use std::path::PathBuf;

use ferridriver_test::model::TestStatus;
use ferridriver_test::reporter::{Reporter, ReporterEvent};

pub struct BddRerunReporter {
  output_path: PathBuf,
  failed: Vec<String>,
}

impl BddRerunReporter {
  pub fn new(output_path: PathBuf) -> Self {
    Self {
      output_path,
      failed: Vec::new(),
    }
  }
}

#[async_trait::async_trait]
impl Reporter for BddRerunReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    if let ReporterEvent::TestFinished { test_id, outcome } = event {
      if matches!(outcome.status, TestStatus::Failed | TestStatus::TimedOut) {
        self.failed.push(test_id.file_location());
      }
    }
  }

  async fn finalize(&mut self) -> Result<(), String> {
    if self.failed.is_empty() {
      return Ok(());
    }

    self.failed.sort();
    self.failed.dedup();

    let content = self.failed.join("\n") + "\n";

    if let Some(parent) = self.output_path.parent() {
      std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&self.output_path, content)
      .map_err(|e| format!("cannot write {}: {e}", self.output_path.display()))?;

    tracing::info!(
      "BDD rerun file written to {} ({} failed)",
      self.output_path.display(),
      self.failed.len()
    );
    Ok(())
  }
}
