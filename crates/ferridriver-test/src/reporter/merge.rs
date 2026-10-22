//! Folding several runs' event streams (blobs, stream files) into one.
//!
//! Each input carries its own run boundary and its own view of the tests it
//! ran. Shards are disjoint, but a grid that loses a worker re-runs that
//! worker's tests elsewhere, and then one test has attempts in two inputs,
//! each numbered from 1. Tests are matched by stable id under their project
//! (computed from the config-root-relative path, so inputs written from
//! different working directories agree), their attempts ordered by start
//! time and renumbered, and the run's totals recomputed from the folded
//! attempts: a test that failed on one machine and passed on another is
//! flaky, not one failure plus one pass.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use rustc_hash::{FxHashMap, FxHashSet};

use super::{ReporterEvent, RunStatus};
use crate::model::{TestOutcome, TestOutcomeKind, TestStatus, outcome_kind};

type TestKey = (String, String);

/// The test an event belongs to, if any.
fn key_of(event: &ReporterEvent) -> Option<TestKey> {
  let (project, id) = match event {
    ReporterEvent::TestStarted { test_id, project, .. } => (project.as_str(), test_id),
    ReporterEvent::StepStarted(s) => (s.project.as_str(), &s.test_id),
    ReporterEvent::StepFinished(s) => (s.project.as_str(), &s.test_id),
    ReporterEvent::TestOutput(o) => (o.project.as_str(), &o.test_id),
    ReporterEvent::TestFinished { outcome } => (outcome.project_name.as_str(), &outcome.test_id),
    _ => return None,
  };
  Some((project.to_string(), id.stable_id(project)))
}

/// One attempt's events from one input.
struct Chunk {
  events: Vec<ReporterEvent>,
  finished: Option<Arc<TestOutcome>>,
  input: usize,
}

/// Merge the event streams of several runs, in input order, into the one
/// stream a single run over all of them would have produced.
#[must_use]
pub fn merge_runs(inputs: Vec<Vec<ReporterEvent>>) -> Vec<ReporterEvent> {
  // Which inputs finished each test.
  let mut finished_in: FxHashMap<TestKey, FxHashSet<usize>> = FxHashMap::default();
  for (input, events) in inputs.iter().enumerate() {
    for event in events {
      if let ReporterEvent::TestFinished { .. } = event
        && let Some(key) = key_of(event)
      {
        finished_in.entry(key).or_default().insert(input);
      }
    }
  }
  let folded: FxHashSet<TestKey> = finished_in
    .iter()
    .filter(|(_, inputs)| inputs.len() > 1)
    .map(|(key, _)| key.clone())
    .collect();

  // Lay the inputs end to end. A folded test's events are cut into
  // attempts and set aside; the slot of its last finish is where they all
  // go back, in order.
  enum Slot {
    Event(ReporterEvent),
    Folded(TestKey),
  }
  let mut slots: Vec<Slot> = Vec::new();
  let mut chunks: FxHashMap<TestKey, Vec<Chunk>> = FxHashMap::default();
  let mut open: FxHashMap<(usize, TestKey), Chunk> = FxHashMap::default();
  let mut last_slot: FxHashMap<TestKey, usize> = FxHashMap::default();
  let mut boundaries: Vec<ReporterEvent> = Vec::new();
  let mut run_errors = false;

  for (input, events) in inputs.into_iter().enumerate() {
    for event in events {
      match &event {
        ReporterEvent::RunStarted { .. } | ReporterEvent::RunFinished { .. } => {
          boundaries.push(event);
          continue;
        },
        ReporterEvent::RunError { .. } => run_errors = true,
        _ => {},
      }
      let Some(key) = key_of(&event).filter(|key| folded.contains(key)) else {
        slots.push(Slot::Event(event));
        continue;
      };
      let slot = (input, key.clone());
      if matches!(event, ReporterEvent::TestStarted { .. })
        && let Some(stale) = open.remove(&slot)
      {
        // A start with the previous attempt never finished: that worker
        // was lost mid-test, and its partial attempt is not a result.
        drop(stale);
      }
      let chunk = open.entry(slot.clone()).or_insert_with(|| Chunk {
        events: Vec::new(),
        finished: None,
        input,
      });
      if let ReporterEvent::TestFinished { outcome } = &event {
        chunk.finished = Some(Arc::clone(outcome));
        chunk.events.push(event);
        if let Some(done) = open.remove(&slot) {
          chunks.entry(key.clone()).or_default().push(done);
        }
        last_slot.insert(key.clone(), slots.len());
        slots.push(Slot::Folded(key));
      } else {
        chunk.events.push(event);
      }
    }
  }

  for attempts in chunks.values_mut() {
    attempts.sort_by_key(|chunk| {
      (
        chunk.finished.as_ref().map_or(SystemTime::UNIX_EPOCH, |o| o.start_time),
        chunk.input,
        chunk.finished.as_ref().map_or(0, |o| o.attempt),
      )
    });
    renumber(attempts);
  }

  let mut body: Vec<ReporterEvent> = Vec::with_capacity(slots.len());
  for (index, slot) in slots.into_iter().enumerate() {
    match slot {
      Slot::Event(event) => body.push(event),
      Slot::Folded(key) => {
        if last_slot.get(&key) == Some(&index)
          && let Some(attempts) = chunks.remove(&key)
        {
          body.extend(attempts.into_iter().flat_map(|chunk| chunk.events));
        }
      },
    }
  }

  let tally = Tally::of(&body);
  wrap_in_boundary(body, boundaries, &tally, &finished_in, run_errors)
}

