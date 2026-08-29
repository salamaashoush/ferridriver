//! The event-driven page-load simulation.
//!
//! From devtools-frontend `lantern/simulation/Simulator.ts`, plus the
//! connection pool and DNS cache it drives.
//!
//! The loop is: start whatever can start, work out which in-flight node
//! finishes soonest, advance every node by exactly that much, repeat. It
//! is the "advance to the next event" shape rather than a fixed tick, so
//! its cost is proportional to the number of state changes and not to
//! the simulated duration.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::lantern::constants::Throttling;
use crate::lantern::graph::{Graph, NodeId, NodeKind, RequestFacts};
use crate::lantern::network_analyzer::{DEFAULT_SERVER_RESPONSE_TIME, NetworkAnalysis};
use crate::lantern::tcp::{DownloadOptions, TcpConnection};

/// Chrome's own cap on parallel delayable requests.
const MAXIMUM_CONCURRENT_REQUESTS: usize = 10;

/// Connections a browser opens per origin over HTTP/1.
const CONNECTIONS_PER_ORIGIN: usize = 6;

/// Past this a task is almost certainly not CPU-bound, and scaling it
/// further just invents time.
const MAXIMUM_CPU_TASK_DURATION_MS: f64 = 10_000.0;

/// DNS is assumed to cost this many round trips.
const DNS_RESOLUTION_RTT_MULTIPLIER: f64 = 2.0;

/// Guard against a graph that never settles.
const MAX_ITERATIONS: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
  NotReady,
  Ready,
  InProgress,
  Complete,
}

#[derive(Debug, Clone, Default)]
struct Timing {
  start_time: f64,
  end_time: f64,
  time_elapsed: f64,
  /// A node can be told to advance past the boundary it was measured
  /// to; the excess is carried so it is not charged twice.
  time_elapsed_overshoot: f64,
  bytes_downloaded: f64,
  estimated_time_elapsed: f64,
}

#[derive(Debug, Clone)]
pub struct SimulationResult {
  /// Simulated wall time in milliseconds.
  pub time_ms: f64,
  /// Per node, when it started and finished in that simulated timeline.
  pub node_timings: FxHashMap<NodeId, (f64, f64)>,
}

#[derive(Debug, thiserror::Error)]
pub enum SimulationError {
  #[error("cannot simulate a graph containing a cycle")]
  Cycle,
  #[error("simulation stalled: no node could start")]
  Stalled,
  #[error("simulation exceeded {MAX_ITERATIONS} iterations")]
  TooManyIterations,
}

pub struct Simulator<'a> {
  facts: &'a [RequestFacts],
  rtt: f64,
  throughput: f64,
  cpu_slowdown: f64,
  layout_slowdown: f64,
  analysis: &'a NetworkAnalysis,
}

impl<'a> Simulator<'a> {
  #[must_use]
  pub fn new(facts: &'a [RequestFacts], analysis: &'a NetworkAnalysis, throttling: Throttling) -> Self {
    Self {
      facts,
      rtt: throttling.rtt_ms,
      throughput: throttling.throughput_bps,
      cpu_slowdown: throttling.cpu_slowdown_multiplier,
      // Upstream stacks the two rather than choosing between them, so
      // Slow 4G lays out at 2x and not at half speed.
      layout_slowdown: throttling.cpu_slowdown_multiplier * throttling.layout_task_multiplier,
      analysis,
    }
  }

