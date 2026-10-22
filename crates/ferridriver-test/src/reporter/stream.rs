//! `stream` reporter: the blob reporter's event lines, written as they
//! happen instead of zipped at the end.
//!
//! Each reporter event becomes one `WireEvent` JSON line (the blob schema,
//! [`super::blob::SCHEMA_VERSION`]), preceded by the same header line a
//! blob's `events.jsonl` starts with, and flushed before the next event, so
//! a process tailing the output sees a test the moment it finishes. The
//! concatenated lines are a valid blob event body: `merge-reports` reads a
//! stream file the way it reads a blob.
//!
//! A zip can hold artifact bytes; a line stream cannot, so bytes a test
//! attached inline (a failure screenshot, `testInfo.attach(name, { body })`)
//! are written to `<outputDir>/stream-attachments/` and referenced from the
//! event by absolute path. Attachments that already live on disk keep their
//! path.
//!
//! The destination is `stdout` (the default, also `-`), `stderr`, an
//! inherited file descriptor (`fd:3`), or a file path, given as the
//! reporter's `outputFile` option or on the command line as
//! `--reporter stream=fd:3`.

use std::io::Write;
use std::path::PathBuf;

use async_trait::async_trait;

use super::blob::{ResourceSink, Stored, WireEvent, extension_for, sanitize, write_event};
use super::{Reporter, ReporterEvent};

/// Where the lines go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamTarget {
  Stdout,
  Stderr,
  /// A descriptor the parent process left open, opened through
  /// `/dev/fd/<n>` so no descriptor is adopted unsafely.
  Fd(u32),
  File(PathBuf),
}

impl StreamTarget {
  /// `stdout`, `-` or empty; `stderr`; `fd:<n>`; anything else is a path.
  #[must_use]
  pub fn parse(spec: &str) -> Self {
    match spec.trim() {
      "" | "-" | "stdout" => Self::Stdout,
      "stderr" => Self::Stderr,
      other => match other.strip_prefix("fd:").and_then(|n| n.parse().ok()) {
        Some(1) => Self::Stdout,
        Some(2) => Self::Stderr,
        Some(fd) => Self::Fd(fd),
        None => Self::File(PathBuf::from(other)),
      },
    }
  }

  fn open(&self) -> std::io::Result<Box<dyn Write + Send + Sync>> {
    Ok(match self {
      Self::Stdout => Box::new(std::io::stdout()),
      Self::Stderr => Box::new(std::io::stderr()),
      Self::Fd(fd) => Box::new(std::fs::OpenOptions::new().append(true).open(format!("/dev/fd/{fd}"))?),
      Self::File(path) => {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
          std::fs::create_dir_all(parent)?;
        }
        Box::new(std::fs::File::create(path)?)
      },
    })
  }
}

/// Inline artifact bytes, written to files and referenced by path.
struct FileResources {
  dir: PathBuf,
  seen: std::collections::HashSet<PathBuf>,
}

impl ResourceSink for FileResources {
  fn store(&mut self, name_hint: &str, content_type: &str, bytes: &[u8]) -> Option<Stored> {
    let digest = ferridriver::tracing::sha1_hex(bytes);
    let path = self.dir.join(format!(
      "{}-{}{}",
      sanitize(name_hint),
      &digest[..16],
      extension_for(content_type)
    ));
    if !self.seen.contains(&path) {
      if let Err(e) = std::fs::create_dir_all(&self.dir).and_then(|()| std::fs::write(&path, bytes)) {
        tracing::warn!("stream: could not write attachment {}: {e}", path.display());
        return None;
      }
      self.seen.insert(path.clone());
    }
    Some(Stored::Path(path.display().to_string()))
  }
}

pub struct StreamReporter {
  target: StreamTarget,
  out: Option<Box<dyn Write + Send + Sync>>,
  failed: bool,
  resources: FileResources,
  shard: Option<(u32, u32)>,
  line: Vec<u8>,
}

impl StreamReporter {
  /// Stream to `target`, writing inline attachments under `output_dir`.
  #[must_use]
  pub fn new(target: StreamTarget, output_dir: &std::path::Path) -> Self {
    let dir = std::path::absolute(output_dir)
      .unwrap_or_else(|_| output_dir.to_path_buf())
      .join("stream-attachments");
    Self {
      target,
      out: None,
      failed: false,
      resources: FileResources {
        dir,
        seen: std::collections::HashSet::new(),
      },
      shard: None,
      line: Vec::new(),
    }
  }

  /// Record the shard in the header line, as the blob reporter does.
  #[must_use]
  pub fn with_shard(mut self, current: u32, total: u32) -> Self {
    self.shard = Some((current, total));
    self
  }

