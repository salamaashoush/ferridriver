//! What the graph means, once it is decoded.
//!
//! Ported from `HeapSnapshot.ts::initialize`, in that order, because
//! the order decides the numbers. `calculateShallowSizes` moves an
//! owned node's size onto its owner, and the retained-size pass reads
//! the sizes AFTER that move; doing the two the other way round changes
//! every size in the report and still produces a plausible-looking one.
//!
//! Checked against the engine rather than against this description --
//! see `tests/differential.rs`.

mod classify;
mod context;
mod graph;

use crate::format::Snapshot;
use classify::{
  InterfaceIndex, apply_interface_definitions, calculate_object_names, infer_interface_definitions, is_plain_js_object,
};
use context::calculate_native_context_attribution;
use graph::{
  Retainers, build_dominated_nodes, build_retainers, calculate_distances, calculate_flags, calculate_shallow_sizes,
  dominators_and_retained_sizes, init_essential_edges,
};

pub use classify::{Classification, InterfaceDefinition};
pub(crate) use context::{is_context_object, is_native_context};
pub(crate) use graph::node_bfs_from_root;

/// `HeapSnapshotModel.baseSystemDistance`. Anything only the system can
/// reach is pushed past every real distance so it sorts last.
pub const BASE_SYSTEM_DISTANCE: i64 = 100_000_000;

/// The sentinel `calculateDistances` fills the array with first.
pub(crate) const NO_DISTANCE: i64 = -5;

/// `nodeFlags` in `JSHeapSnapshot`, a bitfield per node.
pub(super) const FLAG_CAN_BE_QUERIED: u8 = 1;
pub(super) const FLAG_DETACHED_DOM_TREE_NODE: u8 = 2;
pub(super) const FLAG_PAGE_OBJECT: u8 = 4;

/// `nodeNativeContextAttribution`: which native context owns a node.
/// Anything else is the owning node's own ordinal.
pub const NO_NATIVE_CONTEXT: i64 = -1;
pub const SHARED_NATIVE_CONTEXT: i64 = -2;

/// The graph with everything derived from it.
#[derive(Debug)]
pub struct Analysis {
  pub snapshot: Snapshot,
  /// Per node, the bitfield above.
  pub flags: Vec<u8>,
  /// `retaining_nodes[i]` / `retaining_edges[i]` are the source node
  /// ordinal and edge ordinal of one edge pointing AT the node whose
  /// slice this is; `first_retainer_index` bounds each node's slice.
  pub retaining_nodes: Vec<usize>,
  pub retaining_edges: Vec<usize>,
  pub first_retainer_index: Vec<usize>,
  /// Which edges count when deciding what dominates what.
  pub essential_edges: Vec<bool>,
  /// The immediate dominator of each node, as an ordinal. The root
  /// dominates itself.
  pub dominators: Vec<usize>,
  /// Self size plus everything the node's dominated subtree holds.
  pub retained_sizes: Vec<u64>,
  /// Edges from the root, in the shortest walk that reaches each node.
  pub distances: Vec<i64>,
  /// Per node, the string index of the class it is grouped under.
  pub class_index: Vec<usize>,
  /// `dominated_nodes[first_dominated[o]..first_dominated[o + 1]]` are
  /// the ordinals `o` immediately dominates. The dominator tree read
  /// downwards; [`Analysis::dominators`] reads it upwards.
  pub dominated_nodes: Vec<usize>,
  pub first_dominated: Vec<usize>,
  /// The plain-object shapes this snapshot named for itself. A diff
  /// against another snapshot re-classifies one side under the other's.
  pub interfaces: Vec<InterfaceDefinition>,
  /// Per node, the ordinal of the native context that owns it, or one
  /// of [`NO_NATIVE_CONTEXT`] / [`SHARED_NATIVE_CONTEXT`]. A page with
  /// an iframe has more than one, and telling their memory apart is the
  /// question this answers.
  pub native_context: Vec<i64>,
  /// The native contexts themselves, in node order.
  pub native_context_ordinals: Vec<usize>,
}