  /// Run the graph to completion and report how long it took.
  ///
  /// # Errors
  ///
  /// [`SimulationError::Cycle`] when the graph is not a DAG, and
  /// [`SimulationError::Stalled`] when no node can start, which happens
  /// on a graph whose cache and connection-reuse flags disagree.
  pub fn simulate(&self, graph: &Graph) -> Result<SimulationResult, SimulationError> {
    if graph.has_cycle() {
      return Err(SimulationError::Cycle);
    }
    let reachable = graph.reachable();
    if reachable.is_empty() {
      return Ok(SimulationResult {
        time_ms: 0.0,
        node_timings: FxHashMap::default(),
      });
    }

    let mut pool = ConnectionPool::new(self.facts, self.analysis, self.rtt, self.throughput);
    let mut dns = DnsCache::new(self.rtt);
    let mut state: FxHashMap<NodeId, State> = reachable.iter().map(|n| (*n, State::NotReady)).collect();
    let mut timings: FxHashMap<NodeId, Timing> = FxHashMap::default();
    let mut total_elapsed = 0.0f64;

    Self::mark_ready_if_possible(graph, &mut state, graph.root, total_elapsed, &mut timings);

    for iteration in 0.. {
      if iteration > MAX_ITERATIONS {
        return Err(SimulationError::TooManyIterations);
      }
      let ready: Vec<NodeId> = ordered(&state, State::Ready, graph, self.facts);
      if ready.is_empty() && !state.values().any(|s| *s == State::InProgress) {
        break;
      }

      for node in ready {
        self.start_if_possible(graph, node, total_elapsed, &mut state, &mut timings, &mut pool);
      }
      let in_progress: Vec<NodeId> = state
        .iter()
        .filter(|(_, s)| **s == State::InProgress)
        .map(|(n, _)| *n)
        .collect();
      if in_progress.is_empty() {
        return Err(SimulationError::Stalled);
      }

      // Every in-flight request shares the link.
      let in_flight = in_progress
        .iter()
        .filter(|n| matches!(graph.nodes[**n].kind, NodeKind::Network(_)))
        .count();
      if in_flight > 0 {
        pool.set_shared_throughput(self.throughput / crate::units::len_to_f64(in_flight));
      }

      let minimum = in_progress
        .iter()
        .map(|node| self.estimate_time_remaining(graph, *node, &mut timings, &mut pool, &mut dns))
        .fold(f64::INFINITY, f64::min);
      if !minimum.is_finite() {
        return Err(SimulationError::TooManyIterations);
      }
      total_elapsed += minimum;

      for node in in_progress {
        self.advance(
          graph,
          node,
          minimum,
          total_elapsed,
          &mut state,
          &mut timings,
          &mut pool,
          &mut dns,
        );
      }
      // A completed node may have unblocked others.
      for node in graph.reachable() {
        if state.get(&node) == Some(&State::NotReady) {
          Self::mark_ready_if_possible(graph, &mut state, node, total_elapsed, &mut timings);
        }
      }
    }

    Ok(SimulationResult {
      time_ms: total_elapsed,
      node_timings: timings
        .into_iter()
        .map(|(node, t)| (node, (t.start_time, t.end_time)))
        .collect(),
    })
  }

  fn mark_ready_if_possible(
    graph: &Graph,
    state: &mut FxHashMap<NodeId, State>,
    node: NodeId,
    now: f64,
    timings: &mut FxHashMap<NodeId, Timing>,
  ) {
    if graph.dependencies[node]
      .iter()
      .all(|d| state.get(d) == Some(&State::Complete))
    {
      state.insert(node, State::Ready);
      timings.entry(node).or_default().start_time = now;
    }
  }

