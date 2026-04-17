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

use std::sync::Mutex;
use std::time::Instant;

use crate::result::{ConsoleEntry, ConsoleLevel};

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
      inner: Mutex::new(ConsoleInner {
        entries: Vec::new(),
        total_bytes: 0,
        truncated: false,
      }),
    }
  }

  /// Record one entry.
  ///
  /// `message` is clamped to `max_entry_bytes`, and the entry is only
  /// appended if both the count and total-byte budgets still allow it.
  /// Once any budget is exceeded, a single `system` entry is appended
  /// noting truncation and all further calls are silently dropped.
  pub fn push(&self, level: ConsoleLevel, message: impl Into<String>) {
    let mut message = message.into();
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
  fn capture_truncates_long_entry() {
    let cap = ConsoleCapture::new(10, 10_000, 5);
    cap.push(ConsoleLevel::Log, "abcdefgh");
    let entries = cap.drain();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].message.starts_with("abcde"));
    assert!(entries[0].message.ends_with('…'));
  }
}
