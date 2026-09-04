//! Why a node is still alive: every route from it back to a GC root.
//!
//! A dominator chain answers "what would have to let go", one step at a
//! time. This answers the wider question `get_heapsnapshot_retaining_paths`
//! asks -- ALL the ways the graph reaches this object -- and it is a
//! forest rather than a list, because a leak usually has more than one
//! holder and the interesting one is rarely the first.
//!
//! Ported from `HeapSnapshot.ts::getRetainingPaths`, minus the
//! retainers-view ignore list. That set is a `DevTools` UI affordance --
//! a reader clicks a retainer to stop it being counted -- and nothing
//! outside the panel ever adds to it, so upstream's two checks against
//! it are constant here.

use serde::{Deserialize, Serialize};

use crate::analysis::Analysis;

/// Distance 0 is the synthetic root and 1 is `(GC roots)`, so a node
/// two edges out is already a root as far as a retaining path is
/// concerned and the walk stops there.
const ROOT_DISTANCE: i64 = 2;

/// The limits `get_heapsnapshot_retaining_paths` defaults to.
pub const DEFAULT_MAX_DEPTH: usize = 30;
pub const DEFAULT_MAX_NODES: usize = 5000;
pub const DEFAULT_MAX_SIBLINGS: usize = 100;

/// One retaining edge, with everything it in turn is retained by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetainingEdge {
  pub edge_index: usize,
  pub edge_name: String,
  pub edge_type: String,
  pub node_id: u64,
  pub node_index: usize,
  pub node_name: String,
  pub distance: i64,
  pub children: Vec<RetainingEdge>,
}

/// Which limit stopped the walk, so a caller can tell a complete answer
/// from a truncated one.
///
/// Upstream leaves a bound out of its own JSON when it did not bite;
/// all three are always present here, because a caller reading
/// `limitsReached.depth` should not have to tell `false` from absent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LimitsReached {
  pub depth: bool,
  pub nodes: bool,
  pub siblings: bool,
}

/// Everything holding one node, and what the search gave up on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetainingPaths {
  pub paths: Vec<RetainingEdge>,
  pub limits_reached: LimitsReached,
}

/// The limits a search runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathLimits {
  pub depth: usize,
  pub nodes: usize,
  pub siblings: usize,
}

impl Default for PathLimits {
  fn default() -> Self {
    Self {
      depth: DEFAULT_MAX_DEPTH,
      nodes: DEFAULT_MAX_NODES,
      siblings: DEFAULT_MAX_SIBLINGS,
    }
  }
}

impl Analysis {
  /// Every route from this node back to a GC root, nearest root first.
  ///
  /// Bounded three ways, because the retainer graph of a real page is
  /// enormous and cyclic: how deep a path may go, how many nodes the
  /// whole search may touch, and how many retainers one node may
  /// contribute. Whichever bound bit is reported alongside the result
  /// rather than silently truncating it.
  #[must_use]
  pub fn retaining_paths(&self, ordinal: usize, limits: PathLimits) -> RetainingPaths {
    let mut walk = Walk {
      analysis: self,
      limits,
      traversed: 0,
      visiting: vec![false; self.snapshot.node_count],
      visited: vec![usize::MAX; self.snapshot.node_count],
      reached: LimitsReached::default(),
    };
    let paths = walk.build(ordinal, 0);
    RetainingPaths {
      paths,
      limits_reached: walk.reached,
    }
  }
}

/// One retainer of the node being expanded, kept until they can be
/// ordered by how close to a root they sit.
struct Candidate {
  slot: usize,
  distance: i64,
  ordinal: usize,
}

struct Walk<'a> {
  analysis: &'a Analysis,
  limits: PathLimits,
  traversed: usize,
  /// On the current path, so following it again would be a cycle.
  visiting: Vec<bool>,
  /// The shallowest depth this node has already been expanded at.
  /// `usize::MAX` stands for upstream's absent map entry.
  visited: Vec<usize>,
  reached: LimitsReached,
}

