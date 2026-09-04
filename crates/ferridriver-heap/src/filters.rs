//! Narrowing a snapshot to the objects one question is about.
//!
//! `aggregatesWithFilter` takes a `NodeFilter`, and the heap tools
//! expose seven names for it. Each one answers a different form of "who
//! is holding this", and four of them share a shape worth stating
//! outright: they walk the graph AVOIDING something, and keep whatever
//! the walk could not reach.
//!
//! That inversion is the whole trick. To find what detached DOM nodes
//! are keeping alive you do not follow the detached nodes; you walk
//! everything else and see what is left over. A node the walk missed is
//! one every path to which goes through a detached node, which is
//! exactly the set that would be freed if the detachment were fixed.
//!
//! Ported from `HeapSnapshot.ts::createNamedFilter`.

use serde::{Deserialize, Serialize};

use crate::analysis::{
  Analysis, NO_NATIVE_CONTEXT, SHARED_NATIVE_CONTEXT, is_context_object, is_native_context, node_bfs_from_root,
};

/// Which objects a question is about.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeFilter {
  /// Everything, which is what a default `NodeFilter` means.
  #[default]
  AllObjects,
  /// Reachable only through a closure's captured scope.
  ObjectsRetainedByContexts,
  /// Reachable only through a DOM node the document has let go of.
  ObjectsRetainedByDetachedDomNodes,
  /// Reachable only through a global the `DevTools` console is holding,
  /// which is the set an open console is responsible for.
  ObjectsRetainedByConsole,
  /// Reachable only through an event handler.
  ObjectsRetainedByEventHandlers,
  /// Reached from more than one realm, so no single one can be blamed.
  SharedNativeContext,
  /// Reached from no realm at all.
  NoNativeContext,
  /// Owned by one realm, named by the object id of its `NativeContext`.
  AttributedToNativeContext(u64),
}

impl NodeFilter {
  /// The filter a caller named, as `get_heapsnapshot_details` takes it:
  /// a name, and an object id for the one name that needs one.
  ///
  /// Here rather than in each binding layer, so the two cannot disagree
  /// about what an unknown name or a missing id means.
  ///
  /// # Errors
  ///
  /// [`crate::HeapError::Format`] where the name is not one of these,
  /// or `attributedToNativeContext` arrives without an id.
  pub fn parse(name: Option<&str>, object_id: Option<u64>) -> crate::Result<Self> {
    let Some(name) = name else {
      return Ok(Self::AllObjects);
    };
    Ok(match name {
      "allObjects" => Self::AllObjects,
      "objectsRetainedByContexts" => Self::ObjectsRetainedByContexts,
      "objectsRetainedByDetachedDomNodes" => Self::ObjectsRetainedByDetachedDomNodes,
      "objectsRetainedByConsole" => Self::ObjectsRetainedByConsole,
      "objectsRetainedByEventHandlers" => Self::ObjectsRetainedByEventHandlers,
      "sharedNativeContext" => Self::SharedNativeContext,
      "noNativeContext" => Self::NoNativeContext,
      "attributedToNativeContext" => Self::AttributedToNativeContext(object_id.ok_or_else(|| {
        crate::HeapError::Format(
          "attributedToNativeContext needs the objectId of the native context to attribute to".to_string(),
        )
      })?),
      other => {
        return Err(crate::HeapError::Format(format!(
          "unknown node filter {other:?}; one of {}",
          Self::NAMES.join(", ")
        )));
      },
    })
  }

  /// Every name [`NodeFilter::parse`] accepts.
  pub const NAMES: [&'static str; 8] = [
    "allObjects",
    "objectsRetainedByContexts",
    "objectsRetainedByDetachedDomNodes",
    "objectsRetainedByConsole",
    "objectsRetainedByEventHandlers",
    "sharedNativeContext",
    "noNativeContext",
    "attributedToNativeContext",
  ];
}

/// How much of the heap each realm accounts for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeContextSizes {
  pub native_contexts: Vec<NativeContextSize>,
  /// What more than one realm can reach, which belongs to none of them.
  pub shared_size: u64,
  pub no_attribution_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeContextSize {
  pub node_id: u64,
  pub node_index: usize,
  pub node_name: String,
  /// Every byte whose owner is this realm, which is not the same as
  /// what it retains: a realm dominates far less than it owns.
  pub attributed_size: u64,
  pub retained_size: u64,
  pub self_size: u64,
}

