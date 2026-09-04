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

use crate::format::Snapshot;

/// `HeapSnapshotModel.baseSystemDistance`. Anything only the system can
/// reach is pushed past every real distance so it sorts last.
pub const BASE_SYSTEM_DISTANCE: i64 = 100_000_000;

/// The sentinel `calculateDistances` fills the array with first.
const NO_DISTANCE: i64 = -5;

/// `nodeFlags` in `JSHeapSnapshot`, a bitfield per node.
const FLAG_CAN_BE_QUERIED: u8 = 1;
const FLAG_DETACHED_DOM_TREE_NODE: u8 = 2;
const FLAG_PAGE_OBJECT: u8 = 4;

/// Sentinels for the owner array in [`Analysis::calculate_shallow_sizes`].
const UNVISITED: u32 = u32::MAX;
const MULTIPLE_OWNERS: u32 = u32::MAX - 1;

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
fn format_property_name(name: &str) -> String {
  if name.contains([',', '\'', '"', '{', '}']) {
    return serde_json::to_string(name).unwrap_or_else(|_| name.to_string());
  }
  name.to_string()
}

// ── Predicates ──────────────────────────────────────────────────────────

fn is_synthetic(snapshot: &Snapshot, ordinal: usize) -> bool {
  snapshot.node_type(ordinal) == snapshot.node_types.synthetic
}

/// The synthetic node every DOM tree hangs off, which counts as a user
/// root even though it is synthetic.
fn is_document_dom_trees_root(snapshot: &Snapshot, ordinal: usize) -> bool {
  is_synthetic(snapshot, ordinal) && snapshot.raw_node_name(ordinal) == "(Document DOM trees)"
}