  fn write(&mut self, event: &WireEvent) {
    if self.failed {
      return;
    }
    if self.out.is_none() {
      match self.target.open() {
        Ok(out) => self.out = Some(out),
        Err(e) => {
          tracing::error!("stream: could not open {:?}: {e}", self.target);
          self.failed = true;
          return;
        },
      }
      let header = WireEvent::header(self.shard.map(|s| s.0), self.shard.map(|s| s.1));
      self.emit(&header);
    }
    self.emit(event);
  }

  fn emit(&mut self, event: &WireEvent) {
    self.line.clear();
    write_event(&mut self.line, event);
    let Some(out) = self.out.as_mut() else { return };
    // A reader that went away (a closed pipe) ends the stream, not the run.
    if let Err(e) = out.write_all(&self.line).and_then(|()| out.flush()) {
      tracing::warn!("stream: write failed, no further events will be written: {e}");
      self.failed = true;
    }
  }
}

#[async_trait]
impl Reporter for StreamReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    if let Some(wire) = WireEvent::from_runtime(event, &mut self.resources) {
      self.write(&wire);
    }
  }

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    if self.out.is_none() && !self.failed {
      // Nothing ran: a header alone is still a valid, empty stream.
      if let Ok(out) = self.target.open() {
        self.out = Some(out);
        let header = WireEvent::header(self.shard.map(|s| s.0), self.shard.map(|s| s.1));
        self.emit(&header);
      }
    }
    if let Some(out) = self.out.as_mut() {
      let _ = out.flush();
    }
    Ok(())
  }

  fn prints_to_stdio(&self) -> bool {
    matches!(self.target, StreamTarget::Stdout | StreamTarget::Stderr)
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::time::Duration;

  use super::*;
  use crate::model::{Attachment, AttachmentBody, TestFailure, TestId, TestOutcome, TestStatus};
  use crate::reporter::blob;

  #[test]
  fn targets_parse() {
    assert_eq!(StreamTarget::parse(""), StreamTarget::Stdout);
    assert_eq!(StreamTarget::parse("-"), StreamTarget::Stdout);
    assert_eq!(StreamTarget::parse("fd:1"), StreamTarget::Stdout);
    assert_eq!(StreamTarget::parse("fd:2"), StreamTarget::Stderr);
    assert_eq!(StreamTarget::parse("fd:3"), StreamTarget::Fd(3));
    assert_eq!(
      StreamTarget::parse("out/events.jsonl"),
      StreamTarget::File(PathBuf::from("out/events.jsonl"))
    );
  }

  #[tokio::test]
  async fn lines_are_a_blob_body_with_attachments_by_path() {
    let dir = std::env::temp_dir().join(format!("ferri-stream-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let file = dir.join("events.jsonl");
    let mut reporter = StreamReporter::new(StreamTarget::File(file.clone()), &dir).with_shard(2, 3);

    let outcome = TestOutcome {
      test_id: TestId {
        file: "a.spec.ts".into(),
        name: "works".into(),
        root_path: Some("a.spec.ts".into()),
        ..TestId::default()
      },
      status: TestStatus::Failed,
      duration: Duration::from_millis(5),
      attempt: 1,
      max_attempts: 1,
      error: Some(TestFailure {
        message: "boom".into(),
        stack: None,
        diff: None,
        screenshot: Some(vec![1, 2, 3]),
        error_context: None,
      }),
      attachments: vec![Attachment {
        name: "note".into(),
        content_type: "text/plain".into(),
        body: AttachmentBody::Bytes(b"hello".to_vec()),
        step_id: None,
      }],
      ..Default::default()
    };
    reporter
      .on_event(&ReporterEvent::TestFinished {
        outcome: Arc::new(outcome),
      })
      .await;
    // Flushed per line: readable before finalize.
    let text = std::fs::read_to_string(&file).expect("stream");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    let header: serde_json::Value = serde_json::from_str(lines[0]).expect("header");
    assert_eq!(header["kind"], "header");
    assert_eq!(header["schema"], blob::SCHEMA_VERSION);
    assert_eq!(header["shard_index"], 2);
    reporter.finalize().await.expect("finalize");

    let events = blob::read_event_stream(&file).expect("read back");
    let ReporterEvent::TestFinished { outcome } = &events[0] else {
      panic!("expected TestFinished: {events:?}");
    };
    assert_eq!(outcome.test_id.root_path.as_deref(), Some("a.spec.ts"));
    let AttachmentBody::Path(path) = &outcome.attachments[0].body else {
      panic!("attachment by path");
    };
    assert!(path.is_absolute());
    assert_eq!(std::fs::read(path).expect("attachment file"), b"hello");
    assert_eq!(
      outcome.error.as_ref().and_then(|e| e.screenshot.clone()),
      Some(vec![1, 2, 3])
    );
    std::fs::remove_dir_all(&dir).ok();
  }
}
