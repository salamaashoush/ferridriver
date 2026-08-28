//! The page dependency graph.
//!
//! From devtools-frontend `lantern/graph/*`. Upstream models this with
//! objects holding pointers to each other; here it is an arena, so a
//! node is a `usize` and the edges are index lists. That keeps the graph
//! `Clone` and free of reference cycles, which matters because the
//! simulator runs it repeatedly with nodes removed.
//!
//! The graph is a DAG with a single root that everything reaches, which
//! is what lets traversal start anywhere and terminate.

use rustc_hash::FxHashSet;

use crate::handlers::network::NetworkRequest;

/// Index into [`Graph::nodes`].
pub type NodeId = usize;

#[derive(Debug, Clone)]
pub enum NodeKind {
  /// A network request, by index into the analysed request list.
  Network(usize),
  /// A slice of main-thread work.
  Cpu {
    /// Microseconds of observed CPU time.
    duration_us: i64,
    /// Layout is less CPU-bound than script, so it is scaled by a
    /// smaller multiplier under simulated throttling.
    did_perform_layout: bool,
  },
}

#[derive(Debug, Clone)]
pub struct Node {
  pub kind: NodeKind,
  pub start_time_us: i64,
  pub end_time_us: i64,
  pub is_main_document: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Graph {
  pub nodes: Vec<Node>,
  pub dependencies: Vec<Vec<NodeId>>,
  pub dependents: Vec<Vec<NodeId>>,
  /// The node everything else descends from.
  pub root: NodeId,
}

impl Graph {
  pub fn add_node(&mut self, node: Node) -> NodeId {
    self.nodes.push(node);
    self.dependencies.push(Vec::new());
    self.dependents.push(Vec::new());
    self.nodes.len() - 1
  }

  /// Record that `node` cannot start until `dependency` finishes.
  ///
  /// Self-edges and duplicates are ignored rather than rejected: the
  /// graph is built from observed initiator links, which can point at
  /// themselves after a redirect.
  pub fn add_dependency(&mut self, node: NodeId, dependency: NodeId) {
    if node == dependency || self.dependencies[node].contains(&dependency) {
      return;
    }
    self.dependencies[node].push(dependency);
    self.dependents[dependency].push(node);
  }

  #[must_use]
  pub fn len(&self) -> usize {
    self.nodes.len()
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.nodes.is_empty()
  }

  /// Every node reachable from the root, root first.
  ///
  /// A visited set is kept because the graph is a DAG rather than a
  /// tree: a node with two dependents is reached twice.
  #[must_use]
  pub fn reachable(&self) -> Vec<NodeId> {
    let mut seen = FxHashSet::default();
    let mut order = Vec::new();
    let mut stack = vec![self.root];
    while let Some(node) = stack.pop() {
      if !seen.insert(node) {
        continue;
      }
      order.push(node);
      stack.extend(self.dependents[node].iter().copied());
    }
    order
  }

  /// Whether the graph contains a cycle, which would make the
  /// simulation loop forever.
  #[must_use]
  pub fn has_cycle(&self) -> bool {
    // Iterative depth-first search with an explicit on-path set; the
    // recursive form overflows on a deep request chain.
    let mut visited = vec![false; self.nodes.len()];
    let mut on_path = vec![false; self.nodes.len()];

    for start in 0..self.nodes.len() {
      if visited[start] {
        continue;
      }
      let mut stack = vec![(start, 0usize)];
      on_path[start] = true;
      visited[start] = true;
      while let Some((node, index)) = stack.pop() {
        if let Some(&next) = self.dependents[node].get(index) {
          stack.push((node, index + 1));
          if on_path[next] {
            return true;
          }
          if !visited[next] {
            visited[next] = true;
            on_path[next] = true;
            stack.push((next, 0));
          }
        } else {
          on_path[node] = false;
        }
      }
    }
    false
  }

  /// A copy with `removed` and everything that depended only on it
  /// taken out, which is how "what if this request were not here" is
  /// answered.
  #[must_use]
  pub fn without(&self, removed: &FxHashSet<NodeId>) -> Self {
    let mut graph = Self {
      nodes: Vec::new(),
      dependencies: Vec::new(),
      dependents: Vec::new(),
      root: 0,
    };
    let mut mapping = vec![usize::MAX; self.nodes.len()];
    for (old, node) in self.nodes.iter().enumerate() {
      if removed.contains(&old) {
        continue;
      }
      mapping[old] = graph.add_node(node.clone());
      if old == self.root {
        graph.root = mapping[old];
      }
    }
    for (old, deps) in self.dependencies.iter().enumerate() {
      if mapping[old] == usize::MAX {
        continue;
      }
      for dependency in deps {
        if mapping[*dependency] != usize::MAX {
          graph.add_dependency(mapping[old], mapping[*dependency]);
        }
      }
    }
    graph
  }
}

/// What the simulator needs to know about a request, resolved once so
/// the hot loop does no string work.
#[derive(Debug, Clone)]
pub struct RequestFacts {
  pub origin: String,
  pub host: String,
  pub transfer_size: i64,
  pub resource_size: i64,
  pub delivery: Delivery,
  pub priority: String,
}

/// How the bytes reach the page, which decides the cost model applied
/// to them.
///
/// These are alternatives, not flags: a response served from cache does
/// not also open a TLS connection, and a `data:` URL has no server to
/// talk to. Modelling them as independent booleans permits states the
/// browser never produces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Delivery {
  #[default]
  Network,
  /// Over the network with TLS, HTTP/2, or both.
  Secure { is_tls: bool, is_h2: bool },
  /// `data:`, `blob:` and friends never touch the network.
  Connectionless,
  /// Already on disk or in memory.
  Cached,
}

impl Delivery {
  #[must_use]
  pub fn is_connectionless(self) -> bool {
    self == Self::Connectionless
  }

  #[must_use]
  pub fn is_cached(self) -> bool {
    self == Self::Cached
  }

  #[must_use]
  pub fn is_tls(self) -> bool {
    matches!(self, Self::Secure { is_tls: true, .. })
  }

  #[must_use]
  pub fn is_h2(self) -> bool {
    matches!(self, Self::Secure { is_h2: true, .. })
  }
}

impl RequestFacts {
  #[must_use]
  pub fn from_request(request: &NetworkRequest) -> Self {
    let scheme = request.url.split(':').next().unwrap_or_default().to_ascii_lowercase();
    let (origin, host) = split_origin(&request.url);
    Self {
      origin,
      host,
      transfer_size: request.encoded_data_length.max(0),
      resource_size: request.decoded_body_length.max(0),
      // Order matters: a cached response never reaches the network, so
      // that check comes before the transport one.
      delivery: if matches!(scheme.as_str(), "data" | "blob" | "intent" | "file" | "filesystem") {
        Delivery::Connectionless
      } else if request.timing.is_disk_cached || request.timing.is_memory_cached {
        Delivery::Cached
      } else {
        Delivery::Secure {
          is_tls: scheme == "https" || scheme == "wss",
          is_h2: request.protocol == "h2",
        }
      },
      priority: request.priority.clone(),
    }
  }
}

/// `(scheme://authority, host)`.
fn split_origin(url: &str) -> (String, String) {
  let Some((scheme, rest)) = url.split_once("://") else {
    return (url.to_string(), String::new());
  };
  let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
  let host = authority
    .split('@')
    .next_back()
    .unwrap_or_default()
    .split(':')
    .next()
    .unwrap_or_default();
  (format!("{scheme}://{authority}"), host.to_string())
}
