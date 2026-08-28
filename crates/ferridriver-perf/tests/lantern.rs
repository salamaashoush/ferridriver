//! The simulation, over hand-built graphs.
//!
//! Lantern answers "how much faster without X", which cannot be checked
//! against the trace it came from. What CAN be checked is that the model
//! behaves: more bytes take longer, a chain costs more than the same
//! requests in parallel, and removing work never makes the page slower.

use ferridriver_perf::lantern;
use ferridriver_perf::lantern::constants::MOBILE_SLOW_4G;
use ferridriver_perf::lantern::graph::{Delivery, Graph, Node, NodeKind, RequestFacts};
use ferridriver_perf::lantern::network_analyzer::NetworkAnalysis;
use ferridriver_perf::lantern::simulator::Simulator;

fn fact(origin: &str, bytes: i64) -> RequestFacts {
  RequestFacts {
    origin: origin.into(),
    host: origin.trim_start_matches("https://").into(),
    transfer_size: bytes,
    resource_size: bytes,
    delivery: Delivery::Secure {
      is_tls: true,
      is_h2: false,
    },
    priority: "VeryHigh".into(),
  }
}

/// `n` requests, all depending only on the first.
fn parallel_graph(count: usize) -> Graph {
  let mut graph = Graph::default();
  let root = graph.add_node(Node {
    kind: NodeKind::Network(0),
    start_time_us: 0,
    end_time_us: 1000,
    is_main_document: true,
  });
  graph.root = root;
  for index in 1..count {
    let node = graph.add_node(Node {
      kind: NodeKind::Network(index),
      start_time_us: 1000,
      end_time_us: 2000,
      is_main_document: false,
    });
    graph.add_dependency(node, root);
  }
  graph
}

/// `n` requests in a line, each waiting for the one before.
fn chain_graph(count: usize) -> Graph {
  let mut graph = Graph::default();
  let root = graph.add_node(Node {
    kind: NodeKind::Network(0),
    start_time_us: 0,
    end_time_us: 1000,
    is_main_document: true,
  });
  graph.root = root;
  let mut previous = root;
  for index in 1..count {
    let node = graph.add_node(Node {
      kind: NodeKind::Network(index),
      start_time_us: 1000,
      end_time_us: 2000,
      is_main_document: false,
    });
    graph.add_dependency(node, previous);
    previous = node;
  }
  graph
}

/// A failed simulation becomes NaN, which compares false against
/// everything, so the assertion that used it fails rather than the
/// helper panicking somewhere unhelpful.
fn simulate(graph: &Graph, facts: &[RequestFacts]) -> f64 {
  let analysis = NetworkAnalysis::default();
  Simulator::new(facts, &analysis, MOBILE_SLOW_4G)
    .simulate(graph)
    .map_or(f64::NAN, |result| result.time_ms)
}

#[test]
fn a_bigger_download_takes_longer() {
  let small: Vec<RequestFacts> = (0..2).map(|_| fact("https://a.example", 1_000)).collect();
  let large: Vec<RequestFacts> = (0..2).map(|_| fact("https://a.example", 500_000)).collect();
  let graph = parallel_graph(2);
  assert!(
    simulate(&graph, &large) > simulate(&graph, &small),
    "500KB should take longer than 1KB"
  );
}

/// The whole point of the dependency graph: a chain pays one set of
/// round trips per hop, and those cannot overlap.
///
/// The payloads are deliberately small. With large ones the two shapes
/// converge, because the simulator divides throughput between in-flight
/// requests exactly as upstream does, so parallel downloads of the same
/// total bytes take the same total transfer time. What a chain adds is
/// LATENCY, and latency only dominates when there are few bytes to hide
/// it behind. That is also the case `NetworkDependencyTree` is about.
#[test]
fn a_chain_of_small_requests_costs_more_than_the_same_requests_in_parallel() {
  let facts: Vec<RequestFacts> = (0..5).map(|_| fact("https://a.example", 500)).collect();
  let chained = simulate(&chain_graph(5), &facts);
  let parallel = simulate(&parallel_graph(5), &facts);
  assert!(chained > parallel, "chain {chained} should exceed parallel {parallel}");
}