impl Analysis {
  /// Run the whole pipeline over a parsed snapshot.
  #[must_use]
  pub fn new(mut snapshot: Snapshot) -> Self {
    let node_count = snapshot.node_count;
    let root = 0usize;

    let flags = calculate_flags(&snapshot);
    let (retaining_nodes, retaining_edges, first_retainer_index) = build_retainers(&snapshot);
    // Essential edges are decided BEFORE shallow sizes move, matching
    // `startInitStep2InSecondThread` running ahead of
    // `calculateShallowSizes`. Neither reads the other, but keeping the
    // order means a future change to either cannot silently reorder.
    let essential_edges = init_essential_edges(&snapshot, &flags);

    calculate_shallow_sizes(&mut snapshot);

    let self_sizes: Vec<u64> = (0..node_count)
      .map(|ordinal| snapshot.node_self_size(ordinal))
      .collect();
    let (dominators, retained_sizes) = dominators_and_retained_sizes(
      &snapshot,
      root,
      &essential_edges,
      &Retainers {
        nodes: &retaining_nodes,
        edges: &retaining_edges,
        first: &first_retainer_index,
      },
      &self_sizes,
    );
    let distances = calculate_distances(&snapshot, root);
    let mut class_index = calculate_object_names(&mut snapshot);
    let interfaces = infer_interface_definitions(&snapshot);
    apply_interface_definitions(&mut snapshot, &mut class_index, &interfaces);
    let (dominated_nodes, first_dominated) = build_dominated_nodes(root, &dominators);
    // After the naming, and after retained sizes, which is where
    // `initialize` puts it: the sizes it reports are read from both.
    let (native_context, native_context_ordinals) = calculate_native_context_attribution(&snapshot);

    Self {
      snapshot,
      flags,
      retaining_nodes,
      retaining_edges,
      first_retainer_index,
      essential_edges,
      dominators,
      retained_sizes,
      distances,
      class_index,
      dominated_nodes,
      first_dominated,
      interfaces,
      native_context,
      native_context_ordinals,
    }
  }

  /// The name this node is grouped under, which is not its own name: a
  /// `<div id="a">` is grouped as `<div>`, and everything hidden as
  /// `(system)`.
  #[must_use]
  pub fn class_name(&self, ordinal: usize) -> &str {
    self
      .snapshot
      .strings
      .get(self.class_index[ordinal])
      .map_or("", String::as_str)
  }

  /// Re-file every plain object under ANOTHER snapshot's interface
  /// definitions, without disturbing this one's own classification.
  ///
  /// A diff needs both sides grouped by the same names. Each snapshot
  /// infers its shapes from the objects it happens to hold, so the same
  /// class can be `{id, name}` in one and `Object` in the other, and
  /// comparing those keys reports a class wholly added and a class
  /// wholly removed where nothing changed.
  #[must_use]
  pub fn classify_under(&self, definitions: &[InterfaceDefinition]) -> Classification {
    let index = InterfaceIndex::build(definitions);
    let mut class_index = self.class_index.clone();
    let mut extra: Vec<String> = Vec::new();
    let mut interned: rustc_hash::FxHashMap<String, usize> = rustc_hash::FxHashMap::default();
    for (ordinal, slot) in class_index.iter_mut().enumerate() {
      if !is_plain_js_object(&self.snapshot, ordinal) {
        continue;
      }
      // No match means the object's own name index, not the one this
      // snapshot's own definitions gave it.
      *slot = match index.best_match(&self.snapshot, ordinal) {
        None => self.snapshot.raw_node_name_index(ordinal),
        Some(best) => *interned.entry(best.name.clone()).or_insert_with(|| {
          extra.push(best.name);
          self.snapshot.strings.len() + extra.len() - 1
        }),
      };
    }
    Classification { class_index, extra }
  }

  /// The name at a class index, which may name one of the strings a
  /// [`Classification`] added rather than one the snapshot carries.
  #[must_use]
  pub fn class_name_at<'a>(&'a self, at: usize, extra: &'a [String]) -> &'a str {
    if at < self.snapshot.strings.len() {
      return &self.snapshot.strings[at];
    }
    extra.get(at - self.snapshot.strings.len()).map_or("", String::as_str)
  }

