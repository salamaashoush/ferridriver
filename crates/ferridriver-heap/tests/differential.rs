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
//! # Two fixtures, because one of them cannot reach half the engine
//!
//! A snapshot taken over CDP -- by `HeapProfiler.takeHeapSnapshot`,
//! which is the only way this tool will ever obtain one -- has no user
//! roots: the synthetic root's single child is `(GC roots)`, and every
//! child of that is synthetic too. Measured, not assumed: over http and
//! file, with and without `exposeInternals`, `captureNumericValue` and
//! `treatGlobalObjectsAsRoots`, and through Puppeteer's own
//! `captureHeapSnapshot`, which is what `chrome-devtools-mcp` calls.
//!
//! Upstream reads no user roots as "the snapshot was taken with
//! internals exposed" and skips `calculateShallowSizes` entirely, so
//! over `leaky` three passes never run on either side -- the
//! shallow-size transfer, the page-object marking that feeds the
//! essential-edge filter, and the first half of the distance walk.
//! Agreement there is agreement about a branch neither side takes.
//!
//! `handmade` exists for exactly those branches, built by
//! `scripts/perf-diff/make-heapsnapshot.mjs` the way upstream's own
//! `HeapSnapshot.test.ts` builds snapshots. It is still a differential:
//! the real engine analyses it too, and the recording is whatever IT
//! concluded. Twenty nodes, each there to make one branch discriminate
//! -- a backing store with one retainer and another with two, a hidden
//! node the JS-array branch would otherwise claim, an ephemeron pair,
//! a weak-only retainer, a detached subtree.
//!
//! It earned its place immediately: it found the ephemeron name parser
//! matching nothing, so both edges of a `WeakMap` pair counted and the
//! value came out dominated by the window rather than by its key.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;

use ferridriver_heap::{Analysis, DominatorStep, DuplicateStringGroup, EdgeSummary, ObjectInfo, Snapshot};
use serde::Deserialize;

#[derive(Deserialize)]
struct Recording {
  #[serde(rename = "nodeFieldCount")]
  node_field_count: usize,
  statistics: Statistics,
  #[serde(rename = "staticData")]
  static_data: StaticData,
  nodes: Vec<NodeInfo>,
  /// The node-addressed queries, over a sub-sample of `nodes`.
  queried: Vec<Queried>,
  #[serde(rename = "duplicateStrings")]
  duplicate_strings: Vec<DuplicateStringGroup>,
}