/// `JSHeapSnapshot::isUserRoot`: anything the page owns rather than the
/// system, which is where distances are measured from.
fn is_user_root(snapshot: &Snapshot, ordinal: usize) -> bool {
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
fn parse_weak_map_edge_name(name: &str) -> Option<(&str, &str)> {
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

// ── Flags ───────────────────────────────────────────────────────────────

/// `JSHeapSnapshot::calculateFlags`, all three markers.
fn calculate_flags(snapshot: &Snapshot) -> Vec<u8> {
  let mut flags = vec![0u8; snapshot.node_count];
  mark_detached_dom_tree_nodes(snapshot, &mut flags);
  mark_queriable_heap_objects(snapshot, &mut flags);
  mark_page_owned_nodes(snapshot, &mut flags);
  flags
}

fn mark_detached_dom_tree_nodes(snapshot: &Snapshot, flags: &mut [u8]) {
  for (ordinal, flag) in flags.iter_mut().enumerate().take(snapshot.node_count) {
    if snapshot.node_type(ordinal) != snapshot.node_types.native {
      continue;
    }
    if snapshot.node_detachedness(ordinal) == u64::from(crate::format::DOM_LINK_STATE_DETACHED) {
      *flag |= FLAG_DETACHED_DOM_TREE_NODE;
    }
  }
}

/// Objects reachable from a user root by ordinary references. Asking
/// V8 about anything else can crash it, because a wrapper's internal
/// state may be inconsistent.
fn mark_queriable_heap_objects(snapshot: &Snapshot, flags: &mut [u8]) {
  let mut list: Vec<usize> = Vec::new();
  for edge in snapshot.first_edge_index[0]..snapshot.first_edge_index[1] {
    // The NODE's own `isUserRoot`, which is only "not synthetic".
    // `(Document DOM trees)` passes the snapshot-level test and fails
    // this one, and seeding from the wrong one marks the whole DOM as
    // queriable when `DevTools` says it is not.
    if let Ok(child) = snapshot.edge_target(edge)
      && !is_synthetic(snapshot, child)
    {
      list.push(child);
    }
  }
  while let Some(ordinal) = list.pop() {
    if flags[ordinal] & FLAG_CAN_BE_QUERIED != 0 {
      continue;
    }
    flags[ordinal] |= FLAG_CAN_BE_QUERIED;
    for edge in snapshot.first_edge_index[ordinal]..snapshot.first_edge_index[ordinal + 1] {
      let Ok(child) = snapshot.edge_target(edge) else {
        continue;
      };
      if flags[child] & FLAG_CAN_BE_QUERIED != 0 {
        continue;
      }
      let kind = snapshot.edge_type(edge);
      if kind == snapshot.edge_types.hidden
        || kind == snapshot.edge_types.invisible
        || kind == snapshot.edge_types.internal
        || kind == snapshot.edge_types.weak
      {
        continue;
      }
      list.push(child);
    }
  }
}

/// Everything reachable from a Window or a DOM tree root, which is what
/// separates the page's own objects from the debugger's.
fn mark_page_owned_nodes(snapshot: &Snapshot, flags: &mut [u8]) {
  let mut to_visit: Vec<usize> = Vec::new();
  for edge in snapshot.first_edge_index[0]..snapshot.first_edge_index[1] {
    let kind = snapshot.edge_type(edge);
    let Ok(child) = snapshot.edge_target(edge) else {
      continue;
    };
    if kind == snapshot.edge_types.element {
      if !is_document_dom_trees_root(snapshot, child) {
        continue;
      }
    } else if kind != snapshot.edge_types.shortcut {
      continue;
    }
    to_visit.push(child);
    flags[child] |= FLAG_PAGE_OBJECT;
  }

  while let Some(ordinal) = to_visit.pop() {
    for edge in snapshot.first_edge_index[ordinal]..snapshot.first_edge_index[ordinal + 1] {
      let Ok(child) = snapshot.edge_target(edge) else {
        continue;
      };
      if flags[child] & FLAG_PAGE_OBJECT != 0 {
        continue;
      }
      if snapshot.edge_type(edge) == snapshot.edge_types.weak {
        continue;
      }
      to_visit.push(child);
      flags[child] |= FLAG_PAGE_OBJECT;
    }
  }
}

/// Group every node under a class, as `calculateObjectNames` does.
///
/// The grouping name is not the node's name. An element carrying
/// attributes is grouped by its tag alone, so `<div id="a">` and
/// `<div class="b">` land together; everything hidden is `(system)`,
/// compiled code is `(compiled code)`, and a function is `Function`
/// whatever it is called.
///
/// New names are appended to the string table, and objects and natives
/// deliberately reuse their existing name index rather than adding one,
/// because two nodes only group together when their class index is the
/// same number.
fn calculate_object_names(snapshot: &mut Snapshot) -> Vec<usize> {
  let mut interned: rustc_hash::FxHashMap<String, usize> = rustc_hash::FxHashMap::default();
  let mut intern = |snapshot: &mut Snapshot, value: &str| -> usize {
    if let Some(&at) = interned.get(value) {
      return at;
    }
    snapshot.strings.push(value.to_string());
    let at = snapshot.strings.len() - 1;
    interned.insert(value.to_string(), at);
    at
  };

  let hidden_class = intern(snapshot, "(system)");
  let code_class = intern(snapshot, "(compiled code)");
  let function_class = intern(snapshot, "Function");
  let regexp_class = intern(snapshot, "RegExp");
  let regexp_type = snapshot
    .node_type_names
    .iter()
    .position(|name| name == "regexp")
    .map_or(u64::MAX, |at| at as u64);

  let mut class_index = vec![0usize; snapshot.node_count];
  for (ordinal, slot) in class_index.iter_mut().enumerate() {
    let kind = snapshot.node_type(ordinal);
    let types = snapshot.node_types;
    *slot = if kind == types.hidden {
      hidden_class
    } else if kind == types.code {
      code_class
    } else if kind == types.closure {
      function_class
    } else if kind == regexp_type {
      regexp_class
    } else if kind == types.object || kind == types.native {
      let name = snapshot.raw_node_name(ordinal).to_string();
      // `<div id="a">` groups as `<div>`. The prefixed form is what a
      // detached node was renamed to earlier.
      if let Some(tag) = element_tag(&name) {
        intern(snapshot, &tag)
      } else {
        // The name's own index, NOT a fresh one: an interned copy would
        // put identically named objects in different groups.
        snapshot.raw_node_name_index(ordinal)
      }
    } else {
      let label = format!("({})", snapshot.node_type_name(ordinal));
      intern(snapshot, &label)
    };
  }
  class_index
}

/// `<div id="a">` -> `<div>`, and the same for a detached one.
fn element_tag(name: &str) -> Option<String> {
  for prefix in ["<", "Detached <"] {
    if !name.starts_with(prefix) {
      continue;
    }
    let Some(space) = name[prefix.len() - 1..].find(' ').map(|at| at + prefix.len() - 1) else {
      return Some(name.to_string());
    };
    return Some(format!("{}>", &name[..space]));
  }
  None
}

/// After this many characters an inferred interface name stops taking
/// properties, unless it has none yet.
const MAX_INTERFACE_NAME_LENGTH: usize = 120;
/// An interface matching one object is not a category.
const MIN_OBJECT_COUNT_PER_INTERFACE: usize = 2;
/// And one matching fewer than a thousandth of the plain objects is a
/// long tail nobody reads.
const MIN_OBJECT_PROPORTION_PER_INTERFACE: usize = 1000;

/// One shape that enough plain objects share to be worth naming.
///
/// Public because a snapshot diff has to classify the BASE snapshot
/// under the CURRENT one's definitions: two snapshots infer their own
/// shapes independently, and a class named `{a, b}` in one and `Object`
/// in the other would compare as a whole class added and a whole class
/// removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceDefinition {
  pub(crate) name: String,
  pub(crate) properties: Vec<String>,
}

/// One snapshot's nodes grouped under a foreign set of interface
/// definitions.
///
/// Indexes below the snapshot's own string count name a string it
/// already holds; the rest name one of `extra`, so classifying under
/// someone else's definitions never rewrites the string table.
#[derive(Debug, Clone)]
pub struct Classification {
  pub class_index: Vec<usize>,
  pub extra: Vec<String>,
}

/// Only a plain `Object` is reshaped; everything else already has a
/// name worth grouping by.
fn is_plain_js_object(snapshot: &Snapshot, ordinal: usize) -> bool {
  snapshot.node_type(ordinal) == snapshot.node_types.object && snapshot.raw_node_name(ordinal) == "Object"
}

/// Name the shapes that plain objects keep repeating.
///
/// Every plain `Object` reports as `Object`, which groups a page's
/// entire object graph into one line. Upstream instead reads each one's
/// properties IN ORDER, counts how often each sequence recurs, and
/// keeps the sequences shared by at least two objects and a thousandth
/// of them.
fn infer_interface_definitions(snapshot: &Snapshot) -> Vec<InterfaceDefinition> {
  // Insertion-ordered, because ties are broken by which was seen first.
  let mut order: Vec<String> = Vec::new();
  let mut candidates: rustc_hash::FxHashMap<String, (Vec<String>, usize)> = rustc_hash::FxHashMap::default();
  let mut total = 0usize;

  for ordinal in 0..snapshot.node_count {
    if !is_plain_js_object(snapshot, ordinal) {
      continue;
    }
    total += 1;
    let mut name = String::from("{");
    let mut properties: Vec<String> = Vec::new();
    for edge in snapshot.first_edge_index[ordinal]..snapshot.first_edge_index[ordinal + 1] {
      if snapshot.edge_type(edge) != snapshot.edge_types.property {
        continue;
      }
      let Some(raw) = snapshot.edge_name(edge) else {
        continue;
      };
      if raw == "__proto__" {
        continue;
      }
      let formatted = format_property_name(raw);
      if name.len() > 1 && name.len() + formatted.len() > MAX_INTERFACE_NAME_LENGTH {
        break;
      }
      if name.len() != 1 {
        name.push_str(", ");
      }
      name.push_str(&formatted);
      properties.push(raw.to_string());
    }
    // An object with no properties is not a shape, and reading `{}` as
    // "objects with nothing in them" would be wrong anyway.
    if properties.is_empty() {
      continue;
    }
    name.push('}');
    if let Some(entry) = candidates.get_mut(&name) {
      entry.1 += 1;
    } else {
      order.push(name.clone());
      candidates.insert(name, (properties, 1));
    }
  }

  let least = MIN_OBJECT_COUNT_PER_INTERFACE.max(total / MIN_OBJECT_PROPORTION_PER_INTERFACE);
  let mut sorted: Vec<InterfaceDefinition> = Vec::new();
  let mut counted: Vec<(String, usize)> = order
    .into_iter()
    .filter_map(|name| candidates.get(&name).map(|entry| (name, entry.1)))
    .collect();
  // Most popular first, ties in the order they were met, so the
  // commonest ordering of the same properties wins.
  counted.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
  for (name, count) in counted {
    if count < least {
      break;
    }
    let properties = candidates.get(&name).map(|entry| entry.0.clone()).unwrap_or_default();
    sorted.push(InterfaceDefinition { name, properties });
  }
  sorted
}

/// The definitions arranged so one walk over an object's sorted
/// properties visits every definition it could match.
///
/// Each edge adds one property name, so a definition is a path from the
/// root and an object matches every definition whose path its own
/// sorted properties contain.
#[derive(Debug)]
struct InterfaceIndex {
  tree: Vec<PropertyTreeNode>,
}

impl InterfaceIndex {
  fn build(definitions: &[InterfaceDefinition]) -> Self {
    let mut tree = vec![PropertyTreeNode::default()];
    for (at, definition) in definitions.iter().enumerate() {
      let mut properties = definition.properties.clone();
      properties.sort();
      let mut current = 0usize;
      for property in &properties {
        current = if let Some(node) = tree[current].next.get(property).copied() {
          node
        } else {
          tree.push(PropertyTreeNode::default());
          let node = tree.len() - 1;
          tree[current].next.insert(property.clone(), node);
          node
        };
      }
      // An earlier definition keeps the slot, which is what makes the
      // popularity order that produced them decide ties.
      if tree[current].matched.is_none() {
        tree[current].matched = Some(Match {
          name: definition.name.clone(),
          property_count: properties.len(),
          at,
        });
      }
    }
    Self { tree }
  }

  /// The best interface this object matches: the one naming the most
  /// properties, and among equals the one defined earliest.
  fn best_match(&self, snapshot: &Snapshot, ordinal: usize) -> Option<Match> {
    let mut properties: Vec<&str> = (snapshot.first_edge_index[ordinal]..snapshot.first_edge_index[ordinal + 1])
      .filter(|&edge| snapshot.edge_type(edge) == snapshot.edge_types.property)
      .filter_map(|edge| snapshot.edge_name(edge))
      .collect();
    properties.sort_unstable();

    let mut states: Vec<usize> = vec![0];
    let mut best: Option<Match> = self.tree[0].matched.clone();
    for property in properties {
      let mut next_states = Vec::new();
      for &state in &states {
        // Sorted properties mean a state whose greatest key is behind
        // us can never advance again.
        let exhausted = self.tree[state]
          .next
          .keys()
          .max()
          .is_none_or(|greatest| property >= greatest.as_str());
        if !exhausted {
          next_states.push(state);
        }
        if let Some(&node) = self.tree[state].next.get(property) {
          next_states.push(node);
          best = better_match(best, self.tree[node].matched.clone());
        }
      }
      states = next_states;
    }
    best
  }
}

/// Re-file every plain object under the best interface it matches,
/// interning the names it needs into the snapshot's string table.
///
/// An object matching nothing goes back to its own name index, which is
/// what makes a second pass over a different definition list correct
/// rather than a merge of the two.
fn apply_interface_definitions(
  snapshot: &mut Snapshot,
  class_index: &mut [usize],
  definitions: &[InterfaceDefinition],
) {
  let index = InterfaceIndex::build(definitions);
  // Matched first and applied second, because interning a new name
  // needs the string table mutably while matching still needs to read
  // the graph.
  let matched: Vec<(usize, Option<String>)> = (0..snapshot.node_count)
    .filter(|&ordinal| is_plain_js_object(snapshot, ordinal))
    .map(|ordinal| (ordinal, index.best_match(snapshot, ordinal).map(|best| best.name)))
    .collect();

  let mut interned: rustc_hash::FxHashMap<String, usize> = rustc_hash::FxHashMap::default();
  for (ordinal, name) in matched {
    class_index[ordinal] = match name {
      None => snapshot.raw_node_name_index(ordinal),
      Some(name) => *interned.entry(name.clone()).or_insert_with(|| {
        snapshot.strings.push(name);
        snapshot.strings.len() - 1
      }),
    };
  }
}

#[derive(Debug, Default)]
struct PropertyTreeNode {
  next: std::collections::BTreeMap<String, usize>,
  matched: Option<Match>,
}

#[derive(Debug, Clone)]
struct Match {
  name: String,
  property_count: usize,
  at: usize,
}

fn better_match(a: Option<Match>, b: Option<Match>) -> Option<Match> {
  match (a, b) {
    (Some(a), Some(b)) => Some(if a.property_count > b.property_count {
      a
    } else if b.property_count > a.property_count {
      b
    } else if a.at <= b.at {
      a
    } else {
      b
    }),
    (some, None) | (None, some) => some,
  }
}

/// The dominator tree read downwards: which nodes each node dominates.
///
/// A counting sort like the retainer index, filling each slice
/// backwards from a count parked in its first slot.
fn build_dominated_nodes(root: usize, dominators: &[usize]) -> (Vec<usize>, Vec<usize>) {
  let node_count = dominators.len();
  let mut first_dominated = vec![0usize; node_count + 1];
  if node_count == 0 {
    return (Vec::new(), first_dominated);
  }
  // Every node but the root has a dominator; the root dominates itself
  // and is skipped so it does not appear in its own slice.
  let mut dominated = vec![0usize; node_count - 1];
  for ordinal in 0..node_count {
    if ordinal == root {
      continue;
    }
    first_dominated[dominators[ordinal]] += 1;
  }
  let mut next_slot = 0usize;
  for slot in first_dominated.iter_mut().take(node_count) {
    let count = *slot;
    *slot = next_slot;
    if count > 0 {
      dominated[next_slot] = count;
    }
    next_slot += count;
  }
  first_dominated[node_count] = dominated.len();
  for ordinal in 0..node_count {
    if ordinal == root {
      continue;
    }
    let slice_start = first_dominated[dominators[ordinal]];
    dominated[slice_start] -= 1;
    let at = slice_start + dominated[slice_start];
    dominated[at] = ordinal;
  }
  (dominated, first_dominated)
}

// ── Retainers ───────────────────────────────────────────────────────────

/// Invert the edge list: for each node, which edges point at it.
///
/// A counting sort in place, as `HeapSnapshot.ts::buildRetainers` does
/// it: count the arrivals per node, turn the counts into slice starts,
/// then fill each slice backwards.
fn build_retainers(snapshot: &Snapshot) -> (Vec<usize>, Vec<usize>, Vec<usize>) {
  let node_count = snapshot.node_count;
  let edge_count = snapshot.edge_count;
  let mut first_retainer_index = vec![0usize; node_count + 1];

  for edge in 0..edge_count {
    if let Ok(target) = snapshot.edge_target(edge) {
      first_retainer_index[target] += 1;
    }
  }

  let mut retaining_nodes = vec![0usize; edge_count];
  let mut next_slot = 0usize;
  for start in first_retainer_index.iter_mut().take(node_count) {
    let arrivals = *start;
    *start = next_slot;
    // The slice's first slot holds its own remaining count while it
    // fills, which is what lets this run without a second array.
    retaining_nodes[next_slot] = arrivals;
    next_slot += arrivals;
  }
  first_retainer_index[node_count] = retaining_nodes.len();

  let mut retaining_edges = vec![0usize; edge_count];
  for source in 0..node_count {
    for edge in snapshot.first_edge_index[source]..snapshot.first_edge_index[source + 1] {
      let Ok(target) = snapshot.edge_target(edge) else {
        continue;
      };
      let slice_start = first_retainer_index[target];
      retaining_nodes[slice_start] -= 1;
      let slot = slice_start + retaining_nodes[slice_start];
      retaining_nodes[slot] = source;
      retaining_edges[slot] = edge;
    }
  }

  (retaining_nodes, retaining_edges, first_retainer_index)
}

// ── Essential edges ─────────────────────────────────────────────────────

/// Which edges are allowed to decide dominance.
///
/// Four exclusions, each for its own reason
/// (`computeIsEssentialEdge`): a `WeakMap` value is retained by key and
/// table together so only the key's edge counts; a weak edge retains
/// nothing at all; a self edge dominates nothing; and a shortcut from
/// the root marks a user global rather than holding it. The last one
/// keeps debugger-retained objects from changing the page's dominators.
fn init_essential_edges(snapshot: &Snapshot, flags: &[u8]) -> Vec<bool> {
  let mut essential = vec![false; snapshot.edge_count];
  for ordinal in 0..snapshot.node_count {
    for edge in snapshot.first_edge_index[ordinal]..snapshot.first_edge_index[ordinal + 1] {
      if let Some(slot) = essential.get_mut(edge) {
        *slot = is_essential_edge(snapshot, flags, ordinal, edge);
      }
    }
  }
  essential
}

fn is_essential_edge(snapshot: &Snapshot, flags: &[u8], ordinal: usize, edge: usize) -> bool {
  let kind = snapshot.edge_type(edge);

  if kind == snapshot.edge_types.internal
    && let Some(name) = snapshot.edge_name(edge)
    && let Some((_, table)) = parse_weak_map_edge_name(name)
    && table.parse::<u64>().is_ok_and(|id| id == snapshot.node_id(ordinal))
  {
    // This is the table's own edge to the value; the key's edge
    // carries the same value and produces the more useful answer.
    return false;
  }

  if kind == snapshot.edge_types.weak {
    return false;
  }

  let Ok(child) = snapshot.edge_target(edge) else {
    return false;
  };
  if ordinal == child {
    return false;
  }

  if ordinal != 0 {
    if kind == snapshot.edge_types.shortcut {
      return false;
    }
    // An edge from something the page does not own into something it
    // does would otherwise let the debugger move the page's dominators.
    let owns = flags[ordinal] & FLAG_PAGE_OBJECT != 0;
    let child_owns = flags[child] & FLAG_PAGE_OBJECT != 0;
    if child_owns && !owns {
      return false;
    }
  }

  true
}

// ── Shallow sizes ───────────────────────────────────────────────────────

/// Move the size of an owned array or hidden node onto its owner.
///
/// A JS array's elements live in a separate backing store, and
/// reporting that store as its own object tells a reader nothing they
/// can act on. Where exactly one node owns such a store, its size is
/// transferred and the store is left at zero -- which is why so many
/// nodes report a self size of zero.
///
/// Skipped entirely when the snapshot has no user roots, because that
/// means it was taken with internals exposed and the caller wants the
/// real allocations.
fn calculate_shallow_sizes(snapshot: &mut Snapshot) {
  if !has_user_roots(snapshot) {
    return;
  }
  let node_count = snapshot.node_count;
  let mut owners = vec![UNVISITED; node_count];
  let mut worklist: Vec<usize> = Vec::new();

  for (ordinal, owner) in owners.iter_mut().enumerate() {
    let kind = snapshot.node_type(ordinal);
    let owned = kind == snapshot.node_types.hidden
      || kind == snapshot.node_types.array
      || (kind == snapshot.node_types.native && snapshot.raw_node_name(ordinal) == "system / ExternalStringData");
    if owned {
      *owner = UNVISITED;
    } else {
      *owner = u32::try_from(ordinal).unwrap_or(MULTIPLE_OWNERS);
      worklist.push(ordinal);
    }
  }

  while let Some(ordinal) = worklist.pop() {
    let owner = owners[ordinal];
    for edge in snapshot.first_edge_index[ordinal]..snapshot.first_edge_index[ordinal + 1] {
      if snapshot.edge_type(edge) == snapshot.edge_types.weak {
        continue;
      }
      let Ok(target) = snapshot.edge_target(edge) else {
        continue;
      };
      let current = owners[target];
      if current == UNVISITED {
        owners[target] = owner;
        worklist.push(target);
      } else if current != owner && current != MULTIPLE_OWNERS && current as usize != target {
        // A second owner means neither can claim the size.
        owners[target] = MULTIPLE_OWNERS;
        worklist.push(target);
      }
    }
  }

  for (ordinal, &owner) in owners.iter().enumerate() {
    if owner == UNVISITED || owner == MULTIPLE_OWNERS || owner as usize == ordinal {
      continue;
    }
    let owner = owner as usize;
    // Charging a synthetic or the root tells a reader nothing.
    if is_synthetic(snapshot, owner) || owner == 0 {
      continue;
    }
    let moved = snapshot.node_self_size(ordinal);
    snapshot.set_node_self_size(ordinal, 0);
    snapshot.set_node_self_size(owner, snapshot.node_self_size(owner) + moved);
  }
}

fn has_user_roots(snapshot: &Snapshot) -> bool {
  (snapshot.first_edge_index[0]..snapshot.first_edge_index[1])
    .filter_map(|edge| snapshot.edge_target(edge).ok())
    .any(|child| is_user_root(snapshot, child))
}

// ── Dominators and retained sizes ───────────────────────────────────────

/// Lengauer-Tarjan, as `calculateDominatorsAndRetainedSizes` runs it.
///
/// "A fast algorithm for finding dominators in a flowgraph", Lengauer
/// and Tarjan 1979. The vectors are 1-indexed because the algorithm
/// uses 0 as its invalid value, and both the depth-first walk and the
/// path compression are iterative: a heap graph is deep enough to blow
/// a recursive version's stack.
struct Retainers<'a> {
  nodes: &'a [usize],
  edges: &'a [usize],
  first: &'a [usize],
}