  /// Everything the node's dominated subtree holds, itself included.
  #[must_use]
  pub fn retained_size(&self, ordinal: usize) -> u64 {
    self.retained_sizes[ordinal]
  }

  /// Edges from a user root, or a number past every real distance for
  /// what only the system reaches.
  #[must_use]
  pub fn distance(&self, ordinal: usize) -> i64 {
    self.distances[ordinal]
  }

  #[must_use]
  pub fn can_be_queried(&self, ordinal: usize) -> bool {
    self.flags[ordinal] & FLAG_CAN_BE_QUERIED != 0
  }

  #[must_use]
  pub fn is_detached_dom_tree_node(&self, ordinal: usize) -> bool {
    self.flags[ordinal] & FLAG_DETACHED_DOM_TREE_NODE != 0
  }

  #[must_use]
  pub fn is_page_object(&self, ordinal: usize) -> bool {
    self.flags[ordinal] & FLAG_PAGE_OBJECT != 0
  }

  /// The name `DevTools` shows, which is the raw name only for the
  /// nodes that need no help.
  ///
  /// Two do. A concatenated string is stored as a tree of pieces and has
  /// no name of its own, so it is assembled by walking it. A plain
  /// `Object` is named for every other `Object` too, which tells a
  /// reader nothing, so it is named after the properties it carries.
  #[must_use]
  pub fn node_name(&self, ordinal: usize) -> String {
    let snapshot = &self.snapshot;
    let kind = snapshot.node_type(ordinal);
    if kind == snapshot.node_types.cons_string {
      return self.cons_string_name(ordinal);
    }
    if kind == snapshot.node_types.object && snapshot.raw_node_name(ordinal) == "Object" {
      return self.plain_object_name(ordinal);
    }
    snapshot.raw_node_name(ordinal).to_string()
  }

  /// Assemble a concatenated string from its pieces.
  ///
  /// V8 stores `a + b` as a node with `first` and `second` internal
  /// edges rather than as text. Pushing second before first pops them
  /// in reading order; upstream stops at 1024 characters and so does
  /// this, since the result is a label rather than the value.
  fn cons_string_name(&self, ordinal: usize) -> String {
    let snapshot = &self.snapshot;
    let mut name = String::new();
    let mut stack = vec![ordinal];

    while let Some(current) = stack.pop() {
      if name.chars().count() >= 1024 {
        break;
      }
      if snapshot.node_type(current) != snapshot.node_types.cons_string {
        name.push_str(snapshot.raw_node_name(current));
        continue;
      }
      let (mut first, mut second) = (None, None);
      for edge in snapshot.first_edge_index[current]..snapshot.first_edge_index[current + 1] {
        if first.is_some() && second.is_some() {
          break;
        }
        if snapshot.edge_type(edge) != snapshot.edge_types.internal {
          continue;
        }
        match snapshot.edge_name(edge) {
          Some("first") => first = snapshot.edge_target(edge).ok(),
          Some("second") => second = snapshot.edge_target(edge).ok(),
          _ => {},
        }
      }
      // A missing half is an ordinal of 0 upstream, which is the root;
      // leaving it out says the same thing without inventing text.
      if let Some(second) = second {
        stack.push(second);
      }
      if let Some(first) = first {
        stack.push(first);
      }
    }
    name
  }

