//! Deterministic sharding for distributing tests across CI machines.

use std::hash::{Hash, Hasher};

use rustc_hash::FxHasher;

use crate::model::{ShardInfo, TestPlan};

/// Deterministic sharding: assigns each test to a shard based on its ID hash.
/// Same test always goes to the same shard regardless of other tests.
fn belongs_to_shard(full_name: &str, shard: &ShardInfo) -> bool {
  let mut hasher = FxHasher::default();
  full_name.hash(&mut hasher);
  let hash = hasher.finish();
  (hash % u64::from(shard.total)) == u64::from(shard.current - 1)
}

/// Filter a test plan to only include tests for this shard.
pub fn filter_by_shard(plan: &mut TestPlan, shard: &ShardInfo) {
  for suite in &mut plan.suites {
    suite.tests.retain(|test| belongs_to_shard(&test.id.full_name(), shard));
  }
  plan.suites.retain(|s| !s.tests.is_empty());
  plan.total_tests = plan.suites.iter().map(|s| s.tests.len()).sum();
  plan.shard = Some(shard.clone());
}
