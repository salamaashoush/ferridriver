//! How long each test took when it last ran, kept between runs so the
//! scheduler can start the longest work first. Running long tests last is
//! what leaves workers idle at the end of a run: with `m` workers, any
//! order can take up to `2 - 1/m` times the shortest possible run, and
//! longest-first stays within `4/3 - 1/(3m)` of it (Graham, 1969). Jest
//! and Vitest order their files the same way from the same kind of cache.
//!
//! Keyed by a test's stable id under its project, so the same test in two
//! projects (two browsers) keeps two durations.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::dispatcher::WorkItem;

const FILE: &str = "test-timings.json";
const VERSION: u32 = 1;

#[derive(Default, Serialize, Deserialize)]
struct Stored {
  version: u32,
  /// Milliseconds, by stable test id.
  tests: FxHashMap<String, u64>,
}

pub(crate) struct Timings {
  path: PathBuf,
  tests: Mutex<FxHashMap<String, u64>>,
}

impl Timings {
  /// The durations recorded under `root`, or none when there is no
  /// readable record of this version.
  pub(crate) fn load(root: &Path) -> Self {
    let path = root.join(".ferridriver").join(FILE);
    let tests = std::fs::read(&path)
      .ok()
      .and_then(|bytes| serde_json::from_slice::<Stored>(&bytes).ok())
      .filter(|stored| stored.version == VERSION)
      .map(|stored| stored.tests)
      .unwrap_or_default();
    Self {
      path,
      tests: Mutex::new(tests),
    }
  }

  pub(crate) fn record(&self, id: String, duration: std::time::Duration) {
    let ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
    self.lock().insert(id, ms);
  }

  /// What `test` took when it last ran under `project`.
  pub(crate) fn last(&self, test: &crate::model::TestCase, project: &str) -> Option<u64> {
    self.lock().get(&test.id.stable_id(project)).copied()
  }

  /// What `item` is expected to take: the sum of its tests' last
  /// durations, or `None` while any of them has never run.
  pub(crate) fn estimate(&self, item: &WorkItem, project: &str) -> Option<u64> {
    item.tests().try_fold(0u64, |total, test| {
      Some(total.saturating_add(self.last(test, project)?))
    })
  }

  /// What `tests` are expected to take, counting a test that never ran
  /// as a typical one.
  pub(crate) fn expected<'a>(&self, tests: impl IntoIterator<Item = &'a crate::model::TestCase>, project: &str) -> u64 {
    let typical = self.typical();
    tests
      .into_iter()
      .map(|test| self.last(test, project).unwrap_or(typical))
      .fold(0, u64::saturating_add)
  }

  /// The mean recorded duration, or a second when nothing is recorded.
  fn typical(&self) -> u64 {
    let tests = self.lock();
    let count = tests.len() as u64;
    if count == 0 {
      return 1_000;
    }
    tests.values().fold(0u64, |total, &ms| total.saturating_add(ms)) / count
  }

  /// Write the record back, keeping tests this run did not reach.
  pub(crate) fn save(&self) -> std::io::Result<()> {
    let stored = Stored {
      version: VERSION,
      tests: self.lock().clone(),
    };
    let bytes = serde_json::to_vec(&stored)?;
    let dir = self.path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    // A run killed mid-write must not leave half a file the next run
    // cannot read, so the record is replaced, never rewritten in place.
    let partial = dir.join(format!("{FILE}.{}", std::process::id()));
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, &self.path).inspect_err(|_| {
      if let Err(error) = std::fs::remove_file(&partial) {
        tracing::debug!(target: "ferridriver::runner", "leftover {}: {error}", partial.display());
      }
    })
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, FxHashMap<String, u64>> {
    self.tests.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
  }
}
