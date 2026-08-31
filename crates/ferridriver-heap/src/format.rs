//! The `.heapsnapshot` file, decoded.
//!
//! V8 writes a heap snapshot as flat integer arrays plus a `meta`
//! describing how wide each record is and what each field means. Nothing
//! about the layout is fixed: `node_fields` names the columns and
//! `node_types[0]` is the enum table for the first of them, so a reader
//! that hard-codes "type is column 0, six columns per node" happens to
//! work on today's Chrome and is wrong by construction.
//!
//! Everything here is read from the file's own `meta`, and the offsets
//! are resolved by NAME once at load
//! (`HeapSnapshot.ts::initialize`). A field the file does not declare is
//! an error rather than a default, because guessing produces a graph
//! that parses and means nothing.

use rustc_hash::FxHashMap;
use serde::Deserialize;

use crate::error::{HeapError, Result};

/// The `meta` block: the column layout of every record in the file.
#[derive(Debug, Clone, Deserialize)]
pub struct Meta {
  pub node_fields: Vec<String>,
  /// Positionally matched to `node_fields`. An entry is either a list of
  /// enum names (for `type`) or a scalar kind like `"string"`.
  pub node_types: Vec<serde_json::Value>,
  pub edge_fields: Vec<String>,
  pub edge_types: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SnapshotHeader {
  pub meta: Meta,
  pub node_count: usize,
  pub edge_count: usize,
  /// Bytes V8 accounts for outside the heap it walked. Part of the
  /// total, and absent from older files.
  #[serde(default)]
  pub extra_native_bytes: u64,
}

#[derive(Debug, Deserialize)]
pub struct RawProfile {
  pub snapshot: SnapshotHeader,
  pub nodes: Vec<u64>,
  pub edges: Vec<u64>,
  pub strings: Vec<String>,
}

/// Where each node column sits, resolved by name.
#[derive(Debug, Clone, Copy)]
pub struct NodeLayout {
  pub field_count: usize,
  pub type_offset: usize,
  pub name_offset: usize,
  pub id_offset: usize,
  pub self_size_offset: usize,
  pub edge_count_offset: usize,
  /// Absent from snapshots taken before V8 tracked it.
  pub detachedness_offset: Option<usize>,
}

/// Where each edge column sits, resolved by name.
#[derive(Debug, Clone, Copy)]
pub struct EdgeLayout {
  pub field_count: usize,
  pub type_offset: usize,
  pub name_or_index_offset: usize,
  pub to_node_offset: usize,
}

/// The node type enum, by the names the file gives them.
#[derive(Debug, Clone, Copy)]
pub struct NodeTypes {
  pub hidden: u64,
  pub array: u64,
  pub string: u64,
  pub object: u64,
  pub code: u64,
  pub closure: u64,
  pub native: u64,
  pub synthetic: u64,
  pub cons_string: u64,
  pub sliced_string: u64,
}

/// The edge type enum, by the names the file gives them.
#[derive(Debug, Clone, Copy)]
pub struct EdgeTypes {
  pub context: u64,
  pub element: u64,
  pub property: u64,
  pub internal: u64,
  pub hidden: u64,
  pub shortcut: u64,
  pub weak: u64,
  /// Absent from the files Chrome writes today. `u64::MAX` matches
  /// nothing, which is the same as the type never occurring.
  pub invisible: u64,
}

/// A parsed snapshot: the raw arrays plus the layout needed to read them.
#[derive(Debug)]
pub struct Snapshot {
  pub nodes: Vec<u64>,
  pub edges: Vec<u64>,
  pub strings: Vec<String>,
  pub node_layout: NodeLayout,
  pub edge_layout: EdgeLayout,
  pub node_types: NodeTypes,
  pub edge_types: EdgeTypes,
  pub node_count: usize,
  pub edge_count: usize,
  pub extra_native_bytes: u64,
  /// Every node type's name, indexed by the enum value, for reporting.
  pub node_type_names: Vec<String>,
  pub edge_type_names: Vec<String>,
  /// `first_edge_index[ordinal]` is the ordinal of that node's first
  /// edge, and `[node_count]` closes the last range. Edges are stored
  /// grouped by source node in node order, so a node's edges are the
  /// half-open range to the next entry.
  pub first_edge_index: Vec<usize>,
  /// Attachment state after propagation, NOT the raw field: an object
  /// reachable from an attached one is attached, and one reachable only
  /// from a detached one is detached. See [`Snapshot::propagate_dom_state`].
  pub detachedness: Vec<u8>,
}

/// `HeapSnapshotModel.DOMLinkState`. V8 writes 0 for "no idea", and
/// the propagation pass replaces it wherever the graph settles it.
pub const DOM_LINK_STATE_UNKNOWN: u8 = 0;
pub const DOM_LINK_STATE_ATTACHED: u8 = 1;
pub const DOM_LINK_STATE_DETACHED: u8 = 2;

/// A stored value used as an index. On a 64-bit target this is exact;
/// anywhere narrower, a value too large to be an index is a malformed
/// file, and every lookup that uses one already answers for a miss.
fn index_of(value: u64) -> usize {
  usize::try_from(value).unwrap_or(usize::MAX)
}

fn offset_of(fields: &[String], name: &str) -> Result<usize> {
  fields
    .iter()
    .position(|field| field == name)
    .ok_or_else(|| HeapError::Format(format!("the snapshot declares no `{name}` field")))
}

/// The enum table for a column, which the file stores as a list in the
/// `*_types` slot matching that column's position.
fn enum_table(types: &[serde_json::Value], offset: usize) -> Result<Vec<String>> {
  let entry = types
    .get(offset)
    .ok_or_else(|| HeapError::Format("the type column has no enum table".into()))?;
  let names = entry
    .as_array()
    .ok_or_else(|| HeapError::Format("the type column's enum table is not a list".into()))?;
  names
    .iter()
    .map(|name| {
      name
        .as_str()
        .map(std::string::ToString::to_string)
        .ok_or_else(|| HeapError::Format("a type name is not a string".into()))
    })
    .collect()
}

/// The enum value for `name`. Absent names answer `None` rather than
/// erroring: V8 has added types over time, and a snapshot that predates
/// one simply never uses it.
fn value_of(table: &[String], name: &str) -> Option<u64> {
  table.iter().position(|entry| entry == name).map(|at| at as u64)
}

/// A type this reader cannot do without. `hidden`, `native` and the
/// string types all decide sizes, so a file lacking one would be
/// silently mis-measured.
fn require(table: &[String], name: &str) -> Result<u64> {
  value_of(table, name).ok_or_else(|| HeapError::Format(format!("the snapshot declares no `{name}` node type")))
}

impl Snapshot {
  /// Parse a `.heapsnapshot`.
  ///
  /// # Errors
  ///
  /// [`HeapError::Json`] when the text is not a snapshot at all, and
  /// [`HeapError::Format`] when it is one whose `meta` omits something
  /// this reader needs or whose arrays do not match the counts it
  /// declares.
  pub fn parse(text: &str) -> Result<Self> {
    let raw: RawProfile = serde_json::from_str(text)?;
    Self::from_raw(raw)
  }

