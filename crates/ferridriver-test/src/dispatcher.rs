//! Test dispatcher: MPMC work queue supporting parallel and serial suites.
//!
//! Parallel tests go to a shared channel (natural load balancing).
//! Serial suites go as batches — one worker picks up the entire suite.

use std::sync::Arc;

use crate::model::{Hooks, SuiteMode, TestCase};

/// A single test assigned to a worker.
pub struct TestAssignment {
  pub test: TestCase,
  pub attempt: u32,
  /// Suite key for hook tracking (e.g. `"file.rs::suite_name"`).
  pub suite_key: String,
  /// Shared hooks for this test's suite.
  pub hooks: Arc<Hooks>,
  /// Suite execution mode.
  pub suite_mode: SuiteMode,
}

/// A batch of serial tests — all run on one worker, in order.
pub struct SerialBatch {
  pub suite_key: String,
  pub assignments: Vec<TestAssignment>,
  pub hooks: Arc<Hooks>,
}

/// One more attempt of a test that failed.
pub struct Retry {
  /// The case exactly as it was first scheduled.
  pub test: TestCase,
  pub attempt: u32,
  pub suite_key: String,
  pub hooks: Arc<Hooks>,
  pub suite_mode: SuiteMode,
  /// The rest of the serial group, which resumes after the retry.
  pub serial_tail: Vec<TestAssignment>,
}

/// Work item pulled by a worker.
pub enum WorkItem {
  /// Single parallel test.
  Single(Box<TestAssignment>),
  /// Batch of serial tests (one worker runs all, in order).
  Serial(SerialBatch),
}

impl WorkItem {
  /// Every lock the item's tests declared: the whole item holds them, as
  /// a serial suite runs on one worker from first test to last.
  pub(crate) fn lock_names(&self) -> Vec<String> {
    self
      .tests()
      .flat_map(|test| crate::locks::names(&test.annotations))
      .map(str::to_string)
      .collect()
  }

  /// The tests the item runs, in order.
  pub(crate) fn tests(&self) -> impl Iterator<Item = &TestCase> {
    let assignments = match self {
      Self::Single(assignment) => std::slice::from_ref(&**assignment),
      Self::Serial(batch) => batch.assignments.as_slice(),
    };
    assignments.iter().map(|assignment| &assignment.test)
  }

  pub(crate) fn repeat_each_index(&self) -> u32 {
    match self {
      Self::Single(assignment) => assignment.test.id.repeat_each_index,
      Self::Serial(batch) => batch
        .assignments
        .first()
        .map_or(0, |assignment| assignment.test.id.repeat_each_index),
    }
  }
}

/// Dispatch strategy: parallel tests via shared MPMC channel,
/// serial suites as batches on the same channel.
pub struct Dispatcher {
  tx: async_channel::Sender<WorkItem>,
  rx: async_channel::Receiver<WorkItem>,
  /// Hard-stop signal. When set, workers must drop their current `recv()`
  /// outcome rather than processing it — `close()` alone keeps already-buffered
  /// items in the queue, which means `--max-failures N` and `-x` would still
  /// run every test that was enqueued before the threshold tripped.
  stopped: Arc<std::sync::atomic::AtomicBool>,
}

impl Dispatcher {
  #[must_use]
  pub fn new() -> Self {
    let (tx, rx) = async_channel::unbounded();
    Self {
      tx,
      rx,
      stopped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }
  }

  /// Enqueue a single parallel test.
  pub fn enqueue_single(&self, assignment: TestAssignment) {
    let _ = self.tx.try_send(WorkItem::Single(Box::new(assignment)));
  }

  /// Enqueue an entire serial suite as a batch.
  pub fn enqueue_serial(&self, batch: SerialBatch) {
    let _ = self.tx.try_send(WorkItem::Serial(batch));
  }

  /// Re-enqueue a failed test for another attempt with every setting it
  /// was scheduled with. A serial test goes back as a batch together with
  /// the rest of its group, so the group still runs in order on one worker.
  pub fn retry_shared(&self, retry: Retry) {
    let assignment = TestAssignment {
      test: retry.test,
      attempt: retry.attempt,
      suite_key: retry.suite_key.clone(),
      hooks: Arc::clone(&retry.hooks),
      suite_mode: retry.suite_mode,
    };
    let item = if retry.suite_mode == SuiteMode::Serial {
      let mut assignments = Vec::with_capacity(retry.serial_tail.len() + 1);
      assignments.push(assignment);
      assignments.extend(retry.serial_tail);
      WorkItem::Serial(SerialBatch {
        suite_key: retry.suite_key,
        assignments,
        hooks: retry.hooks,
      })
    } else {
      WorkItem::Single(Box::new(assignment))
    };
    let _ = self.tx.try_send(item);
  }

