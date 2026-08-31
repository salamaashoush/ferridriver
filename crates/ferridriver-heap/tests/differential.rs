#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Our heap snapshot analysis against the engine it is a port of.
//!
//! `scripts/perf-diff/record-heap.mjs` drives the real
//! `devtools-heap-snapshot-worker.js` -- the same one DevTools and
//! `chrome-devtools-mcp` use -- over the snapshots in `tests/fixtures/`
//! and records what it concluded. This replays that, so it needs
//! neither node nor a browser. `just heap-diff` re-derives the
//! recording and fails if it has drifted.
//!
//! The snapshot itself is what is checked in, not the page: a heap
//! snapshot of a live page differs run to run in object ids, addresses
//! and how much of V8 happens to be alive, so re-capturing would make
//! the comparison a race. Re-capture is `--capture`, a deliberate act.
//!
//! What is compared is deliberately not only the summary. Every
//! predicted number in `ferridriver-perf` is one simulation minus
//! another, and a comparison of those conclusions stayed exact for a
//! whole session while the analyser underneath had one RTT estimator
//! where upstream has four. So this compares the MODEL, node by node,
//! sampled across the whole node array.
//!
//! # What the recording holds that nothing here compares yet
//!
//! The engine was asked for more than the port can currently answer,
//! so `engine.json` already carries the targets for the layers still to
//! be written: `statistics`, and per-node `name`, `selfSize`,
//! `retainedSize` and `distance`. Every one of those needs analysis
//! this crate does not do yet -- retained sizes need the dominator
//! tree, `selfSize` needs the pass that moves an owned node's size onto
//! its owner, `name` needs the cons-string and plain-object naming
//! rules, `distance` needs the two-phase breadth-first walk.
//!
//! They are recorded rather than left out so that adding each layer is
//! a matter of comparing a field that is already there, and so that the
//! gap is visible in the fixture rather than only in someone's memory.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;

use ferridriver_heap::Snapshot;
use serde::Deserialize;

#[derive(Deserialize)]
struct Recording {
  #[serde(rename = "nodeFieldCount")]
  node_field_count: usize,
  statistics: Statistics,
  #[serde(rename = "staticData")]
  static_data: StaticData,
  nodes: Vec<NodeInfo>,
}

#[derive(Deserialize)]
struct Statistics {
  total: f64,
  native: NativeStatistics,
  v8heap: V8Statistics,
}

#[derive(Deserialize)]
struct NativeStatistics {
  total: f64,
  #[serde(rename = "typedArrays")]
  typed_arrays: f64,
}

#[derive(Deserialize)]
struct V8Statistics {
  total: f64,
  code: f64,
  #[serde(rename = "jsArrays")]
  js_arrays: f64,
  strings: f64,
  system: f64,
}

#[derive(Deserialize)]
struct StaticData {
  #[serde(rename = "nodeCount")]
  node_count: usize,
  #[serde(rename = "rootNodeIndex")]
  root_node_index: usize,
  #[serde(rename = "totalSize")]
  total_size: f64,
  #[serde(rename = "maxJSObjectId")]
  max_js_object_id: u64,
}

/// One node as the engine described it.
#[derive(Deserialize)]
struct NodeInfo {
  ordinal: usize,
  id: u64,
  name: String,
  #[serde(rename = "type")]
  kind: String,
  #[serde(rename = "selfSize")]
  self_size: f64,
  #[serde(rename = "retainedSize")]
  retained_size: f64,
  distance: i64,
  detachedness: u64,
}

