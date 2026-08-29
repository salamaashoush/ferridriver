//! Metric-specific graphs.
//!
//! From devtools-frontend `lantern/metrics/Metric.ts`
//! (`getFirstPaintBasedGraph`, `getRenderBlockingNodeData`) and
//! `lantern/metrics/FirstContentfulPaint.ts`.
//!
//! A saving is never measured against the whole page load. "How much
//! sooner would the page have painted" is a question about the subgraph
//! first paint depends on, and simulating the whole graph instead
//! answers a different, larger question: on the same fixture that gives
//! 300ms where the paint-only graph gives 75.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::event::Micro;
use crate::handlers::network::NetworkRequest;
use crate::lantern::graph::{Graph, NodeId, NodeKind};

/// The subgraph first contentful paint waited on.
///
/// Optimistic in upstream's sense: a resource that merely looks
/// render-blocking but was fetched by a script is left out, because it
/// does not technically block the paint.
#[must_use]
pub fn first_contentful_paint_graph(
  graph: &Graph,
  requests: &[NetworkRequest],
  node_of: &[Option<NodeId>],
  cutoff: Micro,
) -> Graph {
  first_paint_based_graph(
    graph,
    requests,
    node_of,
    &Shape {
      cutoff,
      treat_as_blocking: &blocks_first_paint,
      additional_cpu: None,
    },
  )
}

/// The same subgraph, pessimistically: a resource fetched by a script
/// counts too.
///
/// The optimistic and pessimistic graphs bracket the answer and the
/// estimate is the average of the two, so neither is meant to be right
/// on its own.
#[must_use]
pub fn first_contentful_paint_graph_pessimistic(
  graph: &Graph,
  requests: &[NetworkRequest],
  node_of: &[Option<NodeId>],
  cutoff: Micro,
) -> Graph {
  first_paint_based_graph(
    graph,
    requests,
    node_of,
    &Shape {
      cutoff,
      treat_as_blocking: &has_render_blocking_priority,
      additional_cpu: None,
    },
  )
}

/// The largest-paint subgraph, pessimistically: everything that
/// finished before the paint counts, and so does every task that laid
/// out.
#[must_use]
pub fn largest_contentful_paint_graph_pessimistic(
  graph: &Graph,
  requests: &[NetworkRequest],
  node_of: &[Option<NodeId>],
  cutoff: Micro,
) -> Graph {
  first_paint_based_graph(
    graph,
    requests,
    node_of,
    &Shape {
      cutoff,
      treat_as_blocking: &|_| true,
      additional_cpu: Some(&|kind| matches!(kind, NodeKind::Cpu { did_perform_layout, .. } if *did_perform_layout)),
    },
  )
}

/// What separates one paint subgraph from another.
struct Shape<'p> {
  /// Everything after this is not something the paint waited for.
  cutoff: Micro,
  /// Whether a request counts as holding the paint up.
  treat_as_blocking: &'p dyn Fn(&NetworkRequest) -> bool,
  /// Main-thread tasks to keep beyond the four any paint waits on.
  /// Only the pessimistic largest-paint graph uses it.
  additional_cpu: Option<&'p dyn Fn(&NodeKind) -> bool>,
}

/// The subgraph largest contentful paint waited on.
///
/// The same construction with a looser rule about what counts as
/// blocking: everything except an image the browser gave low priority,
/// because a low-priority image is one the browser decided was not
/// worth painting first.
#[must_use]
pub fn largest_contentful_paint_graph(
  graph: &Graph,
  requests: &[NetworkRequest],
  node_of: &[Option<NodeId>],
  cutoff: Micro,
) -> Graph {
  first_paint_based_graph(
    graph,
    requests,
    node_of,
    &Shape {
      cutoff,
      treat_as_blocking: &is_not_low_priority_image,
      additional_cpu: None,
    },
  )
}

/// `Metric.getFirstPaintBasedGraph`, shared by both paint subgraphs.
fn first_paint_based_graph(
  graph: &Graph,
  requests: &[NetworkRequest],
  node_of: &[Option<NodeId>],
  shape: &Shape<'_>,
) -> Graph {
  let (cutoff, treat_as_blocking) = (shape.cutoff, shape.treat_as_blocking);
  let request_of: Vec<Option<usize>> = {
    let mut map = vec![None; graph.nodes.len()];
    for (index, node) in node_of.iter().enumerate() {
      if let Some(node) = node {
        map[*node] = Some(index);
      }
    }
    map
  };
  let url_of = |node: NodeId| request_of[node].map(|index| requests[index].url.as_str());

  let blocking = render_blocking_nodes(graph, requests, &request_of, shape);

  // Upstream's predicate, node for node. A main-thread task is in only
  // if it is one of the few the paint provably waited on; keeping every
  // task that merely started before the paint instead builds a
  // page-load graph wearing a paint graph's name, and every saving
  // measured against it comes out several times too large.
  let mut keep: FxHashSet<NodeId> = FxHashSet::default();
  for node in graph.reachable() {
    let entry = &graph.nodes[node];
    let kept = match entry.kind {
      NodeKind::Cpu { .. } => blocking.cpu_nodes.contains(&node),
      NodeKind::Network(index) => {
        // Anything still in flight at the paint, or started after it,
        // cannot have blocked it. The document is exempt because it is
        // the graph's root, not because it painted.
        let ended_after_paint = entry.end_time_us > cutoff || entry.start_time_us > cutoff;
        let ruled_out = (ended_after_paint && !entry.is_main_document)
          || url_of(node).is_some_and(|url| blocking.not_blocking_scripts.contains(url));
        !ruled_out && (entry.is_main_document || treat_as_blocking(&requests[index]))
      },
    };
    if kept {
      keep.insert(node);
    }
  }

  // `cloneWithRelationships` keeps what its predicate accepts AND every
  // dependency above it, so a kept task still has the request it waited
  // on. Dropping those instead would let the task start at time zero
  // and the subgraph would simulate faster than it could ever run.
  let mut pending: Vec<NodeId> = keep.iter().copied().collect();
  while let Some(node) = pending.pop() {
    for dependency in &graph.dependencies[node] {
      if keep.insert(*dependency) {
        pending.push(*dependency);
      }
    }
  }

  let removed: FxHashSet<NodeId> = (0..graph.nodes.len()).filter(|n| !keep.contains(n)).collect();
  graph.without(&removed)
}