  /// `{first, second, …, secondToLast, last}`.
  ///
  /// Properties are taken alternately from each end so the label shows
  /// both what an object starts and ends with, and the budget stops it
  /// growing past what fits in a panel.
  fn plain_object_name(&self, ordinal: usize) -> String {
    let snapshot = &self.snapshot;
    let mut start = String::from("{");
    let mut end = String::from("}");
    // Signed, because the end cursor is allowed to cross the start one:
    // that crossing is exactly how upstream knows nothing was left out,
    // and clamping it invents an ellipsis for an object that fitted.
    let mut from_start = snapshot.first_edge_index[ordinal].cast_signed();
    let mut from_end = snapshot.first_edge_index[ordinal + 1].cast_signed() - 1;
    let mut take_from_end = false;

    while from_start <= from_end {
      let edge = if take_from_end { from_end } else { from_start };
      let Ok(edge) = usize::try_from(edge) else {
        break;
      };
      let name = snapshot.edge_name(edge);
      // `__proto__` is on every object and says nothing about this one.
      if snapshot.edge_type(edge) != snapshot.edge_types.property || name == Some("__proto__") {
        if take_from_end {
          from_end -= 1;
        } else {
          from_start += 1;
        }
        continue;
      }
      let formatted = format_property_name(name.unwrap_or_default());

      // The first property goes in whatever its length; past that the
      // label has to stay short enough to read.
      if start.len() > 1 && start.len() + end.len() + formatted.len() > 100 {
        break;
      }

      if take_from_end {
        from_end -= 1;
        if end.len() > 1 {
          end.insert_str(0, ", ");
        }
        end.insert_str(0, &formatted);
      } else {
        from_start += 1;
        if start.len() > 1 {
          start.push_str(", ");
        }
        start.push_str(&formatted);
      }
      take_from_end = !take_from_end;
    }

    if from_start <= from_end {
      start.push_str(", …");
    }
    if end.len() > 1 {
      start.push_str(", ");
    }
    start.push_str(&end);
    start
  }

  /// The whole heap, which is what the root retains plus whatever V8
  /// accounted for outside the graph it walked.
  #[must_use]
  pub fn total_size(&self) -> u64 {
    self.retained_sizes[0] + self.snapshot.extra_native_bytes
  }

  /// Where the heap went, in the breakdown `get_heapsnapshot_summary`
  /// reports.
  ///
  /// Ported from `JSHeapSnapshot::calculateStatistics`. The order of
  /// the branches is the order upstream tests them, and it matters:
  /// hidden nodes are counted as system and skipped before anything
  /// else looks at them, so a hidden node that is also a string never
  /// reaches the string total.
  #[must_use]
  pub fn statistics(&self) -> Statistics {
    let snapshot = &self.snapshot;
    let types = &snapshot.node_types;
    let mut native = snapshot.extra_native_bytes;
    let (mut typed_arrays, mut code, mut strings, mut js_arrays, mut system) = (0u64, 0u64, 0u64, 0u64, 0u64);

    for ordinal in 0..snapshot.node_count {
      let size = snapshot.node_self_size(ordinal);
      let kind = snapshot.node_type(ordinal);
      if kind == types.hidden {
        system += size;
        continue;
      }
      if kind == types.native {
        native += size;
        if snapshot.raw_node_name(ordinal) == "system / JSArrayBufferData" {
          typed_arrays += size;
        }
      } else if kind == types.code {
        code += size;
      } else if kind == types.cons_string || kind == types.sliced_string || kind == types.string {
        strings += size;
      } else if snapshot.raw_node_name(ordinal) == "Array" {
        js_arrays += self.array_size(ordinal);
      }
    }

    let total = self.total_size();
    Statistics {
      total,
      native: NativeStatistics {
        total: native,
        typed_arrays,
      },
      v8heap: V8Statistics {
        total: total.saturating_sub(native),
        code,
        js_arrays,
        strings,
        system,
      },
    }
  }

  /// A JS array plus the backing store it alone owns.
  ///
  /// The elements live in a separate node, and counting it with the
  /// array is only right where nothing else points at it -- otherwise
  /// the same bytes would be counted under every array sharing it.
  fn array_size(&self, ordinal: usize) -> u64 {
    let snapshot = &self.snapshot;
    let mut size = snapshot.node_self_size(ordinal);
    for edge in snapshot.first_edge_index[ordinal]..snapshot.first_edge_index[ordinal + 1] {
      if snapshot.edge_type(edge) != snapshot.edge_types.internal {
        continue;
      }
      if snapshot.edge_name(edge) != Some("elements") {
        continue;
      }
      let Ok(elements) = snapshot.edge_target(edge) else {
        break;
      };
      let retainers = self.first_retainer_index[elements + 1] - self.first_retainer_index[elements];
      if retainers == 1 {
        size += snapshot.node_self_size(elements);
      }
      break;
    }
    size
  }
}

