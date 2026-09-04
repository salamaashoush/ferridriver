//! `HeapSnapshot` — NAPI binding for a captured V8 heap.
//!
//! Returned by `page.takeHeapSnapshot()`. Every question is answered by
//! [`ferridriver::heap::HeapSnapshot`]; this layer only lowers the
//! answers into `#[napi(object)]` shapes so the generated `.d.ts` names
//! them, and lifts the option bags back.
//!
//! Chromium-only, because the format is V8's. On WebKit and Firefox the
//! capture throws the typed `Unsupported` the core raises.

use napi::Result;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

use crate::error::IntoNapi;

/// One object as every list here reports it.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct HeapNode {
  pub id: i64,
  /// What `DevTools` shows: a concatenated string assembled from its
  /// pieces, a plain object named after the properties it carries.
  pub name: String,
  /// Edges from a user root, or a number past every real distance for
  /// an object only the system can reach.
  pub distance: i64,
  /// The object's offset in the snapshot's flat node array.
  pub node_index: i64,
  pub retained_size: i64,
  pub self_size: i64,
  #[napi(js_name = "type")]
  pub kind: String,
  pub can_be_queried: bool,
  #[napi(js_name = "detachedDOMTreeNode")]
  pub detached_dom_tree_node: bool,
}

impl From<ferridriver::heap::NodeSummary> for HeapNode {
  fn from(node: ferridriver::heap::NodeSummary) -> Self {
    Self {
      id: node.id as i64,
      name: node.name,
      distance: node.distance,
      node_index: node.node_index as i64,
      retained_size: node.retained_size as i64,
      self_size: node.self_size as i64,
      kind: node.kind,
      can_be_queried: node.can_be_queried,
      detached_dom_tree_node: node.detached_dom_tree_node,
    }
  }
}

/// One reference, with the object at the other end of it.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct HeapEdge {
  /// The property, element index or internal slot this reference sits
  /// in.
  pub name: String,
  pub node: HeapNode,
  #[napi(js_name = "type")]
  pub kind: String,
  pub edge_index: i64,
}

impl From<ferridriver::heap::EdgeSummary> for HeapEdge {
  fn from(edge: ferridriver::heap::EdgeSummary) -> Self {
    Self {
      name: edge.name,
      node: edge.node.into(),
      kind: edge.kind,
      edge_index: edge.edge_index as i64,
    }
  }
}

/// Everything known about one object.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct HeapObject {
  pub id: i64,
  pub name: String,
  #[napi(js_name = "type")]
  pub kind: String,
  pub node_index: i64,
  /// `0` attached, `1` reachable from a detached node, `2` detached,
  /// after propagation through the graph -- which is what makes it
  /// worth reading.
  pub detachedness: i64,
  pub self_size: i64,
  pub retained_size: i64,
  pub distance: i64,
  pub edge_count: i64,
  pub retainer_count: i64,
}

impl From<ferridriver::heap::ObjectInfo> for HeapObject {
  fn from(info: ferridriver::heap::ObjectInfo) -> Self {
    Self {
      id: info.id as i64,
      name: info.name,
      kind: info.kind,
      node_index: info.node_index as i64,
      detachedness: info.detachedness as i64,
      self_size: info.self_size as i64,
      retained_size: info.retained_size as i64,
      distance: info.distance,
      edge_count: info.edge_count as i64,
      retainer_count: info.retainer_count as i64,
    }
  }
}

/// One step of the chain from an object up to the root.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct HeapDominator {
  pub node_id: i64,
  pub node_index: i64,
  pub node_name: String,
  pub retained_size: i64,
  pub self_size: i64,
}

impl From<ferridriver::heap::DominatorStep> for HeapDominator {
  fn from(step: ferridriver::heap::DominatorStep) -> Self {
    Self {
      node_id: step.node_id as i64,
      node_index: step.node_index as i64,
      node_name: step.node_name,
      retained_size: step.retained_size as i64,
      self_size: step.self_size as i64,
    }
  }
}

/// One retaining edge, with everything IT is retained by.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct HeapRetainingEdge {
  pub edge_index: i64,
  pub edge_name: String,
  pub edge_type: String,
  pub node_id: i64,
  pub node_index: i64,
  pub node_name: String,
  pub distance: i64,
  pub children: Vec<HeapRetainingEdge>,
}

