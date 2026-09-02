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

/// The first sighting of one truncated string's exact shape. Two
/// truncated strings match only when the visible prefix, the full
/// length and the hash all agree.
struct TruncatedSighting {
  ordinal: usize,
  length: Option<i64>,
  hash: Option<i64>,
}

/// One string that appears in the heap more than once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateStringGroup {
  pub value: String,
  pub count: usize,
  pub total_self_size: u64,
  pub total_retained_size: u64,
  pub nodes: Vec<DuplicateStringNode>,
  /// V8 stores only a prefix of a very long string, so two of them can
  /// look equal and not be. Those carry a length and a hash, and are
  /// grouped on all three.
  pub truncated: bool,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub length: Option<i64>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub hash: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateStringNode {
  pub id: u64,
  pub self_size: u64,
  pub retained_size: u64,
  pub distance: i64,
}

impl Analysis {
  /// Strings the page is holding more than one copy of, heaviest first.
  ///
  /// The obvious implementation -- group every string by its text -- is
  /// wrong three ways, and upstream guards all three. A cons string
  /// with an empty half is one V8 has flattened, and reporting it as a
  /// duplicate of its own content says nothing anyone can act on. A
  /// string node of zero size is V8's encoding for a number, not a
  /// string. And a truncated string is a PREFIX, so two that read the
  /// same may differ past the cut; those group on length and hash too.
  #[must_use]
  pub fn duplicate_strings(&self) -> Vec<DuplicateStringGroup> {
    let snapshot = &self.snapshot;
    let mut candidates: Vec<usize> = Vec::new();
    // First seen wins the slot; a second sighting marks both.
    let mut untruncated: rustc_hash::FxHashMap<String, usize> = rustc_hash::FxHashMap::default();
    let mut truncated: rustc_hash::FxHashMap<String, Vec<TruncatedSighting>> = rustc_hash::FxHashMap::default();
    let mut duplicated = vec![false; snapshot.node_count];

    for ordinal in 0..snapshot.node_count {
      let kind = snapshot.node_type(ordinal);
      if kind != snapshot.node_types.string && kind != snapshot.node_types.cons_string {
        continue;
      }
      if self.is_flat_cons_string(ordinal) || snapshot.node_self_size(ordinal) == 0 {
        continue;
      }
      let name = self.node_name(ordinal);
      if self.is_truncated_string(ordinal) {
        let shape = (self.string_length(ordinal), self.string_hash(ordinal));
        let entries = truncated.entry(name).or_default();
        if let Some(seen) = entries.iter().find(|seen| (seen.length, seen.hash) == shape) {
          duplicated[seen.ordinal] = true;
          duplicated[ordinal] = true;
        } else {
          entries.push(TruncatedSighting {
            ordinal,
            length: shape.0,
            hash: shape.1,
          });
        }
      } else if let Some(&first) = untruncated.get(&name) {
        duplicated[first] = true;
        duplicated[ordinal] = true;
      } else {
        untruncated.insert(name, ordinal);
      }
    }

    for (ordinal, &is_duplicate) in duplicated.iter().enumerate() {
      if is_duplicate {
        candidates.push(ordinal);
      }
    }

    // Grouped in node order so the members of a group are listed the
    // way the heap holds them.
    let mut plain: Vec<DuplicateStringGroup> = Vec::new();
    let mut plain_at: rustc_hash::FxHashMap<String, usize> = rustc_hash::FxHashMap::default();
    let mut cut: Vec<DuplicateStringGroup> = Vec::new();
    let mut cut_at: rustc_hash::FxHashMap<(String, Option<i64>, Option<i64>), usize> = rustc_hash::FxHashMap::default();

    for ordinal in candidates {
      let name = self.node_name(ordinal);
      let member = DuplicateStringNode {
        id: snapshot.node_id(ordinal),
        self_size: snapshot.node_self_size(ordinal),
        retained_size: self.retained_size(ordinal),
        distance: self.distance(ordinal),
      };
      let group = if self.is_truncated_string(ordinal) {
        let key = (name.clone(), self.string_length(ordinal), self.string_hash(ordinal));
        let at = *cut_at.entry(key.clone()).or_insert_with(|| {
          cut.push(DuplicateStringGroup {
            value: name,
            count: 0,
            total_self_size: 0,
            total_retained_size: 0,
            nodes: Vec::new(),
            truncated: true,
            length: key.1,
            hash: key.2,
          });
          cut.len() - 1
        });
        &mut cut[at]
      } else {
        let at = *plain_at.entry(name.clone()).or_insert_with(|| {
          plain.push(DuplicateStringGroup {
            value: name,
            count: 0,
            total_self_size: 0,
            total_retained_size: 0,
            nodes: Vec::new(),
            truncated: false,
            length: None,
            hash: None,
          });
          plain.len() - 1
        });
        &mut plain[at]
      };
      group.count += 1;
      group.total_self_size += member.self_size;
      group.total_retained_size += member.retained_size;
      group.nodes.push(member);
    }

    let mut all = plain;
    all.append(&mut cut);
    // Stable, so groups that hold the same amount stay in the order the
    // heap presented them.
    all.sort_by_key(|group| std::cmp::Reverse(group.total_retained_size));
    all
  }