  fn from_raw(raw: RawProfile) -> Result<Self> {
    let meta = &raw.snapshot.meta;

    let node_type_offset = offset_of(&meta.node_fields, "type")?;
    let node_layout = NodeLayout {
      field_count: meta.node_fields.len(),
      type_offset: node_type_offset,
      name_offset: offset_of(&meta.node_fields, "name")?,
      id_offset: offset_of(&meta.node_fields, "id")?,
      self_size_offset: offset_of(&meta.node_fields, "self_size")?,
      edge_count_offset: offset_of(&meta.node_fields, "edge_count")?,
      detachedness_offset: offset_of(&meta.node_fields, "detachedness").ok(),
    };

    let edge_type_offset = offset_of(&meta.edge_fields, "type")?;
    let edge_layout = EdgeLayout {
      field_count: meta.edge_fields.len(),
      type_offset: edge_type_offset,
      name_or_index_offset: offset_of(&meta.edge_fields, "name_or_index")?,
      to_node_offset: offset_of(&meta.edge_fields, "to_node")?,
    };

    let node_type_names = enum_table(&meta.node_types, node_type_offset)?;
    let edge_type_names = enum_table(&meta.edge_types, edge_type_offset)?;

    let node_types = NodeTypes {
      hidden: require(&node_type_names, "hidden")?,
      array: require(&node_type_names, "array")?,
      string: require(&node_type_names, "string")?,
      object: require(&node_type_names, "object")?,
      code: require(&node_type_names, "code")?,
      closure: require(&node_type_names, "closure")?,
      native: require(&node_type_names, "native")?,
      synthetic: require(&node_type_names, "synthetic")?,
      // Both are absent from very old snapshots. `u64::MAX` matches
      // nothing, which is the same as the type never occurring.
      cons_string: value_of(&node_type_names, "concatenated string").unwrap_or(u64::MAX),
      sliced_string: value_of(&node_type_names, "sliced string").unwrap_or(u64::MAX),
    };

    let edge_types = EdgeTypes {
      context: require_edge(&edge_type_names, "context")?,
      element: require_edge(&edge_type_names, "element")?,
      property: require_edge(&edge_type_names, "property")?,
      internal: require_edge(&edge_type_names, "internal")?,
      hidden: require_edge(&edge_type_names, "hidden")?,
      shortcut: require_edge(&edge_type_names, "shortcut")?,
      weak: require_edge(&edge_type_names, "weak")?,
      invisible: value_of(&edge_type_names, "invisible").unwrap_or(u64::MAX),
    };

    let node_count = raw.snapshot.node_count;
    let edge_count = raw.snapshot.edge_count;
    // The header's counts and the arrays have to agree, or every index
    // computed from them addresses the wrong record.
    if raw.nodes.len() != node_count * node_layout.field_count {
      return Err(HeapError::Format(format!(
        "the snapshot declares {node_count} nodes of {} fields but carries {} values",
        node_layout.field_count,
        raw.nodes.len()
      )));
    }
    if raw.edges.len() != edge_count * edge_layout.field_count {
      return Err(HeapError::Format(format!(
        "the snapshot declares {edge_count} edges of {} fields but carries {} values",
        edge_layout.field_count,
        raw.edges.len()
      )));
    }

    let mut snapshot = Self {
      nodes: raw.nodes,
      edges: raw.edges,
      strings: raw.strings,
      node_layout,
      edge_layout,
      node_types,
      edge_types,
      node_count,
      edge_count,
      extra_native_bytes: raw.snapshot.extra_native_bytes,
      node_type_names,
      edge_type_names,
      first_edge_index: Vec::new(),
      detachedness: Vec::new(),
    };
    snapshot.build_edge_index();
    snapshot.init_detachedness();
    snapshot.propagate_dom_state();
    Ok(snapshot)
  }

