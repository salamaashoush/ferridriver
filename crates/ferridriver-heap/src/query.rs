//! The questions the heap-snapshot tools ask.
//!
//! Each is a read over the graph [`crate::Analysis`] already built, in
//! the shape `chrome-devtools-mcp` serialises it: it drives the
//! `DevTools`
//! own providers (`createRetainingEdgesProvider`,
//! `createEdgesProvider`, `getDominatorsOf`, `getObjectInfo`) and
//! reports what they return, so these mirror those fields rather than
//! inventing a shape.
//!
//! Compared against the engine node by node -- see
//! `tests/differential.rs`.

use serde::{Deserialize, Serialize};

use crate::analysis::Analysis;

/// A node as the providers embed it in their results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeSummary {
  pub id: u64,
  pub name: String,
  pub distance: i64,
  /// The node's offset in the flat array, which is the address every
  /// `DevTools` API takes. `ordinal * node_field_count`.
  pub node_index: usize,
  pub retained_size: u64,
  pub self_size: u64,
  #[serde(rename = "type")]
  pub kind: String,
  pub can_be_queried: bool,
  /// Upstream spells this `detachedDOMTreeNode`, which no rename rule
  /// produces, so it is named outright.
  #[serde(rename = "detachedDOMTreeNode")]
  pub detached_dom_tree_node: bool,
}

/// One edge, with the node it points at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeSummary {
  pub name: String,
  pub node: NodeSummary,
  #[serde(rename = "type")]
  pub kind: String,
  pub edge_index: usize,
}

/// Everything `get_heapsnapshot_object_details` reports about one node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectInfo {
  pub id: u64,
  pub name: String,
  #[serde(rename = "type")]
  pub kind: String,
  pub node_index: usize,
  pub detachedness: u64,
  pub self_size: u64,
  pub retained_size: u64,
  pub distance: i64,
  pub edge_count: usize,
  pub retainer_count: usize,
}

/// One step of the chain `get_heapsnapshot_dominators` walks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DominatorStep {
  pub node_id: u64,
  pub node_index: usize,
  pub node_name: String,
  pub retained_size: u64,
  pub self_size: u64,
}

/// Which end of an edge a result describes. A containment edge is read
/// toward what it points at; a retaining edge, toward what points.
enum EdgeEnd {
  Target,
  Source(usize),
}

impl Analysis {
  /// The flat-array offset for an ordinal, which is what the `DevTools`
  /// APIs call a node index.
  #[must_use]
  pub fn node_index(&self, ordinal: usize) -> usize {
    ordinal * self.snapshot.node_layout.field_count
  }

  /// Find a node by the id V8 gave it.
  ///
  /// A linear scan, as upstream's `nodeIndexForId` is: ids are not
  /// ordered, so there is nothing to bisect.
  #[must_use]
  pub fn ordinal_for_id(&self, id: u64) -> Option<usize> {
    (0..self.snapshot.node_count).find(|&ordinal| self.snapshot.node_id(ordinal) == id)
  }

  /// How an edge names itself.
  ///
  /// `element` and `hidden` edges carry an index rather than a string.
  /// A shortcut carries a string that upstream renders as a number when
  /// it parses as one, which normalises `007` to `7`.
  #[must_use]
  pub fn edge_display_name(&self, edge: usize) -> String {
    let snapshot = &self.snapshot;
    let kind = snapshot.edge_type(edge);
    let raw = snapshot.edge_name_or_index(edge);
    let has_string_name = kind != snapshot.edge_types.element && kind != snapshot.edge_types.hidden;
    let name = if has_string_name {
      snapshot
        .strings
        .get(usize::try_from(raw).unwrap_or(usize::MAX))
        .cloned()
        .unwrap_or_default()
    } else {
      raw.to_string()
    };
    if kind == snapshot.edge_types.shortcut
      && let Ok(number) = name.parse::<i64>()
    {
      return number.to_string();
    }
    name
  }

  #[must_use]
  pub fn edge_type_name(&self, edge: usize) -> &str {
    let raw = usize::try_from(self.snapshot.edge_type(edge)).unwrap_or(usize::MAX);
    self.snapshot.edge_type_names.get(raw).map_or("", String::as_str)
  }