fn fixtures_dir() -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn recordings() -> BTreeMap<String, Recording> {
  let path = fixtures_dir().join("engine.json");
  let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
  serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// Snapshots are stored gzipped: even a trivial page's is megabytes.
fn snapshot(name: &str) -> Snapshot {
  let path = fixtures_dir().join(format!("{name}.heapsnapshot.gz"));
  let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
  let mut text = String::new();
  flate2::read::GzDecoder::new(&bytes[..])
    .read_to_string(&mut text)
    .unwrap_or_else(|e| panic!("decompress {}: {e}", path.display()));
  Snapshot::parse(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// The format layer: the column layout resolved from `meta`, and the
/// node records read through it.
///
/// This is the half that cannot be checked by eye. A reader that took
/// the field order on faith would still parse, still report a plausible
/// node count, and be reading the wrong column for every size in the
/// file.
#[test]
fn nodes_decode_the_way_the_engine_decodes_them() {
  for (name, recording) in recordings() {
    let snapshot = snapshot(&name);

    assert_eq!(
      snapshot.node_layout.field_count, recording.node_field_count,
      "{name}: node record width disagrees with the engine"
    );
    assert_eq!(
      snapshot.node_count, recording.static_data.node_count,
      "{name}: node count disagrees with the engine"
    );

    let mut differences = Vec::new();
    for node in &recording.nodes {
      let ordinal = node.ordinal;
      let id = snapshot.node_id(ordinal);
      if id != node.id {
        differences.push(format!("node {ordinal}: engine id {}, ours {id}", node.id));
      }
      let kind = snapshot.node_type_name(ordinal);
      if kind != node.kind {
        differences.push(format!("node {ordinal}: engine type {}, ours {kind}", node.kind));
      }
      let detachedness = snapshot.node_detachedness(ordinal);
      if detachedness != node.detachedness {
        differences.push(format!(
          "node {ordinal}: engine detachedness {}, ours {detachedness}",
          node.detachedness
        ));
      }
    }

    assert!(
      differences.is_empty(),
      "our reading of {name} disagrees with the DevTools heap engine on {} of {} sampled nodes:\n  {}",
      differences.len(),
      recording.nodes.len(),
      differences.join("\n  ")
    );
  }
}

/// A recording where every node is a zero-sized synthetic would agree by
/// both sides finding nothing, which is how the accessibility
/// comparison passed for a week over seven rules that all passed.
#[test]
fn the_fixtures_still_carry_something_worth_comparing() {
  for (name, recording) in recordings() {
    let sampled = recording.nodes.len();
    assert!(sampled >= 100, "{name}: only {sampled} nodes sampled");

    let kinds: std::collections::BTreeSet<&str> = recording.nodes.iter().map(|node| node.kind.as_str()).collect();
    assert!(
      kinds.len() >= 5,
      "{name}: the sample covers only {} node types, so most of the type table is untested",
      kinds.len()
    );

    let retaining = recording.nodes.iter().filter(|node| node.retained_size > 0.0).count();
    assert!(
      retaining >= 20,
      "{name}: only {retaining} sampled nodes retain anything, so the dominator tree is barely exercised"
    );

    assert!(
      recording.statistics.native.typed_arrays > 0.0,
      "{name}: no typed arrays, so the branch that finds them is untested"
    );
    assert!(
      recording.statistics.v8heap.js_arrays > 0.0,
      "{name}: no JS arrays, so calculateArraySize is untested"
    );
    assert!(
      recording.static_data.total_size > 0.0 && recording.statistics.total > 0.0,
      "{name}: the snapshot measures nothing"
    );
    assert!(
      recording.static_data.max_js_object_id > 0,
      "{name}: no JS objects at all"
    );
    assert!(
      recording.nodes.iter().any(|node| node.distance > 1),
      "{name}: every sampled node is a user root, so the distance walk is untested"
    );
    assert!(
      recording.statistics.v8heap.strings > 0.0 && recording.statistics.v8heap.code > 0.0,
      "{name}: the string and code size branches are untested"
    );
    assert!(
      recording.nodes.iter().any(|node| node.self_size > 0.0),
      "{name}: every sampled node is zero-sized"
    );
    assert!(
      !recording.nodes.iter().all(|node| node.name.is_empty()),
      "{name}: no names"
    );
    assert!(
      recording.statistics.v8heap.total > 0.0 && recording.statistics.native.total > 0.0,
      "{name}: the heap is entirely on one side of the native split"
    );
  }
}
