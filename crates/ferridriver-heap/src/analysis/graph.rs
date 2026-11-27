//! Graph passes over a decoded heap snapshot: node flags, retainer index,
//! essential edges, shallow-size attribution, Lengauer-Tarjan dominators,
//! retained sizes, and BFS distances.

use rustc_hash::FxHashSet;

use super::{
  BASE_SYSTEM_DISTANCE, FLAG_CAN_BE_QUERIED, FLAG_DETACHED_DOM_TREE_NODE, FLAG_PAGE_OBJECT, NO_DISTANCE,
  is_document_dom_trees_root, is_synthetic, is_user_root, parse_weak_map_edge_name,
};
use crate::format::Snapshot;

/// Sentinels for the owner array in [`calculate_shallow_sizes`].
const UNVISITED: u32 = u32::MAX;
const MULTIPLE_OWNERS: u32 = u32::MAX - 1;

// ── Flags ───────────────────────────────────────────────────────────────

/// `JSHeapSnapshot::calculateFlags`, all three markers.
pub(super) fn calculate_flags(snapshot: &Snapshot) -> Vec<u8> {
  let mut flags = vec![0u8; snapshot.node_count];
  mark_detached_dom_tree_nodes(snapshot, &mut flags);
  mark_queryable_heap_objects(snapshot, &mut flags);
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
fn mark_queryable_heap_objects(snapshot: &Snapshot, flags: &mut [u8]) {
  let mut list: Vec<usize> = Vec::new();
  for edge in snapshot.first_edge_index[0]..snapshot.first_edge_index[1] {
    // The NODE's own `isUserRoot`, which is only "not synthetic".
    // `(Document DOM trees)` passes the snapshot-level test and fails
    // this one, and seeding from the wrong one marks the whole DOM as
    // queryable when `DevTools` says it is not.
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

// ── Dominated children & Retainers ──────────────────────────────────────

/// The dominator tree read downwards: which nodes each node dominates.
///
/// A counting sort like the retainer index, filling each slice
/// backwards from a count parked in its first slot.
pub(super) fn build_dominated_nodes(root: usize, dominators: &[usize]) -> (Vec<usize>, Vec<usize>) {
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

/// Invert the edge list: for each node, which edges point at it.
///
/// A counting sort in place, as `HeapSnapshot.ts::buildRetainers` does
/// it: count the arrivals per node, turn the counts into slice starts,
/// then fill each slice backwards.
pub(super) fn build_retainers(snapshot: &Snapshot) -> (Vec<usize>, Vec<usize>, Vec<usize>) {
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
pub(super) fn init_essential_edges(snapshot: &Snapshot, flags: &[u8]) -> Vec<bool> {
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
pub(super) fn calculate_shallow_sizes(snapshot: &mut Snapshot) {
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
pub(super) struct Retainers<'a> {
  pub(super) nodes: &'a [usize],
  pub(super) edges: &'a [usize],
  pub(super) first: &'a [usize],
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

pub(super) fn dominators_and_retained_sizes(
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

/// Reachability from the synthetic root under an arbitrary rule.
///
/// The named node filters all start here: they walk avoiding something
/// and read what the walk missed. Distances are the walk's scratch, not
/// its answer, which is why only the flags come back.
pub(crate) fn node_bfs_from_root(snapshot: &Snapshot, keep: impl FnMut(usize, usize, usize) -> bool) -> Vec<bool> {
  let mut distances = vec![NO_DISTANCE; snapshot.node_count];
  let mut queue = vec![0usize];
  distances[0] = 0;
  bfs_with(snapshot, &mut queue, &mut distances, keep);
  distances.into_iter().map(|distance| distance != NO_DISTANCE).collect()
}

/// Edges from a user root, breadth-first, in two phases.
///
/// The page's own objects are measured from the user roots; whatever is
/// left is measured from the system root and starts at
/// [`BASE_SYSTEM_DISTANCE`], so anything only the system holds sorts
/// behind everything the page does.
pub(super) fn calculate_distances(snapshot: &Snapshot, root: usize) -> Vec<i64> {
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
  // identifies the pair. Borrowed directly from `snapshot.strings`.
  let mut pending_ephemerons: FxHashSet<&str> = FxHashSet::default();
  bfs_with(snapshot, queue, distances, |ordinal, edge, _child| {
    distance_filter(snapshot, ordinal, edge, &mut pending_ephemerons)
  });
}

/// The walk itself, over an arbitrary rule for which edges count.
///
/// `calculateDistances` passes the three-way filter below; the named
/// node filters pass their own and NOT that one, which is why this is
/// the shared half rather than an option on `bfs`.
fn bfs_with(
  snapshot: &Snapshot,
  queue: &mut Vec<usize>,
  distances: &mut [i64],
  mut keep: impl FnMut(usize, usize, usize) -> bool,
) {
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
      if !keep(ordinal, edge, child) {
        continue;
      }
      distances[child] = distance;
      queue.push(child);
    }
  }
}

/// `JSHeapSnapshot::calculateDistances`' filter: three edges that would
/// otherwise report a shorter walk than really exists.
fn distance_filter<'a>(
  snapshot: &'a Snapshot,
  ordinal: usize,
  edge: usize,
  pending_ephemerons: &mut FxHashSet<&'a str>,
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
      pending_ephemerons.insert(pair);
      return false;
    }
  }

  true
}