  /// Get a receiver clone for a worker.
  #[must_use]
  pub fn receiver(&self) -> async_channel::Receiver<WorkItem> {
    self.rx.clone()
  }

  /// Signal no more work will be enqueued.
  pub fn close(&self) {
    self.tx.close();
  }

  /// Trip the hard-stop signal so workers drop any remaining queued items
  /// instead of processing them. Used by `--max-failures` / `-x`.
  pub fn stop(&self) {
    self.stopped.store(true, std::sync::atomic::Ordering::SeqCst);
    self.tx.close();
  }

  /// Worker-side check: should the next item be processed or dropped?
  #[must_use]
  pub fn is_stopped(&self) -> bool {
    self.stopped.load(std::sync::atomic::Ordering::SeqCst)
  }

  /// Worker-side handle to the same atomic flag (cheap clone of the Arc).
  #[must_use]
  pub fn stop_flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
    Arc::clone(&self.stopped)
  }
}

impl Default for Dispatcher {
  fn default() -> Self {
    Self::new()
  }
}

/// The run's `workers` slots, shared by every project running at once.
/// A worker holds its slot while its project has work queued, so a slot
/// changes project (and browser) only when one runs dry; a launch per
/// handoff is what keeps a slot from moving test by test. A freed slot
/// goes to the waiting project with the longest critical path left
/// ([`WorkerRoster::priority`]), and waits for a project still starting
/// that outranks every waiter, so a dependency chain or a project held to
/// one worker starts first rather than running alone at the end. A slot's
/// index is the `parallelIndex` of the worker holding it, unique among
/// the workers running together.
pub(crate) struct RunSlots {
  state: std::sync::Mutex<SlotState>,
}

struct SlotState {
  free: std::collections::BTreeSet<u32>,
  waiters: Vec<Waiter>,
  /// Projects launched that have not queued their tests yet.
  starting: Vec<Arc<WorkerRoster>>,
  asked: u64,
}

struct Waiter {
  roster: Arc<WorkerRoster>,
  asked: u64,
  grant: tokio::sync::oneshot::Sender<SlotLease>,
}

/// A held slot, given back when dropped.
pub(crate) struct SlotLease {
  slots: Option<Arc<RunSlots>>,
  index: u32,
  /// Tracks this slot's claim on the roster's head item until the worker
  /// actually pops its first item (or drops the lease).
  pending_grant: Option<Arc<PendingGrant>>,
}

/// Counts a granted slot against its roster's queue head until the worker
/// dequeues its first item. Without this, granting multiple free slots in
/// one `assign()` loop sees the same unpopped head item for every waiter of
/// a project and hands the entire slot budget to one project at a time.
pub(crate) struct PendingGrant {
  roster: Arc<WorkerRoster>,
  active: std::sync::atomic::AtomicBool,
}

impl PendingGrant {
  fn resolve_under_lock(&self, state: &mut RosterState) {
    if self.active.swap(false, std::sync::atomic::Ordering::SeqCst) {
      state.pending_grants = state.pending_grants.saturating_sub(1);
    }
  }
}

impl Drop for PendingGrant {
  fn drop(&mut self) {
    if self.active.swap(false, std::sync::atomic::Ordering::SeqCst) {
      let mut state = self.roster.lock();
      state.pending_grants = state.pending_grants.saturating_sub(1);
    }
  }
}

impl RunSlots {
  pub(crate) fn new(count: u32) -> Arc<Self> {
    Arc::new(Self {
      state: std::sync::Mutex::new(SlotState {
        free: (0..count).collect(),
        waiters: Vec::new(),
        starting: Vec::new(),
        asked: 0,
      }),
    })
  }

  /// A project about to start: until [`Self::started`], a slot it would
  /// outrank every waiter for is kept for it.
  pub(crate) fn starting(&self, roster: &Arc<WorkerRoster>) {
    self.lock().starting.push(Arc::clone(roster));
  }

  /// The project has queued its tests, or is not going to.
  pub(crate) fn started(self: &Arc<Self>, roster: &Arc<WorkerRoster>) {
    self.lock().starting.retain(|held| !Arc::ptr_eq(held, roster));
    self.assign();
  }

