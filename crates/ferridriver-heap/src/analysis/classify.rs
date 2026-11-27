//! Heap object class naming and plain-object interface inference.

use rustc_hash::FxHashMap;

use super::format_property_name;
use crate::format::Snapshot;

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
pub(super) fn calculate_object_names(snapshot: &mut Snapshot) -> Vec<usize> {
  let mut interned: FxHashMap<String, usize> = FxHashMap::default();
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
      // `<div id="a">` groups as `<div>`. The prefixed form is what a
      // detached node was renamed to earlier. Check without allocating for
      // ordinary non-element object names.
      if let Some(tag) = element_tag(snapshot.raw_node_name(ordinal)) {
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
    let Some(rest) = name.strip_prefix(prefix) else {
      continue;
    };
    let Some((tag, _)) = rest.split_once(' ') else {
      return Some(name.to_string());
    };
    return Some(format!("{prefix}{tag}>"));
  }
  None
}

/// Only a plain `Object` is reshaped; everything else already has a
/// name worth grouping by.
pub(super) fn is_plain_js_object(snapshot: &Snapshot, ordinal: usize) -> bool {
  snapshot.node_type(ordinal) == snapshot.node_types.object && snapshot.raw_node_name(ordinal) == "Object"
}

/// Name the shapes that plain objects keep repeating.
///
/// Every plain `Object` reports as `Object`, which groups a page's
/// entire object graph into one line. Upstream instead reads each one's
/// properties IN ORDER, counts how often each sequence recurs, and
/// keeps the sequences shared by at least two objects and a thousandth
/// of them.
pub(super) fn infer_interface_definitions(snapshot: &Snapshot) -> Vec<InterfaceDefinition> {
  // Insertion-ordered, because ties are broken by which was seen first.
  let mut order: Vec<String> = Vec::new();
  let mut candidates: FxHashMap<String, (Vec<String>, usize)> = FxHashMap::default();
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
pub(super) struct InterfaceIndex {
  tree: Vec<PropertyTreeNode>,
}

impl InterfaceIndex {
  pub(super) fn build(definitions: &[InterfaceDefinition]) -> Self {
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
  pub(super) fn best_match(&self, snapshot: &Snapshot, ordinal: usize) -> Option<Match> {
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
pub(super) fn apply_interface_definitions(
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

  let mut interned: FxHashMap<String, usize> = FxHashMap::default();
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
pub(super) struct Match {
  pub(super) name: String,
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
