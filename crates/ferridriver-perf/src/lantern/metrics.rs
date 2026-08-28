//! Metric-specific graphs.
//!
//! From devtools-frontend `lantern/metrics/FirstContentfulPaint.ts`.
//!
//! A saving is never measured against the whole page load. "How much
//! sooner would the page have painted" is a question about the subgraph
//! first paint depends on, and simulating the whole graph instead
//! answers a different, larger question: on the same fixture that gives
//! 300ms where the paint-only graph gives 75.

use rustc_hash::FxHashSet;

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
  let request_of: Vec<Option<usize>> = {
    let mut map = vec![None; graph.nodes.len()];
    for (index, node) in node_of.iter().enumerate() {
      if let Some(node) = node {
        map[*node] = Some(index);
      }
    }
    map
  };

  // A CPU task that ran before the paint is part of getting there.
  let mut keep: FxHashSet<NodeId> = FxHashSet::default();
  for node in graph.reachable() {
    let entry = &graph.nodes[node];
    match entry.kind {
      NodeKind::Cpu { .. } => {
        if entry.start_time_us <= cutoff {
          keep.insert(node);
        }
      },
      NodeKind::Network(_) => {
        // The document always counts: without it there is no paint.
        if entry.is_main_document {
          keep.insert(node);
          continue;
        }
        // Anything still in flight at the paint, or started after it,
        // cannot have blocked it.
        if entry.end_time_us > cutoff || entry.start_time_us > cutoff {
          continue;
        }
        let Some(index) = request_of[node] else { continue };
        let request = &requests[index];
        if is_render_blocking(request) && request.initiator_type != "script" {
          keep.insert(node);
        }
      },
    }
  }

  let removed: FxHashSet<NodeId> = (0..graph.nodes.len()).filter(|n| !keep.contains(n)).collect();
  graph.without(&removed)
}

/// Whether the browser treated the request as holding up rendering.
fn is_render_blocking(request: &NetworkRequest) -> bool {
  match request.render_blocking.as_str() {
    "blocking" => true,
    // Only a high-priority in-body resource actually stalls the parser.
    "in_body_parser_blocking" => {
      matches!(request.priority.as_str(), "VeryHigh" | "High")
    },
    _ => false,
  }
}