  /// A cons string V8 has already flattened, which has an empty half.
  fn is_flat_cons_string(&self, ordinal: usize) -> bool {
    let snapshot = &self.snapshot;
    if snapshot.node_type(ordinal) != snapshot.node_types.cons_string {
      return false;
    }
    (snapshot.first_edge_index[ordinal]..snapshot.first_edge_index[ordinal + 1]).any(|edge| {
      if snapshot.edge_type(edge) != snapshot.edge_types.internal {
        return false;
      }
      if !matches!(snapshot.edge_name(edge), Some("first" | "second")) {
        return false;
      }
      snapshot
        .edge_target(edge)
        .is_ok_and(|target| self.node_name(target).is_empty())
    })
  }

  fn internal_edge_target(&self, ordinal: usize, name: &str) -> Option<usize> {
    let snapshot = &self.snapshot;
    (snapshot.first_edge_index[ordinal]..snapshot.first_edge_index[ordinal + 1])
      .find(|&edge| snapshot.edge_type(edge) == snapshot.edge_types.internal && snapshot.edge_name(edge) == Some(name))
      .and_then(|edge| snapshot.edge_target(edge).ok())
  }

  /// V8 writes an integer as a `number` node named `int` whose internal
  /// `value` edge points at a string node holding the digits.
  fn node_value_as_int(&self, ordinal: usize) -> Option<i64> {
    let snapshot = &self.snapshot;
    if snapshot.node_type(ordinal) != snapshot.node_types.number || snapshot.raw_node_name(ordinal) != "int" {
      return None;
    }
    let value = self.internal_edge_target(ordinal, "value")?;
    snapshot.raw_node_name(value).parse().ok()
  }

  /// The same encoding for booleans, named `bool` with `true`/`false`.
  fn node_value_as_bool(&self, ordinal: usize) -> Option<bool> {
    let snapshot = &self.snapshot;
    if snapshot.node_type(ordinal) != snapshot.node_types.number || snapshot.raw_node_name(ordinal) != "bool" {
      return None;
    }
    let value = self.internal_edge_target(ordinal, "value")?;
    match snapshot.raw_node_name(value) {
      "true" => Some(true),
      "false" => Some(false),
      _ => None,
    }
  }

  fn is_truncated_string(&self, ordinal: usize) -> bool {
    self
      .internal_edge_target(ordinal, "truncated")
      .and_then(|target| self.node_value_as_bool(target))
      == Some(true)
  }

  fn string_length(&self, ordinal: usize) -> Option<i64> {
    self
      .internal_edge_target(ordinal, "length")
      .and_then(|target| self.node_value_as_int(target))
  }

  fn string_hash(&self, ordinal: usize) -> Option<i64> {
    self
      .internal_edge_target(ordinal, "hash")
      .and_then(|target| self.node_value_as_int(target))
  }
}

/// Every object of one class, counted and measured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Aggregate {
  pub count: usize,
  /// The nearest any of them sits to a user root.
  pub distance: i64,
  /// Their sizes added up.
  #[serde(rename = "self")]
  pub self_size: u64,
  /// What the class holds that nothing outside it holds: the retained
  /// sizes of its members, counting each byte once even where one
  /// member dominates another.
  pub max_ret: u64,
  pub name: String,
  /// Node indexes, in node order.
  ///
  /// Upstream can sort these by object id, which is stable across
  /// snapshots where a node index is not, but `aggregatesWithFilter`
  /// asks it not to -- and that is the call the tools make.
  pub idxs: Vec<usize>,
}

