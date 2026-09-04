//! A captured heap, and the questions worth asking it.
//!
//! [`crate::page::Page::take_heap_snapshot`] hands back one of these.
//! The analysis is `ferridriver-heap`, a port of `DevTools`' own heap
//! engine checked against it node by node; what this adds is the shape
//! a caller works in. Every method here is addressed by OBJECT ID,
//! because that is what the answers carry and what survives from one
//! snapshot to the next -- a node's position in the file does not.
//!
//! # One handle, not thirteen tools
//!
//! `chrome-devtools-mcp` spends thirteen MCP tools on this, each taking
//! a file path and re-reading the snapshot. Here it is one object with
//! the queries on it, so a leak hunt is a program:
//!
//! ```no_run
//! # async fn example(page: &std::sync::Arc<ferridriver::page::Page>) -> ferridriver::error::Result<()> {
//! let before = page.take_heap_snapshot().await?;
//! // ... make the page do the thing ...
//! let after = page.take_heap_snapshot().await?;
//!
//! for diff in after.diff_since(&before) {
//!   if diff.count_delta > 100 {
//!     println!("{} grew by {} objects", diff.class_key, diff.count_delta);
//!   }
//! }
//! # Ok(())
//! # }
//! ```

use ferridriver_heap::{Analysis, Snapshot};
use serde::{Deserialize, Serialize};

use crate::error::{FerriError, Result};

// The analysis types cross into the bindings unchanged, so they are
// re-exported here rather than making every binding layer depend on
// `ferridriver-heap` directly.
pub use ferridriver_heap::{
  DominatorStep, DuplicateStringGroup, DuplicateStringNode, EdgeSummary, LimitsReached, NativeContextSize,
  NativeContextSizes, NativeStatistics, NodeFilter, NodeSummary, ObjectInfo, ObjectQuery, PathLimits, QuerySort,
  RetainedByContextSummary, RetainingEdge, RetainingPaths, Statistics, V8Statistics,
};

/// One class of objects, counted and measured.
///
/// `class_key` is what [`HeapSnapshot::class_objects`] takes:
/// `,<name>` for an ordinary class, and `<script>,<line>,<column>,<name>`
/// for an object whose constructor has a location, so two constructors
/// of the same name from different scripts stay apart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeapClass {
  pub class_key: String,
  pub name: String,
  pub count: usize,
  /// The nearest any of them sits to a user root.
  pub distance: i64,
  /// Their own sizes added up.
  pub self_size: u64,
  /// What the class holds that nothing outside it holds, counting each
  /// byte once even where one member dominates another. The number to
  /// read when asking what is using the memory.
  pub max_retained_size: u64,
}

/// How one class changed between two snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeapClassDiff {
  pub class_key: String,
  pub name: String,
  pub added_count: usize,
  pub removed_count: usize,
  pub added_size: u64,
  pub removed_size: u64,
  pub count_delta: i64,
  pub size_delta: i64,
  /// The objects allocated since the base snapshot, oldest first. Pass
  /// one to [`HeapSnapshot::retaining_paths`] to find out what is
  /// holding it.
  pub added_ids: Vec<u64>,
  pub added_self_sizes: Vec<u64>,
  /// And the ones collected, which is the half a leak hunt hopes to see.
  pub deleted_ids: Vec<u64>,
  pub deleted_self_sizes: Vec<u64>,
}

/// A heap snapshot, parsed and analysed.
#[derive(Debug)]
pub struct HeapSnapshot {
  json: String,
  analysis: Analysis,
}

impl HeapSnapshot {
  /// Read a `.heapsnapshot`.
  ///
  /// # Errors
  ///
  /// Returns [`FerriError::Backend`] where the text is not a snapshot,
  /// or declares a layout its own arrays contradict.
  pub fn parse(json: impl Into<String>) -> Result<Self> {
    let json = json.into();
    let snapshot = Snapshot::parse(&json).map_err(|e| FerriError::backend(e.to_string()))?;
    Ok(Self {
      json,
      analysis: Analysis::new(snapshot),
    })
  }

  /// The file this was read from, or would be written to. Writing it
  /// out gives a `.heapsnapshot` the `DevTools` Memory panel opens.
  #[must_use]
  pub fn as_json(&self) -> &str {
    &self.json
  }

  /// The analysis underneath, for anything this surface does not name.
  #[must_use]
  pub fn analysis(&self) -> &Analysis {
    &self.analysis
  }

