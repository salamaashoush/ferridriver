#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Our heap snapshot analysis against the engine it is a port of.
//!
//! `scripts/perf-diff/record-heap.mjs` drives the real
//! `devtools-heap-snapshot-worker.js` -- the same one `DevTools` and
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
//! Per-node `name`, which needs the cons-string and plain-object naming
//! rules. It is recorded rather than left out so that adding the layer
//! is a matter of comparing a field that is already there, and so the
//! gap is visible in the fixture rather than only in someone's memory.
//!
//! # What this fixture cannot exercise
//!
//! A snapshot taken over CDP -- by `HeapProfiler.takeHeapSnapshot`,
//! which is the only way this tool will ever obtain one -- has no user
//! roots: the synthetic root's single child is `(GC roots)`, and every
//! child of that is synthetic too. Measured, not assumed: over http and
//! file, with and without `exposeInternals`, `captureNumericValue` and
//! `treatGlobalObjectsAsRoots`.
//!
//! Upstream reads no user roots as "the snapshot was taken with
//! internals exposed" and skips `calculateShallowSizes` entirely, so
//! three passes never run on either side -- the shallow-size transfer,
//! the page-object marking that feeds the essential-edge filter, and
//! the first half of the distance walk. Both implementations agree
//! about that, which is why this comparison passes; it is agreement
//! about a branch neither side takes.
//!
//! Two smaller branches are unexercised for their own reasons, both
//! established by removing the code and watching this pass: the
//! hidden-node branch of the statistics pass, because nothing hidden in
//! this snapshot has a size (`system` is 0), and the single-retainer
//! test in the JS-array measurement, because every backing store here
//! has exactly one retainer. The measurement itself IS gated -- drop
//! the backing store and `jsArrays` reports 256 against the engine's
//! 7920.
//!
//! Closing these needs a snapshot with user roots in it, which a
//! browser will not produce over CDP; it has to be built by hand and
//! run through the same engine. Until then this comparison is evidence
//! about the format, the dominator tree, the distance walk from the
//! system root, and the statistics -- and about nothing else.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;

use ferridriver_heap::{Analysis, Snapshot};
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
  total: u64,
  native: NativeStatistics,
  v8heap: V8Statistics,
}

#[derive(Deserialize)]
struct NativeStatistics {
  total: u64,
  #[serde(rename = "typedArrays")]
  typed_arrays: u64,
}

#[derive(Deserialize)]
struct V8Statistics {
  total: u64,
  code: u64,
  #[serde(rename = "jsArrays")]
  js_arrays: u64,
  strings: u64,
  system: u64,
}

#[derive(Deserialize)]
struct StaticData {
  #[serde(rename = "nodeCount")]
  node_count: usize,
  #[serde(rename = "rootNodeIndex")]
  root_node_index: usize,
  #[serde(rename = "totalSize")]
  total_size: u64,
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
  self_size: u64,
  #[serde(rename = "retainedSize")]
  retained_size: u64,
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
fn analysed(name: &str) -> Analysis {
  Analysis::new(snapshot(name))
}

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
      "our reading of {name} disagrees with the `DevTools` heap engine on {} of {} sampled nodes:\n  {}",
      differences.len(),
      recording.nodes.len(),
      differences.join("\n  ")
    );
  }
}

/// Everything derived from the graph: the sizes after an owned node's
/// size moves to its owner, what each node's dominated subtree holds,
/// and how far each node sits from a user root.
///
/// These are the numbers nothing else can check. A retained size is a
/// sum over a dominator tree built by an algorithm whose output is not
/// inspectable by eye, and a wrong one still looks like a size.
#[test]
fn the_analysis_agrees_with_the_engine() {
  for (name, recording) in recordings() {
    let analysis = analysed(&name);

    assert_eq!(
      analysis.total_size(),
      recording.static_data.total_size,
      "{name}: total heap size disagrees with the engine"
    );

    let statistics = analysis.statistics();
    let engine = &recording.statistics;
    assert_eq!(statistics.total, engine.total, "{name}: total");
    assert_eq!(statistics.native.total, engine.native.total, "{name}: native total");
    assert_eq!(
      statistics.native.typed_arrays, engine.native.typed_arrays,
      "{name}: typed arrays"
    );
    assert_eq!(statistics.v8heap.total, engine.v8heap.total, "{name}: v8 heap total");
    assert_eq!(statistics.v8heap.code, engine.v8heap.code, "{name}: code");
    assert_eq!(
      statistics.v8heap.js_arrays, engine.v8heap.js_arrays,
      "{name}: JS arrays"
    );
    assert_eq!(statistics.v8heap.strings, engine.v8heap.strings, "{name}: strings");
    assert_eq!(statistics.v8heap.system, engine.v8heap.system, "{name}: system");

    let mut differences = Vec::new();
    for node in &recording.nodes {
      let ordinal = node.ordinal;
      let self_size = analysis.snapshot.node_self_size(ordinal);
      if self_size != node.self_size {
        differences.push(format!(
          "node {ordinal}: engine selfSize {}, ours {self_size}",
          node.self_size
        ));
      }
      let retained = analysis.retained_size(ordinal);
      if retained != node.retained_size {
        differences.push(format!(
          "node {ordinal}: engine retainedSize {}, ours {retained}",
          node.retained_size
        ));
      }
      let distance = analysis.distance(ordinal);
      if distance != node.distance {
        differences.push(format!(
          "node {ordinal}: engine distance {}, ours {distance}",
          node.distance
        ));
      }
    }

    assert!(
      differences.is_empty(),
      "our analysis of {name} disagrees with the `DevTools` heap engine on {} of {} sampled nodes:\n  {}",
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

    let retaining = recording.nodes.iter().filter(|node| node.retained_size > 0).count();
    assert!(
      retaining >= 20,
      "{name}: only {retaining} sampled nodes retain anything, so the dominator tree is barely exercised"
    );

    assert!(
      recording.statistics.native.typed_arrays > 0,
      "{name}: no typed arrays, so the branch that finds them is untested"
    );
    assert!(
      recording.statistics.v8heap.js_arrays > 0,
      "{name}: no JS arrays, so calculateArraySize is untested"
    );
    assert!(
      recording.static_data.total_size > 0 && recording.statistics.total > 0,
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
      recording.statistics.v8heap.strings > 0 && recording.statistics.v8heap.code > 0,
      "{name}: the string and code size branches are untested"
    );
    assert!(
      recording.nodes.iter().any(|node| node.self_size > 0),
      "{name}: every sampled node is zero-sized"
    );
    assert!(
      !recording.nodes.iter().all(|node| node.name.is_empty()),
      "{name}: no names"
    );
    assert!(
      recording.statistics.v8heap.total > 0 && recording.statistics.native.total > 0,
      "{name}: the heap is entirely on one side of the native split"
    );
    assert_eq!(
      recording.static_data.root_node_index, 0,
      "{name}: the root is not the first node, which every index here assumes"
    );
    assert_eq!(
      recording.statistics.total,
      recording.statistics.native.total + recording.statistics.v8heap.total,
      "{name}: the recorded totals do not add up, so one of them is not what it claims"
    );
    assert!(
      recording.statistics.v8heap.system <= recording.statistics.v8heap.total,
      "{name}: the system size exceeds the heap it is part of"
    );
  }
}
