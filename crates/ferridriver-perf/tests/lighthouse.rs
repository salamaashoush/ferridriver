#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Our two live-page audits against Lighthouse's, on the fixture pages
//! in `tests/fixtures/lighthouse/`.
//!
//! `page.check_accessibility()` first, then `page.check_page_quality()`.
//! Both read the same recordings, because `lighthouse.mjs` records
//! everything Lighthouse concluded about a page and each comparison
//! takes the part it needs.
//!
//! # Accessibility
//!
//! Both sides run axe-core, pinned to the same version, so where they
//! overlap they must agree exactly: the same rules failing, on the same
//! number of elements. What they do not share is scope. Lighthouse
//! narrows axe to the rules its own audits wrap and reports only those;
//! this runs the engine and reports every rule it has. Extra rules on
//! our side are the point, so the comparison is over the intersection --
//! but only in that direction. A rule Lighthouse reached and we never
//! ran is a gap whichever way its verdict went, and fails here. Three
//! did, which is why `LIGHTHOUSE_ENABLED_RULES` exists.
//!
//! # Page quality
//!
//! The ten audits that score a live page, ported in
//! `ferridriver::audits`. This is the comparison with no shared engine
//! underneath: both sides are separate implementations of the same
//! arithmetic, so nothing agrees by construction. Verdicts and element
//! counts are both compared, because an audit can reach the right
//! answer from the wrong set of elements.
//!
//! # The fixtures
//!
//! Unlike the trace differential the input cannot be a recording: an
//! audit has to look at a live DOM. So the PAGE is what is checked in,
//! served here on a loopback port, and Lighthouse's verdict about it is
//! the recording beside it. That keeps this offline -- no node, no
//! Lighthouse, no network. `just lh-record` re-derives the recordings
//! and `scripts/perf-diff/record-lighthouse.py` with no argument proves
//! they have not drifted.
//!
//! Each page is broken in one dimension and sound in the others, so
//! neither comparison can agree because both sides found nothing. That
//! is what the first two fixtures were doing: seven rules compared, all
//! seven passing. `is-crawlable` gets three pages of its own for the
//! same reason -- it has three independent blocking sources, and a page
//! tripping all three would agree whichever two went unread.
//!
//! Two of the pages are not just a page: a `<name>.http` sidecar gives
//! the status and headers the fixture is served with, and both this
//! server and the recorder's read it, so the two cannot drift.
//!
//! Needs a Chromium and axe-core on disk:
//! `ferridriver install chromium axe`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::thread;

use ferridriver::audits::AUDIT_IDS;
use serde::Deserialize;

/// What `scripts/perf-diff/lighthouse.mjs` recorded.
#[derive(Deserialize)]
struct Recording {
  page: String,
  lighthouse: String,
  /// The audit ids that are axe rules, taken from Lighthouse's own
  /// accessibility category rather than a list maintained by hand.
  /// Outside it an id like `meta-description` is Lighthouse's own audit
  /// and means nothing to axe.
  #[serde(rename = "axeRules")]
  axe_rules: BTreeSet<String>,
  audits: BTreeMap<String, Audit>,
}

#[derive(Deserialize)]
struct Audit {
  /// 1 pass, 0 fail, null for the modes that do not score.
  score: Option<f64>,
  mode: String,
  /// How many elements the audit objected to.
  #[serde(rename = "itemCount", default)]
  item_count: usize,
}

impl Audit {
  /// How many elements Lighthouse objected to, counting a passing audit
  /// as none: a passing audit can still carry rows listing what it
  /// checked, so the score has to be consulted first.
  fn failing_items(&self) -> usize {
    if self.score == Some(0.0) { self.item_count } else { 0 }
  }
}

fn fixtures_dir() -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lighthouse")
}