  /// How the heap divides between V8 and the objects it does not own,
  /// as the `DevTools` Memory panel's summary reports it.
  #[must_use]
  pub fn statistics(&self) -> Statistics {
    self.analysis.statistics()
  }

  /// Every byte the snapshot accounts for.
  #[must_use]
  pub fn total_size(&self) -> u64 {
    self.analysis.total_size()
  }

  #[must_use]
  pub fn node_count(&self) -> usize {
    self.analysis.snapshot.node_count
  }

  /// Every class, heaviest first.
  ///
  /// Ordered by what each holds rather than by name, which is
  /// `HeapSnapshotFormatter`'s order and the one worth reading: the
  /// class at the top is where the memory went.
  #[must_use]
  pub fn classes(&self) -> Vec<HeapClass> {
    Self::sorted_classes(self.analysis.aggregates())
  }

  fn sorted_classes(aggregates: std::collections::BTreeMap<String, ferridriver_heap::Aggregate>) -> Vec<HeapClass> {
    let mut classes: Vec<HeapClass> = aggregates
      .into_iter()
      .map(|(class_key, aggregate)| HeapClass {
        class_key,
        name: aggregate.name,
        count: aggregate.count,
        distance: aggregate.distance,
        self_size: aggregate.self_size,
        max_retained_size: aggregate.max_ret,
      })
      .collect();
    classes.sort_by(|a, b| {
      b.max_retained_size
        .cmp(&a.max_retained_size)
        .then_with(|| a.class_key.cmp(&b.class_key))
    });
    classes
  }

  /// Every class, over only the objects a filter keeps.
  ///
  /// Four of the filters answer "what is X holding" by walking the
  /// graph AVOIDING X and keeping what the walk missed, so what comes
  /// back is what would be freed if X let go. The other three read a
  /// node's realm.
  ///
  /// # Errors
  ///
  /// Returns [`FerriError::InvalidArgument`] where
  /// [`NodeFilter::AttributedToNativeContext`] names an id that is not
  /// a native context in this snapshot.
  pub fn classes_with_filter(&self, filter: NodeFilter) -> Result<Vec<HeapClass>> {
    let aggregates = self
      .analysis
      .aggregates_with_filter(filter)
      .map_err(|e| FerriError::invalid_argument("filter", e.to_string()))?;
    Ok(Self::sorted_classes(aggregates))
  }

  /// Every object of one class, in the order the heap holds them.
  ///
  /// # Errors
  ///
  /// Returns [`FerriError::InvalidArgument`] where no class has that
  /// key. Keys come from [`HeapSnapshot::classes`].
  pub fn class_objects(&self, class_key: &str) -> Result<Vec<NodeSummary>> {
    self
      .analysis
      .nodes_for_class(class_key)
      .ok_or_else(|| FerriError::invalid_argument("classKey", format!("no class {class_key:?} in this snapshot")))
  }

  /// The same, over only the objects a filter keeps. The key has to
  /// come from [`HeapSnapshot::classes_with_filter`] under the SAME
  /// filter: a filter changes which classes exist.
  ///
  /// # Errors
  ///
  /// As [`HeapSnapshot::class_objects`] and
  /// [`HeapSnapshot::classes_with_filter`].
  pub fn class_objects_with_filter(&self, class_key: &str, filter: NodeFilter) -> Result<Vec<NodeSummary>> {
    self
      .analysis
      .nodes_for_class_with_filter(class_key, filter)
      .map_err(|e| FerriError::invalid_argument("filter", e.to_string()))?
      .ok_or_else(|| FerriError::invalid_argument("classKey", format!("no class {class_key:?} under this filter")))
  }

  /// Every JavaScript realm, and how much of the heap each one owns.
  ///
  /// A page with an iframe has more than one, and an object's realm is
  /// three hops away through its Map: nothing on the object says which.
  #[must_use]
  pub fn native_contexts(&self) -> NativeContextSizes {
    self.analysis.native_context_sizes()
  }

  /// How much of the heap only a closure's captured scope is holding.
  #[must_use]
  pub fn context_summary(&self) -> RetainedByContextSummary {
    self.analysis.retained_by_context_summary()
  }