/// The converse, stated so the trade-off is not mistaken for a bug: on
/// large payloads the shapes converge, because bandwidth rather than
/// latency is the constraint.
#[test]
fn on_large_payloads_a_chain_and_parallel_requests_are_comparable() {
  let facts: Vec<RequestFacts> = (0..5).map(|_| fact("https://a.example", 50_000)).collect();
  let chained = simulate(&chain_graph(5), &facts);
  let parallel = simulate(&parallel_graph(5), &facts);
  let ratio = chained / parallel;
  assert!(
    (0.8..1.25).contains(&ratio),
    "expected comparable, got chain {chained} vs parallel {parallel}"
  );
}

/// Latency is per connection, so more origins means more handshakes.
#[test]
fn spreading_requests_over_more_origins_costs_more_handshakes() {
  let one_origin: Vec<RequestFacts> = (0..6).map(|_| fact("https://a.example", 40_000)).collect();
  let many_origins: Vec<RequestFacts> = (0..6)
    .map(|i| fact(&format!("https://origin{i}.example"), 40_000))
    .collect();
  let graph = chain_graph(6);
  assert!(
    simulate(&graph, &many_origins) >= simulate(&graph, &one_origin),
    "six origins should cost at least as much as one"
  );
}

/// A connectionless URL has no handshake and no server to wait for.
#[test]
fn a_data_url_is_far_cheaper_than_a_network_request() {
  let mut network = vec![fact("https://a.example", 10_000), fact("https://a.example", 10_000)];
  let mut inline = network.clone();
  inline[1].delivery = Delivery::Connectionless;

  let graph = chain_graph(2);
  assert!(
    simulate(&graph, &inline) < simulate(&graph, &network),
    "a data URL should beat a network fetch"
  );
  network[1].delivery = Delivery::Cached;
  assert!(
    simulate(&graph, &network) < simulate(&graph, &vec![fact("https://a.example", 10_000); 2]),
    "a cached response should beat a network fetch"
  );
}

#[test]
fn a_cycle_is_rejected_rather_than_looping_forever() {
  let mut graph = parallel_graph(2);
  // Close the loop: root now also depends on its own dependent.
  graph.add_dependency(graph.root, 1);
  let facts: Vec<RequestFacts> = (0..2).map(|_| fact("https://a.example", 1000)).collect();
  let analysis = NetworkAnalysis::default();
  assert!(
    Simulator::new(&facts, &analysis, MOBILE_SLOW_4G)
      .simulate(&graph)
      .is_err(),
    "a cyclic graph must be refused"
  );
}

#[test]
fn removing_nodes_never_makes_the_simulated_page_slower() {
  let facts: Vec<RequestFacts> = (0..4).map(|_| fact("https://a.example", 80_000)).collect();
  let graph = chain_graph(4);
  let full = simulate(&graph, &facts);

  let mut removed = rustc_hash::FxHashSet::default();
  removed.insert(3);
  let trimmed = simulate(&graph.without(&removed), &facts);
  assert!(
    trimmed <= full,
    "removing a request should not cost more: {trimmed} > {full}"
  );
}

/// The saving from not shipping bytes is throughput-bound and rounded to
/// 10ms, because the input is an estimate.
#[test]
fn wasted_bytes_convert_to_a_rounded_millisecond_saving() {
  let analysis = NetworkAnalysis::default();
  let saving = lantern::wasted_ms_from_bytes(200_000.0, &analysis);
  assert!(saving > 0.0);
  assert!(
    (saving / 10.0).fract().abs() < f64::EPSILON,
    "should be a multiple of 10ms, got {saving}"
  );
  assert!(lantern::wasted_ms_from_bytes(0.0, &analysis).abs() < f64::EPSILON);
}