  /// Wait for a slot for one of `roster`'s workers.
  pub(crate) async fn take(self: &Arc<Self>, roster: &Arc<WorkerRoster>) -> SlotLease {
    loop {
      let (grant, granted) = tokio::sync::oneshot::channel();
      {
        let mut state = self.lock();
        state.asked += 1;
        let asked = state.asked;
        state.waiters.push(Waiter {
          roster: Arc::clone(roster),
          asked,
          grant,
        });
      }
      self.assign();
      // `assign` takes a waiter off the queue only to grant it a slot, so
      // a dropped grant would mean that changed; asking again keeps the
      // worker waiting rather than lost.
      if let Ok(lease) = granted.await {
        return lease;
      }
    }
  }

  /// Hand free slots to the waiters with the longest critical path, ties
  /// to whoever asked first.
  fn assign(self: &Arc<Self>) {
    loop {
      let mut state = self.lock();
      if state.free.is_empty() {
        return;
      }
      let Some((position, priority)) = state
        .waiters
        .iter()
        .enumerate()
        .map(|(position, waiter)| (position, waiter.roster.priority(), waiter.asked))
        .max_by(|a, b| a.1.cmp(&b.1).then(b.2.cmp(&a.2)))
        .map(|(position, priority, _)| (position, priority))
      else {
        return;
      };
      if state.starting.iter().any(|roster| roster.priority() > priority) {
        return;
      }
      let Some(index) = state.free.pop_first() else {
        return;
      };
      let waiter = state.waiters.remove(position);
      let pending_grant = waiter.roster.grant_pending();
      drop(state);
      let lease = SlotLease {
        slots: Some(Arc::clone(self)),
        index,
        pending_grant: Some(pending_grant),
      };
      // A waiter that stopped waiting leaves the slot free for the next.
      if let Err(mut unclaimed) = waiter.grant.send(lease) {
        unclaimed.slots = None;
        unclaimed.pending_grant = None;
        self.lock().free.insert(unclaimed.index);
      }
    }
  }

  fn release(self: &Arc<Self>, index: u32) {
    self.lock().free.insert(index);
    self.assign();
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, SlotState> {
    self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
  }
}

impl SlotLease {
  pub(crate) fn index(&self) -> u32 {
    self.index
  }

  pub(crate) fn pending_grant(&self) -> Option<Arc<PendingGrant>> {
    self.pending_grant.as_ref().map(Arc::clone)
  }
}

impl Drop for SlotLease {
  fn drop(&mut self) {
    self.pending_grant = None;
    if let Some(slots) = self.slots.take() {
      slots.release(self.index);
    }
  }
}

/// A project's live workers and the expected length of the work queued
/// for them. A worker leaves when the project's queue is empty, giving
/// its slot to another project's waiting worker, and work queued
/// afterwards (a retry) enlists new ones. Leaving, enlisting and queueing
/// all happen under one lock, so no item is queued in the moment between
/// a worker finding the queue empty and leaving it, and the estimates
/// stay in step with the queue.
pub struct WorkerRoster {
  state: std::sync::Mutex<RosterState>,
  /// Slots in the run: a project allowed fewer runs its queue partly in
  /// series.
  run_slots: usize,
  /// Milliseconds of work that cannot start before this project ends:
  /// the longest chain of projects that depend on it.
  after: u64,
  emptied: tokio::sync::Notify,
}

struct RosterState {
  live: usize,
  cap: usize,
  /// Slots granted to waiters of this roster that have not yet popped their
  /// first item from `queued`.
  pending_grants: usize,
  /// Expected milliseconds of each queued item, in queue order.
  queued: std::collections::VecDeque<u64>,
}

/// One worker's place on the roster, given up when dropped unless the
/// worker already left through [`RosterPlace::next_or_leave`].
pub struct RosterPlace {
  roster: Arc<WorkerRoster>,
  present: std::sync::atomic::AtomicBool,
  pending_grant: std::sync::OnceLock<Arc<PendingGrant>>,
}

impl WorkerRoster {
  /// A roster for a project of `cap` workers in a run of `run_slots`.
  #[must_use]
  pub fn new(cap: usize, run_slots: usize) -> Arc<Self> {
    Self::planned(cap, run_slots, 0, Vec::new())
  }