  /// Group edges by source node, as `HeapSnapshot.ts::buildEdgeIndexes`
  /// does. The file stores them in that order already; this records
  /// where each group starts.
  fn build_edge_index(&mut self) {
    let mut index = Vec::with_capacity(self.node_count + 1);
    let mut edge_ordinal = 0usize;
    for ordinal in 0..self.node_count {
      index.push(edge_ordinal);
      edge_ordinal += self.node_edge_count(ordinal);
    }
    index.push(edge_ordinal);
    self.first_edge_index = index;
  }

  /// Seed attachment state from the field V8 wrote.
  ///
  /// Older snapshots have no such field, and upstream falls back to
  /// treating natives whose name starts with `Detached ` as detached
  /// (`initDetachednessAndClassIndex`).
  fn init_detachedness(&mut self) {
    if self.node_layout.detachedness_offset.is_some() {
      self.detachedness = (0..self.node_count)
        .map(|ordinal| {
          let raw = self.nodes
            [ordinal * self.node_layout.field_count + self.node_layout.detachedness_offset.unwrap_or_default()];
          u8::try_from(raw).unwrap_or(0)
        })
        .collect();
      return;
    }
    self.detachedness = (0..self.node_count)
      .map(|ordinal| {
        let native = self.node_type(ordinal) == self.node_types.native;
        u8::from(native && self.raw_node_name(ordinal).starts_with("Detached ")) * DOM_LINK_STATE_DETACHED
      })
      .collect();
  }

