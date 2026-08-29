//! Handlers turn the flat trace-event array into a model.
//!
//! Each one owns a single pass over the events and answers a single
//! question, mirroring devtools-frontend's `handlers/` directory.

pub mod interactions;
pub mod meta;
pub mod network;
pub mod page_load;
pub mod page_signals;
pub mod paint;
pub mod renderer;
pub mod scripts;

use rustc_hash::FxHashMap;

use crate::event::TraceEvent;

/// What Chrome has called a top-level task across versions.
const SCHEDULABLE_TASKS: [&str; 4] = [
  "RunTask",
  "ThreadControllerImpl::RunTask",
  "ThreadControllerImpl::DoWork",
  "TaskQueueManager::ProcessTaskFromWorkQueue",
];

/// `(pid, tid)` of `CrRendererMain`, the thread a page's own script and
/// layout run on.
///
/// The renderer with the most top-level tasks is taken when several are
/// named, which happens with out-of-process iframes; the page's own
/// renderer is the busy one.
pub fn main_thread(events: &[TraceEvent<'_>]) -> Option<(i64, i64)> {
  let mut candidates: Vec<(i64, i64)> = events
    .iter()
    .filter(|e| e.name == "thread_name")
    .filter(|e| e.args_get("name").and_then(serde_json::Value::as_str) == Some("CrRendererMain"))
    .map(|e| (e.pid, e.tid))
    .collect();
  if candidates.len() > 1 {
    let mut counts: FxHashMap<(i64, i64), usize> = FxHashMap::default();
    for event in events.iter().filter(|e| SCHEDULABLE_TASKS.contains(&e.name.as_ref())) {
      *counts.entry((event.pid, event.tid)).or_default() += 1;
    }
    candidates.sort_by_key(|key| std::cmp::Reverse(counts.get(key).copied().unwrap_or(0)));
  }
  candidates.first().copied()
}
