//! Lantern: what the page load would have looked like under different
//! conditions.
//!
//! Ported from devtools-frontend `models/trace/lantern`. The point is to
//! answer "how much faster would this be without X", which cannot be
//! measured from one trace: you can observe what happened, but not what
//! would have happened. Lantern answers it by rebuilding the load as a
//! dependency graph and re-running it under a fixed network and CPU
//! model, once as observed and once with X removed. The difference is
//! the saving.
//!
//! Because the model is fixed rather than the machine the trace came
//! from, two runs of the same page give the same answer, which an
//! observed measurement does not.
//!
//! ```no_run
//! # use ferridriver_perf::lantern;
//! # fn main() {
//! # let (requests, document_url) = (vec![], "");
//! let savings = lantern::savings_from_removing(&requests, document_url, &[]);
//! # let _ = savings;
//! # }
//! ```

pub mod constants;
pub mod graph;
pub mod network_analyzer;
pub mod page_graph;
pub mod simulator;
pub mod tcp;

use rustc_hash::FxHashSet;

use crate::handlers::network::NetworkRequest;
use constants::MOBILE_SLOW_4G;
use network_analyzer::NetworkAnalysis;
use simulator::Simulator;

/// What the simulation says about a page load. All times are
/// milliseconds of simulated wall clock.
#[derive(Debug, Clone)]
pub struct Estimate {
  /// The load as observed.
  pub observed: f64,
  /// The load with the named requests removed.
  pub without: f64,
  /// The difference, floored at zero. Removing work cannot make a page
  /// slower in this model, so a negative result is simulation noise
  /// rather than a finding.
  pub savings: f64,
}

/// Simulate the load twice, with and without `removed_urls`, and report
/// the difference.
///
/// Returns `None` when the graph cannot be simulated, which happens on a
/// trace with no document request or one whose initiator links form a
/// cycle. A caller should report the observed measurement rather than
/// invent a prediction.
#[must_use]
pub fn savings_from_removing(
  requests: &[NetworkRequest],
  document_url: &str,
  removed_urls: &[&str],
) -> Option<Estimate> {
  if requests.is_empty() {
    return None;
  }
  let facts = page_graph::facts(requests);
  let analysis = NetworkAnalysis::analyze(requests, &facts);
  let (graph, node_of) = page_graph::build(requests, document_url);
  if graph.is_empty() {
    return None;
  }

  let simulator = Simulator::new(&facts, &analysis, MOBILE_SLOW_4G);
  let observed = simulator.simulate(&graph).ok()?;

  let removed: FxHashSet<graph::NodeId> = requests
    .iter()
    .enumerate()
    .filter(|(_, request)| removed_urls.contains(&request.url.as_str()))
    .filter_map(|(index, _)| node_of[index])
    // The document itself is never removable: without it there is no
    // page to load.
    .filter(|node| *node != graph.root)
    .collect();

  let without = if removed.is_empty() {
    observed.clone()
  } else {
    simulator.simulate(&graph.without(&removed)).ok()?
  };

  Some(Estimate {
    observed: observed.time_ms,
    without: without.time_ms,
    savings: (observed.time_ms - without.time_ms).max(0.0),
  })
}

/// Milliseconds saved by not transferring `wasted_bytes`.
///
/// From `Simulator.computeWastedMsFromWastedBytes`. Rounded to 10ms
/// because the input is itself an estimate and finer precision would
/// imply confidence the number does not have.
#[must_use]
pub fn wasted_ms_from_bytes(wasted_bytes: f64, analysis: &NetworkAnalysis) -> f64 {
  // A zero throughput setting means no additional throttling is wanted,
  // so fall back to what the trace actually achieved.
  let bits_per_second = if MOBILE_SLOW_4G.throughput_bps > 0.0 {
    MOBILE_SLOW_4G.throughput_bps
  } else {
    analysis.throughput
  };
  if bits_per_second <= 0.0 {
    return 0.0;
  }
  let wasted_ms = wasted_bytes * 8.0 / bits_per_second * 1000.0;
  (wasted_ms / 10.0).round() * 10.0
}
