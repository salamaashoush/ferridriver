//! Playwright's test locks: tests that name the same `lock` never run at
//! the same time, across files, workers and projects.
//!
//! Workers and projects all run in this process, so one table of named
//! mutexes covers them. A worker takes the locks of the work it picked up
//! before it starts, in name order so two workers wanting the same pair
//! cannot each hold one, and waiting for a lock is not time the test is
//! charged for.

use std::sync::{Arc, LazyLock, Mutex};

use rustc_hash::FxHashMap;

use crate::model::TestAnnotation;

static TABLE: LazyLock<Mutex<FxHashMap<String, Arc<tokio::sync::Mutex<()>>>>> = LazyLock::new(Mutex::default);

/// The locks held for one piece of work; dropping it releases them.
pub struct Held {
  _guards: Vec<tokio::sync::OwnedMutexGuard<()>>,
}

/// The lock names a test declared, on itself or through its suites.
pub fn names(annotations: &[TestAnnotation]) -> impl Iterator<Item = &str> {
  annotations.iter().filter_map(|a| match a {
    TestAnnotation::Lock(name) => Some(name.as_str()),
    _ => None,
  })
}

/// Take every lock in `names`, waiting for whoever holds one.
pub async fn acquire<'a>(names: impl Iterator<Item = &'a str>) -> Held {
  let mut wanted: Vec<&str> = names.collect();
  wanted.sort_unstable();
  wanted.dedup();
  let mut held = Vec::with_capacity(wanted.len());
  for name in wanted {
    let lock = {
      let mut table = TABLE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      Arc::clone(table.entry(name.to_string()).or_default())
    };
    held.push(lock.lock_owned().await);
  }
  Held { _guards: held }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn a_shared_name_serializes_and_distinct_names_do_not() {
    let first = acquire(["locks-test-a", "locks-test-b"].into_iter()).await;
    let other = acquire(std::iter::once("locks-test-c")).await;
    let mut blocked = Box::pin(acquire(["locks-test-b", "locks-test-a"].into_iter()));
    assert!(futures::poll!(&mut blocked).is_pending());
    drop(first);
    drop(blocked.await);
    drop(other);
  }
}