/// The depth-first numbering the algorithm runs on, and the tree it
/// walked. Every array is 1-indexed because the algorithm uses 0 as its
/// invalid value.
struct DfsNumbering {
  parent: Vec<usize>,
  vertex: Vec<usize>,
  label: Vec<usize>,
  semi: Vec<usize>,
  /// How many nodes were numbered.
  numbered: usize,
}

/// Number every node depth-first over the essential edges, starting at
/// the root and then picking up whatever that could not reach.
///
/// Iterative: a heap graph is deep enough that the recursive form in
/// the paper overflows the stack.
fn number_depth_first(snapshot: &Snapshot, root: usize, essential: &[bool], retainers: &Retainers<'_>) -> DfsNumbering {
  let node_count = snapshot.node_count;
  let length = node_count + 1;
  let mut state = DfsNumbering {
    parent: vec![0usize; length],
    vertex: vec![0usize; length],
    label: vec![0usize; length],
    semi: vec![0usize; length],
    numbered: 0,
  };
  let mut next_edge = vec![0usize; length];

  let walk = |start: usize, state: &mut DfsNumbering, next_edge: &mut Vec<usize>| {
    next_edge[start - 1] = snapshot.first_edge_index[start - 1];
    let mut current = start;
    while current != 0 {
      if state.semi[current] == 0 {
        state.numbered += 1;
        state.semi[current] = state.numbered;
        state.vertex[state.numbered] = current;
        state.label[current] = current;
      }
      let mut next = state.parent[current];
      let ordinal = current - 1;
      while next_edge[ordinal] < snapshot.first_edge_index[ordinal + 1] {
        let edge = next_edge[ordinal];
        next_edge[ordinal] += 1;
        if !essential[edge] {
          continue;
        }
        let Ok(child) = snapshot.edge_target(edge) else {
          continue;
        };
        if state.semi[child + 1] == 0 {
          state.parent[child + 1] = current;
          next_edge[child] = snapshot.first_edge_index[child];
          next = child + 1;
          break;
        }
      }
      current = next;
    }
  };

  walk(root + 1, &mut state, &mut next_edge);

  // Nodes only weak retainers can reach are unreachable above, so they
  // get their own walk with the root as their parent.
  if state.numbered < node_count {
    for candidate in 1..=node_count {
      if state.semi[candidate] == 0 && has_only_weak_retainers(candidate - 1, essential, retainers) {
        state.parent[candidate] = root + 1;
        walk(candidate, &mut state, &mut next_edge);
      }
    }
  }
  // A clique that only retains itself is still unreachable; number
  // those individually so the main loop covers every node.
  if state.numbered < node_count {
    for candidate in 1..=node_count {
      if state.semi[candidate] == 0 {
        state.parent[candidate] = root + 1;
        state.numbered += 1;
        state.semi[candidate] = state.numbered;
        state.vertex[state.numbered] = candidate;
        state.label[candidate] = candidate;
      }
    }
  }
  state
}