fn read_recording(path: &std::path::Path) -> Recording {
  let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
  serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// What Lighthouse said about a page, snapshot first.
///
/// Snapshot is the recording of record: it reads the page as it stands,
/// so its verdicts are the ones a `checkPageQuality()` on a settled page
/// should match. Navigation only fills in the audits snapshot cannot
/// score at all -- `canonical` resolves against the main resource, so
/// Lighthouse marks it notApplicable without a network log. Merging the
/// other way would silently swap in verdicts reached under navigation's
/// own emulation.
fn recording(name: &str) -> Recording {
  let dir = fixtures_dir();
  let mut snapshot = read_recording(&dir.join(format!("{name}.lighthouse.json")));
  let navigation = read_recording(&dir.join(format!("{name}.navigation.json")));
  for (id, audit) in navigation.audits {
    snapshot.audits.entry(id).or_insert(audit);
  }
  snapshot
}

/// The status line and extra headers a `<name>.http` sidecar asks for.
///
/// `http-status-code` and `is-crawlable` are functions of the response
/// rather than of the page, and a status or a header has nowhere to
/// live in an HTML file. The sidecar carries them: one line of status,
/// then headers, reading as the response head it becomes.
/// `scripts/perf-diff/record-lighthouse.py` parses the same file, which
/// is why it is a file rather than a rule written twice.
fn response_head(page: &std::path::Path) -> (String, Vec<String>) {
  let Ok(sidecar) = std::fs::read_to_string(page.with_extension("http")) else {
    return ("200 OK".to_string(), Vec::new());
  };
  let mut lines = sidecar.lines();
  let status = lines.next().unwrap_or("200 OK").to_string();
  (
    status,
    lines.filter(|line| line.contains(':')).map(str::to_string).collect(),
  )
}

/// Serve the fixture directory on a loopback port, returning its origin.
///
/// A thread per connection and `Connection: close` on every reply, both
/// deliberately: a browser opens speculative connections that carry no
/// request and hold them idle, and an accept loop that reads them one at
/// a time starves the request that matters.
fn serve() -> String {
  let dir = fixtures_dir();
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
  let addr = listener.local_addr().expect("local addr");
  thread::spawn(move || {
    for stream in listener.incoming() {
      let Ok(mut stream) = stream else { continue };
      let dir = dir.clone();
      thread::spawn(move || {
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        let Ok(peer) = stream.try_clone() else { return };
        let mut reader = BufReader::new(peer);
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).is_err() || request_line.is_empty() {
          return;
        }
        let path = request_line.split_whitespace().nth(1).unwrap_or("/").to_string();
        // A file name and nothing else: no traversal, no directories.
        let name = path.split('?').next().unwrap_or("/").trim_start_matches('/');
        let page = dir.join(name);
        let body = if name.contains('/') || name.contains("..") {
          None
        } else {
          std::fs::read(&page).ok()
        };
        let response = match body {
          Some(bytes) => {
            let (status, extra) = response_head(&page);
            let content_type = match page.extension().and_then(std::ffi::OsStr::to_str) {
              Some("html") => "text/html; charset=utf-8",
              Some("txt") => "text/plain; charset=utf-8",
              _ => "application/octet-stream",
            };
            let mut head = format!(
              "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
              bytes.len()
            );
            for header in extra {
              head.push_str(&header);
              head.push_str("\r\n");
            }
            head.push_str("\r\n");
            [head.into_bytes(), bytes].concat()
          },
          None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
        };
        let _ = stream.write_all(&response);
        let _ = stream.flush();
      });
    }
  });
  format!("http://{addr}")
}

/// Open a fixture page in a real Chromium and hand it to `read`.
fn on_page<T, F>(page_name: &str, read: F) -> T
where
  F: AsyncFnOnce(&std::sync::Arc<ferridriver::Page>) -> T,
{
  let url = format!("{}/{page_name}", serve());
  tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .expect("tokio runtime")
    .block_on(async move {
      let browser = ferridriver::browser_type::chromium()
        .launch(ferridriver::options::LaunchOptions {
          headless: Some(true),
          ..Default::default()
        })
        .await
        .expect("launch chromium");
      let context = browser.new_context().await.expect("new context");
      let page = context.new_page().await.expect("new page");
      page.goto(&url).await.expect("goto fixture page");
      page.wait_for_load_state(None).await.expect("load state");
      let result = read(&page).await;
      browser.close().await.expect("close browser");
      result
    })
}

