//! Lantern: what the page load would have looked like under different
//! conditions.
//!
//! Ported from devtools-frontend `models/trace/lantern`. The point is to
//! answer "how much faster would this be without X", which cannot be
//! measured from one trace: you can observe what happened, but not what
//! would have happened. Lantern answers it by rebuilding the load as a
//! dependency graph and re-running it under a network and CPU model,
//! once as observed and once with X removed. The difference is the
//! saving.
//!
//! Which model is used decides what the answer means. `DevTools` runs
//! its insights on the conditions the trace itself recorded, so the
//! saving it shows is in the milliseconds that visit actually had, and
//! [`Context`] does the same. [`constants`] carries the throttling
//! presets as well, which answer the different question of what the page
//! would cost someone on a slower connection.
//!
//! ```no_run
//! # use ferridriver_perf::lantern;
//! # fn main() {
//! # let (requests, events) = (vec![], vec![]);
//! let context = lantern::Context::build(&requests, "https://site.example/", &events, None, None);
//! # let _ = context;
//! # }
//! ```

pub mod constants;
pub mod cpu_graph;
pub mod graph;
pub mod metrics;
pub mod network_analyzer;
pub mod page_graph;
pub mod simulator;
pub mod tcp;

use std::collections::VecDeque;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::event::{Micro, TraceEvent};
use crate::handlers::network::NetworkRequest;
use constants::{MOBILE_SLOW_4G, Throttling};
use graph::{Graph, NodeId, NodeKind, RequestFacts};
use network_analyzer::NetworkAnalysis;
use simulator::{SimulationResult, Simulator};

/// Savings rounded to the nearest this many milliseconds.
///
/// From `Metric.GRAPH_SAVINGS_PRECISION`. A byte-savings estimate is two
/// simulations of a model fitted to aggregate data, and reporting it to
/// the millisecond would imply a resolution the model does not have.
const GRAPH_SAVINGS_PRECISION_MS: f64 = 50.0;

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
  /// How long each removed request's own node took in the observed
  /// simulation, rounded to whole milliseconds. A caller that only
  /// reports a saving above some floor needs the simulated duration
  /// rather than the measured one, because the saving it is deciding
  /// about came from the same model.
  pub removed_durations_ms: Vec<(String, f64)>,
}

/// Milliseconds a change would save each paint metric.
///
/// Mirrors `metricSavings` on the `DevTools` insight models, which is
/// what the panel puts beside a finding.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MetricSavings {
  pub fcp_ms: f64,
  pub lcp_ms: f64,
}

/// The graph and simulator every insight shares.
///
/// Upstream builds one of these per navigation and hands it to every
/// insight that needs a prediction. Building it per insight instead
/// would walk the whole event list once each to collect main-thread
/// tasks, which is the most expensive thing in the analysis, and
/// re-simulate the same unchanged page eight times over.
pub struct Context {
  /// The requests with every redirect hop as its own entry, which is
  /// what the simulator reasons about. Not the list the insights read.
  requests: Vec<NetworkRequest>,
  facts: Vec<RequestFacts>,
  analysis: NetworkAnalysis,
  /// The subgraph first contentful paint waited on.
  fcp: Graph,
  /// The subgraph largest contentful paint waited on.
  lcp: Graph,
  /// Both subgraphs as observed. Every saving is a difference against
  /// these and they never change, so simulating them once here rather
  /// than once per insight removes ten runs of the same unchanged page.
  fcp_observed: SimulationResult,
  lcp_observed: SimulationResult,
  /// The pessimistic halves. Nothing an insight reports uses these;
  /// they exist for [`Self::paint_estimate`], which averages the two
  /// halves the way upstream's metric estimates do.
  fcp_pessimistic: Graph,
  lcp_pessimistic: Graph,
}

/// What the paints would land at under a given connection.
///
/// From `Metric.compute`: each metric is simulated twice, once over the
/// subgraph that assumes everything avoidable was avoided and once over
/// the one that assumes nothing was, and the answer is the midpoint.
/// Upstream weights them 0.5/0.5 for both paints.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaintEstimate {
  pub first_contentful_paint_ms: f64,
  pub largest_contentful_paint_ms: f64,
}