/// The heap broken down the way `get_heapsnapshot_summary` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Statistics {
  pub total: u64,
  pub native: NativeStatistics,
  pub v8heap: V8Statistics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeStatistics {
  pub total: u64,
  pub typed_arrays: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct V8Statistics {
  pub total: u64,
  pub code: u64,
  pub js_arrays: u64,
  pub strings: u64,
  pub system: u64,
}

/// A property name that could be mistaken for the punctuation around it
/// is quoted, exactly as `JSON.stringify` would quote it.
pub(super) fn format_property_name(name: &str) -> String {
  if name.contains([',', '\'', '"', '{', '}']) {
    return serde_json::to_string(name).unwrap_or_else(|_| name.to_string());
  }
  name.to_string()
}

// ── Predicates ──────────────────────────────────────────────────────────

pub(super) fn is_synthetic(snapshot: &Snapshot, ordinal: usize) -> bool {
  snapshot.node_type(ordinal) == snapshot.node_types.synthetic
}

/// The synthetic node every DOM tree hangs off, which counts as a user
/// root even though it is synthetic.
pub(super) fn is_document_dom_trees_root(snapshot: &Snapshot, ordinal: usize) -> bool {
  is_synthetic(snapshot, ordinal) && snapshot.raw_node_name(ordinal) == "(Document DOM trees)"
}

/// `JSHeapSnapshot::isUserRoot`: anything the page owns rather than the
/// system, which is where distances are measured from.
pub(super) fn is_user_root(snapshot: &Snapshot, ordinal: usize) -> bool {
  !is_synthetic(snapshot, ordinal) || is_document_dom_trees_root(snapshot, ordinal)
}

/// The edge name V8 writes for an ephemeron, split into the part that
/// identifies the pair and the table's object id. It reads
/// `12345 / part of key (... @1) -> value (... @2) pair in WeakMap
/// (table @678)`.
///
/// Two passes need it and for opposite reasons: the dominator pass
/// drops the table's edge so the value is dominated by the key, and the
/// distance pass drops whichever of the pair it meets first so the
/// value's distance comes from the later, larger one.
pub(super) fn parse_weak_map_edge_name(name: &str) -> Option<(&str, &str)> {
  // Upstream's `^\d+(?<duplicatedPart> \/ part of key \(.*? @\d+\) ->
  // value \(.*? @\d+\) pair in WeakMap \(table @(?<tableId>\d+)\))$`.
  // The captured part starts at the SPACE after the leading digits, and
  // it is what identifies the pair: both edges of one ephemeron carry
  // the same text there.
  let digits = name.len() - name.trim_start_matches(|c: char| c.is_ascii_digit()).len();
  if digits == 0 {
    return None;
  }
  let duplicated = name.get(digits..)?;
  if !duplicated.starts_with(" / part of key (") {
    return None;
  }
  let table = duplicated
    .rsplit_once("pair in WeakMap (table @")?
    .1
    .strip_suffix(')')?;
  if table.is_empty() || !table.bytes().all(|b| b.is_ascii_digit()) {
    return None;
  }
  Some((duplicated, table))
}

#[cfg(test)]
mod tests {
  use super::parse_weak_map_edge_name;

  const EPHEMERON: &str = "1 / part of key (Key @27) -> value (Value @29) pair in WeakMap (table @25)";

  #[test]
  fn an_ephemeron_edge_name_splits_into_its_pair_and_its_table() {
    let (pair, table) = parse_weak_map_edge_name(EPHEMERON).expect("an ephemeron name");
    assert_eq!(table, "25");
    // Both edges of one pair carry the same text after the count, which
    // is what lets the distance walk match them up.
    assert_eq!(
      pair,
      " / part of key (Key @27) -> value (Value @29) pair in WeakMap (table @25)"
    );
  }

  #[test]
  fn an_ordinary_edge_name_is_not_an_ephemeron() {
    for name in [
      "elements",
      "",
      "1 / part of key (Key @27)",
      // No leading count.
      "/ part of key (K @1) -> value (V @2) pair in WeakMap (table @3)",
      // A table id that is not a number.
      "1 / part of key (K @1) -> value (V @2) pair in WeakMap (table @x)",
    ] {
      assert_eq!(parse_weak_map_edge_name(name), None, "{name:?} is not an ephemeron");
    }
  }
}
