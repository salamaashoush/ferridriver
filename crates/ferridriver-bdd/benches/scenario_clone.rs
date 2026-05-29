//! Measures the per-scenario-execution clone cost that `translate_scenario`
//! pays on the path to running a scenario.
//!
//! Before: the scenario was deep-cloned once at closure capture and again on
//! every invocation (`ScenarioExecution: Clone` -> full `Vec<ScenarioStep>`).
//! After: the scenario is moved into an `Arc` and each invocation does a
//! refcount bump. This bench contrasts the two so the win is concrete.

use std::path::PathBuf;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use ferridriver_bdd::data_table::DataTable;
use ferridriver_bdd::scenario::{ScenarioExecution, ScenarioStep};
use rustc_hash::FxHashMap;
use std::hint::black_box;

/// Build a realistic expanded scenario: a Background + body totalling `n`
/// steps, one of which carries a small data table and one a doc string.
fn make_scenario(n: usize) -> ScenarioExecution {
  let mut steps = Vec::with_capacity(n);
  for i in 0..n {
    let table = (i == 2).then(|| {
      DataTable::new(vec![
        vec!["name".into(), "email".into()],
        vec!["Ada".into(), "ada@example.com".into()],
        vec!["Linus".into(), "linus@example.com".into()],
      ])
    });
    let docstring = (i == 5).then(|| "a multi-line\npayload body\nfor the step".to_string());
    steps.push(ScenarioStep {
      keyword: "When ".to_string(),
      text: format!("I click the \"submit-button-number-{i}\" control"),
      table,
      docstring,
      line: i + 3,
    });
  }

  let mut example_values = FxHashMap::default();
  example_values.insert("count".to_string(), "42".to_string());

  ScenarioExecution {
    feature_name: "Checkout flow with a fairly long descriptive name".to_string(),
    feature_path: PathBuf::from("tests/features/checkout.feature"),
    name: "Buy items as a returning customer (Examples: row 3)".to_string(),
    tags: vec![
      "@smoke".to_string(),
      "@checkout".to_string(),
      "@slow(ci)".to_string(),
      "@owner(team-payments)".to_string(),
    ],
    steps,
    location: "tests/features/checkout.feature:17".to_string(),
    example_values: Some(example_values),
  }
}

fn bench_clone(c: &mut Criterion) {
  for &n in &[5usize, 20, 60] {
    let scenario = make_scenario(n);
    let arc = Arc::new(scenario.clone());

    let mut group = c.benchmark_group(format!("scenario_capture/{n}_steps"));
    group.bench_function("deep_clone", |b| b.iter(|| black_box(scenario.clone())));
    group.bench_function("arc_clone", |b| b.iter(|| black_box(Arc::clone(&arc))));
    group.finish();
  }
}

criterion_group!(benches, bench_clone);
criterion_main!(benches);
