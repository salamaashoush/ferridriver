//! V8 heap snapshot analysis, ported from `DevTools`' own engine.
//!
//! A `.heapsnapshot` is flat integer arrays plus a `meta` describing the
//! column layout; every question anyone asks of it -- what retains this
//! object, what does it retain, which class is holding the memory -- is
//! a query over the graph those arrays encode. The graph is built here
//! the way `front_end/entrypoints/heap_snapshot_worker/HeapSnapshot.ts`
//! builds it, in the same order, because the order is load-bearing:
//! shallow sizes are moved from owned nodes to their owners BEFORE
//! retained sizes are propagated, so doing it afterwards changes every
//! number downstream.
//!
//! # Checked against the engine, not against a reading of the format
//!
//! `scripts/perf-diff/record-heap.mjs` runs the real
//! `devtools-heap-snapshot-worker.js` over the snapshots in
//! `tests/fixtures/` and records what it concluded;
//! `cargo test -p ferridriver-heap --test differential` replays that
//! offline. `just heap-diff` re-derives it.
//!
//! That harness exists because the alternative has already failed once
//! here: `ferridriver-perf` passed 62 of its own tests and was still
//! wrong in ten places, every one of them found by running the engine
//! it was a port of. A heap snapshot is a worse case still, because
//! almost nothing in it is checkable by eye.

//!
//! # What is here
//!
//! The graph and everything derived from it ([`analysis`]), the reads a
//! caller addresses by node ([`query`]), the two searches that find a
//! node in the first place ([`paths`], [`search`]) and what changed
//! between two snapshots of one heap ([`diff`]).

pub mod analysis;
pub mod diff;
pub mod error;
pub mod format;
pub mod paths;
pub mod query;
pub mod search;

pub use analysis::{Analysis, Classification, InterfaceDefinition, NativeStatistics, Statistics, V8Statistics};
pub use diff::ClassDiff;
pub use error::{HeapError, Result};
pub use format::Snapshot;
pub use paths::{LimitsReached, PathLimits, RetainingEdge, RetainingPaths};
pub use query::{
  Aggregate, DominatorStep, DuplicateStringGroup, DuplicateStringNode, EdgeSummary, NodeSummary, ObjectInfo,
};
pub use search::{ObjectQuery, QuerySort};
