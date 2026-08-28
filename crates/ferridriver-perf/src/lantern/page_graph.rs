//! Building the dependency graph from the observed trace.
//!
//! From devtools-frontend `lantern/graph/PageDependencyGraph.ts`.
//!
//! This builds the network half. Main-thread tasks are added on top by
//! [`crate::lantern::cpu_graph`], which needs the requests to already be
//! in place so it can attach each task to the ones it waited on.

use rustc_hash::FxHashMap;

use crate::handlers::network::NetworkRequest;
use crate::lantern::graph::{Graph, Node, NodeId, NodeKind, RequestFacts};

/// Build the graph, returning it alongside the node each request became.
#[must_use]
pub fn build(requests: &[NetworkRequest], document_url: &str) -> (Graph, Vec<Option<NodeId>>) {
  let mut graph = Graph::default();
  let mut node_of: Vec<Option<NodeId>> = vec![None; requests.len()];
  let mut by_url: FxHashMap<&str, usize> = FxHashMap::default();

  // The document is the root: nothing on the page can start before the
  // HTML that asked for it.
  let Some(root_request) = requests
    .iter()
    .position(|r| r.url == document_url)
    .or(if requests.is_empty() { None } else { Some(0) })
  else {
    return (graph, node_of);
  };

  for (index, request) in requests.iter().enumerate() {
    let node = graph.add_node(Node {
      kind: NodeKind::Network(index),
      start_time_us: request.start_time,
      end_time_us: request.end_time,
      is_main_document: index == root_request,
    });
    node_of[index] = Some(node);
    by_url.entry(request.url.as_str()).or_insert(index);
    if index == root_request {
      graph.root = node;
    }
  }

  for (index, request) in requests.iter().enumerate() {
    let Some(node) = node_of[index] else { continue };
    if index == root_request {
      continue;
    }
    // A request depends on whatever initiated it, and on the document
    // when nothing else is named: the browser could not have known to
    // ask for it before the HTML arrived.
    let parent = by_url
      .get(request.initiator_url.as_str())
      .copied()
      .filter(|parent| *parent != index)
      .unwrap_or(root_request);
    if let Some(parent_node) = node_of[parent] {
      graph.add_dependency(node, parent_node);
    }
  }

  (graph, node_of)
}

/// The facts the simulator needs, in request order.
#[must_use]
pub fn facts(requests: &[NetworkRequest]) -> Vec<RequestFacts> {
  requests.iter().map(RequestFacts::from_request).collect()
}