impl From<ferridriver::heap::RetainingEdge> for HeapRetainingEdge {
  fn from(edge: ferridriver::heap::RetainingEdge) -> Self {
    Self {
      edge_index: edge.edge_index as i64,
      edge_name: edge.edge_name,
      edge_type: edge.edge_type,
      node_id: edge.node_id as i64,
      node_index: edge.node_index as i64,
      node_name: edge.node_name,
      distance: edge.distance,
      children: edge.children.into_iter().map(Into::into).collect(),
    }
  }
}

/// Which bound stopped the search, so a truncated answer says so.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct HeapPathLimitsReached {
  pub depth: bool,
  pub nodes: bool,
  pub siblings: bool,
}

/// Everything holding one object, and what the search gave up on.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct HeapRetainingPaths {
  pub paths: Vec<HeapRetainingEdge>,
  pub limits_reached: HeapPathLimitsReached,
}

impl From<ferridriver::heap::RetainingPaths> for HeapRetainingPaths {
  fn from(paths: ferridriver::heap::RetainingPaths) -> Self {
    Self {
      paths: paths.paths.into_iter().map(Into::into).collect(),
      limits_reached: HeapPathLimitsReached {
        depth: paths.limits_reached.depth,
        nodes: paths.limits_reached.nodes,
        siblings: paths.limits_reached.siblings,
      },
    }
  }
}

/// How far a retaining-path search may go. Omitted fields keep the
/// defaults `get_heapsnapshot_retaining_paths` sends: 30, 5000, 100.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct HeapRetainingPathOptions {
  #[napi(js_name = "maxDepth")]
  pub depth: Option<u32>,
  #[napi(js_name = "maxNodes")]
  pub nodes: Option<u32>,
  #[napi(js_name = "maxSiblings")]
  pub siblings: Option<u32>,
}

impl From<HeapRetainingPathOptions> for ferridriver::heap::PathLimits {
  fn from(options: HeapRetainingPathOptions) -> Self {
    let defaults = Self::default();
    Self {
      depth: options.depth.map_or(defaults.depth, |v| v as usize),
      nodes: options.nodes.map_or(defaults.nodes, |v| v as usize),
      siblings: options.siblings.map_or(defaults.siblings, |v| v as usize),
    }
  }
}

/// One class of objects, counted and measured.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct HeapClass {
  /// What `classObjects` takes. `,<name>` for an ordinary class, and
  /// `<script>,<line>,<column>,<name>` where the constructor has a
  /// location, so two constructors of the same name from different
  /// scripts stay apart.
  pub class_key: String,
  pub name: String,
  pub count: i64,
  pub distance: i64,
  pub self_size: i64,
  /// What the class holds that nothing outside it holds. The number to
  /// read when asking where the memory went.
  pub max_retained_size: i64,
}

impl From<ferridriver::heap::HeapClass> for HeapClass {
  fn from(class: ferridriver::heap::HeapClass) -> Self {
    Self {
      class_key: class.class_key,
      name: class.name,
      count: class.count as i64,
      distance: class.distance,
      self_size: class.self_size as i64,
      max_retained_size: class.max_retained_size as i64,
    }
  }
}

/// How one class changed between two snapshots.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct HeapClassDiff {
  pub class_key: String,
  pub name: String,
  pub added_count: i64,
  pub removed_count: i64,
  pub added_size: i64,
  pub removed_size: i64,
  pub count_delta: i64,
  pub size_delta: i64,
  /// The objects allocated since the base snapshot, oldest first.
  pub added_ids: Vec<i64>,
  pub added_self_sizes: Vec<i64>,
  /// And the ones collected.
  pub deleted_ids: Vec<i64>,
  pub deleted_self_sizes: Vec<i64>,
}

impl From<ferridriver::heap::HeapClassDiff> for HeapClassDiff {
  fn from(diff: ferridriver::heap::HeapClassDiff) -> Self {
    Self {
      class_key: diff.class_key,
      name: diff.name,
      added_count: diff.added_count as i64,
      removed_count: diff.removed_count as i64,
      added_size: diff.added_size as i64,
      removed_size: diff.removed_size as i64,
      count_delta: diff.count_delta,
      size_delta: diff.size_delta,
      added_ids: diff.added_ids.into_iter().map(|id| id as i64).collect(),
      added_self_sizes: diff.added_self_sizes.into_iter().map(|s| s as i64).collect(),
      deleted_ids: diff.deleted_ids.into_iter().map(|id| id as i64).collect(),
      deleted_self_sizes: diff.deleted_self_sizes.into_iter().map(|s| s as i64).collect(),
    }
  }
}