  #[must_use]
  pub fn node_summary(&self, ordinal: usize) -> NodeSummary {
    NodeSummary {
      id: self.snapshot.node_id(ordinal),
      name: self.node_name(ordinal),
      distance: self.distance(ordinal),
      node_index: self.node_index(ordinal),
      retained_size: self.retained_size(ordinal),
      self_size: self.snapshot.node_self_size(ordinal),
      kind: self.snapshot.node_type_name(ordinal).to_string(),
      can_be_queried: self.can_be_queried(ordinal),
      detached_dom_tree_node: self.is_detached_dom_tree_node(ordinal),
    }
  }

  #[must_use]
  pub fn object_info(&self, ordinal: usize) -> ObjectInfo {
    ObjectInfo {
      id: self.snapshot.node_id(ordinal),
      name: self.node_name(ordinal),
      kind: self.snapshot.node_type_name(ordinal).to_string(),
      node_index: self.node_index(ordinal),
      detachedness: self.snapshot.node_detachedness(ordinal),
      self_size: self.snapshot.node_self_size(ordinal),
      retained_size: self.retained_size(ordinal),
      distance: self.distance(ordinal),
      edge_count: self.snapshot.node_edge_count(ordinal),
      retainer_count: self.first_retainer_index[ordinal + 1] - self.first_retainer_index[ordinal],
    }
  }

  /// What this node points at.
  ///
  /// Filtered by `JSHeapSnapshot::containmentEdgesFilter`, which drops
  /// invisible edges. Chrome does not write that type today, so the
  /// filter is a no-op on a current snapshot and is here because the
  /// absence of a type is not a promise about the next version.
  #[must_use]
  pub fn edges_of(&self, ordinal: usize) -> Vec<EdgeSummary> {
    (self.snapshot.first_edge_index[ordinal]..self.snapshot.first_edge_index[ordinal + 1])
      .filter(|&edge| self.is_visible_edge(edge))
      .filter_map(|edge| self.edge_summary(edge, &EdgeEnd::Target))
      .collect()
  }

  /// What points at this node.
  ///
  /// The node embedded in each result is the one doing the retaining,
  /// not the one being retained: a retaining edge is read from the
  /// other end.
  ///
  /// `JSHeapSnapshot::retainingEdgesFilter` drops three kinds. The root
  /// is not a useful answer to "what holds this", and a weak edge holds
  /// nothing at all, so a node whose only retainer is either looks
  /// unretained here -- which is what it is.
  #[must_use]
  pub fn retainers_of(&self, ordinal: usize) -> Vec<EdgeSummary> {
    (self.first_retainer_index[ordinal]..self.first_retainer_index[ordinal + 1])
      .filter(|&slot| {
        let edge = self.retaining_edges[slot];
        self.is_visible_edge(edge)
          && self.retaining_nodes[slot] != 0
          && self.snapshot.edge_type(edge) != self.snapshot.edge_types.weak
      })
      .filter_map(|slot| self.edge_summary(self.retaining_edges[slot], &EdgeEnd::Source(self.retaining_nodes[slot])))
      .collect()
  }

  fn is_visible_edge(&self, edge: usize) -> bool {
    self.snapshot.edge_type(edge) != self.snapshot.edge_types.invisible
  }

  fn edge_summary(&self, edge: usize, end: &EdgeEnd) -> Option<EdgeSummary> {
    let ordinal = match *end {
      EdgeEnd::Target => self.snapshot.edge_target(edge).ok()?,
      EdgeEnd::Source(ordinal) => ordinal,
    };
    Some(EdgeSummary {
      name: self.edge_display_name(edge),
      node: self.node_summary(ordinal),
      kind: self.edge_type_name(edge).to_string(),
      edge_index: edge * self.snapshot.edge_layout.field_count,
    })
  }

  /// The chain of dominators from a node up to the root, the node
  /// itself first and the root last.
  ///
  /// Every step is something that, if it let go, would free the one
  /// before it, which is the question `get_heapsnapshot_dominators`
  /// exists to answer.
  #[must_use]
  pub fn dominator_chain(&self, ordinal: usize) -> Vec<DominatorStep> {
    let mut chain = vec![self.dominator_step(ordinal)];
    let mut current = ordinal;
    // The root dominates itself, which is where this stops.
    while self.dominators[current] != current {
      current = self.dominators[current];
      chain.push(self.dominator_step(current));
    }
    chain
  }

  fn dominator_step(&self, ordinal: usize) -> DominatorStep {
    DominatorStep {
      node_id: self.snapshot.node_id(ordinal),
      node_index: self.node_index(ordinal),
      node_name: self.node_name(ordinal),
      retained_size: self.retained_size(ordinal),
      self_size: self.snapshot.node_self_size(ordinal),
    }
  }
}