// ── Accessibility ───────────────────────────────────────────────────────

/// The floor each page has to keep clearing.
///
/// A fixture that makes everything pass is not evidence: before
/// `a11y-broken.html` existed this comparison ran over seven rules on
/// pages where all seven passed, and agreement meant almost nothing.
/// These numbers fail if a fixture is quietly defused -- edited until it
/// stops tripping rules, or re-recorded after being broken.
struct Floor {
  /// Rules both engines reached a verdict on.
  compared: usize,
  /// Of those, how many Lighthouse says the page fails.
  failing: usize,
}

fn compare_accessibility(name: &str, floor: &Floor) {
  let recording = recording(name);
  let ours = on_page(&recording.page, async |page| {
    page
      .check_accessibility(None)
      .await
      .expect("run the accessibility audit (is axe installed? `ferridriver install axe`)")
  });

  let failing: BTreeMap<&str, usize> = ours
    .violations
    .iter()
    .map(|rule| (rule.id.as_str(), rule.nodes.len()))
    .collect();
  // Every rule the engine reached, whatever it concluded. `inapplicable`
  // belongs here: a rule that found nothing to look at still ran, and
  // leaving it out would read as a coverage gap.
  let known: BTreeSet<&str> = [&ours.violations, &ours.passes, &ours.incomplete, &ours.inapplicable]
    .into_iter()
    .flatten()
    .map(|rule| rule.id.as_str())
    .collect();

  let mut differences = Vec::new();
  let (mut compared, mut their_failures) = (0usize, 0usize);
  for (id, audit) in &recording.audits {
    if !recording.axe_rules.contains(id) || audit.mode != "binary" {
      continue;
    }
    if !known.contains(id.as_str()) {
      differences.push(format!("{id}: Lighthouse evaluated the rule, we never ran it"));
      continue;
    }
    compared += 1;
    let theirs = audit.failing_items();
    if theirs > 0 {
      their_failures += 1;
    }
    let our = failing.get(id.as_str()).copied().unwrap_or(0);
    if (theirs > 0) != (our > 0) {
      let verdict = |n: usize| if n > 0 { "failed" } else { "passed" };
      differences.push(format!(
        "{id}: Lighthouse {} it, we {} it",
        verdict(theirs),
        verdict(our)
      ));
    } else if theirs != our {
      differences.push(format!("{id}: Lighthouse found {theirs} element(s), we found {our}"));
    }
  }

  assert!(
    differences.is_empty(),
    "{} disagrees with Lighthouse {} on {}:\n  {}",
    ours.engine,
    recording.lighthouse,
    recording.page,
    differences.join("\n  ")
  );
  assert!(
    compared >= floor.compared,
    "{} compared only {compared} rules against Lighthouse, below the {} this fixture is meant to reach",
    recording.page,
    floor.compared
  );
  assert!(
    their_failures >= floor.failing,
    "{} tripped only {their_failures} rules, below the {} this fixture is meant to trip",
    recording.page,
    floor.failing
  );
}

#[test]
fn accessibility_on_a_page_full_of_barriers() {
  compare_accessibility(
    "a11y-broken",
    &Floor {
      compared: 30,
      failing: 20,
    },
  );
}

#[test]
fn accessibility_on_a_page_with_nothing_wrong_with_it() {
  compare_accessibility(
    "clean",
    &Floor {
      compared: 18,
      failing: 0,
    },
  );
}

/// The page built to fail the SEO and best-practices audits, which
/// should be clean here.
#[test]
fn accessibility_on_a_page_broken_in_the_other_dimension() {
  compare_accessibility(
    "quality-broken",
    &Floor {
      compared: 10,
      failing: 0,
    },
  );
}