fn dominators_and_retained_sizes(
  snapshot: &Snapshot,
  root: usize,
  essential: &[bool],
  retainers: &Retainers<'_>,
  self_sizes: &[u64],
) -> (Vec<usize>, Vec<u64>) {
  let node_count = snapshot.node_count;
  let length = node_count + 1;
  let numbering = number_depth_first(snapshot, root, essential, retainers);
  let DfsNumbering {
    parent,
    vertex,
    mut label,
    mut semi,
    numbered,
  } = numbering;

  let mut ancestor = vec![0usize; length];
  let mut bucket: Vec<Vec<usize>> = vec![Vec::new(); length];
  let mut dom = vec![0usize; length];
  let mut compression_stack = vec![0usize; length];
  let root_vertex = root + 1;

  // Process vertices in decreasing depth-first order, which is what
  // makes each semidominator final by the time it is read.
  for step in (2..=numbered).rev() {
    let target = vertex[step];
    let ordinal = target - 1;
    let mut orphan = true;
    for slot in retainers.first[ordinal]..retainers.first[ordinal + 1] {
      if !essential[retainers.edges[slot]] {
        continue;
      }
      orphan = false;
      let source = retainers.nodes[slot] + 1;
      let candidate = evaluate(source, &mut ancestor, &mut label, &semi, &mut compression_stack);
      if semi[candidate] < semi[target] {
        semi[target] = semi[candidate];
      }
    }
    if orphan {
      // Treated as retained by the root alone.
      semi[target] = semi[root_vertex];
    }

    bucket[vertex[semi[target]]].push(target);
    ancestor[target] = parent[target];

    let pending = std::mem::take(&mut bucket[parent[target]]);
    for deferred in pending {
      let candidate = evaluate(deferred, &mut ancestor, &mut label, &semi, &mut compression_stack);
      dom[deferred] = if semi[candidate] < semi[deferred] {
        candidate
      } else {
        parent[target]
      };
    }
  }

  // The root dominates itself, and slot 0 carries it too so that
  // unreachable nodes come out dominated by the root.
  dom[0] = root_vertex;
  dom[root_vertex] = root_vertex;
  for step in 2..=numbered {
    let target = vertex[step];
    if dom[target] != vertex[semi[target]] {
      dom[target] = dom[dom[target]];
    }
  }

  let mut dominators = vec![0usize; node_count];
  for (ordinal, slot) in dominators.iter_mut().enumerate() {
    *slot = dom[ordinal + 1].saturating_sub(1);
  }
  let mut retained = self_sizes.to_vec();
  // Reverse depth-first order guarantees a node is added to its
  // dominator only after everything it dominates has been added to it.
  for step in (2..=numbered).rev() {
    let ordinal = vertex[step] - 1;
    let dominator = dominators[ordinal];
    retained[dominator] += retained[ordinal];
  }

  (dominators, retained)
}

