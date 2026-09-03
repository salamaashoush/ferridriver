//! What one snapshot holds that an earlier one did not.
//!
//! Ported from `HeapSnapshot.ts::calculateSnapshotDiff`, which is what
//! `compare_heapsnapshots` reports. Two snapshots of the same page are
//! compared class by class, and within a class by OBJECT ID: an id is
//! assigned once and never reused, so an id in both snapshots is the
//! same object surviving, an id only in the base was collected, and an
//! id only in the current was allocated in between.
//!
//! Two things make that harder than a set difference.
//!
//! Each snapshot names its plain objects after the shapes IT holds, so
//! the same class can be `{id, name}` in one and `Object` in the other.
//! The base is therefore re-classified under the current's definitions
//! before anything is compared, which is what
//! [`Analysis::aggregates_for_diff`] is for.
//!
//! And the merge walks both id lists once, so both have to ascend --
//! hence the id ordering that `aggregates_for_diff` applies and
//! [`Analysis::aggregates`] deliberately does not.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::analysis::Analysis;
use crate::query::Aggregate;

/// How one class changed between two snapshots.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassDiff {
  pub name: String,
  pub added_count: usize,
  pub removed_count: usize,
  pub added_size: u64,
  pub removed_size: u64,
  pub count_delta: i64,
  pub size_delta: i64,
  pub added_indexes: Vec<usize>,
  pub added_ids: Vec<u64>,
  pub added_self_sizes: Vec<u64>,
  pub deleted_indexes: Vec<usize>,
  pub deleted_ids: Vec<u64>,
  pub deleted_self_sizes: Vec<u64>,
}

impl Analysis {
  /// Every class that gained or lost an object since `base`.
  ///
  /// Keyed the way [`Analysis::aggregates`] keys its classes, so a
  /// caller can go straight from a changed class to its members.
  /// Classes that neither gained nor lost are left out entirely -- a
  /// class whose objects all survived is not a finding, however large.
  #[must_use]
  pub fn diff_since(&self, base: &Analysis) -> BTreeMap<String, ClassDiff> {
    // Both sides grouped by the CURRENT snapshot's shape names: the
    // base's own names describe the objects the base held.
    let before = base.aggregates_for_diff(&self.interfaces);
    let after = self.aggregates_for_diff(&self.interfaces);

    let mut out = BTreeMap::new();
    for (key, aggregate) in &before {
      if let Some(diff) = diff_for_class(base, Some(aggregate), self, after.get(key)) {
        out.insert(key.clone(), diff);
      }
    }
    for (key, aggregate) in &after {
      if before.contains_key(key) {
        continue;
      }
      if let Some(diff) = diff_for_class(base, None, self, Some(aggregate)) {
        out.insert(key.clone(), diff);
      }
    }
    out
  }
}

/// One class's members in both snapshots, merged on id.
///
/// Returns `None` where nothing was added and nothing removed, which is
/// upstream's way of saying the class is unchanged even if its members
/// grew or shrank.
fn diff_for_class(
  base: &Analysis,
  before: Option<&Aggregate>,
  current: &Analysis,
  after: Option<&Aggregate>,
) -> Option<ClassDiff> {
  let deleted: Vec<usize> = before.map(|aggregate| aggregate.idxs.clone()).unwrap_or_default();
  let added: Vec<usize> = after.map(|aggregate| aggregate.idxs.clone()).unwrap_or_default();

  // Upstream reads the name off the current snapshot's aggregate and
  // falls back to the base's. Which it picks cannot matter: the class
  // key holds the name, so two aggregates under one key agree about it.
  let mut diff = ClassDiff {
    name: after
      .or(before)
      .map(|aggregate| aggregate.name.clone())
      .unwrap_or_default(),
    ..ClassDiff::default()
  };

  let (mut i, mut j) = (0usize, 0usize);
  while i < deleted.len() && j < added.len() {
    let before_id = base.snapshot.node_id(ordinal(base, deleted[i]));
    let after_id = current.snapshot.node_id(ordinal(current, added[j]));
    match before_id.cmp(&after_id) {
      std::cmp::Ordering::Less => {
        record_deleted(&mut diff, base, deleted[i]);
        i += 1;
      },
      std::cmp::Ordering::Greater => {
        record_added(&mut diff, current, added[j]);
        j += 1;
      },
      // The same object in both, so it is neither news.
      std::cmp::Ordering::Equal => {
        i += 1;
        j += 1;
      },
    }
  }
  while i < deleted.len() {
    record_deleted(&mut diff, base, deleted[i]);
    i += 1;
  }
  while j < added.len() {
    record_added(&mut diff, current, added[j]);
    j += 1;
  }

  diff.count_delta =
    i64::try_from(diff.added_count).unwrap_or(i64::MAX) - i64::try_from(diff.removed_count).unwrap_or(i64::MAX);
  diff.size_delta =
    i64::try_from(diff.added_size).unwrap_or(i64::MAX) - i64::try_from(diff.removed_size).unwrap_or(i64::MAX);
  if diff.added_count == 0 && diff.removed_count == 0 {
    return None;
  }
  Some(diff)
}

fn ordinal(analysis: &Analysis, node_index: usize) -> usize {
  node_index / analysis.snapshot.node_layout.field_count
}

fn record_added(diff: &mut ClassDiff, current: &Analysis, node_index: usize) {
  let at = ordinal(current, node_index);
  let self_size = current.snapshot.node_self_size(at);
  diff.added_indexes.push(node_index);
  diff.added_ids.push(current.snapshot.node_id(at));
  diff.added_self_sizes.push(self_size);
  diff.added_count += 1;
  diff.added_size += self_size;
}

fn record_deleted(diff: &mut ClassDiff, base: &Analysis, node_index: usize) {
  let at = ordinal(base, node_index);
  let self_size = base.snapshot.node_self_size(at);
  diff.deleted_indexes.push(node_index);
  diff.deleted_ids.push(base.snapshot.node_id(at));
  diff.deleted_self_sizes.push(self_size);
  diff.removed_count += 1;
  diff.removed_size += self_size;
}
