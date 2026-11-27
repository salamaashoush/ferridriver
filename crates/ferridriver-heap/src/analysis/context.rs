//! Native-context (realm) identification and ownership propagation.

use super::{NO_NATIVE_CONTEXT, SHARED_NATIVE_CONTEXT};
use crate::format::Snapshot;

/// A `system / NativeContext`, which is one JavaScript realm's root.
///
/// The detached spelling counts too: a realm whose frame has gone still
/// owns everything it allocated, and that is exactly the realm anyone
/// hunting a leak is looking for.
pub(crate) fn is_native_context(snapshot: &Snapshot, ordinal: usize) -> bool {
  let name = snapshot.raw_node_name(ordinal);
  for prefix in ["system / NativeContext", "Detached system / NativeContext"] {
    if let Some(rest) = name.strip_prefix(prefix)
      && (rest.is_empty() || rest.starts_with(" / "))
    {
      return true;
    }
  }
  false
}

/// A `system / Context`: one closure's captured scope.
pub(crate) fn is_context_object(snapshot: &Snapshot, ordinal: usize) -> bool {
  let name = snapshot.raw_node_name(ordinal);
  name == "system / Context" || name.starts_with("system / Context / ")
}

/// Which native context owns each node.
///
/// Two passes, and the first is the one that would not occur to anyone.
/// An object does not point at its realm: it points at its Map, the Map
/// points at its meta-Map, and the meta-Map -- one per realm -- carries
/// the `native_context` edge. So the owner of an object is three hops
/// away through internal edges, and reading it off the object directly
/// finds nothing.
///
/// What that pass fixes cannot be overwritten. Everything else takes
/// its owner from whatever reaches it, and a node reached from two
/// realms becomes [`SHARED_NATIVE_CONTEXT`] rather than belonging to
/// whichever arrived first.
pub(super) fn calculate_native_context_attribution(snapshot: &Snapshot) -> (Vec<i64>, Vec<usize>) {
  let mut attribution = vec![NO_NATIVE_CONTEXT; snapshot.node_count];
  let mut fixed = vec![false; snapshot.node_count];
  let mut contexts = Vec::new();
  let targets = build_init_edge_targets(snapshot);

  for ordinal in 0..snapshot.node_count {
    if is_native_context(snapshot, ordinal) {
      contexts.push(ordinal);
      attribution[ordinal] = i64::try_from(ordinal).unwrap_or(i64::MAX);
      fixed[ordinal] = true;
    } else if let Some(owner) = infer_fixed_native_context(ordinal, &targets) {
      attribution[ordinal] = i64::try_from(owner).unwrap_or(i64::MAX);
      fixed[ordinal] = true;
    }
  }

  propagate_native_context(snapshot, &mut attribution, &fixed);
  (attribution, contexts)
}

/// The `native_context` and `map` edge of every node, resolved once.
///
/// Both are internal edges named by string, and the attribution walk
/// asks for them per node; finding them by name each time is a scan of
/// that node's edges for every one of the three hops.
struct InitEdgeTargets {
  native_context: Vec<Option<usize>>,
  map: Vec<Option<usize>>,
}

fn build_init_edge_targets(snapshot: &Snapshot) -> InitEdgeTargets {
  let mut native_context = vec![None; snapshot.node_count];
  let mut map = vec![None; snapshot.node_count];

  for ordinal in 0..snapshot.node_count {
    for edge in snapshot.first_edge_index[ordinal]..snapshot.first_edge_index[ordinal + 1] {
      if snapshot.edge_type(edge) != snapshot.edge_types.internal {
        continue;
      }
      let Ok(child) = snapshot.edge_target(edge) else {
        continue;
      };
      match snapshot.edge_name(edge) {
        Some("native_context") if native_context[ordinal].is_none() && is_native_context(snapshot, child) => {
          native_context[ordinal] = Some(child);
        },
        Some("map") if map[ordinal].is_none() => map[ordinal] = Some(child),
        _ => {},
      }
    }
  }

  InitEdgeTargets { native_context, map }
}

/// Object -> Map -> meta-Map -> `NativeContext`.
fn infer_fixed_native_context(ordinal: usize, targets: &InitEdgeTargets) -> Option<usize> {
  let map = targets.map[ordinal]?;
  let meta_map = targets.map[map]?;
  targets.native_context[meta_map]
}

/// Two owners of one node make it shared, and stay shared.
fn merge_native_context(current: i64, incoming: i64) -> i64 {
  if current == SHARED_NATIVE_CONTEXT || incoming == SHARED_NATIVE_CONTEXT {
    return SHARED_NATIVE_CONTEXT;
  }
  if current == NO_NATIVE_CONTEXT || current == incoming {
    return incoming;
  }
  SHARED_NATIVE_CONTEXT
}

/// Spread each fixed owner along the edges that hold, re-queueing a
/// node whose owner CHANGED so the change reaches what it holds.
///
/// A shortcut is a root marker rather than a reference and a weak edge
/// retains nothing, so neither carries ownership.
fn propagate_native_context(snapshot: &Snapshot, attribution: &mut [i64], fixed: &[bool]) {
  let mut queue: Vec<usize> = (0..snapshot.node_count).filter(|&ordinal| fixed[ordinal]).collect();
  let mut at = 0usize;

  while at < queue.len() {
    let ordinal = queue[at];
    at += 1;
    let current = attribution[ordinal];

    for edge in snapshot.first_edge_index[ordinal]..snapshot.first_edge_index[ordinal + 1] {
      let kind = snapshot.edge_type(edge);
      if kind == snapshot.edge_types.shortcut || kind == snapshot.edge_types.weak {
        continue;
      }
      let Ok(child) = snapshot.edge_target(edge) else {
        continue;
      };
      if child == ordinal || fixed[child] {
        continue;
      }
      let merged = merge_native_context(attribution[child], current);
      if merged != attribution[child] {
        attribution[child] = merged;
        queue.push(child);
      }
    }
  }
}