fn has_only_weak_retainers(ordinal: usize, essential: &[bool], retainers: &Retainers<'_>) -> bool {
  (retainers.first[ordinal]..retainers.first[ordinal + 1]).all(|slot| !essential[retainers.edges[slot]])
}

/// `eval` from the paper, with the path compression spelled out
/// iteratively.
fn evaluate(start: usize, ancestor: &mut [usize], label: &mut [usize], semi: &[usize], stack: &mut [usize]) -> usize {
  if ancestor[start] == 0 {
    return start;
  }
  let mut node = start;
  let mut depth = 0usize;
  while ancestor[ancestor[node]] != 0 {
    depth += 1;
    stack[depth] = node;
    node = ancestor[node];
  }
  while depth > 0 {
    let step = stack[depth];
    depth -= 1;
    if semi[label[ancestor[step]]] < semi[label[step]] {
      label[step] = label[ancestor[step]];
    }
    ancestor[step] = ancestor[ancestor[step]];
  }
  label[start]
}

// ── Distances ───────────────────────────────────────────────────────────

/// Edges from a user root, breadth-first, in two phases.
///
/// The page's own objects are measured from the user roots; whatever is
/// left is measured from the system root and starts at
/// [`BASE_SYSTEM_DISTANCE`], so anything only the system holds sorts
/// behind everything the page does.
fn calculate_distances(snapshot: &Snapshot, root: usize) -> Vec<i64> {
  let mut distances = vec![NO_DISTANCE; snapshot.node_count];
  let mut queue: Vec<usize> = Vec::with_capacity(snapshot.node_count);

  for edge in snapshot.first_edge_index[root]..snapshot.first_edge_index[root + 1] {
    if let Ok(child) = snapshot.edge_target(edge)
      && is_user_root(snapshot, child)
    {
      distances[child] = 1;
      queue.push(child);
    }
  }
  let had_user_roots = !queue.is_empty();
  bfs(snapshot, &mut queue, &mut distances);

  distances[root] = if had_user_roots { BASE_SYSTEM_DISTANCE } else { 0 };
  queue.clear();
  queue.push(root);
  bfs(snapshot, &mut queue, &mut distances);

  distances
}

