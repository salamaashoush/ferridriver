//! Main-thread work, and what it depends on.
//!
//! From devtools-frontend `lantern/graph/PageDependencyGraph.ts`
//! (`getCPUNodes`, `linkCPUNodes`) and `graph/CPUNode.ts`.
//!
//! A network-only graph under-estimates any page whose critical path is
//! script rather than fetching, because nothing in it takes main-thread
//! time. These nodes put that time back: each is one schedulable task,
//! wired to the requests it waited on and the requests it started.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::event::{MILLIS_TO_MICROS, Micro, TraceEvent};
use crate::handlers::network::NetworkRequest;
use crate::lantern::graph::{Graph, Node, NodeId, NodeKind};
use crate::units::f64_to_micros;

/// A task shorter than this only survives if it is the first of its
/// kind; anything else is pruned so the graph stays small enough to
/// simulate repeatedly.
const SIGNIFICANT_DURATION_MS: f64 = 10.0;

/// What Chrome has called a top-level task across versions.
/// Only these request types may be made to wait on a CPU task.
///
/// Upstream restricts it to these three because linking images and
/// stylesheets regressed its LCP simulation. It also keeps the document
/// out, which matters structurally: the document is the graph root, and
/// a root with a dependency can never start, so the whole simulation
/// returns zero.
const LINKABLE_RESOURCE_TYPES: [&str; 3] = ["XHR", "Fetch", "Script"];

/// The events inside a task that say anything about what it waited on
/// or what it started. Everything else in a task is ignored, and this
/// list is what lets that be decided without parsing the payload.
const LINKING_EVENTS: [&str; 10] = [
  "TimerInstall",
  "TimerFire",
  "InvalidateLayout",
  "ScheduleStyleRecalculation",
  "EvaluateScript",
  "XHRReadyStateChange",
  "FunctionCall",
  "v8.compile",
  "ParseAuthorStyleSheet",
  "ResourceSendRequest",
];

const SCHEDULABLE_TASKS: [&str; 4] = [
  "RunTask",
  "ThreadControllerImpl::RunTask",
  "ThreadControllerImpl::DoWork",
  "TaskQueueManager::ProcessTaskFromWorkQueue",
];

/// One schedulable task and everything that ran inside it.
struct Task<'a> {
  event: &'a TraceEvent<'a>,
  children: Vec<&'a TraceEvent<'a>>,
  /// Chrome sometimes emits overlapping tasks (crbug 329678173); the end
  /// is pulled back to just before the next task starts.
  end: Micro,
}

/// Add main-thread tasks to a graph that already holds the requests.
///
/// `node_of` maps request index to graph node, as
/// [`crate::lantern::page_graph::build`] returns it.
pub fn add_cpu_nodes(
  graph: &mut Graph,
  events: &[TraceEvent<'_>],
  requests: &[NetworkRequest],
  node_of: &[Option<NodeId>],
) {
  // Only the renderer's main thread. Upstream is handed
  // `mainThreadEvents` already filtered; walking the whole trace instead
  // lets a task on one thread truncate a task on another, because the
  // containment scan stops at the next schedulable task it sees in
  // document order. That produced a thousand tasks totalling 28ms on a
  // trace whose click handler alone ran for 60.
  let Some((pid, tid)) = crate::handlers::main_thread(events) else {
    return;
  };
  // Borrowed, not cloned. On a layout-heavy trace nearly every event is
  // on this thread, and copying them was two thirds of the cost of the
  // whole analysis: each clone is three heap strings and the boxed
  // `args` slice, thirty thousand times over.
  let main_thread_events: Vec<&TraceEvent<'_>> = events.iter().filter(|e| e.pid == pid && e.tid == tid).collect();
  let tasks = collect_tasks(&main_thread_events);
  if tasks.is_empty() {
    return;
  }

  // Requests indexed the two ways the linking needs them.
  let by_request_id: FxHashMap<&str, usize> = requests
    .iter()
    .enumerate()
    .map(|(index, request)| (request.request_id.as_str(), index))
    .collect();
  let mut by_url: FxHashMap<&str, Vec<usize>> = FxHashMap::default();
  for (index, request) in requests.iter().enumerate() {
    by_url.entry(request.url.as_str()).or_default().push(index);
  }

  let root = graph.root;
  let mut cpu_nodes = Vec::with_capacity(tasks.len());
  for task in &tasks {
    let node = graph.add_node(Node {
      kind: NodeKind::Cpu {
        duration_us: task.end - task.event.ts,
        did_perform_layout: task.children.iter().any(|e| e.name == "Layout"),
        did_paint: task.children.iter().any(|e| e.name == "Paint"),
        did_parse_html: task.children.iter().any(|e| e.name == "ParseHTML"),
        evaluate_script_urls: evaluate_script_urls(task),
      },
      start_time_us: task.event.ts,
      end_time_us: task.end,
      is_main_document: false,
    });
    cpu_nodes.push(node);
  }

  let mut links = Links {
    by_request_id,
    by_url,
    requests,
    node_of,
    root,
    root_start: graph.nodes[root].start_time_us,
  };
  // `TimerFire` depends on whichever task installed the timer.
  let mut timers: FxHashMap<String, usize> = FxHashMap::default();
  for (position, task) in tasks.iter().enumerate() {
    link_task(graph, &mut links, &tasks, &cpu_nodes, position, task, &mut timers);
  }

  prune_short_tasks(graph, &tasks, &cpu_nodes);
}

/// The scripts a task evaluated, deduplicated, from `CPUNode`'s
/// `getEvaluateScriptURLs`.
fn evaluate_script_urls(task: &Task<'_>) -> Vec<String> {
  let mut urls: Vec<String> = Vec::new();
  for child in task.children.iter().filter(|e| e.name == "EvaluateScript") {
    let Some(url) = child
      .data()
      .and_then(|d| d.get("url").and_then(serde_json::Value::as_str))
    else {
      continue;
    };
    if !urls.iter().any(|seen| seen == url) {
      urls.push(url.to_string());
    }
  }
  urls
}

/// Everything the linking needs to resolve a reference to a request.
struct Links<'a> {
  by_request_id: FxHashMap<&'a str, usize>,
  by_url: FxHashMap<&'a str, Vec<usize>>,
  requests: &'a [NetworkRequest],
  node_of: &'a [Option<NodeId>],
  root: NodeId,
  root_start: Micro,
}