impl Context {
  /// Build the graph once for the whole report.
  ///
  /// Returns `None` when the page has no paint to reason about, no
  /// document request, or a graph that cannot be simulated. Upstream
  /// behaves the same way and simply omits every predicted saving in
  /// that case, which is better than inventing one: with no paint
  /// timestamp there is no subgraph, and a saving measured against the
  /// whole load answers a larger question than the one asked.
  #[must_use]
  pub fn build(
    requests: &[NetworkRequest],
    document_url: &str,
    events: &[TraceEvent<'_>],
    first_paint_ts: Option<Micro>,
    largest_paint_ts: Option<Micro>,
  ) -> Option<Self> {
    let (first_paint_ts, largest_paint_ts) = (first_paint_ts?, largest_paint_ts?);
    if requests.is_empty() {
      return None;
    }
    let root_url = page_graph::chain_head(requests, document_url);
    let requests = page_graph::expand_redirects(requests);
    let requests = requests.as_slice();
    let facts = page_graph::facts(requests);
    let analysis = NetworkAnalysis::analyze(requests, &facts);
    let (mut graph, node_of) = page_graph::build(requests, &root_url);
    if graph.is_empty() {
      return None;
    }
    // Main-thread work goes in too, or a page held up by script rather
    // than by fetching simulates as though the script were free.
    cpu_graph::add_cpu_nodes(&mut graph, events, requests, &node_of);

    let fcp = metrics::first_contentful_paint_graph(&graph, requests, &node_of, first_paint_ts);
    let lcp = metrics::largest_contentful_paint_graph(&graph, requests, &node_of, largest_paint_ts);
    if fcp.is_empty() || lcp.is_empty() {
      return None;
    }

    let fcp_pessimistic = metrics::first_contentful_paint_graph_pessimistic(&graph, requests, &node_of, first_paint_ts);
    let lcp_pessimistic =
      metrics::largest_contentful_paint_graph_pessimistic(&graph, requests, &node_of, largest_paint_ts);

    let throttling = Throttling::observed(analysis.rtt, analysis.throughput);
    let (fcp_observed, lcp_observed) = {
      let simulator = Simulator::new(&facts, &analysis, throttling);
      (simulator.simulate(&fcp).ok()?, simulator.simulate(&lcp).ok()?)
    };
    Some(Self {
      requests: requests.to_vec(),
      facts,
      analysis,
      fcp,
      lcp,
      fcp_observed,
      lcp_observed,
      fcp_pessimistic,
      lcp_pessimistic,
    })
  }

  fn simulator<'f>(&'f self, facts: &'f [RequestFacts]) -> Simulator<'f> {
    // `DevTools` runs its insight simulations on the conditions the
    // trace recorded, not on a preset, so a saving it reports is in the
    // milliseconds that visit actually had. Simulating Slow 4G instead
    // answers a different question and puts every number out of step
    // with the panel.
    Simulator::new(
      facts,
      &self.analysis,
      Throttling::observed(self.analysis.rtt, self.analysis.throughput),
    )
  }

  /// What the trace itself says the connection was doing.
  #[must_use]
  pub fn network_analysis(&self) -> &NetworkAnalysis {
    &self.analysis
  }

  /// What the paints would land at over `throttling`.
  ///
  /// Pass [`constants::MOBILE_SLOW_4G`] to ask what the page would do
  /// for someone on a slow phone, or [`Throttling::observed`] with this
  /// trace's own conditions to ask what the model makes of the load
  /// that was actually recorded. The second is a check on the model
  /// rather than a prediction.
  ///
  /// Agreement with what `DevTools` computes for the same trace is
  /// checked directly, in `tests/differential.rs`, rather than inferred
  /// from the insights: every predicted saving is a DIFFERENCE of two
  /// simulations, so a model that is wrong in the same direction on
  /// both sides cancels out and leaves the insights looking right.
  #[must_use]
  pub fn paint_estimate(&self, throttling: Throttling) -> Option<PaintEstimate> {
    let facts = &self.facts;
    let simulator = Simulator::new(facts, &self.analysis, throttling);
    let at = |graph: &Graph, largest: bool| -> Option<f64> {
      let result = simulator.simulate(graph).ok()?;
      Some(if largest {
        self.largest_paint_time(graph, &result)
      } else {
        result.time_ms
      })
    };
    // Upstream weights the two halves evenly for both paints, with a
    // zero intercept, so the estimate is their midpoint.
    let midpoint = |a: f64, b: f64| a.mul_add(0.5, b * 0.5);
    let fcp = midpoint(at(&self.fcp, false)?, at(&self.fcp_pessimistic, false)?);
    let lcp = midpoint(at(&self.lcp, true)?, at(&self.lcp_pessimistic, true)?);
    Some(PaintEstimate {
      first_contentful_paint_ms: fcp,
      // The largest paint cannot land before the first one.
      largest_contentful_paint_ms: lcp.max(fcp),
    })
  }

  /// `LargestContentfulPaint.getEstimateFromSimulation`: the last thing
  /// to finish that was not an image the browser deprioritised, rather
  /// than the last thing to finish.
  fn largest_paint_time(&self, graph: &Graph, result: &SimulationResult) -> f64 {
    result
      .node_timings
      .iter()
      .filter(|(node, _)| match graph.nodes[**node].kind {
        NodeKind::Network(index) => is_not_low_priority_image(&self.requests[index]),
        NodeKind::Cpu { .. } => true,
      })
      .map(|(_, (_, end))| *end)
      .fold(0.0, f64::max)
  }

  /// Simulate the paint twice, with and without `removed_urls`, and
  /// report the difference.
  ///
  /// Removing a request removes whatever was waiting on it too, so the
  /// answer is what the page would have cost had it never asked for
  /// that resource, not what it would cost with a hole in the middle of
  /// the graph.
  #[must_use]
  pub fn savings_from_removing(&self, removed_urls: &[&str]) -> Option<Estimate> {
    let graph = &self.fcp;
    let observed = &self.fcp_observed;

    // Node ids are re-numbered by the subgraph, so removal is matched on
    // the URL the node carries rather than on the original index.
    let named: Vec<NodeId> = graph
      .nodes
      .iter()
      .enumerate()
      .filter(|(id, node)| *id != graph.root && !node.is_main_document)
      .filter(|(_, node)| match node.kind {
        NodeKind::Network(index) => removed_urls.contains(&self.requests[index].url.as_str()),
        NodeKind::Cpu { .. } => false,
      })
      .map(|(id, _)| id)
      .collect();

    // Removing a request removes everything that was waiting on it: a
    // stylesheet the page never fetches cannot have a font that only its
    // rules referenced. Upstream reaches the same set by traversing each
    // node's dependents before cloning the graph without them, and a
    // removal that kept the dependents would leave nodes whose only
    // predecessor is gone and understate the saving.
    let mut removed: FxHashSet<NodeId> = FxHashSet::default();
    let mut pending: VecDeque<NodeId> = named.iter().copied().collect();
    while let Some(node) = pending.pop_front() {
      if node == graph.root || !removed.insert(node) {
        continue;
      }
      pending.extend(graph.dependents[node].iter().copied());
    }

    let without = if removed.is_empty() {
      observed.clone()
    } else {
      self.simulator(&self.facts).simulate(&graph.without(&removed)).ok()?
    };

    let removed_durations_ms = named
      .iter()
      .filter_map(|id| {
        let NodeKind::Network(index) = graph.nodes[*id].kind else {
          return None;
        };
        let (start, end) = observed.node_timings.get(id)?;
        Some((self.requests[index].url.clone(), (end - start).round()))
      })
      .collect();

    Some(Estimate {
      observed: observed.time_ms,
      without: without.time_ms,
      // Rounded to whole milliseconds, as upstream does: the input is a
      // simulation, and a fractional millisecond implies precision it
      // does not have.
      savings: (observed.time_ms - without.time_ms).max(0.0).round(),
      removed_durations_ms,
    })
  }

  /// What each paint would gain if the named requests transferred that
  /// many fewer bytes.
  ///
  /// From `metricSavingsForWastedBytes`. The bytes are not removed from
  /// the graph: the request still happens, still costs a connection and
  /// still blocks whatever waited on it, and only its download shrinks.
  /// That is the difference between "compress this" and "delete this",
  /// and it is why [`Self::savings_from_removing`] cannot answer it.
  #[must_use]
  pub fn savings_from_wasted_bytes(&self, wasted_by_url: &FxHashMap<&str, f64>) -> MetricSavings {
    if wasted_by_url.is_empty() {
      return MetricSavings::none();
    }
    self.savings(
      &|facts, request| {
        if let Some(wasted) = wasted_by_url.get(request.url.as_str()) {
          let lighter = crate::units::count_to_f64(facts.transfer_size) - wasted;
          facts.transfer_size = crate::units::f64_to_count(lighter.max(0.0));
        }
      },
      // Rounded to the nearest fifty: a byte-savings estimate is two
      // runs of a model fitted to aggregate data, and a millisecond of
      // it would be precision the model does not have.
      &|savings| (savings / GRAPH_SAVINGS_PRECISION_MS).round() * GRAPH_SAVINGS_PRECISION_MS,
    )
  }

  /// What each paint would gain if the named requests were served over
  /// HTTP/2 rather than HTTP/1.
  ///
  /// From `ModernHTTP`'s `computeWasteWithGraph`. Nothing about the
  /// bytes changes; what changes is that they stop queueing behind a
  /// six-connection-per-origin limit.
  #[must_use]
  pub fn savings_from_multiplexing(&self, urls: &FxHashSet<&str>) -> MetricSavings {
    if urls.is_empty() {
      return MetricSavings::none();
    }
    self.savings(
      &|facts, request| {
        if urls.contains(request.url.as_str())
          && let graph::Delivery::Secure { is_tls, .. } = facts.delivery
        {
          facts.delivery = graph::Delivery::Secure { is_tls, is_h2: true };
        }
      },
      // Upstream floors this one to a tenth of a millisecond rather than
      // rounding it to fifty. Multiplexing either unblocks a queue or it
      // does not, so the answer is not the same kind of estimate.
      &|savings| (savings * 10.0).floor() / 10.0,
    )
  }

  /// Simulate both paint subgraphs twice, once as observed and once with
  /// `adjust` applied to every request's facts.
  fn savings(&self, adjust: &dyn Fn(&mut RequestFacts, &NetworkRequest), round: &dyn Fn(f64) -> f64) -> MetricSavings {
    let mut changed = self.facts.clone();
    for (facts, request) in changed.iter_mut().zip(&self.requests) {
      adjust(facts, request);
    }
    MetricSavings {
      fcp_ms: self.savings_on(&self.fcp, &self.fcp_observed, &changed, round),
      lcp_ms: self.savings_on(&self.lcp, &self.lcp_observed, &changed, round),
    }
  }

  fn savings_on(
    &self,
    graph: &Graph,
    observed: &SimulationResult,
    changed: &[RequestFacts],
    round: &dyn Fn(f64) -> f64,
  ) -> f64 {
    let Ok(after) = self.simulator(changed).simulate(graph) else {
      return 0.0;
    };
    round(observed.time_ms - after.time_ms)
  }
}

impl MetricSavings {
  /// Nothing to save, which upstream still reports: an insight that
  /// found no offenders has computed a zero rather than declined to
  /// answer.
  #[must_use]
  pub fn none() -> Self {
    Self {
      fcp_ms: 0.0,
      lcp_ms: 0.0,
    }
  }
}

/// `LargestContentfulPaint.isNotLowPriorityImageNode`, which decides
/// which nodes the largest-paint estimate is measured against.
fn is_not_low_priority_image(request: &NetworkRequest) -> bool {
  let low_priority = matches!(request.priority.as_str(), "Low" | "VeryLow");
  request.resource_type != "Image" || !low_priority
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
