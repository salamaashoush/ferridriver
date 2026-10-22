//! Deterministic sharding for distributing tests across CI machines.
//!
//! The unit of sharding is what must run together: a serial group (every
//! suite naming one serial scope) moves as a whole, and every other test
//! moves alone. A unit is keyed by its group key or the test's stable id,
//! both computed from the config-root-relative file, so two machines with
//! different working directories agree on every assignment.

use std::collections::HashMap;

use crate::model::{ShardInfo, TestPlan};

/// Where each unit of `plan` goes: the shard an explicit assignment names
/// for any of its tests, else the hash of its key.
///
/// # Errors
/// When the assignment puts two tests of one serial group on different
/// shards; running either half alone would break the group.
pub fn filter_by_shard(
  plan: &mut TestPlan,
  shard: &ShardInfo,
  project: &str,
  assignment: Option<&HashMap<String, u32>>,
) -> Result<(), String> {
  let mut groups: HashMap<String, Option<u32>> = HashMap::new();
  if let Some(assignment) = assignment {
    for suite in &plan.suites {
      let Some(key) = suite.group_key(project) else { continue };
      let entry = groups.entry(key.clone()).or_default();
      for test in &suite.tests {
        let Some(&assigned) = assignment.get(&test.id.stable_id(project)) else {
          continue;
        };
        match *entry {
          Some(existing) if existing != assigned => {
            return Err(format!(
              "--shard-assignment splits the serial group `{key}` across shards {existing} and {assigned}; \
               a serial group runs on one shard"
            ));
          },
          _ => *entry = Some(assigned),
        }
      }
    }
  }

  for suite in &mut plan.suites {
    match suite.group_key(project) {
      Some(key) => {
        let target = groups
          .get(&key)
          .copied()
          .flatten()
          .unwrap_or_else(|| hashed_shard(&key, shard.total));
        if target != shard.current {
          suite.tests.clear();
        }
      },
      None => suite.tests.retain(|test| {
        let id = test.id.stable_id(project);
        let target = assignment
          .and_then(|assignment| assignment.get(&id).copied())
          .unwrap_or_else(|| hashed_shard(&id, shard.total));
        target == shard.current
      }),
    }
  }
  plan.suites.retain(|s| !s.tests.is_empty());
  plan.total_tests = plan.suites.iter().map(|s| s.tests.len()).sum();
  plan.shard = Some(shard.clone());
  Ok(())
}