/// Which nodes the paint is treated as having waited on.
struct RenderBlockingNodes<'a> {
  /// Main-thread tasks to keep.
  cpu_nodes: FxHashSet<NodeId>,
  /// Scripts that look render-blocking but whose evaluation had not
  /// started by the paint, so the paint did not wait for them.
  not_blocking_scripts: FxHashSet<&'a str>,
}

/// `Metric.getRenderBlockingNodeData`.
fn render_blocking_nodes<'a>(
  graph: &Graph,
  requests: &'a [NetworkRequest],
  request_of: &[Option<usize>],
  shape: &Shape<'_>,
) -> RenderBlockingNodes<'a> {
  let (cutoff, treat_as_blocking) = (shape.cutoff, shape.treat_as_blocking);
  let reachable = graph.reachable();

  let started_before_paint: FxHashSet<NodeId> = reachable
    .iter()
    .copied()
    .filter(|node| matches!(graph.nodes[*node].kind, NodeKind::Cpu { .. }))
    .filter(|node| graph.nodes[*node].start_time_us <= cutoff)
    .collect();

  // The earliest task to evaluate each script, taken over every task
  // rather than only the early ones: a script first evaluated after the
  // paint is evidence the paint did not wait for it, which is the
  // distinction the two sets below turn on.
  let mut evaluated_by: FxHashMap<&str, NodeId> = FxHashMap::default();
  for node in &reachable {
    let NodeKind::Cpu {
      ref evaluate_script_urls,
      ..
    } = graph.nodes[*node].kind
    else {
      continue;
    };
    for url in evaluate_script_urls {
      let earlier = evaluated_by
        .get(url.as_str())
        .is_some_and(|held| graph.nodes[*held].start_time_us <= graph.nodes[*node].start_time_us);
      if !earlier {
        evaluated_by.insert(url.as_str(), *node);
      }
    }
  }

  let mut cpu_nodes: FxHashSet<NodeId> = FxHashSet::default();
  let mut not_blocking_scripts: FxHashSet<&str> = FxHashSet::default();
  for node in &reachable {
    let Some(index) = request_of[*node] else { continue };
    let request = &requests[index];
    if request.resource_type != "Script" || graph.nodes[*node].end_time_us > cutoff || !treat_as_blocking(request) {
      continue;
    }
    match evaluated_by.get(request.url.as_str()) {
      None => {},
      Some(task) if started_before_paint.contains(task) => {
        cpu_nodes.insert(*task);
      },
      Some(_) => {
        not_blocking_scripts.insert(request.url.as_str());
      },
    }
  }

  // The first task of each kind stands in for the work any paint has to
  // do regardless of what the page asked for.
  let mut ordered: Vec<NodeId> = started_before_paint.iter().copied().collect();
  ordered.sort_by_key(|node| graph.nodes[*node].start_time_us);
  let mut first_where = |predicate: fn(&NodeKind) -> bool| {
    if let Some(node) = ordered.iter().find(|node| predicate(&graph.nodes[**node].kind)) {
      cpu_nodes.insert(*node);
    }
  };
  first_where(|kind| matches!(kind, NodeKind::Cpu { did_perform_layout, .. } if *did_perform_layout));
  first_where(|kind| matches!(kind, NodeKind::Cpu { did_paint, .. } if *did_paint));
  first_where(|kind| matches!(kind, NodeKind::Cpu { did_parse_html, .. } if *did_parse_html));

  if let Some(extra) = shape.additional_cpu {
    for node in &ordered {
      if extra(&graph.nodes[*node].kind) {
        cpu_nodes.insert(*node);
      }
    }
  }

  RenderBlockingNodes {
    cpu_nodes,
    not_blocking_scripts,
  }
}

/// `NetworkNode.hasRenderBlockingPriority` and the optimistic FCP
/// graph's extra condition.
///
/// Deliberately reads the priority Chrome assigned rather than the
/// `renderBlocking` field: a resource can be marked render-blocking and
/// still be fetched at a priority that lets the paint go ahead without
/// it.
fn blocks_first_paint(request: &NetworkRequest) -> bool {
  has_render_blocking_priority(request) && request.initiator_type != "script"
}

/// `NetworkNode.hasRenderBlockingPriority` on its own, which is what the
/// pessimistic first-paint graph uses.
fn has_render_blocking_priority(request: &NetworkRequest) -> bool {
  let high_priority_of_type =
    request.priority == "High" && matches!(request.resource_type.as_str(), "Script" | "Document");
  request.priority == "VeryHigh" || high_priority_of_type
}

/// `LargestContentfulPaint.isNotLowPriorityImageNode`.
///
/// The largest paint waits on nearly everything, so the rule is stated
/// as an exclusion: an image the browser deprioritised is one it judged
/// was not what the user was waiting to see.
fn is_not_low_priority_image(request: &NetworkRequest) -> bool {
  let low_priority = matches!(request.priority.as_str(), "Low" | "VeryLow");
  request.resource_type != "Image" || !low_priority
}