/// One string the page holds more than one copy of.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct HeapDuplicateString {
  pub value: String,
  pub count: i64,
  pub total_self_size: i64,
  pub total_retained_size: i64,
  pub nodes: Vec<HeapDuplicateStringNode>,
  /// V8 stores only a prefix of a very long string, so two that read
  /// alike can differ past the cut. Those group on length and hash too.
  pub truncated: bool,
  pub length: Option<i64>,
  pub hash: Option<i64>,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct HeapDuplicateStringNode {
  pub id: i64,
  pub self_size: i64,
  pub retained_size: i64,
  pub distance: i64,
}

impl From<ferridriver::heap::DuplicateStringGroup> for HeapDuplicateString {
  fn from(group: ferridriver::heap::DuplicateStringGroup) -> Self {
    Self {
      value: group.value,
      count: group.count as i64,
      total_self_size: group.total_self_size as i64,
      total_retained_size: group.total_retained_size as i64,
      nodes: group
        .nodes
        .into_iter()
        .map(|node| HeapDuplicateStringNode {
          id: node.id as i64,
          self_size: node.self_size as i64,
          retained_size: node.retained_size as i64,
          distance: node.distance,
        })
        .collect(),
      truncated: group.truncated,
      length: group.length,
      hash: group.hash,
    }
  }
}

/// The heap broken down the way the `DevTools` Memory panel's summary
/// reports it.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct HeapStatistics {
  pub total: i64,
  pub native: HeapNativeStatistics,
  #[napi(js_name = "v8heap")]
  pub v8heap: HeapV8Statistics,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct HeapNativeStatistics {
  pub total: i64,
  pub typed_arrays: i64,
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct HeapV8Statistics {
  pub total: i64,
  pub code: i64,
  pub js_arrays: i64,
  pub strings: i64,
  pub system: i64,
}

impl From<ferridriver::heap::Statistics> for HeapStatistics {
  fn from(stats: ferridriver::heap::Statistics) -> Self {
    Self {
      total: stats.total as i64,
      native: HeapNativeStatistics {
        total: stats.native.total as i64,
        typed_arrays: stats.native.typed_arrays as i64,
      },
      v8heap: HeapV8Statistics {
        total: stats.v8heap.total as i64,
        code: stats.v8heap.code as i64,
        js_arrays: stats.v8heap.js_arrays as i64,
        strings: stats.v8heap.strings as i64,
        system: stats.v8heap.system as i64,
      },
    }
  }
}

/// What to look for. Every field left out is a filter not applied.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct HeapQueryOptions {
  /// A regular expression against the object's displayed name, matched
  /// case-insensitively and unanchored.
  pub class_name: Option<String>,
  /// The same, against the name of any reference the object holds.
  pub property_name: Option<String>,
  /// A V8 node type: `object`, `closure`, `string`, `array`, `code`.
  pub node_type: Option<String>,
  pub min_retained_size: Option<i64>,
  pub max_retained_size: Option<i64>,
  pub min_self_size: Option<i64>,
  pub max_self_size: Option<i64>,
  pub is_detached: Option<bool>,
  #[napi(ts_type = "'retainedSize' | 'selfSize' | 'id'")]
  pub sort_by: Option<String>,
}

impl TryFrom<HeapQueryOptions> for ferridriver::heap::ObjectQuery {
  type Error = napi::Error;

  fn try_from(options: HeapQueryOptions) -> Result<Self> {
    let sort_by = match options.sort_by.as_deref() {
      None => None,
      Some("retainedSize") => Some(ferridriver::heap::QuerySort::RetainedSize),
      Some("selfSize") => Some(ferridriver::heap::QuerySort::SelfSize),
      Some("id") => Some(ferridriver::heap::QuerySort::Id),
      Some(other) => {
        return Err(napi::Error::from_reason(format!(
          "sortBy must be 'retainedSize', 'selfSize' or 'id', got {other:?}"
        )));
      },
    };
    Ok(Self {
      class_name: options.class_name,
      property_name: options.property_name,
      node_type: options.node_type,
      min_retained_size: options.min_retained_size.map(|v| v as u64),
      max_retained_size: options.max_retained_size.map(|v| v as u64),
      min_self_size: options.min_self_size.map(|v| v as u64),
      max_self_size: options.max_self_size.map(|v| v as u64),
      is_detached: options.is_detached,
      sort_by,
    })
  }
}