/// Wire one task to the requests it waited on and the ones it started.
fn link_task(
  graph: &mut Graph,
  links: &mut Links<'_>,
  tasks: &[Task<'_>],
  cpu_nodes: &[NodeId],
  position: usize,
  task: &Task<'_>,
  timers: &mut FxHashMap<String, usize>,
) {
  let node = cpu_nodes[position];
  for child in &task.children {
    // The name decides everything, so it is checked BEFORE the payload
    // is touched. Reading `args` first parses a `Value` for every event
    // inside the task, and on a layout-heavy trace half of them are
    // `Layout` and `UpdateLayoutTree`, which nothing below reads.
    if !LINKING_EVENTS.contains(&child.name.as_ref()) {
      continue;
    }
    let Some(data) = child.data() else { continue };
    let args_url = data.get("url").and_then(serde_json::Value::as_str).unwrap_or_default();
    let frame = data.get("frame").and_then(serde_json::Value::as_str);
    let stack_urls: Vec<&str> = data
      .get("stackTrace")
      .and_then(serde_json::Value::as_array)
      .map(|frames| {
        frames
          .iter()
          .filter_map(|f| f.get("url").and_then(serde_json::Value::as_str))
          .collect()
      })
      .unwrap_or_default();

    match child.name.as_ref() {
      "TimerInstall" => {
        if let Some(id) = data.get("timerId").and_then(serde_json::Value::as_str) {
          timers.insert(id.to_string(), position);
        }
        for url in &stack_urls {
          depend_on_url(graph, node, url, links, task.event.ts);
        }
      },
      "TimerFire" => {
        // The installing task has to have finished first, or this is
        // not the timer that caused it.
        if let Some(id) = data.get("timerId").and_then(serde_json::Value::as_str)
          && let Some(&installer) = timers.get(id)
          && tasks[installer].end <= task.event.ts
        {
          graph.add_dependency(node, cpu_nodes[installer]);
        }
      },
      "InvalidateLayout" | "ScheduleStyleRecalculation" => {
        depend_on_document(graph, node, frame, links, task.event.ts);
        for url in &stack_urls {
          depend_on_url(graph, node, url, links, task.event.ts);
        }
      },
      "EvaluateScript" | "XHRReadyStateChange" => {
        // An XHR only counts once it has actually completed.
        if child.name == "XHRReadyStateChange" && data.get("readyState").and_then(serde_json::Value::as_i64) != Some(4)
        {
          continue;
        }
        if child.name == "EvaluateScript" {
          depend_on_document(graph, node, frame, links, task.event.ts);
        }
        depend_on_url(graph, node, args_url, links, task.event.ts);
        for url in &stack_urls {
          depend_on_url(graph, node, url, links, task.event.ts);
        }
      },
      "FunctionCall" | "v8.compile" => {
        depend_on_document(graph, node, frame, links, task.event.ts);
        depend_on_url(graph, node, args_url, links, task.event.ts);
      },
      "ParseAuthorStyleSheet" => {
        depend_on_document(graph, node, frame, links, task.event.ts);
        if let Some(url) = data.get("styleSheetUrl").and_then(serde_json::Value::as_str) {
          depend_on_url(graph, node, url, links, task.event.ts);
        }
      },
      // The task STARTED this request, so the request waits on it.
      "ResourceSendRequest" => {
        depend_on_document(graph, node, frame, links, task.event.ts);
        if let Some(id) = data.get("requestId").and_then(serde_json::Value::as_str)
            && let Some(&index) = links.by_request_id.get(id)
            && let Some(request_node) = links.node_of[index]
            // A request that started before the task cannot have been
            // started BY it.
            && links.requests[index].start_time > task.event.ts
            && LINKABLE_RESOURCE_TYPES.contains(&links.requests[index].resource_type.as_str())
            && request_node != links.root
        {
          graph.add_dependency(request_node, node);
        }
        for url in &stack_urls {
          depend_on_url(graph, node, url, links, task.event.ts);
        }
      },
      _ => {},
    }
  }

  // A task that depends on nothing hangs off the document, unless it
  // started before the document did.
  if graph.dependencies[node].is_empty() && task.event.ts >= links.root_start {
    graph.add_dependency(node, links.root);
  }
}

/// Tasks, each carrying the events that ran inside it.
fn collect_tasks<'a>(events: &[&'a TraceEvent<'a>]) -> Vec<Task<'a>> {
  let mut tasks = Vec::new();
  let mut index = 0;

  while index < events.len() {
    let event = events[index];
    index += 1;
    if !SCHEDULABLE_TASKS.contains(&event.name.as_ref()) || event.dur.is_none() {
      continue;
    }

    let declared_end = event.end();
    let mut end = declared_end;
    let mut children = Vec::new();
    while index < events.len() && events[index].ts < declared_end {
      let child = events[index];
      // Chrome can emit overlapping tasks; the earlier one is treated as
      // ending just before the later starts.
      if SCHEDULABLE_TASKS.contains(&child.name.as_ref()) && child.dur.is_some() {
        end = child.ts - 1;
        break;
      }
      children.push(child);
      index += 1;
    }
    tasks.push(Task { event, children, end });
  }
  tasks
}

/// Depend on the document request, when this task is for the main frame
/// and did not start before it.
fn depend_on_document(graph: &mut Graph, node: NodeId, frame: Option<&str>, links: &Links<'_>, task_start: Micro) {
  if frame.is_none() || task_start < links.root_start {
    return;
  }
  graph.add_dependency(node, links.root);
}

/// Depend on the most recent request for `url` that finished before this
/// task started.
///
/// The closest preceding one is the right answer: a URL fetched twice
/// means the task was waiting on the fetch nearest to it, not the first.
fn depend_on_url(graph: &mut Graph, node: NodeId, url: &str, links: &Links<'_>, task_start: Micro) {
  if url.is_empty() {
    return;
  }
  let Some(candidates) = links.by_url.get(url) else {
    return;
  };
  let best = candidates
    .iter()
    .filter(|index| links.requests[**index].end_time <= task_start)
    .min_by_key(|index| task_start - links.requests[**index].end_time);
  if let Some(&index) = best
    && let Some(request_node) = links.node_of[index]
  {
    graph.add_dependency(node, request_node);
  }
}

/// Drop tasks too short to matter, rewiring around them.
///
/// The first `Layout`, `Paint` and `ParseHTML` are kept whatever their
/// length:
/// they mark the page reaching a state, and removing them loses that
/// even though the task itself was quick (Lighthouse #9627).
fn prune_short_tasks(graph: &mut Graph, tasks: &[Task<'_>], cpu_nodes: &[NodeId]) {
  let minimum = f64_to_micros(SIGNIFICANT_DURATION_MS * MILLIS_TO_MICROS);
  let (mut seen_layout, mut seen_paint, mut seen_parse) = (false, false, false);

  for (position, task) in tasks.iter().enumerate() {
    let node = cpu_nodes[position];
    let has = |name: &str| task.children.iter().any(|e| e.name == name);

    let mut is_first = false;
    let first_of = |seen: &mut bool, present: bool| {
      if !*seen && present {
        *seen = true;
        return true;
      }
      false
    };
    is_first |= first_of(&mut seen_layout, has("Layout"));
    is_first |= first_of(&mut seen_paint, has("Paint"));
    is_first |= first_of(&mut seen_parse, has("ParseHTML"));
    if is_first || task.end - task.event.ts >= minimum {
      continue;
    }

    // Rewiring replaces M + N edges with M * N, which is only a saving
    // when one side has at most one edge.
    if graph.dependencies[node].len() == 1 || graph.dependents[node].len() <= 1 {
      prune(graph, node);
    }
  }
}

/// Remove a node, connecting each of its dependencies to each of its
/// dependents so the ordering it enforced survives.
fn prune(graph: &mut Graph, node: NodeId) {
  let dependencies = std::mem::take(&mut graph.dependencies[node]);
  let dependents = std::mem::take(&mut graph.dependents[node]);

  for dependency in &dependencies {
    graph.dependents[*dependency].retain(|n| *n != node);
  }
  for dependent in &dependents {
    graph.dependencies[*dependent].retain(|n| *n != node);
  }
  for dependency in &dependencies {
    for dependent in &dependents {
      graph.add_dependency(*dependent, *dependency);
    }
  }
}

/// Whether the graph has any main-thread work in it, for callers that
/// report the difference.
#[must_use]
pub fn has_cpu_nodes(graph: &Graph) -> bool {
  graph.nodes.iter().any(|node| matches!(node.kind, NodeKind::Cpu { .. }))
}

/// Nodes unreachable from the root contribute nothing and would only
/// slow the simulation down.
#[must_use]
pub fn reachable_set(graph: &Graph) -> FxHashSet<NodeId> {
  graph.reachable().into_iter().collect()
}