/// Number a folded test's attempts 1..n in the order they ran; every one
/// but the last is then a retry that a later attempt followed. Every
/// attempt also takes the first attempt's id: inputs written from
/// different working directories name the file differently, and a
/// reporter keys a test by its whole id.
fn renumber(attempts: &mut [Chunk]) {
  let total = u32::try_from(attempts.len()).unwrap_or(u32::MAX);
  let canonical = attempts
    .first()
    .and_then(|chunk| chunk.finished.as_ref())
    .map(|outcome| outcome.test_id.clone());
  for (index, chunk) in attempts.iter_mut().enumerate() {
    let attempt = u32::try_from(index + 1).unwrap_or(u32::MAX);
    for event in &mut chunk.events {
      if let Some(id) = &canonical {
        retarget(event, id);
      }
      match event {
        ReporterEvent::TestStarted { attempt: a, .. } => *a = attempt,
        ReporterEvent::TestFinished { outcome } => {
          let mut renumbered = (**outcome).clone();
          renumbered.attempt = attempt;
          renumbered.max_attempts = total;
          *outcome = Arc::new(renumbered);
        },
        _ => {},
      }
    }
  }
}

/// Point a test-scoped event at `id`.
fn retarget(event: &mut ReporterEvent, id: &crate::model::TestId) {
  match event {
    ReporterEvent::TestStarted { test_id, .. } => test_id.clone_from(id),
    ReporterEvent::StepStarted(step) if step.test_id != *id => {
      let mut moved = (**step).clone();
      moved.test_id = id.clone();
      *step = Arc::new(moved);
    },
    ReporterEvent::StepFinished(step) if step.test_id != *id => {
      let mut moved = (**step).clone();
      moved.test_id = id.clone();
      *step = Arc::new(moved);
    },
    ReporterEvent::TestOutput(output) if output.test_id != *id => {
      let mut moved = (**output).clone();
      moved.test_id = id.clone();
      *output = Arc::new(moved);
    },
    ReporterEvent::TestFinished { outcome } if outcome.test_id != *id => {
      let mut moved = (**outcome).clone();
      moved.test_id = id.clone();
      *outcome = Arc::new(moved);
    },
    _ => {},
  }
}

/// Final verdicts per test, from the merged body.
#[derive(Default)]
struct Tally {
  tests: usize,
  passed: usize,
  failed: usize,
  skipped: usize,
  flaky: usize,
}