  /// A roster for a project not yet started, ranked meanwhile by the
  /// work it is expected to queue (`planned`, in milliseconds per item)
  /// and by `after`, the work waiting on it.
  pub(crate) fn planned(cap: usize, run_slots: usize, after: u64, mut planned: Vec<u64>) -> Arc<Self> {
    planned.sort_unstable_by(|a, b| b.cmp(a));
    Arc::new(Self {
      state: std::sync::Mutex::new(RosterState {
        live: 0,
        cap: cap.max(1),
        pending_grants: 0,
        queued: planned.into(),
      }),
      run_slots: run_slots.max(1),
      after,
      emptied: tokio::sync::Notify::new(),
    })
  }

  fn grant_pending(self: &Arc<Self>) -> Arc<PendingGrant> {
    self.lock().pending_grants += 1;
    Arc::new(PendingGrant {
      roster: Arc::clone(self),
      active: std::sync::atomic::AtomicBool::new(true),
    })
  }

  /// The project's queue is about to be filled for real, for `cap`
  /// workers: what was planned no longer stands in for it.
  pub(crate) fn begin(&self, cap: usize) {
    let mut state = self.lock();
    state.cap = cap.max(1);
    state.queued.clear();
  }

  /// Queue an item expected to take `estimate` milliseconds; `send` puts
  /// it on the dispatcher's queue.
  pub(crate) fn enqueue(&self, estimate: u64, send: impl FnOnce()) {
    let mut state = self.lock();
    state.queued.push_back(estimate);
    send();
  }

  /// How long the run has to go at the least once this project's next
  /// item starts: the item, the rest of the queue where the project is
  /// held to fewer workers than the run has (that part runs in series),
  /// and the projects waiting on it.
  pub(crate) fn priority(&self) -> u64 {
    let state = self.lock();
    let offset = state.pending_grants;
    let Some(&head) = state.queued.get(offset) else {
      return if offset == 0 { self.after } else { 0 };
    };
    let mut path = head.saturating_add(self.after);
    if state.cap < self.run_slots {
      let rest: u64 = state.queued.iter().skip(offset + 1).sum();
      path = path.saturating_add(rest / state.cap as u64);
    }
    path
  }

  /// A place for one worker, whatever the queue holds: a worker driven
  /// by hand rather than by the runner.
  #[must_use]
  pub fn join(self: &Arc<Self>) -> RosterPlace {
    self.lock().live += 1;
    RosterPlace {
      roster: Arc::clone(self),
      present: std::sync::atomic::AtomicBool::new(true),
      pending_grant: std::sync::OnceLock::new(),
    }
  }

  /// Places for as many new workers as the queued work can use, within
  /// the project's cap. Every live worker is busy (an idle one leaves),
  /// so each queued item past them wants a worker of its own.
  pub(crate) fn enlist(self: &Arc<Self>, rx: &async_channel::Receiver<WorkItem>) -> Vec<RosterPlace> {
    let mut state = self.lock();
    let wanted = state.cap.saturating_sub(state.live).min(rx.len());
    state.live += wanted;
    (0..wanted)
      .map(|_| RosterPlace {
        roster: Arc::clone(self),
        present: std::sync::atomic::AtomicBool::new(true),
        pending_grant: std::sync::OnceLock::new(),
      })
      .collect()
  }

  /// Whether no worker is left.
  pub(crate) fn is_empty(&self) -> bool {
    self.lock().live == 0
  }

  /// Resolve once no worker is left.
  pub(crate) async fn emptied(&self) {
    loop {
      let emptied = self.emptied.notified();
      if self.is_empty() {
        return;
      }
      emptied.await;
    }
  }

  fn leave(state: &mut RosterState, emptied: &tokio::sync::Notify) {
    state.live = state.live.saturating_sub(1);
    if state.live == 0 {
      emptied.notify_waiters();
    }
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, RosterState> {
    self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
  }
}

impl RosterPlace {
  pub(crate) fn attach_pending_grant(&self, grant: Option<Arc<PendingGrant>>) {
    if let Some(grant) = grant {
      let _ = self.pending_grant.set(grant);
    }
  }

  /// The next queued item, or `None` once the queue is empty or closed,
  /// in which case the worker has left the roster.
  pub fn next_or_leave(&self, rx: &async_channel::Receiver<WorkItem>) -> Option<WorkItem> {
    let mut state = self.roster.lock();
    if let Some(grant) = self.pending_grant.get() {
      grant.resolve_under_lock(&mut state);
    }
    if let Ok(item) = rx.try_recv() {
      state.queued.pop_front();
      return Some(item);
    }
    if self.present.swap(false, std::sync::atomic::Ordering::SeqCst) {
      WorkerRoster::leave(&mut state, &self.roster.emptied);
    }
    None
  }
}

impl Drop for RosterPlace {
  fn drop(&mut self) {
    if self.present.swap(false, std::sync::atomic::Ordering::SeqCst) {
      WorkerRoster::leave(&mut self.roster.lock(), &self.roster.emptied);
    }
  }
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use super::*;
  use crate::model::{ExpectedStatus, TestAnnotation, TestId};