  fn start_if_possible(
    &self,
    graph: &Graph,
    node: NodeId,
    now: f64,
    state: &mut FxHashMap<NodeId, State>,
    timings: &mut FxHashMap<NodeId, Timing>,
    pool: &mut ConnectionPool,
  ) {
    let start = |state: &mut FxHashMap<NodeId, State>, timings: &mut FxHashMap<NodeId, Timing>| {
      state.insert(node, State::InProgress);
      let timing = timings.entry(node).or_default();
      timing.start_time = now;
    };

    match graph.nodes[node].kind {
      // One main thread, so one CPU task at a time.
      NodeKind::Cpu { .. } => {
        if !state
          .iter()
          .any(|(n, s)| *s == State::InProgress && matches!(graph.nodes[*n].kind, NodeKind::Cpu { .. }))
        {
          start(state, timings);
        }
      },
      NodeKind::Network(request) => {
        let fact = &self.facts[request];
        if fact.delivery.is_connectionless() {
          start(state, timings);
          return;
        }
        let active = state
          .iter()
          .filter(|(n, s)| **s == State::InProgress && matches!(graph.nodes[**n].kind, NodeKind::Network(_)))
          .count();
        if active >= MAXIMUM_CONCURRENT_REQUESTS {
          return;
        }
        if pool.acquire(request).is_some() {
          start(state, timings);
        }
      },
    }
  }

  fn estimate_time_remaining(
    &self,
    graph: &Graph,
    node: NodeId,
    timings: &mut FxHashMap<NodeId, Timing>,
    pool: &mut ConnectionPool,
    dns: &mut DnsCache,
  ) -> f64 {
    let timing = timings.entry(node).or_default().clone();
    let estimated = match graph.nodes[node].kind {
      NodeKind::Cpu {
        duration_us,
        did_perform_layout,
        ..
      } => {
        let multiplier = if did_perform_layout {
          self.layout_slowdown
        } else {
          self.cpu_slowdown
        };
        let total = (crate::units::micros_to_ms(duration_us) * multiplier)
          .round()
          .min(MAXIMUM_CPU_TASK_DURATION_MS);
        total - timing.time_elapsed
      },
      NodeKind::Network(request) => {
        let fact = &self.facts[request];
        if fact.delivery.is_cached() {
          // Seek plus a sequential read: 8ms and 20ms per megabyte.
          let mb = crate::units::count_to_f64(fact.resource_size) / 1024.0 / 1024.0;
          8.0 + 20.0 * mb - timing.time_elapsed
        } else if fact.delivery.is_connectionless() {
          // Decoding a data URL: 2ms and 10ms per megabyte.
          let mb = crate::units::count_to_f64(fact.resource_size) / 1024.0 / 1024.0;
          2.0 + 10.0 * mb - timing.time_elapsed
        } else {
          let dns_time = dns.time_until_resolution(&fact.host, timing.start_time, true);
          let connection = pool.active(request);
          let remaining = crate::units::count_to_f64(fact.transfer_size) - timing.bytes_downloaded;
          connection
            .simulate_download_until(
              remaining,
              DownloadOptions {
                dns_resolution_time: dns_time,
                time_already_elapsed: timing.time_elapsed,
                maximum_time_to_elapse: f64::INFINITY,
              },
            )
            .time_elapsed
        }
      },
    };
    let estimated = estimated + timing.time_elapsed_overshoot;
    timings.entry(node).or_default().estimated_time_elapsed = estimated;
    estimated
  }

  #[allow(clippy::too_many_arguments)]
  fn advance(
    &self,
    graph: &Graph,
    node: NodeId,
    period: f64,
    now: f64,
    state: &mut FxHashMap<NodeId, State>,
    timings: &mut FxHashMap<NodeId, Timing>,
    pool: &mut ConnectionPool,
    dns: &mut DnsCache,
  ) {
    let timing = timings.entry(node).or_default().clone();
    // The node whose estimate set the period is the one that finishes.
    let finished = (timing.estimated_time_elapsed - period).abs() < f64::EPSILON;

    let connectionless = match graph.nodes[node].kind {
      NodeKind::Cpu { .. } => true,
      NodeKind::Network(request) => {
        let fact = &self.facts[request];
        fact.delivery.is_connectionless() || fact.delivery.is_cached()
      },
    };

    if connectionless {
      if finished {
        Self::complete(node, now, state, timings);
      } else {
        timings.entry(node).or_default().time_elapsed += period;
      }
      return;
    }

    let NodeKind::Network(request) = graph.nodes[node].kind else {
      return;
    };
    let fact = &self.facts[request];
    let dns_time = dns.time_until_resolution(&fact.host, timing.start_time, true);
    let remaining = crate::units::count_to_f64(fact.transfer_size) - timing.bytes_downloaded;
    let calculation = pool.active(request).simulate_download_until(
      remaining,
      DownloadOptions {
        dns_resolution_time: dns_time,
        time_already_elapsed: timing.time_elapsed,
        maximum_time_to_elapse: period - timing.time_elapsed_overshoot,
      },
    );
    pool.record_progress(
      request,
      calculation.congestion_window,
      calculation.extra_bytes_downloaded,
    );

    if finished {
      pool.release(request);
      Self::complete(node, now, state, timings);
    } else {
      let timing = timings.entry(node).or_default();
      timing.time_elapsed += calculation.time_elapsed;
      timing.time_elapsed_overshoot += calculation.time_elapsed - period;
      timing.bytes_downloaded += calculation.bytes_downloaded;
    }
  }

