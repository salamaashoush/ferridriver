//! Captured `console.*` output with size limits.
//!
//! The engine installs a `console` global inside every script context. Each
//! call (`console.log`, `.info`, `.warn`, `.error`, `.debug`) pushes an entry
//! into a shared `ConsoleCapture`. Output is bounded by three limits:
//!
//! - max entries (count-based),
//! - max total bytes (sum of message lengths),
//! - max per-entry bytes (individual `message` truncation).
//!
//! When a limit is hit, a single `system`-level entry is appended noting
//! truncation and no further entries are recorded.
//!
//! A capture built with [`ConsoleCapture::with_sink`] instead forwards every
//! entry to the sink as it happens and retains nothing — the streaming shape a
//! human at a terminal wants, where the limits above (which exist to bound a
//! single result document) would only mangle the output.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::result::{ConsoleEntry, ConsoleLevel};

/// Receiver for console entries as they are produced.
///
/// Installed via [`ConsoleCapture::with_sink`] / `ScriptEngineConfig::console_sink`
/// by hosts that want live output instead of a buffer drained at the end.
/// `emit` runs on the VM thread inside the `console.*` call, so it must not
/// block for long.
pub trait ConsoleSink: std::fmt::Debug + Send + Sync + 'static {
  fn emit(&self, entry: &ConsoleEntry);

  /// Whether rendered values may carry ANSI styling. Node colours
  /// `util.inspect` output only when the stream is a terminal — and `log` and
  /// `error` go to different streams, so the answer is per level. A sink
  /// writing to a pipe (and the buffered no-sink path) answers `false`.
  fn styled_for(&self, _level: ConsoleLevel) -> bool {
    false
  }

  /// `console.clear()`. Default no-op, like Node's on a non-terminal stream.
  fn clear(&self) {}
}

/// Thread-safe capture buffer.
///
/// Shared between the JS context (via `Arc<ConsoleCapture>`) and the engine,
/// which drains the buffer into the final `ScriptResult` after the script
/// completes.
pub struct ConsoleCapture {
  max_entries: usize,
  max_total_bytes: usize,
  max_entry_bytes: usize,
  started: Instant,
  sink: Option<Arc<dyn ConsoleSink>>,
  secrets: ferridriver::response::Secrets,
  inner: Mutex<ConsoleInner>,
}

struct ConsoleInner {
  entries: Vec<ConsoleEntry>,
  total_bytes: usize,
  truncated: bool,
}

impl ConsoleCapture {
  #[must_use]
  pub fn new(max_entries: usize, max_total_bytes: usize, max_entry_bytes: usize) -> Self {
    Self {
      max_entries,
      max_total_bytes,
      max_entry_bytes,
      started: Instant::now(),
      sink: None,
      secrets: ferridriver::response::Secrets::default(),
      inner: Mutex::new(ConsoleInner {
        entries: Vec::new(),
        total_bytes: 0,
        truncated: false,
      }),
    }
  }

  /// Stream entries to `sink` as they happen instead of buffering them.
  ///
  /// [`Self::drain`] then always returns empty: nothing is retained, and the
  /// count / byte / per-entry limits do not apply, so a streamed message is
  /// never clamped mid-line.
  #[must_use]
  pub fn with_sink(mut self, sink: Arc<dyn ConsoleSink>) -> Self {
    self.sink = Some(sink);
    self
  }

  /// Redact these values out of every message before it is recorded or
  /// forwarded.
  #[must_use]
  pub fn with_secrets(mut self, secrets: ferridriver::response::Secrets) -> Self {
    self.secrets = secrets;
    self
  }

  /// Record one entry.
  ///
  /// `message` is clamped to `max_entry_bytes`, and the entry is only
  /// appended if both the count and total-byte budgets still allow it.
  /// Once any budget is exceeded, a single `system` entry is appended
  /// noting truncation and all further calls are silently dropped.
  pub fn push(&self, level: ConsoleLevel, message: impl Into<String>) {
    let mut message = message.into();

    // Before the sink and before the buffer: a script that logs a credential
    // must not leak it down either path, and truncation below would otherwise
    // be able to cut a secret in half and defeat the match.
    if let std::borrow::Cow::Owned(redacted) = self.secrets.redact(&message) {
      message = redacted;
    }

    if let Some(sink) = &self.sink {
      sink.emit(&ConsoleEntry {
        level,
        message,
        ts_ms: self.started.elapsed().as_millis() as u64,
      });
      return;
    }

    if message.len() > self.max_entry_bytes {
      message.truncate(self.max_entry_bytes);
      message.push('…');
    }

    let Ok(mut inner) = self.inner.lock() else {
      return;
    };

    if inner.truncated {
      return;
    }

    let would_exceed_count = inner.entries.len() >= self.max_entries;
    let would_exceed_bytes = inner.total_bytes.saturating_add(message.len()) > self.max_total_bytes;

    if would_exceed_count || would_exceed_bytes {
      inner.entries.push(ConsoleEntry {
        level: ConsoleLevel::System,
        message: "console capture truncated: limits exceeded".to_string(),
        ts_ms: self.started.elapsed().as_millis() as u64,
      });
      inner.truncated = true;
      return;
    }

    inner.total_bytes = inner.total_bytes.saturating_add(message.len());
    inner.entries.push(ConsoleEntry {
      level,
      message,
      ts_ms: self.started.elapsed().as_millis() as u64,
    });
  }