impl Analysis {
  /// Group every sized node by its class, the way
  /// `get_heapsnapshot_class_nodes` and the aggregate half of
  /// `get_heapsnapshot_details` report it.
  ///
  /// Keyed as upstream exposes it: `,<name>` for an ordinary class, and
  /// `<script>,<line>,<column>,<name>` for an object whose constructor
  /// has a location. Two constructors of the same name from different
  /// scripts are different classes, and a key that ignored the location
  /// would merge them.
  #[must_use]
  pub fn aggregates(&self) -> std::collections::BTreeMap<String, Aggregate> {
    let snapshot = &self.snapshot;
    let mut by_key: rustc_hash::FxHashMap<ClassKey, Aggregate> = rustc_hash::FxHashMap::default();

    for ordinal in 0..snapshot.node_count {
      // A node with no size of its own adds nothing to a total, and
      // upstream leaves it out rather than reporting a class of zeroes.
      let self_size = snapshot.node_self_size(ordinal);
      if self_size == 0 {
        continue;
      }
      let distance = self.distance(ordinal);
      by_key
        .entry(self.class_key(ordinal))
        .and_modify(|aggregate| {
          aggregate.distance = aggregate.distance.min(distance);
          aggregate.count += 1;
          aggregate.self_size += self_size;
          aggregate.idxs.push(self.node_index(ordinal));
        })
        .or_insert_with(|| Aggregate {
          count: 1,
          distance,
          self_size,
          max_ret: 0,
          name: self.class_name(ordinal).to_string(),
          idxs: vec![self.node_index(ordinal)],
        });
    }

    self.add_retained_sizes_per_class(&mut by_key);

    let mut out = std::collections::BTreeMap::new();
    for (key, aggregate) in by_key {
      out.insert(key.exposed(&snapshot.strings), aggregate);
    }
    out
  }

  /// What each class retains, walking the dominator tree downwards.
  ///
  /// The obvious sum -- add up every member's retained size -- double
  /// counts, because one member of a class often dominates another and
  /// the inner one's bytes are already inside the outer one's total.
  /// Upstream walks down from the root and stops crediting a class
  /// again while it is already inside one of its own members, which is
  /// what `seen` tracks.
  fn add_retained_sizes_per_class(&self, aggregates: &mut rustc_hash::FxHashMap<ClassKey, Aggregate>) {
    let mut stack = vec![0usize];
    // Where in the walk each currently-open class was entered.
    let mut open_at: Vec<usize> = Vec::new();
    let mut open_keys: Vec<ClassKey> = Vec::new();
    let mut seen: rustc_hash::FxHashSet<ClassKey> = rustc_hash::FxHashSet::default();

    while let Some(ordinal) = stack.pop() {
      let key = self.class_key(ordinal);
      let already_inside = seen.contains(&key);
      let dominated = self.first_dominated[ordinal]..self.first_dominated[ordinal + 1];

      if !already_inside
        && self.snapshot.node_self_size(ordinal) > 0
        && let Some(aggregate) = aggregates.get_mut(&key)
      {
        aggregate.max_ret += self.retained_size(ordinal);
        if !dominated.is_empty() {
          seen.insert(key.clone());
          open_at.push(stack.len());
          open_keys.push(key);
        }
      }

      for at in dominated {
        stack.push(self.dominated_nodes[at]);
      }

      // Anything opened at this depth is now behind us.
      while open_at.last() == Some(&stack.len()) {
        open_at.pop();
        if let Some(key) = open_keys.pop() {
          seen.remove(&key);
        }
      }
    }
  }

  fn class_key(&self, ordinal: usize) -> ClassKey {
    let snapshot = &self.snapshot;
    // Only objects carry a location worth splitting on. Functions have
    // one too, and splitting by it would give most categories a single
    // member.
    if snapshot.node_type(ordinal) == snapshot.node_types.object
      && let Some(location) = snapshot.locations.get(&ordinal)
    {
      return ClassKey::Located(format!(
        "{},{},{},{}",
        location.script_id,
        location.line,
        location.column,
        self.class_name(ordinal)
      ));
    }
    ClassKey::Index(self.class_index[ordinal])
  }
}

/// How a class is identified while aggregating.
///
/// Cheap to produce, which matters over every node in the heap; the
/// string form callers see is derived once at the end.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ClassKey {
  Index(usize),
  Located(String),
}

impl ClassKey {
  fn exposed(self, strings: &[String]) -> String {
    match self {
      // The empty field before the comma is where a location would be.
      Self::Index(at) => format!(",{}", strings.get(at).map_or("", String::as_str)),
      Self::Located(key) => key,
    }
  }
}