/// How much of the heap is behind a closure scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetainedByContextSummary {
  pub context_count: usize,
  pub retained_by_context_size: u64,
  pub retained_by_context_count: usize,
  pub not_retained_by_context_size: u64,
  pub not_retained_by_context_count: usize,
  pub total_size: u64,
}

impl Analysis {
  /// Which nodes a filter keeps, one flag per node.
  ///
  /// # Errors
  ///
  /// Returns [`crate::HeapError::Format`] where
  /// [`NodeFilter::AttributedToNativeContext`] names an id the snapshot
  /// does not hold, or holds and is not a `NativeContext`.
  pub fn node_filter(&self, filter: NodeFilter) -> crate::Result<Vec<bool>> {
    let snapshot = &self.snapshot;
    Ok(match filter {
      NodeFilter::AllObjects => vec![true; snapshot.node_count],
      NodeFilter::ObjectsRetainedByContexts => {
        self.unreachable_without(|_from, _edge, child| !is_context_object(snapshot, child))
      },
      NodeFilter::ObjectsRetainedByDetachedDomNodes => {
        self.unreachable_without(|_from, _edge, child| snapshot.node_detachedness(child) != DETACHED)
      },
      NodeFilter::ObjectsRetainedByConsole => self.unreachable_without(|from, edge, _child| {
        // A synthetic node whose edge is NAMED after the console: that
        // is how the snapshot records a global the console pinned.
        !(snapshot.node_type(from) == snapshot.node_types.synthetic
          && snapshot
            .edge_name(edge)
            .is_some_and(|name| name.ends_with(" / DevTools console")))
      }),
      NodeFilter::ObjectsRetainedByEventHandlers => {
        let handlers = self.event_handlers();
        self.unreachable_without(|_from, _edge, child| !handlers[child])
      },
      NodeFilter::SharedNativeContext => self
        .native_context
        .iter()
        .map(|&owner| owner == SHARED_NATIVE_CONTEXT)
        .collect(),
      NodeFilter::NoNativeContext => self
        .native_context
        .iter()
        .map(|&owner| owner == NO_NATIVE_CONTEXT)
        .collect(),
      NodeFilter::AttributedToNativeContext(node_id) => {
        let ordinal = self.ordinal_for_id(node_id).ok_or_else(|| {
          crate::HeapError::Format(format!("no object with id {node_id} to attribute a native context to"))
        })?;
        if !is_native_context(snapshot, ordinal) {
          return Err(crate::HeapError::Format(format!(
            "object {node_id} is {:?}, not a native context",
            snapshot.raw_node_name(ordinal)
          )));
        }
        let wanted = i64::try_from(ordinal).unwrap_or(i64::MAX);
        self.native_context.iter().map(|&owner| owner == wanted).collect()
      },
    })
  }

  /// Walk the graph avoiding what `keep` rejects, and answer with what
  /// the walk could NOT reach.
  ///
  /// A node that was ALREADY unreachable before any of this is dropped
  /// again, which is upstream's `markUnreachableNodes` and reads
  /// backwards until you see what the answer means. "Retained by X" is
  /// "would go away if X did", and a node nothing retains would not:
  /// it is already garbage. Keeping it credits X with memory X is not
  /// holding.
  fn unreachable_without(&self, keep: impl FnMut(usize, usize, usize) -> bool) -> Vec<bool> {
    let reached = node_bfs_from_root(&self.snapshot, keep);
    (0..self.snapshot.node_count)
      .map(|ordinal| !reached[ordinal] && self.distance(ordinal) != crate::analysis::NO_DISTANCE)
      .collect()
  }