  /// Drain the captured entries.
  ///
  /// Returns `Vec::new()` if the mutex is poisoned — we prefer silent data
  /// loss over panicking because the engine has no recovery path.
  #[must_use]
  pub fn drain(&self) -> Vec<ConsoleEntry> {
    self
      .inner
      .lock()
      .map(|mut inner| std::mem::take(&mut inner.entries))
      .unwrap_or_default()
  }

  /// Milliseconds since capture was created; used for `ts_ms` in entries.
  #[must_use]
  pub fn elapsed_ms(&self) -> u64 {
    self.started.elapsed().as_millis() as u64
  }

  /// Whether the installed sink renders ANSI styling for `level` — see
  /// [`ConsoleSink::styled_for`]. Always `false` for a buffered capture.
  #[must_use]
  pub fn styled_for(&self, level: ConsoleLevel) -> bool {
    self.sink.as_ref().is_some_and(|s| s.styled_for(level))
  }

  /// Forward `console.clear()` to the sink.
  pub fn clear(&self) {
    if let Some(sink) = &self.sink {
      sink.clear();
    }
  }
}

/// Strip ANSI escape sequences from a captured message so malicious page
/// content (or legitimate page `console.log` bridged through) cannot poison
/// logs with terminal control codes.
#[must_use]
pub fn strip_ansi(input: &str) -> String {
  let mut out = String::with_capacity(input.len());
  let mut chars = input.chars().peekable();
  while let Some(c) = chars.next() {
    if c == '\x1b' && chars.peek() == Some(&'[') {
      chars.next();
      for nc in chars.by_ref() {
        if ('@'..='~').contains(&nc) {
          break;
        }
      }
    } else {
      out.push(c);
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn strip_ansi_removes_color_codes() {
    assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
    assert_eq!(strip_ansi("\x1b[1;34mbold blue\x1b[0m"), "bold blue");
    assert_eq!(strip_ansi("plain"), "plain");
  }

  #[test]
  fn capture_respects_entry_limit() {
    let cap = ConsoleCapture::new(3, 10_000, 1000);
    for i in 0..5 {
      cap.push(ConsoleLevel::Log, format!("line {i}"));
    }
    let entries = cap.drain();
    // 3 real + 1 truncation system entry
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[3].level, ConsoleLevel::System);
  }

  #[test]
  fn capture_respects_byte_limit() {
    let cap = ConsoleCapture::new(100, 20, 100);
    cap.push(ConsoleLevel::Log, "a".repeat(15));
    cap.push(ConsoleLevel::Log, "b".repeat(15));
    let entries = cap.drain();
    // First fits (15 <= 20), second would exceed (15+15=30 > 20) so truncation fires.
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].level, ConsoleLevel::System);
  }

  #[test]
  fn sink_streams_every_entry_and_retains_nothing() {
    #[derive(Debug, Default)]
    struct Collect(Mutex<Vec<(ConsoleLevel, String)>>);
    impl ConsoleSink for Collect {
      fn emit(&self, entry: &ConsoleEntry) {
        if let Ok(mut v) = self.0.lock() {
          v.push((entry.level, entry.message.clone()));
        }
      }
    }

    let sink = Arc::new(Collect::default());
    // Limits that would truncate after one short entry: streaming ignores them.
    let cap = ConsoleCapture::new(1, 4, 4).with_sink(sink.clone());
    cap.push(ConsoleLevel::Log, "first message");
    cap.push(ConsoleLevel::Warn, "second message");

    let seen = sink.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert_eq!(
      seen,
      vec![
        (ConsoleLevel::Log, "first message".to_string()),
        (ConsoleLevel::Warn, "second message".to_string()),
      ]
    );
    assert!(cap.drain().is_empty());
  }

  #[test]
  fn capture_truncates_long_entry() {
    let cap = ConsoleCapture::new(10, 10_000, 5);
    cap.push(ConsoleLevel::Log, "abcdefgh");
    let entries = cap.drain();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].message.starts_with("abcde"));
    assert!(entries[0].message.ends_with('…'));
  }
}