impl Tally {
  fn of(body: &[ReporterEvent]) -> Self {
    let mut attempts: FxHashMap<TestKey, (Vec<TestStatus>, crate::model::ExpectedStatus)> = FxHashMap::default();
    let mut order: Vec<TestKey> = Vec::new();
    for event in body {
      if let ReporterEvent::TestFinished { outcome } = event
        && let Some(key) = key_of(event)
      {
        let entry = attempts.entry(key.clone()).or_insert_with(|| {
          order.push(key);
          (Vec::new(), outcome.expected_status)
        });
        entry.0.push(outcome.status);
        entry.1 = outcome.expected_status;
      }
    }
    let mut tally = Self {
      tests: order.len(),
      ..Self::default()
    };
    for key in &order {
      let (statuses, expected) = &attempts[key];
      match outcome_kind(statuses, *expected) {
        TestOutcomeKind::Expected => tally.passed += 1,
        TestOutcomeKind::Flaky => {
          tally.passed += 1;
          tally.flaky += 1;
        },
        TestOutcomeKind::Skipped => tally.skipped += 1,
        TestOutcomeKind::Unexpected => tally.failed += 1,
      }
    }
    tally
  }
}

/// One `RunStarted` and one `RunFinished` around `body`: the earliest start,
/// the summed workers, every input's suite tree, and totals from `tally`.
fn wrap_in_boundary(
  body: Vec<ReporterEvent>,
  boundaries: Vec<ReporterEvent>,
  tally: &Tally,
  finished_in: &FxHashMap<TestKey, FxHashSet<usize>>,
  run_errors: bool,
) -> Vec<ReporterEvent> {
  let mut planned = 0usize;
  let mut workers = 0u32;
  let mut duration = Duration::ZERO;
  let mut abnormal: Option<RunStatus> = None;
  let mut start_time: Option<SystemTime> = None;
  let mut metadata = serde_json::Value::Null;
  let mut preamble = super::api::RunPreamble::empty();
  let mut finished_totals = 0usize;

  for event in boundaries {
    match event {
      ReporterEvent::RunStarted {
        total_tests,
        num_workers,
        metadata: meta,
        start_time: started,
        preamble: shard,
      } => {
        // Each input's tree covers only the tests it ran; the merged
        // report describes them all.
        preamble.merge_from((*shard).clone());
        planned += total_tests;
        workers += num_workers;
        if metadata.is_null() {
          metadata = meta;
        }
        start_time = Some(start_time.map_or(started, |existing| existing.min(started)));
      },
      ReporterEvent::RunFinished {
        total,
        duration: shard_duration,
        status,
        ..
      } => {
        finished_totals += total;
        // Inputs ran concurrently: the merged wall time is the longest.
        duration = duration.max(shard_duration);
        if matches!(status, RunStatus::Interrupted | RunStatus::TimedOut) {
          abnormal = Some(status);
        }
      },
      _ => {},
    }
  }
  // A test planned in two inputs is one test of the merged run.
  let duplicates: usize = finished_in.values().map(|inputs| inputs.len().saturating_sub(1)).sum();
  let total = planned.max(finished_totals).saturating_sub(duplicates).max(tally.tests);
  let status = if tally.failed > 0 || run_errors {
    RunStatus::Failed
  } else {
    abnormal.unwrap_or(RunStatus::Passed)
  };

  let mut out = Vec::with_capacity(body.len() + 2);
  out.push(ReporterEvent::RunStarted {
    total_tests: total,
    num_workers: workers,
    metadata,
    start_time: start_time.unwrap_or(SystemTime::UNIX_EPOCH),
    preamble: Arc::new(preamble),
  });
  out.extend(body);
  out.push(ReporterEvent::RunFinished {
    total,
    passed: tally.passed,
    failed: tally.failed,
    skipped: tally.skipped,
    flaky: tally.flaky,
    duration,
    status,
  });
  out
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::TestId;

  fn id(name: &str) -> TestId {
    TestId {
      file: format!("/machine-a/{name}.spec.ts"),
      suite: None,
      name: name.into(),
      root_path: Some(format!("tests/{name}.spec.ts")),
      ..TestId::default()
    }
  }

  fn started(total: usize) -> ReporterEvent {
    ReporterEvent::RunStarted {
      total_tests: total,
      num_workers: 1,
      metadata: serde_json::Value::Null,
      start_time: SystemTime::UNIX_EPOCH + Duration::from_secs(10),
      preamble: Arc::new(super::super::api::RunPreamble::empty()),
    }
  }

  fn finished(total: usize, passed: usize, failed: usize) -> ReporterEvent {
    ReporterEvent::RunFinished {
      total,
      passed,
      failed,
      skipped: 0,
      flaky: 0,
      duration: Duration::from_secs(5),
      status: if failed > 0 {
        RunStatus::Failed
      } else {
        RunStatus::Passed
      },
    }
  }

  fn attempt(test: TestId, status: TestStatus, attempt: u32, max: u32, at: u64) -> Vec<ReporterEvent> {
    vec![
      ReporterEvent::TestStarted {
        test_id: test.clone(),
        project: "chromium".into(),
        attempt,
        worker_id: 0,
      },
      ReporterEvent::TestFinished {
        outcome: Arc::new(TestOutcome {
          test_id: test,
          status,
          attempt,
          max_attempts: max,
          project_name: "chromium".into(),
          start_time: SystemTime::UNIX_EPOCH + Duration::from_secs(at),
          ..Default::default()
        }),
      },
    ]
  }

  fn finishes(events: &[ReporterEvent]) -> Vec<(String, u32, u32, TestStatus)> {
    events
      .iter()
      .filter_map(|e| match e {
        ReporterEvent::TestFinished { outcome } => Some((
          outcome.test_id.name.clone(),
          outcome.attempt,
          outcome.max_attempts,
          outcome.status,
        )),
        _ => None,
      })
      .collect()
  }

  #[test]
  fn a_test_retried_on_another_shard_folds_into_one_flaky_test() {
    // Shard 2 ran `cart` later (the grid re-queued it) from another
    // checkout path; the root-relative path is what matches them.
    let mut moved = id("cart");
    moved.file = "/machine-b/work/tests/cart.spec.ts".into();
    let shard1 = [
      vec![started(2)],
      attempt(id("cart"), TestStatus::Failed, 1, 1, 100),
      attempt(id("login"), TestStatus::Passed, 1, 1, 101),
      vec![finished(2, 1, 1)],
    ]
    .concat();
    let shard2 = [
      vec![started(1)],
      attempt(moved, TestStatus::Passed, 1, 1, 200),
      vec![finished(1, 1, 0)],
    ]
    .concat();

    let merged = merge_runs(vec![shard2, shard1]);
    let ids: FxHashSet<String> = merged
      .iter()
      .filter_map(|e| match e {
        ReporterEvent::TestStarted { test_id, .. } if test_id.name == "cart" => Some(test_id.file.clone()),
        ReporterEvent::TestFinished { outcome } if outcome.test_id.name == "cart" => Some(outcome.test_id.file.clone()),
        _ => None,
      })
      .collect();
    assert_eq!(ids.len(), 1, "every attempt names the test the same way: {ids:?}");
    let finishes = finishes(&merged);
    let cart: Vec<_> = finishes.iter().filter(|f| f.0 == "cart").collect();
    assert_eq!(
      cart,
      [
        &("cart".to_string(), 1, 2, TestStatus::Failed),
        &("cart".to_string(), 2, 2, TestStatus::Passed)
      ]
    );
    let ReporterEvent::RunFinished {
      total,
      passed,
      failed,
      flaky,
      status,
      ..
    } = merged.last().expect("boundary")
    else {
      panic!("RunFinished last");
    };
    assert_eq!((*total, *passed, *failed, *flaky), (2, 2, 0, 1));
    assert_eq!(*status, RunStatus::Passed);
    let ReporterEvent::RunStarted { total_tests, .. } = &merged[0] else {
      panic!("RunStarted first");
    };
    assert_eq!(*total_tests, 2);
  }

  #[test]
  fn disjoint_shards_keep_their_attempts_and_sum_their_tests() {
    let shard1 = [
      vec![started(1)],
      attempt(id("a"), TestStatus::Failed, 1, 2, 1),
      attempt(id("a"), TestStatus::Passed, 2, 2, 2),
      vec![finished(1, 1, 0)],
    ]
    .concat();
    let shard2 = [
      vec![started(1)],
      attempt(id("b"), TestStatus::Failed, 1, 1, 3),
      vec![finished(1, 0, 1)],
    ]
    .concat();
    let merged = merge_runs(vec![shard1, shard2]);
    assert_eq!(merged.len(), 2 + 6, "one boundary pair around every test event");
    let ReporterEvent::RunFinished {
      total,
      passed,
      failed,
      flaky,
      status,
      ..
    } = merged.last().expect("boundary")
    else {
      panic!("RunFinished last");
    };
    assert_eq!((*total, *passed, *failed, *flaky), (2, 1, 1, 1));
    assert_eq!(*status, RunStatus::Failed);
  }
}