/// The 1-based shard a key hashes to. SHA-1 rather than the process hasher:
/// the answer has to be the same on every platform and every build.
fn hashed_shard(key: &str, total: u32) -> u32 {
  let digest = ferridriver::tracing::sha1_hex(key.as_bytes());
  let value = u64::from_str_radix(&digest[..16], 16).unwrap_or_default();
  let index = value % u64::from(total.max(1));
  u32::try_from(index).unwrap_or_default() + 1
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{ExpectedStatus, Hooks, SuiteMode, TestCase, TestId, TestSuite};

  fn case(file: &str, suite: &str, name: &str) -> TestCase {
    TestCase {
      id: TestId {
        file: file.into(),
        suite: Some(format!("{file}::{suite}")),
        name: name.into(),
        root_path: Some(file.into()),
        ..TestId::default()
      },
      test_fn: std::sync::Arc::new(|_| Box::pin(async { Ok(()) })),
      fixture_requests: Vec::new(),
      annotations: Vec::new(),
      timeout: None,
      retries: None,
      expected_status: ExpectedStatus::Pass,
      use_options: None,
      metadata: None,
    }
  }

  fn suite(file: &str, name: &str, mode: SuiteMode, scope: Option<&str>, tests: &[&str]) -> TestSuite {
    TestSuite {
      name: name.into(),
      file: file.into(),
      tests: tests.iter().map(|t| case(file, name, t)).collect(),
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode,
      serial_scope: scope.map(str::to_string),
    }
  }

  fn plan(suites: Vec<TestSuite>) -> TestPlan {
    let total_tests = suites.iter().map(|s| s.tests.len()).sum();
    TestPlan {
      suites,
      total_tests,
      ..TestPlan::default()
    }
  }

  fn corpus() -> TestPlan {
    let parallel: Vec<String> = (0..40).map(|i| format!("p{i}")).collect();
    let parallel: Vec<&str> = parallel.iter().map(String::as_str).collect();
    plan(vec![
      suite("a.spec.ts", "loose", SuiteMode::Parallel, None, &parallel),
      suite("b.spec.ts", "checkout", SuiteMode::Serial, None, &["s1", "s2", "s3"]),
      suite(
        "b.spec.ts",
        "checkout > pay",
        SuiteMode::Serial,
        Some("checkout"),
        &["s4", "s5"],
      ),
    ])
  }

  fn names(plan: &TestPlan) -> Vec<String> {
    plan
      .suites
      .iter()
      .flat_map(|s| s.tests.iter().map(|t| t.id.name.clone()))
      .collect()
  }

  fn shard_of(total: u32, assignment: Option<&HashMap<String, u32>>) -> Vec<Vec<String>> {
    (1..=total)
      .map(|current| {
        let mut p = corpus();
        filter_by_shard(&mut p, &ShardInfo { current, total }, "", assignment).expect("shard");
        names(&p)
      })
      .collect()
  }

  #[test]
  fn every_test_lands_on_exactly_one_shard() {
    let shards = shard_of(3, None);
    let mut all: Vec<String> = shards.concat();
    all.sort();
    let mut expected = names(&corpus());
    expected.sort();
    assert_eq!(all, expected);
  }

  #[test]
  fn a_serial_group_with_nested_suites_stays_whole() {
    for total in 2..=6 {
      let shards = shard_of(total, None);
      let holding: Vec<&Vec<String>> = shards.iter().filter(|s| s.iter().any(|n| n.starts_with('s'))).collect();
      assert_eq!(
        holding.len(),
        1,
        "group split across shards at total {total}: {shards:?}"
      );
      let group: Vec<&String> = holding[0].iter().filter(|n| n.starts_with('s')).collect();
      assert_eq!(group.len(), 5);
    }
  }

  #[test]
  fn an_assignment_moves_a_test_and_its_whole_group() {
    let corpus = corpus();
    let id = |name: &str| {
      corpus
        .suites
        .iter()
        .flat_map(|s| &s.tests)
        .find(|t| t.id.name == name)
        .map(|t| t.id.stable_id(""))
        .expect("test")
    };
    let assignment: HashMap<String, u32> = [(id("p7"), 2), (id("s4"), 3)].into_iter().collect();
    let shards = shard_of(3, Some(&assignment));
    assert!(shards[1].contains(&"p7".to_string()));
    for name in ["s1", "s2", "s3", "s4", "s5"] {
      assert!(
        shards[2].contains(&name.to_string()),
        "{name} not on shard 3: {shards:?}"
      );
    }
  }

  #[test]
  fn an_assignment_that_splits_a_group_is_refused() {
    let corpus = corpus();
    let id = |name: &str| {
      corpus
        .suites
        .iter()
        .flat_map(|s| &s.tests)
        .find(|t| t.id.name == name)
        .map(|t| t.id.stable_id(""))
        .expect("test")
    };
    let assignment: HashMap<String, u32> = [(id("s1"), 1), (id("s5"), 2)].into_iter().collect();
    let mut p = corpus.clone();
    let error =
      filter_by_shard(&mut p, &ShardInfo { current: 1, total: 2 }, "", Some(&assignment)).expect_err("split group");
    assert!(error.contains("b.spec.ts > checkout"), "{error}");
  }

  #[test]
  fn the_shard_does_not_depend_on_the_working_directory() {
    // Same root-relative path, different working-directory paths.
    let mut from_root = corpus();
    let mut from_sub = corpus();
    for suite in &mut from_sub.suites {
      for test in &mut suite.tests {
        let file = format!("../repo/{}", test.id.file);
        test.id.suite = test.id.suite.as_ref().map(|s| s.replacen(&test.id.file, &file, 1));
        test.id.file = file;
      }
    }
    let info = ShardInfo { current: 2, total: 4 };
    filter_by_shard(&mut from_root, &info, "", None).expect("shard");
    filter_by_shard(&mut from_sub, &info, "", None).expect("shard");
    assert_eq!(names(&from_root), names(&from_sub));
  }
}