  /// Propagate attachment through the graph, and rename what is
  /// detached, exactly as `HeapSnapshot.ts::propagateDOMState` does.
  ///
  /// Two rules: anything reachable from an attached object is attached,
  /// and anything reachable only from a detached one is detached. The
  /// propagation stops at the first non-native node, because every
  /// entry point into embedder code knows its own state and JavaScript
  /// in between does not.
  ///
  /// This is why the reported detachedness is not the field V8 wrote:
  /// on the fixture here, two of two hundred sampled nodes carry a
  /// state they inherited rather than one they were given.
  fn propagate_dom_state(&mut self) {
    if self.node_layout.detachedness_offset.is_none() {
      return;
    }
    let mut visited = vec![false; self.node_count];
    let mut attached: Vec<usize> = Vec::new();
    let mut detached: Vec<usize> = Vec::new();
    let mut renamed: rustc_hash::FxHashMap<usize, usize> = rustc_hash::FxHashMap::default();

    for ordinal in 0..self.node_count {
      let state = self.detachedness[ordinal];
      if state == DOM_LINK_STATE_UNKNOWN {
        continue;
      }
      self.process_dom_node(ordinal, state, &mut visited, &mut attached, &mut detached, &mut renamed);
    }
    while let Some(ordinal) = attached.pop() {
      for child in self.dom_children(ordinal) {
        self.process_dom_node(
          child,
          DOM_LINK_STATE_ATTACHED,
          &mut visited,
          &mut attached,
          &mut detached,
          &mut renamed,
        );
      }
    }
    while let Some(ordinal) = detached.pop() {
      // Skip anything the attached pass has since claimed.
      if self.detachedness[ordinal] == DOM_LINK_STATE_ATTACHED {
        continue;
      }
      for child in self.dom_children(ordinal) {
        self.process_dom_node(
          child,
          DOM_LINK_STATE_DETACHED,
          &mut visited,
          &mut attached,
          &mut detached,
          &mut renamed,
        );
      }
    }
  }

  /// The children state propagates through: everything but hidden,
  /// invisible and weak edges.
  fn dom_children(&self, ordinal: usize) -> Vec<usize> {
    let mut out = Vec::new();
    for edge in self.first_edge_index[ordinal]..self.first_edge_index[ordinal + 1] {
      let kind = self.edge_type(edge);
      if kind == self.edge_types.hidden || kind == self.edge_types.invisible || kind == self.edge_types.weak {
        continue;
      }
      if let Ok(child) = self.edge_target(edge) {
        out.push(child);
      }
    }
    out
  }

  fn process_dom_node(
    &mut self,
    ordinal: usize,
    state: u8,
    visited: &mut [bool],
    attached: &mut Vec<usize>,
    detached: &mut Vec<usize>,
    renamed: &mut rustc_hash::FxHashMap<usize, usize>,
  ) {
    if visited[ordinal] {
      return;
    }
    // Only embedder nodes carry DOM state, and every entry point into
    // embedder code is native, so JavaScript ends the walk.
    if self.node_type(ordinal) != self.node_types.native {
      visited[ordinal] = true;
      return;
    }
    self.detachedness[ordinal] = state;
    if state == DOM_LINK_STATE_ATTACHED {
      attached.push(ordinal);
    } else if state == DOM_LINK_STATE_DETACHED {
      self.add_detached_prefix(ordinal, renamed);
      detached.push(ordinal);
    }
    visited[ordinal] = true;
  }

  /// A detached node is reported as `Detached <name>`, which means the
  /// string table grows during load.
  fn add_detached_prefix(&mut self, ordinal: usize, renamed: &mut rustc_hash::FxHashMap<usize, usize>) {
    let slot = ordinal * self.node_layout.field_count + self.node_layout.name_offset;
    let old = index_of(self.nodes[slot]);
    let new = *renamed.entry(old).or_insert_with(|| {
      let name = format!("Detached {}", self.strings.get(old).map_or("", String::as_str));
      self.strings.push(name);
      self.strings.len() - 1
    });
    self.nodes[slot] = new.try_into().unwrap_or(u64::MAX);
  }

  // ── Node accessors, by ordinal ────────────────────────────────────────

  fn node_field(&self, ordinal: usize, offset: usize) -> u64 {
    self.nodes[ordinal * self.node_layout.field_count + offset]
  }

