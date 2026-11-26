//! Chrome tracing categories to record a performance trace with.
//!
//! Mirrors Lighthouse's `getDefaultTraceCategories`
//! (`core/gather/gatherers/trace.js`), which is also what the `DevTools`
//! Performance panel records. The leading `-*` drops Chrome's own
//! defaults so only the categories the insight handlers actually read
//! are captured: a default trace is several times larger and none of
//! the extra events are ever looked at.

/// The category set a trace is recorded with when the caller names none.
pub const DEFAULT: &[&str] = &[
  "-*",
  // Used instead of `toplevel` in Chrome 71+.
  "disabled-by-default-lighthouse",
  // Cumulative Layout Shift.
  "loading",
  // Compile/execute events are already captured by their
  // devtools.timeline parents; v8 adds context for <0.5% of trace size.
  "v8",
  // Carries RunMicrotasks, which no parent event is guaranteed to wrap.
  "v8.execute",
  // UserTiming marks and measures.
  "blink.user_timing",
  "blink.console",
  // Where most of what the handlers read comes from.
  "devtools.timeline",
  "disabled-by-default-devtools.timeline",
  // Filmstrip screenshots.
  "disabled-by-default-devtools.screenshot",
  // Adds `stackTrace` to devtools.timeline events rather than its own.
  "disabled-by-default-devtools.timeline.stack",
  "disabled-by-default-devtools.timeline.frame",
  "latencyInfo",
  "disabled-by-default-devtools.target-rundown",
  "disabled-by-default-devtools.v8-source-rundown-sources",
  "disabled-by-default-devtools.v8-source-rundown",
  "blink.webdx_feature_usage",
];