  /// The functions a page installed as event handlers.
  ///
  /// Nothing in the format says "this is a handler", so upstream infers
  /// it: V8 records each listener as a `V8EventListener` whose first
  /// element is the callback, and a callback with a `code` edge IS the
  /// handler. Where it has none the handler is one level down, inside
  /// whatever wrapper the framework put in between, and the first child
  /// carrying `code` stands in for it.
  fn event_handlers(&self) -> Vec<bool> {
    let snapshot = &self.snapshot;
    let mut handlers = vec![false; snapshot.node_count];

    for ordinal in 0..snapshot.node_count {
      if snapshot.raw_node_name(ordinal) != "V8EventListener" {
        continue;
      }
      let Some(callback) = self.edge_target_named(ordinal, "1") else {
        continue;
      };
      if self.edge_target_named(callback, "code").is_some() {
        handlers[callback] = true;
        continue;
      }
      // A framework wrapper: the handler is whichever child holds code.
      for edge in snapshot.first_edge_index[callback]..snapshot.first_edge_index[callback + 1] {
        let Ok(child) = snapshot.edge_target(edge) else {
          continue;
        };
        if self.edge_target_named(child, "code").is_some() {
          handlers[child] = true;
          break;
        }
      }
    }

    handlers
  }

  /// The first edge of any type with this display name.
  fn edge_target_named(&self, ordinal: usize, name: &str) -> Option<usize> {
    (self.snapshot.first_edge_index[ordinal]..self.snapshot.first_edge_index[ordinal + 1])
      .find(|&edge| self.edge_display_name(edge) == name)
      .and_then(|edge| self.snapshot.edge_target(edge).ok())
  }

  /// Every realm, and how much of the heap each one owns.
  #[must_use]
  pub fn native_context_sizes(&self) -> NativeContextSizes {
    let snapshot = &self.snapshot;
    let mut native_contexts: Vec<NativeContextSize> = self
      .native_context_ordinals
      .iter()
      .map(|&ordinal| NativeContextSize {
        node_id: snapshot.node_id(ordinal),
        node_index: self.node_index(ordinal),
        node_name: self.node_name(ordinal),
        attributed_size: 0,
        retained_size: self.retained_size(ordinal),
        self_size: snapshot.node_self_size(ordinal),
      })
      .collect();
    let at: rustc_hash::FxHashMap<usize, usize> = self
      .native_context_ordinals
      .iter()
      .enumerate()
      .map(|(at, &ordinal)| (ordinal, at))
      .collect();

    let (mut shared_size, mut no_attribution_size) = (0u64, 0u64);
    for ordinal in 0..snapshot.node_count {
      let self_size = snapshot.node_self_size(ordinal);
      match self.native_context[ordinal] {
        SHARED_NATIVE_CONTEXT => shared_size += self_size,
        NO_NATIVE_CONTEXT => no_attribution_size += self_size,
        owner => {
          if let Some(&slot) = usize::try_from(owner).ok().and_then(|owner| at.get(&owner)) {
            native_contexts[slot].attributed_size += self_size;
          }
        },
      }
    }

    NativeContextSizes {
      native_contexts,
      shared_size,
      no_attribution_size,
    }
  }

  /// How much of the heap only a closure scope is holding.
  ///
  /// Counted over sized nodes only, the way every other total here is.
  #[must_use]
  pub fn retained_by_context_summary(&self) -> RetainedByContextSummary {
    let snapshot = &self.snapshot;
    // The same filter `aggregatesWithFilter('objectsRetainedByContexts')`
    // builds, so the two can never drift apart.
    let retained = self
      .node_filter(NodeFilter::ObjectsRetainedByContexts)
      .unwrap_or_else(|_| vec![false; snapshot.node_count]);

    let mut summary = RetainedByContextSummary {
      context_count: 0,
      retained_by_context_size: 0,
      retained_by_context_count: 0,
      not_retained_by_context_size: 0,
      not_retained_by_context_count: 0,
      total_size: 0,
    };

    for (ordinal, &retained) in retained.iter().enumerate() {
      let self_size = snapshot.node_self_size(ordinal);
      if self_size == 0 {
        continue;
      }
      if retained {
        summary.retained_by_context_count += 1;
        summary.retained_by_context_size += self_size;
      } else {
        summary.not_retained_by_context_count += 1;
        summary.not_retained_by_context_size += self_size;
      }
      if is_context_object(snapshot, ordinal) {
        summary.context_count += 1;
      }
    }

    summary.total_size = summary.retained_by_context_size + summary.not_retained_by_context_size;
    summary
  }
}

/// `DOMLinkState.DETACHED`.
const DETACHED: u64 = 2;