  fn complete(node: NodeId, now: f64, state: &mut FxHashMap<NodeId, State>, timings: &mut FxHashMap<NodeId, Timing>) {
    state.insert(node, State::Complete);
    timings.entry(node).or_default().end_time = now;
  }
}

/// Ready nodes in the order a browser would start them: by priority,
/// then by when they were observed to start.
fn ordered(state: &FxHashMap<NodeId, State>, want: State, graph: &Graph, facts: &[RequestFacts]) -> Vec<NodeId> {
  let mut nodes: Vec<NodeId> = state.iter().filter(|(_, s)| **s == want).map(|(n, _)| *n).collect();
  nodes.sort_by(|a, b| {
    let key = |node: &NodeId| {
      let penalty = match graph.nodes[*node].kind {
        NodeKind::Network(request) => priority_penalty(&facts[request].priority),
        NodeKind::Cpu { .. } => 0.0,
      };
      crate::units::micros_to_ms(graph.nodes[*node].start_time_us) + penalty * 1000.0
    };
    key(a).total_cmp(&key(b))
  });
  nodes
}

/// Lower-priority requests are started later, in round trips.
fn priority_penalty(priority: &str) -> f64 {
  match priority {
    "VeryHigh" => 0.0,
    "High" => 0.25,
    "Medium" => 0.5,
    "Low" => 1.0,
    _ => 2.0,
  }
}

/// Connections, grouped by origin, as a browser would keep them.
struct ConnectionPool {
  by_origin: FxHashMap<String, Vec<TcpConnection>>,
  /// request index -> (origin, slot)
  assigned: FxHashMap<usize, (String, usize)>,
  in_use: FxHashSet<(String, usize)>,
  /// request index -> origin, so the hot path never re-parses a URL.
  origin_by_request: FxHashMap<usize, String>,
}

impl ConnectionPool {
  fn new(facts: &[RequestFacts], analysis: &NetworkAnalysis, rtt: f64, throughput: f64) -> Self {
    let mut by_origin: FxHashMap<String, Vec<TcpConnection>> = FxHashMap::default();
    let mut origin_by_request = FxHashMap::default();

    for (index, fact) in facts.iter().enumerate() {
      if fact.delivery.is_connectionless() {
        continue;
      }
      origin_by_request.insert(index, fact.origin.clone());
      let additional = analysis
        .additional_rtt_by_origin
        .get(&fact.origin)
        .copied()
        .unwrap_or(0.0);
      // A measured zero means the estimate was clamped: the observed
      // time to first byte came out no larger than the round trip, so
      // the trace could not separate the server from the network. No
      // server answers instantly, and upstream reads a zero the same
      // way — `serverResponseTimeByOrigin.get(origin) || DEFAULT` in
      // JavaScript falls through on zero as well as on absence.
      let response = analysis
        .server_response_time_by_origin
        .get(&fact.origin)
        .copied()
        .filter(|measured| *measured > 0.0)
        .unwrap_or(DEFAULT_SERVER_RESPONSE_TIME);
      let connection = TcpConnection::new(
        rtt + additional,
        throughput,
        response,
        fact.delivery.is_tls(),
        fact.delivery.is_h2(),
      );
      by_origin.entry(fact.origin.clone()).or_default().push(connection);
    }

    // An origin must have enough connections to saturate the link. h2
    // multiplexes, so one is already enough there.
    for connections in by_origin.values_mut() {
      let minimum = if connections.first().is_some_and(|c| c.h2) {
        1
      } else {
        CONNECTIONS_PER_ORIGIN
      };
      while connections.len() < minimum {
        let template = connections[0].clone();
        connections.push(template);
      }
    }

    Self {
      by_origin,
      assigned: FxHashMap::default(),
      in_use: FxHashSet::default(),
      origin_by_request,
    }
  }

