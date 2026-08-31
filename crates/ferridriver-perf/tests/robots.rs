#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `ferridriver::audits::robots` against the package it is a port of.
//!
//! `is-crawlable` does not parse robots.txt itself. Lighthouse imports
//! `robots-parser`, so agreeing with Lighthouse means agreeing with
//! that package -- and the parts most likely to be got wrong are the
//! ones no specification pins: which of two equal-length rules wins,
//! what an empty `Disallow:` does to the `*` fallback, how a pattern is
//! percent-encoded before it is matched against a path.
//!
//! `scripts/perf-diff/record-robots.mjs` asks the real package and
//! writes `tests/fixtures/robots.json`; this replays it, so nothing
//! here needs node. `just robots-diff` re-derives the answers and fails
//! if they have drifted.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ferridriver::audits::robots::Robots;
use serde::Deserialize;

#[derive(Deserialize)]
struct Recording {
  /// The `robots-parser` version that answered. A different one is a
  /// different set of answers.
  parser: String,
  files: BTreeMap<String, String>,
  cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
  /// Which entry of `files` was parsed.
  file: String,
  /// The URL it was served from.
  at: String,
  /// The URL asked about.
  url: String,
  /// `None` is the generic crawler.
  agent: Option<String>,
  /// `None` where the package answered `undefined`: this file governs
  /// another origin, which is not the same answer as "allowed".
  allowed: Option<bool>,
  line: i64,
}

fn recording() -> Recording {
  let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/robots.json");
  let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
  serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

#[test]
fn the_parser_answers_what_robots_parser_answers() {
  let recording = recording();
  let mut differences = Vec::new();

  for case in &recording.cases {
    let contents = recording
      .files
      .get(&case.file)
      .unwrap_or_else(|| panic!("{} names no file in the recording", case.file));
    let robots = Robots::parse(&case.at, contents);
    let agent = case.agent.as_deref();
    let where_ = format!("{} {} {:?}", case.file, case.url, case.agent);

    let allowed = robots.is_allowed(&case.url, agent);
    if allowed != case.allowed {
      differences.push(format!(
        "{where_}: robots-parser said isAllowed {:?}, we said {allowed:?}",
        case.allowed
      ));
    }
    let line = robots.matching_line_number(&case.url, agent);
    if line != case.line {
      differences.push(format!(
        "{where_}: robots-parser said line {}, we said {line}",
        case.line
      ));
    }
  }

  assert!(
    differences.is_empty(),
    "our robots.txt parser disagrees with robots-parser {} on {} of {} answers:\n  {}",
    recording.parser,
    differences.len(),
    recording.cases.len(),
    differences.join("\n  ")
  );
}

/// A recording where everything is allowed would agree by finding
/// nothing, the way the accessibility comparison did before a fixture
/// existed that actually tripped rules.
#[test]
fn the_recording_still_covers_all_three_answers() {
  let recording = recording();
  let count = |want: Option<bool>| recording.cases.iter().filter(|case| case.allowed == want).count();

  assert!(count(Some(false)) >= 100, "too few blocked URLs to be evidence");
  assert!(count(Some(true)) >= 50, "too few allowed URLs to be evidence");
  assert!(
    count(None) >= 20,
    "no case where the file governs another origin, which is the answer most easily confused with `allowed`"
  );
}