/// A captured heap, with the queries on it.
#[napi]
pub struct HeapSnapshot {
  inner: std::sync::Arc<ferridriver::heap::HeapSnapshot>,
}

impl HeapSnapshot {
  pub fn new(inner: ferridriver::heap::HeapSnapshot) -> Self {
    Self {
      inner: std::sync::Arc::new(inner),
    }
  }
}

#[napi]
impl HeapSnapshot {
  /// The snapshot as a `.heapsnapshot` file holds it. Write it out to
  /// open it in the `DevTools` Memory panel.
  #[napi]
  pub fn bytes(&self) -> Buffer {
    Buffer::from(self.inner.as_json().as_bytes())
  }

  /// How the heap divides between V8 and what it does not own.
  #[napi]
  pub fn statistics(&self) -> HeapStatistics {
    self.inner.statistics().into()
  }

  /// Every byte the snapshot accounts for.
  #[napi]
  pub fn total_size(&self) -> i64 {
    self.inner.total_size() as i64
  }

  /// How many nodes the graph holds.
  #[napi]
  pub fn node_count(&self) -> i64 {
    self.inner.node_count() as i64
  }

  /// Every class, heaviest first.
  #[napi]
  pub fn classes(&self) -> Vec<HeapClass> {
    self.inner.classes().into_iter().map(Into::into).collect()
  }

  /// Every object of one class, by the `classKey` from `classes()`.
  #[napi]
  pub fn class_objects(&self, class_key: String) -> Result<Vec<HeapNode>> {
    Ok(
      self
        .inner
        .class_objects(&class_key)
        .into_napi()?
        .into_iter()
        .map(Into::into)
        .collect(),
    )
  }

  /// What one object is.
  #[napi]
  pub fn object(&self, node_id: i64) -> Result<HeapObject> {
    Ok(self.inner.object(node_id as u64).into_napi()?.into())
  }

  /// What this object points at.
  #[napi]
  pub fn edges(&self, node_id: i64) -> Result<Vec<HeapEdge>> {
    Ok(
      self
        .inner
        .edges(node_id as u64)
        .into_napi()?
        .into_iter()
        .map(Into::into)
        .collect(),
    )
  }

  /// What points at this object; each answer names the retainer, not
  /// the object retained.
  #[napi]
  pub fn retainers(&self, node_id: i64) -> Result<Vec<HeapEdge>> {
    Ok(
      self
        .inner
        .retainers(node_id as u64)
        .into_napi()?
        .into_iter()
        .map(Into::into)
        .collect(),
    )
  }

  /// Every route from this object back to a GC root, nearest root
  /// first: the answer to why it is still alive.
  #[napi]
  pub fn retaining_paths(&self, node_id: i64, options: Option<HeapRetainingPathOptions>) -> Result<HeapRetainingPaths> {
    Ok(
      self
        .inner
        .retaining_paths(node_id as u64, Some(options.unwrap_or_default().into()))
        .into_napi()?
        .into(),
    )
  }

  /// What would have to let go for this object to be freed, one step at
  /// a time: itself first and the root last.
  #[napi]
  pub fn dominators(&self, node_id: i64) -> Result<Vec<HeapDominator>> {
    Ok(
      self
        .inner
        .dominators(node_id as u64)
        .into_napi()?
        .into_iter()
        .map(Into::into)
        .collect(),
    )
  }

  /// Strings the page holds more than one copy of, heaviest first.
  #[napi]
  pub fn duplicate_strings(&self) -> Vec<HeapDuplicateString> {
    self.inner.duplicate_strings().into_iter().map(Into::into).collect()
  }

  /// Find objects by what they look like.
  #[napi]
  pub fn query(&self, options: Option<HeapQueryOptions>) -> Result<Vec<HeapNode>> {
    let query = ferridriver::heap::ObjectQuery::try_from(options.unwrap_or_default())?;
    Ok(
      self
        .inner
        .query(&query)
        .into_napi()?
        .into_iter()
        .map(Into::into)
        .collect(),
    )
  }

  /// Every class that gained or lost an object since an earlier
  /// snapshot of the same page, by how much it grew.
  #[napi]
  pub fn diff_since(&self, base: &HeapSnapshot) -> Vec<HeapClassDiff> {
    self.inner.diff_since(&base.inner).into_iter().map(Into::into).collect()
  }
}