  #[must_use]
  pub fn node_type(&self, ordinal: usize) -> u64 {
    self.node_field(ordinal, self.node_layout.type_offset)
  }

  #[must_use]
  pub fn node_type_name(&self, ordinal: usize) -> &str {
    let raw = index_of(self.node_type(ordinal));
    self.node_type_names.get(raw).map_or("", String::as_str)
  }

  /// The name as WRITTEN, before the naming rules that make a cons
  /// string or a plain object readable.
  #[must_use]
  pub fn raw_node_name(&self, ordinal: usize) -> &str {
    let at = index_of(self.node_field(ordinal, self.node_layout.name_offset));
    self.strings.get(at).map_or("", String::as_str)
  }

  #[must_use]
  pub fn node_id(&self, ordinal: usize) -> u64 {
    self.node_field(ordinal, self.node_layout.id_offset)
  }

  #[must_use]
  pub fn node_self_size(&self, ordinal: usize) -> u64 {
    self.node_field(ordinal, self.node_layout.self_size_offset)
  }

  /// Used by the pass that moves an owned node's size onto its owner,
  /// which is the one place a snapshot's numbers are rewritten.
  pub fn set_node_self_size(&mut self, ordinal: usize, size: u64) {
    let slot = ordinal * self.node_layout.field_count + self.node_layout.self_size_offset;
    self.nodes[slot] = size;
  }

  #[must_use]
  pub fn node_edge_count(&self, ordinal: usize) -> usize {
    index_of(self.node_field(ordinal, self.node_layout.edge_count_offset))
  }

  /// Attachment state AFTER propagation, which is what `DevTools`
  /// reports; the raw field is only the seed.
  #[must_use]
  pub fn node_detachedness(&self, ordinal: usize) -> u64 {
    u64::from(self.detachedness[ordinal])
  }

  // ── Edge accessors, by edge ordinal ───────────────────────────────────

  fn edge_field(&self, edge_ordinal: usize, offset: usize) -> u64 {
    self.edges[edge_ordinal * self.edge_layout.field_count + offset]
  }

  #[must_use]
  pub fn edge_type(&self, edge_ordinal: usize) -> u64 {
    self.edge_field(edge_ordinal, self.edge_layout.type_offset)
  }

  /// The raw `name_or_index` value, which is a string index for named
  /// edge types and an array index for the rest.
  #[must_use]
  pub fn edge_name_or_index(&self, edge_ordinal: usize) -> u64 {
    self.edge_field(edge_ordinal, self.edge_layout.name_or_index_offset)
  }

  /// The name for edges that have one. `None` where `name_or_index`
  /// holds an index rather than a string, which is what `element` and
  /// `hidden` edges carry.
  #[must_use]
  pub fn edge_name(&self, edge_ordinal: usize) -> Option<&str> {
    let kind = self.edge_type(edge_ordinal);
    if kind == self.edge_types.element || kind == self.edge_types.hidden {
      return None;
    }
    let at = index_of(self.edge_name_or_index(edge_ordinal));
    self.strings.get(at).map(String::as_str)
  }

  /// The node this edge points at, as an ordinal.
  ///
  /// # Errors
  ///
  /// [`HeapError::Format`] when the stored value is not a node
  /// boundary, which means the file's field widths and its data
  /// disagree.
  pub fn edge_target(&self, edge_ordinal: usize) -> Result<usize> {
    let index = index_of(self.edge_field(edge_ordinal, self.edge_layout.to_node_offset));
    if !index.is_multiple_of(self.node_layout.field_count) {
      return Err(HeapError::Format(format!(
        "edge {edge_ordinal} points at {index}, which is not the start of a node"
      )));
    }
    Ok(index / self.node_layout.field_count)
  }

  /// String index -> the first ordinal that uses it, for the naming
  /// rules that look strings up rather than nodes.
  #[must_use]
  pub fn string_index(&self) -> FxHashMap<&str, usize> {
    let mut out = FxHashMap::default();
    for (at, value) in self.strings.iter().enumerate() {
      out.entry(value.as_str()).or_insert(at);
    }
    out
  }
}

fn require_edge(table: &[String], name: &str) -> Result<u64> {
  value_of(table, name).ok_or_else(|| HeapError::Format(format!("the snapshot declares no `{name}` edge type")))
}