// ── Page quality ────────────────────────────────────────────────────────

fn compare_page_quality(name: &str, expected_failures: usize) {
  let recording = recording(name);
  let ours = on_page(&recording.page, async |page| {
    page.check_page_quality(None).await.expect("run the page audits")
  });

  let mut differences = Vec::new();
  let (mut compared, mut failures) = (0usize, 0usize);
  for id in AUDIT_IDS {
    // Lighthouse reports `notApplicable` when the page had nothing for
    // an audit to look at, and the recorder drops those: not a verdict,
    // so nothing to compare.
    let Some(audit) = recording.audits.get(id) else {
      continue;
    };
    if audit.mode != "binary" {
      continue;
    }
    let Some(our) = ours.audits.iter().find(|our| our.id == id) else {
      differences.push(format!("{id}: Lighthouse scored it, we did not run it"));
      continue;
    };
    compared += 1;
    let their_items = audit.failing_items();
    let their_pass = audit.score == Some(1.0);
    if !their_pass {
      failures += 1;
    }
    if their_pass != our.passed {
      differences.push(format!(
        "{id}: Lighthouse {} it, we {} it",
        if their_pass { "passed" } else { "failed" },
        if our.passed { "passed" } else { "failed" }
      ));
    } else if their_items != our.items.len() {
      differences.push(format!(
        "{id}: Lighthouse found {their_items} element(s), we found {}",
        our.items.len()
      ));
    }
  }

  assert!(
    differences.is_empty(),
    "our page audits disagree with Lighthouse {} on {}:\n  {}",
    recording.lighthouse,
    recording.page,
    differences.join("\n  ")
  );
  assert_eq!(
    failures, expected_failures,
    "{} was expected to fail {expected_failures} of the ten and Lighthouse failed {failures}",
    recording.page
  );
  assert!(
    compared > 0,
    "{} scored none of the ten, so this compared nothing",
    recording.page
  );
}

#[test]
fn page_quality_on_a_page_failing_seven_of_the_ten() {
  // Not `canonical`, which a page with no canonical link scores as
  // notApplicable rather than a failure, and not either of the two that
  // read the response: this page is served 200 and blocks no crawler.
  compare_page_quality("quality-broken", 7);
}

/// A relative canonical, which only navigation mode scores. Its own
/// fixture because that is the whole point of the page.
#[test]
fn page_quality_on_a_page_with_a_relative_canonical() {
  compare_page_quality("canonical-broken", 1);
}

#[test]
fn page_quality_on_a_page_with_nothing_wrong_with_it() {
  compare_page_quality("clean", 0);
}

/// The page built to fail axe rules, which is a different set of
/// mistakes: it has no meta description and its images are fine.
#[test]
fn page_quality_on_a_page_broken_in_the_other_dimension() {
  compare_page_quality("a11y-broken", 1);
}

/// `http-status-code`: the status is the only thing wrong with the page.
#[test]
fn page_quality_on_a_page_served_with_a_404() {
  compare_page_quality("status-404", 1);
}

// `is-crawlable` has three independent blocking sources and scores 0
// only when all five bot user agents are blocked. One fixture apiece,
// because a page blocked by all three would agree with Lighthouse even
// if two of the three were never read.

#[test]
fn page_quality_on_a_page_whose_meta_says_noindex() {
  compare_page_quality("meta-robots-blocked", 1);
}

/// Two `X-Robots-Tag` headers, so this also pins that a repeated header
/// arrives as two directives rather than one joined string, and that
/// `unavailable_after:` is read as a directive and not as a user-agent
/// prefix.
#[test]
fn page_quality_on_a_page_whose_headers_block_indexing() {
  compare_page_quality("x-robots-blocked", 1);
}

#[test]
fn page_quality_on_a_page_disallowed_by_robots_txt() {
  compare_page_quality("robots-blocked", 1);
}