  /// What one object is: its name, type, sizes, distance from a root
  /// and whether the DOM still holds it.
  ///
  /// # Errors
  ///
  /// Returns [`FerriError::InvalidArgument`] where the snapshot holds
  /// no object with that id.
  pub fn object(&self, node_id: u64) -> Result<ObjectInfo> {
    Ok(self.analysis.object_info(self.ordinal(node_id)?))
  }

  /// What this object points at, in the order the graph holds them.
  ///
  /// `get_heapsnapshot_edges` also takes a sort and two filters
  /// (`sortBy`, `minRetainedSize`, `excludePrimitives`). They are not
  /// here because every one of them is a line over the answer: each
  /// result carries the retained size and the type they select on, and
  /// a caller sorting the list themselves does not need a second way to
  /// ask the same question.
  ///
  /// # Errors
  ///
  /// As [`HeapSnapshot::object`].
  pub fn edges(&self, node_id: u64) -> Result<Vec<EdgeSummary>> {
    Ok(self.analysis.edges_of(self.ordinal(node_id)?))
  }

  /// What points at this object. The node in each answer is the one
  /// doing the retaining, not the one retained.
  ///
  /// # Errors
  ///
  /// As [`HeapSnapshot::object`].
  pub fn retainers(&self, node_id: u64) -> Result<Vec<EdgeSummary>> {
    Ok(self.analysis.retainers_of(self.ordinal(node_id)?))
  }

  /// Every route from this object back to a GC root, nearest root
  /// first. The answer to "why is this still alive".
  ///
  /// # Errors
  ///
  /// As [`HeapSnapshot::object`].
  pub fn retaining_paths(&self, node_id: u64, limits: Option<PathLimits>) -> Result<RetainingPaths> {
    Ok(
      self
        .analysis
        .retaining_paths(self.ordinal(node_id)?, limits.unwrap_or_default()),
    )
  }

  /// The chain of objects that would each, on letting go, free this
  /// one: itself first and the root last.
  ///
  /// # Errors
  ///
  /// As [`HeapSnapshot::object`].
  pub fn dominators(&self, node_id: u64) -> Result<Vec<DominatorStep>> {
    Ok(self.analysis.dominator_chain(self.ordinal(node_id)?))
  }

  /// Strings the page holds more than one copy of, heaviest first.
  #[must_use]
  pub fn duplicate_strings(&self) -> Vec<DuplicateStringGroup> {
    self.analysis.duplicate_strings()
  }

  /// Find objects by what they look like: a name, a type, a property
  /// they carry, a size band, whether they are detached.
  ///
  /// # Errors
  ///
  /// Returns [`FerriError::InvalidArgument`] where `className` or
  /// `propertyName` is not a regular expression.
  pub fn query(&self, query: &ObjectQuery) -> Result<Vec<NodeSummary>> {
    self
      .analysis
      .query_objects(query)
      .map_err(|e| FerriError::invalid_argument("query", e.to_string()))
  }

  /// Every class that gained or lost an object since `base`, by how
  /// much it grew.
  ///
  /// A class whose objects all survived is left out entirely, however
  /// large: it is not what changed.
  ///
  /// Both snapshots have to come from one page session. Objects are
  /// matched by id, and an id means the same object only within the
  /// isolate that assigned it, so comparing two runs of a page reports
  /// every object as allocated and collected at once.
  #[must_use]
  pub fn diff_since(&self, base: &Self) -> Vec<HeapClassDiff> {
    let mut diffs: Vec<HeapClassDiff> = self
      .analysis
      .diff_since(&base.analysis)
      .into_iter()
      .map(|(class_key, diff)| HeapClassDiff {
        class_key,
        name: diff.name,
        added_count: diff.added_count,
        removed_count: diff.removed_count,
        added_size: diff.added_size,
        removed_size: diff.removed_size,
        count_delta: diff.count_delta,
        size_delta: diff.size_delta,
        added_ids: diff.added_ids,
        added_self_sizes: diff.added_self_sizes,
        deleted_ids: diff.deleted_ids,
        deleted_self_sizes: diff.deleted_self_sizes,
      })
      .collect();
    diffs.sort_by(|a, b| {
      b.size_delta
        .cmp(&a.size_delta)
        .then_with(|| a.class_key.cmp(&b.class_key))
    });
    diffs
  }

  fn ordinal(&self, node_id: u64) -> Result<usize> {
    self
      .analysis
      .ordinal_for_id(node_id)
      .ok_or_else(|| FerriError::invalid_argument("nodeId", format!("no object with id {node_id} in this snapshot")))
  }
}
