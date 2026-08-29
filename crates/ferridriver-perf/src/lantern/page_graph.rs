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

/// Redirect hops as Lantern sees them: separate requests.
///
/// Everywhere else in this crate a redirected request is one
/// `NetworkRequest` carrying a list of hops, because that is what a
/// developer means by "the request". The simulator needs them apart.
/// Each hop is its own round trip and its own place in the graph, and
/// folding them into the destination hides all of it — including from
/// the connection-reuse inference, which decides that the destination
/// reused the connection the first hop opened.
///
/// The synthetic hop carries no phase timings at all, only the window it
/// occupied: a redirect has no handshake or download to time. Upstream
/// gives it a nominal 400 bytes and a 302, which keeps it out of the
/// throughput measurement.
#[must_use]
pub fn expand_redirects(requests: &[NetworkRequest]) -> Vec<NetworkRequest> {
  /// The size upstream attributes to a redirect response.
  const REDIRECT_TRANSFER_BYTES: i64 = 400;

  let mut expanded = Vec::with_capacity(requests.len());
  for request in requests {
    // Each hop waits on the one before it and the destination waits on
    // the last, so the chain lands on the critical path instead of
    // hanging beside it.
    let mut previous: Option<String> = None;
    for hop in &request.redirects {
      let end = hop.ts + hop.dur;
      let mut synthetic = request.clone();
      synthetic.url.clone_from(&hop.url);
      synthetic.status_code = 302;
      synthetic.resource_type = String::new();
      synthetic.encoded_data_length = REDIRECT_TRANSFER_BYTES;
      synthetic.decoded_body_length = REDIRECT_TRANSFER_BYTES;
      synthetic.redirects = Vec::new();
      synthetic.start_time = hop.ts;
      synthetic.end_time = end;
      synthetic.timing = crate::handlers::network::Timing {
        send_start_time: hop.ts,
        download_start: end,
        finish_time: end,
        first_byte_ts: Some(end),
        ..Default::default()
      };
      // Headers "arrived" when the hop ended, and every other phase is
      // absent. Upstream writes -1 for absent, which is what the
      // estimators test for.
      synthetic.resource_timing = Some(absent_timing(end));
      synthetic.initiator_url = previous.unwrap_or_default();
      previous = Some(hop.url.clone());
      expanded.push(synthetic);
    }
    let mut final_request = request.clone();
    if let Some(last_hop) = previous {
      final_request.initiator_url = last_hop;
    }
    expanded.push(final_request);
  }
  expanded
}

/// Where a redirected document's chain starts.
///
/// The graph's root has to be the first hop, not the destination: the
/// root is the one node with no dependencies, and a root that waits on
/// something can never start, which silently simulates the whole page
/// as taking no time at all.
#[must_use]
pub fn chain_head(requests: &[NetworkRequest], document_url: &str) -> String {
  requests
    .iter()
    .find(|request| request.url == document_url)
    .and_then(|request| request.redirects.first())
    .map_or_else(|| document_url.to_string(), |hop| hop.url.clone())
}

/// A `ResourceTiming` with every phase marked absent, save the moment
/// the response headers were complete.
fn absent_timing(headers_end: crate::event::Micro) -> crate::event::ResourceTiming {
  let headers_end_ms = crate::units::micros_to_ms(headers_end);
  crate::event::ResourceTiming {
    request_time: headers_end_ms / 1000.0,
    receive_headers_start: Some(headers_end_ms),
    receive_headers_end: headers_end_ms,
    proxy_start: -1.0,
    proxy_end: -1.0,
    dns_start: -1.0,
    dns_end: -1.0,
    connect_start: -1.0,
    connect_end: -1.0,
    ssl_start: -1.0,
    ssl_end: -1.0,
    send_start: -1.0,
    send_end: -1.0,
  }
}

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