/// What the engine's providers answered about one node.
#[derive(Deserialize)]
struct Queried {
  ordinal: usize,
  #[serde(rename = "objectInfo")]
  object_info: ObjectInfo,
  dominators: Vec<DominatorStep>,
  edges: Vec<EdgeSummary>,
  retainers: Vec<EdgeSummary>,
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

fn analysed(name: &str) -> Analysis {
  Analysis::new(snapshot(name))
}

/// Captured snapshots are stored gzipped, because even a trivial page's
/// is megabytes; the hand-built one is stored plain, because the point
/// of it is that a reader can follow it.
fn snapshot(name: &str) -> Snapshot {
  let plain = fixtures_dir().join(format!("{name}.heapsnapshot"));
  let text = if plain.exists() {
    std::fs::read_to_string(&plain).unwrap_or_else(|e| panic!("read {}: {e}", plain.display()))
  } else {
    let path = fixtures_dir().join(format!("{name}.heapsnapshot.gz"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut text = String::new();
    flate2::read::GzDecoder::new(&bytes[..])
      .read_to_string(&mut text)
      .unwrap_or_else(|e| panic!("decompress {}: {e}", path.display()));
    text
  };
  Snapshot::parse(&text).unwrap_or_else(|e| panic!("parse {name}: {e}"))
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
      let name = analysis.node_name(ordinal);
      if name != node.name {
        differences.push(format!("node {ordinal}: engine name {:?}, ours {name:?}", node.name));
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

/// The reads the heap-snapshot tools are made of: what a node is, what
/// it points at, what points at it, and what would have to let go for
/// it to be freed.
///
/// Compared whole rather than field by field. These are the structures
/// the tools hand back, and a field that quietly stopped being filled
/// in would be invisible to a comparison that only checked the fields
/// someone remembered to name.
#[test]
fn the_queries_answer_what_the_engine_answers() {
  for (name, recording) in recordings() {
    let analysis = analysed(&name);
    let mut differences = Vec::new();

    for queried in &recording.queried {
      let ordinal = queried.ordinal;

      let ours = analysis.object_info(ordinal);
      if ours != queried.object_info {
        differences.push(format!(
          "node {ordinal} object info:\n    engine {:?}\n    ours   {ours:?}",
          queried.object_info
        ));
      }

      let ours = analysis.dominator_chain(ordinal);
      if ours != queried.dominators {
        differences.push(format!(
          "node {ordinal} dominators: engine {} step(s), ours {}\n    engine {:?}\n    ours   {ours:?}",
          queried.dominators.len(),
          ours.len(),
          queried.dominators
        ));
      }

      compare_edges(
        &mut differences,
        ordinal,
        "edges",
        &analysis.edges_of(ordinal),
        &queried.edges,
      );
      compare_edges(
        &mut differences,
        ordinal,
        "retainers",
        &analysis.retainers_of(ordinal),
        &queried.retainers,
      );
    }

    assert!(
      differences.is_empty(),
      "our queries over {name} disagree with the `DevTools` heap engine on {} of {} nodes:\n  {}",
      differences.len(),
      recording.queried.len(),
      differences.join("\n  ")
    );

    assert!(
      !recording.queried.is_empty(),
      "{name}: no node was queried, so this compared nothing"
    );

    let ours = analysis.duplicate_strings();
    assert_eq!(
      ours.len(),
      recording.duplicate_strings.len(),
      "{name}: engine found {} duplicated string(s), we found {}",
      recording.duplicate_strings.len(),
      ours.len()
    );
    for (at, (ours, engine)) in ours.iter().zip(&recording.duplicate_strings).enumerate() {
      assert_eq!(ours, engine, "{name}: duplicated string group {at}");
    }
  }
}

fn compare_edges(
  differences: &mut Vec<String>,
  ordinal: usize,
  what: &str,
  ours: &[EdgeSummary],
  engine: &[EdgeSummary],
) {
  if ours.len() != engine.len() {
    differences.push(format!(
      "node {ordinal} {what}: engine {} , ours {}",
      engine.len(),
      ours.len()
    ));
    return;
  }
  for (at, (ours, engine)) in ours.iter().zip(engine).enumerate() {
    if ours != engine {
      differences.push(format!(
        "node {ordinal} {what}[{at}]:\n    engine {engine:?}\n    ours   {ours:?}"
      ));
    }
  }
}

/// A recording where every node is a zero-sized synthetic would agree by
/// both sides finding nothing, which is how the accessibility
/// comparison passed for a week over seven rules that all passed.
#[test]
fn the_fixtures_still_carry_something_worth_comparing() {
  for (name, recording) in recordings() {
    let sampled = recording.nodes.len();
    // The hand-built snapshot is small on purpose: every node in it
    // exists to make one branch discriminate, and the assertions below
    // name them.
    let handmade = name == "handmade";
    let (least_nodes, least_retaining) = if handmade { (15, 10) } else { (100, 20) };
    assert!(sampled >= least_nodes, "{name}: only {sampled} nodes sampled");

    let kinds: std::collections::BTreeSet<&str> = recording.nodes.iter().map(|node| node.kind.as_str()).collect();
    assert!(
      kinds.len() >= 5,
      "{name}: the sample covers only {} node types, so most of the type table is untested",
      kinds.len()
    );

    let retaining = recording.nodes.iter().filter(|node| node.retained_size > 0).count();
    assert!(
      retaining >= least_retaining,
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

    if handmade {
      the_handmade_shapes_are_all_still_there(&name, &recording);
    }
  }
}

/// The shapes the hand-built fixture exists for.
///
/// Each is what makes one branch tell itself apart from its own
/// absence -- every one of them was confirmed by deleting the code that
/// handles it and watching this comparison go red. Losing a shape turns
/// a passing comparison back into a vacuous one, which is the failure
/// this whole file is arranged against.
fn the_handmade_shapes_are_all_still_there(name: &str, recording: &Recording) {
  // The shapes this fixture exists for. Each is what makes one
  // branch tell itself apart from its own absence, so losing any of
  // them turns a passing comparison back into a vacuous one.
  assert!(
    recording.statistics.v8heap.system > 0,
    "{name}: nothing hidden has a size, so the statistics early exit is untested"
  );
  assert!(
    recording.nodes.iter().any(|node| node.distance == 1),
    "{name}: no node sits at distance 1, so there are no user roots and three passes do not run"
  );
  assert!(
    recording.nodes.iter().any(|node| node.detachedness == 2),
    "{name}: nothing is detached, so the propagation is untested"
  );
  assert!(
    recording.nodes.iter().any(|node| node.name.starts_with("Detached ")),
    "{name}: nothing was renamed, so the detached-name rewrite is untested"
  );
  assert!(
    recording.nodes.iter().any(|node| node.distance < 0),
    "{name}: everything is reachable, so the weak-retainer walk is untested"
  );
  assert!(
    recording
      .nodes
      .iter()
      .any(|node| node.self_size == 0 && node.kind == "array"),
    "{name}: no backing store was emptied, so the shallow-size transfer is untested"
  );
  assert!(
    recording
      .nodes
      .iter()
      .any(|node| node.kind == "concatenated string" && node.name.contains(' ')),
    "{name}: no assembled string, so the cons-string walk is untested"
  );
  assert!(
    recording
      .nodes
      .iter()
      .any(|node| node.name.starts_with('{') && node.name.contains('"')),
    "{name}: no plain object with a quoted property, so the label escaping is untested"
  );
  assert!(
    recording.nodes.iter().any(|node| node.name.contains('…')),
    "{name}: no plain object overflowed its label, so the budget is untested"
  );

  let duplicated = recording
    .duplicate_strings
    .iter()
    .find(|group| group.value == "dup text")
    .unwrap_or_else(|| panic!("{name}: the duplicated-string group is gone"));
  // Two concatenations and one plain string read the same. A
  // concatenation V8 has flattened and a zero-sized encoding of a
  // number read the same too and are excluded, so a count of anything
  // but three means one of those exclusions stopped mattering.
  assert_eq!(
    duplicated.count, 3,
    "{name}: the duplicated-string group holds {} members, so an exclusion has stopped discriminating",
    duplicated.count
  );
}