  fn case(name: &str) -> TestCase {
    TestCase {
      id: TestId {
        file: "a.spec.ts".into(),
        name: name.into(),
        ..TestId::default()
      },
      test_fn: Arc::new(|_| Box::pin(async { Ok(()) })),
      fixture_requests: vec!["page".into()],
      annotations: vec![
        TestAnnotation::Slow {
          reason: None,
          condition: None,
        },
        TestAnnotation::Lock("db".into()),
      ],
      timeout: Some(Duration::from_millis(400)),
      retries: Some(3),
      expected_status: ExpectedStatus::Fail,
      use_options: Some(serde_json::json!({ "locale": "de-DE" })),
      metadata: Some(serde_json::json!({ "keyword": "Scenario" })),
    }
  }

  fn assignment(name: &str, mode: SuiteMode) -> TestAssignment {
    TestAssignment {
      test: case(name),
      attempt: 1,
      suite_key: "a.spec.ts::group".into(),
      hooks: Arc::new(Hooks::default()),
      suite_mode: mode,
    }
  }

  #[test]
  fn a_retry_keeps_every_setting_the_test_was_scheduled_with() {
    let dispatcher = Dispatcher::new();
    dispatcher.retry_shared(Retry {
      test: case("flaky"),
      attempt: 2,
      suite_key: "a.spec.ts::group".into(),
      hooks: Arc::new(Hooks::default()),
      suite_mode: SuiteMode::Parallel,
      serial_tail: Vec::new(),
    });
    let Ok(WorkItem::Single(retried)) = dispatcher.receiver().try_recv() else {
      panic!("a parallel retry goes back alone");
    };
    assert_eq!(retried.attempt, 2);
    assert_eq!(retried.test.timeout, Some(Duration::from_millis(400)));
    assert_eq!(retried.test.retries, Some(3));
    assert_eq!(retried.test.expected_status, ExpectedStatus::Fail);
    assert_eq!(retried.test.use_options, Some(serde_json::json!({ "locale": "de-DE" })));
    assert_eq!(retried.test.annotations.len(), 2);
    assert_eq!(retried.test.fixture_requests, ["page"]);
  }

  #[test]
  fn a_serial_retry_takes_the_rest_of_its_group_with_it() {
    let dispatcher = Dispatcher::new();
    dispatcher.retry_shared(Retry {
      test: case("two"),
      attempt: 2,
      suite_key: "a.spec.ts::group".into(),
      hooks: Arc::new(Hooks::default()),
      suite_mode: SuiteMode::Serial,
      serial_tail: vec![assignment("three", SuiteMode::Serial)],
    });
    let Ok(WorkItem::Serial(batch)) = dispatcher.receiver().try_recv() else {
      panic!("a serial retry goes back as a batch");
    };
    let order: Vec<(&str, u32)> = batch
      .assignments
      .iter()
      .map(|a| (a.test.id.name.as_str(), a.attempt))
      .collect();
    assert_eq!(order, [("two", 2), ("three", 1)]);
    assert_eq!(batch.assignments[0].suite_mode, SuiteMode::Serial);
  }

  #[tokio::test]
  async fn a_worker_leaves_an_empty_queue_and_later_work_enlists_another() {
    let dispatcher = Dispatcher::new();
    let rx = dispatcher.receiver();
    let roster = WorkerRoster::new(2, 2);
    dispatcher.enqueue_single(assignment("first", SuiteMode::Parallel));
    let places = roster.enlist(&rx);
    assert_eq!(places.len(), 1, "one queued test wants one worker");
    let place = places.into_iter().next().expect("a place");
    assert!(place.next_or_leave(&rx).is_some());
    assert!(
      place.next_or_leave(&rx).is_none(),
      "an empty queue sends the worker away"
    );
    tokio::time::timeout(Duration::from_millis(50), roster.emptied())
      .await
      .expect("the roster empties when its last worker leaves");

    dispatcher.enqueue_single(assignment("retry", SuiteMode::Parallel));
    dispatcher.enqueue_single(assignment("other", SuiteMode::Parallel));
    dispatcher.enqueue_single(assignment("third", SuiteMode::Parallel));
    assert_eq!(roster.enlist(&rx).len(), 2, "no more workers than the project's cap");
  }