  /// Take the free connection with the largest congestion window: it is
  /// already warmed up and will finish soonest.
  fn acquire(&mut self, request: usize) -> Option<()> {
    if self.assigned.contains_key(&request) {
      return Some(());
    }
    let origin = self.origin_of(request)?;
    let connections = self.by_origin.get(&origin)?;
    let slot = connections
      .iter()
      .enumerate()
      .filter(|(slot, _)| !self.in_use.contains(&(origin.clone(), *slot)))
      .max_by(|a, b| a.1.congestion_window.total_cmp(&b.1.congestion_window))
      .map(|(slot, _)| slot)?;

    self.in_use.insert((origin.clone(), slot));
    self.assigned.insert(request, (origin, slot));
    Some(())
  }

  fn active(&self, request: usize) -> &TcpConnection {
    let (origin, slot) = &self.assigned[&request];
    &self.by_origin[origin][*slot]
  }

  fn record_progress(&mut self, request: usize, congestion_window: f64, extra_bytes: f64) {
    if let Some((origin, slot)) = self.assigned.get(&request)
      && let Some(connection) = self.by_origin.get_mut(origin).and_then(|c| c.get_mut(*slot))
    {
      connection.congestion_window = congestion_window;
      connection.h2_overflow_bytes_downloaded = extra_bytes;
    }
  }

  fn release(&mut self, request: usize) {
    if let Some((origin, slot)) = self.assigned.remove(&request) {
      if let Some(connection) = self.by_origin.get_mut(&origin).and_then(|c| c.get_mut(slot)) {
        connection.warmed = true;
      }
      self.in_use.remove(&(origin, slot));
    }
  }

  fn set_shared_throughput(&mut self, throughput: f64) {
    for (origin, slot) in &self.in_use {
      if let Some(connection) = self.by_origin.get_mut(origin).and_then(|c| c.get_mut(*slot)) {
        connection.throughput = throughput;
      }
    }
  }

  fn origin_of(&self, request: usize) -> Option<String> {
    self.origin_by_request.get(&request).cloned()
  }
}

/// Domains already resolved cost nothing to resolve again.
struct DnsCache {
  rtt: f64,
  resolved: FxHashMap<String, f64>,
}

impl DnsCache {
  fn new(rtt: f64) -> Self {
    Self {
      rtt,
      resolved: FxHashMap::default(),
    }
  }

  fn time_until_resolution(&mut self, host: &str, requested_at: f64, update: bool) -> f64 {
    let mut time_until = self.rtt * DNS_RESOLUTION_RTT_MULTIPLIER;
    if let Some(resolved_at) = self.resolved.get(host) {
      time_until = time_until.min((resolved_at - requested_at).max(0.0));
    }
    if update {
      let resolved_at = requested_at + time_until;
      self
        .resolved
        .entry(host.to_string())
        .and_modify(|current| *current = current.min(resolved_at))
        .or_insert(resolved_at);
    }
    time_until
  }
}