fn bfs(snapshot: &Snapshot, queue: &mut Vec<usize>, distances: &mut [i64]) {
  // The ephemeron pairs seen so far, by the part of the edge name that
  // identifies the pair. See `distance_filter`.
  let mut pending_ephemerons: rustc_hash::FxHashSet<String> = rustc_hash::FxHashSet::default();
  let mut at = 0usize;
  while at < queue.len() {
    let ordinal = queue[at];
    at += 1;
    let distance = distances[ordinal] + 1;
    for edge in snapshot.first_edge_index[ordinal]..snapshot.first_edge_index[ordinal + 1] {
      if snapshot.edge_type(edge) == snapshot.edge_types.weak {
        continue;
      }
      let Ok(child) = snapshot.edge_target(edge) else {
        continue;
      };
      if distances[child] != NO_DISTANCE {
        continue;
      }
      if !distance_filter(snapshot, ordinal, edge, &mut pending_ephemerons) {
        continue;
      }
      distances[child] = distance;
      queue.push(child);
    }
  }
}

/// `JSHeapSnapshot::calculateDistances`' filter: three edges that would
/// otherwise report a shorter walk than really exists.
fn distance_filter(
  snapshot: &Snapshot,
  ordinal: usize,
  edge: usize,
  pending_ephemerons: &mut rustc_hash::FxHashSet<String>,
) -> bool {
  let kind = snapshot.node_type(ordinal);
  let name = snapshot.edge_name(edge);

  if kind == snapshot.node_types.hidden
    && name == Some("sloppy_function_map")
    && snapshot.raw_node_name(ordinal) == "system / NativeContext"
  {
    return false;
  }

  if kind == snapshot.node_types.array && snapshot.raw_node_name(ordinal) == "(map descriptors)" {
    // A descriptor array holds three slots per descriptor and maps
    // share them, so the links at `i * 3 + 1` are not valid for every
    // map that points here (crbug.com/413608).
    let index: i64 = name.and_then(|name| name.parse().ok()).unwrap_or(0);
    return index < 2 || index % 3 != 1;
  }

  if snapshot.edge_type(edge) == snapshot.edge_types.internal
    && let Some(name) = name
    && let Some((pair, _)) = parse_weak_map_edge_name(name)
  {
    // A `WeakMap` value arrives twice, once from the map and once from
    // the key. Skipping whichever is met first sets the distance from
    // the later of the two, which is the greater (crbug.com/1290800).
    if !pending_ephemerons.remove(pair) {
      pending_ephemerons.insert(pair.to_string());
      return false;
    }
  }

  true
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