impl Walk<'_> {
  fn build(&mut self, ordinal: usize, depth: usize) -> Vec<RetainingEdge> {
    self.traversed += 1;
    if self.traversed > self.limits.nodes {
      self.reached.nodes = true;
      return Vec::new();
    }
    if depth >= self.limits.depth {
      self.reached.depth = true;
      return Vec::new();
    }
    // Already a root, so there is no path left to describe.
    if self.analysis.distance(ordinal) <= ROOT_DISTANCE {
      return Vec::new();
    }
    if self.visiting[ordinal] {
      return Vec::new();
    }
    // Revisited only from higher up, where a shorter path to a root may
    // still exist that the deeper visit could not reach.
    if self.visited[ordinal] != usize::MAX && depth >= self.visited[ordinal] {
      return Vec::new();
    }

    self.visiting[ordinal] = true;
    let candidates = self.candidates(ordinal, depth);
    let forest = self.expand(candidates, depth);
    self.visiting[ordinal] = false;
    self.visited[ordinal] = depth;
    forest
  }

  /// The retainers worth following.
  ///
  /// A weak edge holds nothing, and an unreachable retainer is no route
  /// to a root. The rest are kept only if a root is still within reach:
  /// a retainer `d` from a root needs exactly `d - 2` more edges, so one
  /// that cannot fit in the remaining depth is dropped here rather than
  /// recursed into and abandoned.
  fn candidates(&mut self, ordinal: usize, depth: usize) -> Vec<Candidate> {
    let analysis = self.analysis;
    let mut candidates = Vec::new();
    for slot in analysis.first_retainer_index[ordinal]..analysis.first_retainer_index[ordinal + 1] {
      let edge = analysis.retaining_edges[slot];
      if analysis.snapshot.edge_type(edge) == analysis.snapshot.edge_types.weak {
        continue;
      }
      let retainer = analysis.retaining_nodes[slot];
      let distance = analysis.distance(retainer);
      if distance < 0 {
        continue;
      }
      let remaining = i64::try_from(self.limits.depth - depth).unwrap_or(i64::MAX);
      if distance - ROOT_DISTANCE < remaining {
        candidates.push(Candidate {
          slot,
          distance,
          ordinal: retainer,
        });
      } else {
        self.reached.depth = true;
      }
    }
    // Nearest to a root first, ties in the order the heap holds them.
    candidates.sort_by_key(|candidate| candidate.distance);
    if candidates.len() > self.limits.siblings {
      self.reached.siblings = true;
      candidates.truncate(self.limits.siblings);
    }
    candidates
  }

  fn expand(&mut self, candidates: Vec<Candidate>, depth: usize) -> Vec<RetainingEdge> {
    let analysis = self.analysis;
    let mut forest = Vec::new();
    for candidate in candidates {
      let children = if candidate.distance == ROOT_DISTANCE {
        // A root ends the path, and costs one node of the budget.
        self.traversed += 1;
        if self.traversed > self.limits.nodes {
          self.reached.nodes = true;
          break;
        }
        Vec::new()
      } else {
        let children = self.build(candidate.ordinal, depth + 1);
        // A retainer that reaches no root is not a retaining path.
        if children.is_empty() {
          continue;
        }
        children
      };

      let edge = analysis.retaining_edges[candidate.slot];
      forest.push(RetainingEdge {
        edge_index: edge * analysis.snapshot.edge_layout.field_count,
        edge_name: analysis.edge_display_name(edge),
        edge_type: analysis.edge_type_name(edge).to_string(),
        node_id: analysis.snapshot.node_id(candidate.ordinal),
        node_index: analysis.node_index(candidate.ordinal),
        node_name: analysis.node_name(candidate.ordinal),
        distance: candidate.distance,
        children,
      });
    }
    forest
  }
}