  #[tokio::test]
  async fn a_slot_given_back_goes_to_the_next_waiter_with_its_index() {
    let slots = RunSlots::new(2);
    let roster = WorkerRoster::new(2, 2);
    let first = slots.take(&roster).await;
    let second = slots.take(&roster).await;
    assert_ne!(first.index(), second.index());
    let waiting = tokio::spawn({
      let slots = Arc::clone(&slots);
      async move { slots.take(&roster).await.index() }
    });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished(), "every slot is held");
    let freed = first.index();
    drop(first);
    assert_eq!(waiting.await.expect("waiter"), freed);
  }

  /// Three slots. A project held to one worker with 60s queued, and one
  /// that 30s of work depends on, outrank a project whose longest test is
  /// 40s: their work cannot be spread over the other slots, so it decides
  /// when the run ends.
  #[test]
  fn the_longest_critical_path_ranks_first() {
    let spread = WorkerRoster::planned(3, 3, 0, vec![40_000, 30_000, 30_000]);
    let serial = WorkerRoster::planned(1, 3, 0, vec![20_000, 20_000, 20_000]);
    let blocking = WorkerRoster::planned(3, 3, 30_000, vec![1_000]);
    assert_eq!(spread.priority(), 40_000);
    assert_eq!(serial.priority(), 60_000);
    assert_eq!(blocking.priority(), 31_000);
    assert!(serial.priority() > blocking.priority() && blocking.priority() < spread.priority());
  }

  #[tokio::test]
  async fn a_freed_slot_goes_to_the_waiter_with_the_longest_path_and_waits_for_a_starting_one() {
    let slots = RunSlots::new(1);
    let short = WorkerRoster::planned(1, 1, 0, vec![1_000]);
    let long = WorkerRoster::planned(1, 1, 0, vec![9_000]);
    let held = slots.take(&short).await;
    let first_asker = tokio::spawn({
      let (slots, short) = (Arc::clone(&slots), Arc::clone(&short));
      async move { slots.take(&short).await }
    });
    tokio::task::yield_now().await;
    let later_but_longer = tokio::spawn({
      let (slots, long) = (Arc::clone(&slots), Arc::clone(&long));
      async move { slots.take(&long).await }
    });
    tokio::task::yield_now().await;
    drop(held);
    let lease = later_but_longer.await.expect("the longer path gets the slot");
    assert!(!first_asker.is_finished());

    let launching = WorkerRoster::planned(1, 1, 0, vec![20_000]);
    slots.starting(&launching);
    drop(lease);
    tokio::task::yield_now().await;
    assert!(!first_asker.is_finished(), "kept for the project still starting");
    slots.started(&launching);
    first_asker.await.expect("given out once that project has started");
  }

  #[tokio::test]
  async fn multi_project_slots_distribute_across_long_tails_instead_of_starving() {
    let slots = RunSlots::new(4);
    let project_a = WorkerRoster::planned(4, 4, 0, vec![35_000, 35_000, 1_000, 1_000]);
    let project_b = WorkerRoster::planned(4, 4, 0, vec![34_000, 34_000, 1_000, 1_000]);
    // Hold all 4 slots while both projects queue their waiters, matching
    // concurrent multi-project startup.
    let blocker = WorkerRoster::new(4, 4);
    let held = vec![
      slots.take(&blocker).await,
      slots.take(&blocker).await,
      slots.take(&blocker).await,
      slots.take(&blocker).await,
    ];
    let mut a_tasks = Vec::new();
    let mut b_tasks = Vec::new();
    for _ in 0..4 {
      let (s, r) = (Arc::clone(&slots), Arc::clone(&project_a));
      a_tasks.push(tokio::spawn(async move { s.take(&r).await }));
      let (s, r) = (Arc::clone(&slots), Arc::clone(&project_b));
      b_tasks.push(tokio::spawn(async move { s.take(&r).await }));
    }
    tokio::task::yield_now().await;
    drop(held);
    tokio::task::yield_now().await;

    let a_granted = a_tasks.iter().filter(|t| t.is_finished()).count();
    let b_granted = b_tasks.iter().filter(|t| t.is_finished()).count();
    assert_eq!(
      (a_granted, b_granted),
      (2, 2),
      "each project should get 2 slots for its two 35s/34s tails"
    );
    for t in a_tasks.into_iter().chain(b_tasks) {
      t.abort();
    }
  }
}
