//! This crate against the engine it is a port of.
//!
//! The other test files build their events by hand, which pins the
//! arithmetic precisely and proves only that the code does what its
//! author thought. The fixtures here are real Chrome traces, and the
//! verdicts beside them are what devtools-frontend's own engine
//! produced for those exact bytes. Four defects survived the hand-built
//! tests and died here, so a regression against real Chrome output now
//! fails the build rather than waiting for someone to run a script.
//!
//! `just perf-diff` re-derives the recordings from the real engine and
//! checks they still say this; `just perf-diff-update` rewrites them
//! after a deliberate change. Neither is needed to run this test, which
//! is the point: the recording is checked in, so the gate is offline.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;

use ferridriver_perf::handlers::{meta::Meta, page_load::PageLoadMetrics};
use ferridriver_perf::insights::Severity;
use ferridriver_perf::lantern::{Context, constants::Throttling};
use ferridriver_perf::{Metrics, Report};
use serde::Deserialize;

/// What `scripts/perf-diff/engine.mjs` recorded from devtools-frontend.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Upstream {
  engine: String,
  metrics: BTreeMap<String, Option<f64>>,
  insights: BTreeMap<String, UpstreamInsight>,
  #[serde(default)]
  lantern: Option<UpstreamLantern>,
}

/// The Lantern numbers, which are not on any insight model.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpstreamLantern {
  rtt: Option<f64>,
  throughput: Option<f64>,
  observed: Option<UpstreamPaints>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpstreamPaints {
  first_contentful_paint_ms: f64,
  largest_contentful_paint_ms: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpstreamInsight {
  state: String,
  #[serde(default)]
  metric_savings: BTreeMap<String, Option<f64>>,
  #[serde(default)]
  checklist: BTreeMap<String, bool>,
  #[serde(default)]
  scalars: BTreeMap<String, f64>,
  #[serde(default)]
  counts: BTreeMap<String, f64>,
}

/// The one place our answer deliberately differs, and why.
///
/// Upstream reports `SlowCSSSelector` as a pass on a trace carrying no
/// selector statistics, which claims a result it has no data for. Ours
/// reports that it was not measured. Do not "fix" this to reach
/// nineteen out of nineteen.
const STATE_DIVERGENCES: &[(&str, &str, &str)] = &[("SlowCSSSelector", "pass", "informative")];

/// A quantity upstream computes, the name we give the same one, and the
/// factor from their unit to ours.
///
/// Every pair here was read out of devtools-frontend's own source
/// before it was written down. A name that merely looks similar would
/// assert an agreement nobody checked, which is the failure mode this
/// whole file exists to catch.
const SCALARS: &[(&str, &str, &str, f64)] = &[
  ("Cache", "wastedBytes", "wastedBytes", 1.0),
  ("ImageDelivery", "wastedBytes", "wastedBytes", 1.0),
  ("DuplicatedJavaScript", "wastedBytes", "wastedBytes", 1.0),
  ("LegacyJavaScript", "wastedBytes", "estimatedWastedBytes", 1.0),
  // `DocumentLatency`'s wastedBytes IS its uncompressedResponseBytes;
  // `finalize` passes the one through as the other.
  ("DocumentLatency", "wastedBytes", "uncompressedResponseBytes", 1.0),
  ("DocumentLatency", "serverResponseTime", "serverResponseTimeMs", 1.0),
  ("DocumentLatency", "redirectDuration", "redirectDurationMs", 1.0),
  ("LCPBreakdown", "lcpMs", "lcpMs", 1.0),
  ("SlowCSSSelector", "totalElapsedMs", "totalElapsedMs", 1.0),
  ("SlowCSSSelector", "totalMatchAttempts", "totalMatchAttempts", 1.0),
  ("SlowCSSSelector", "totalMatchCount", "totalMatchCount", 1.0),
  // Upstream keeps the critical path in microseconds.
  ("NetworkDependencyTree", "maxTime", "maxCriticalPathLatencyMs", 0.001),
];

/// How many of something upstream found, against our count of it.
const COUNTS: &[(&str, &str, &str)] = &[("RenderBlocking", "renderBlockingRequests", "renderBlockingRequests")];

/// Upstream's `metricSavings` keys, against ours.
///
/// Applied to every insight that carries them rather than to a listed
/// few: an insight that gains a saving upstream should start failing
/// here, not pass quietly because nobody added a row.
const SAVINGS: &[(&str, &str)] = &[("FCP", "estimatedSavingsFcpMs"), ("LCP", "estimatedSavingsLcpMs")];

/// Milliseconds. Every quantity compared here is either an integer
/// count or a time both sides round to whole milliseconds, so this is
/// tight enough to catch a model change and loose enough to survive
/// floating-point association.
const TOLERANCE: f64 = 0.001;

/// Fixtures whose paint estimates are not compared.
///
/// Empty, and meant to stay that way. It exists because it was not
/// empty: the redirect fixture sat here while its simulated time
/// disagreed, which is how the disagreement stayed visible instead of
/// being skipped. Put a fixture here only with the reason beside it,
/// and take it out again.
const PAINT_ESTIMATE_GAPS: &[&str] = &[];

/// Throughput is accumulated over microsecond windows on one side and
/// millisecond ones on the other, so the two agree to about seven
/// significant figures rather than exactly.
const THROUGHPUT_TOLERANCE: f64 = 1e-6;

/// Every recorded trace, for the checks that apply to all of them.
const FIXTURES: [&str; 4] = [
  "plain-load",
  "interaction-and-thrash",
  "redirect-and-shift",
  "fonts-and-selectors",
];

#[test]
fn a_plain_load_agrees_with_devtools_frontend() {
  compare("plain-load");
}

#[test]
fn an_interaction_and_a_layout_thrash_agree_with_devtools_frontend() {
  compare("interaction-and-thrash");
}

/// The two that need something a normal capture does not have: a font
/// request, and the CSS selector statistics that only appear when
/// `disabled-by-default-blink.debug` is recorded. This page also never
/// reaches a largest contentful paint, which is the branch where
/// upstream's Lantern context fails to build and half the insights stop
/// predicting anything.
#[test]
fn fonts_and_selector_statistics_agree_with_devtools_frontend() {
  compare("fonts-and-selectors");
}

/// The one with data in it. The other two fixtures come from a page
/// where most insights find nothing, so they agree by both reporting
/// nothing; this one redirects, paints text rather than an image,
/// serves images an order of magnitude larger than it draws them and
/// shifts its layout after load.
#[test]
fn a_redirect_and_a_layout_shift_agree_with_devtools_frontend() {
  compare("redirect-and-shift");
}

fn compare(fixture: &str) {
  let upstream = read_upstream(fixture);
  let report = analyze(fixture);
  let mut differences: Vec<String> = Vec::new();

  let ours = page_metrics(&report.metrics);
  for (name, theirs) in &upstream.metrics {
    let Some(theirs) = theirs else { continue };
    match ours.get(name.as_str()) {
      None => differences.push(format!(
        "metric {name}: upstream reports {theirs}, we do not report it at all"
      )),
      Some(mine) if !agrees(*mine, *theirs) => {
        differences.push(format!("metric {name}: upstream {theirs}, ours {mine}"));
      },
      Some(_) => {},
    }
  }

  for (key, theirs) in &upstream.insights {
    let Some(mine) = report.insights.iter().find(|insight| insight.key == *key) else {
      differences.push(format!("insight {key}: upstream reports it, we do not produce it"));
      continue;
    };
    let state = severity_name(mine.severity);
    let deliberate = STATE_DIVERGENCES.iter().any(|(insight, theirs_state, ours_state)| {
      *insight == key && *theirs_state == theirs.state && *ours_state == state
    });
    if state != theirs.state && !deliberate {
      differences.push(format!("insight {key}: upstream {}, ours {state}", theirs.state));
    }

    for (name, passed) in &theirs.checklist {
      match mine.checks.iter().find(|check| check.name == *name) {
        None => differences.push(format!("insight {key} check {name}: upstream has it, we do not")),
        Some(check) if check.passed != *passed => {
          differences.push(format!(
            "insight {key} check {name}: upstream {passed}, ours {}",
            check.passed
          ));
        },
        Some(_) => {},
      }
    }

    let numbers: BTreeMap<&str, f64> = mine
      .metrics
      .iter()
      .map(|(name, value)| (name.as_str(), *value))
      .collect();
    let scalars = SCALARS
      .iter()
      .filter(|(insight, ..)| *insight == key)
      .filter_map(|(_, name, ours, scale)| theirs.scalars.get(*name).map(|value| (*name, *ours, value * scale)));
    let counts = COUNTS
      .iter()
      .filter(|(insight, ..)| *insight == key)
      .filter_map(|(_, name, ours)| theirs.counts.get(*name).map(|value| (*name, *ours, *value)));
    let savings = SAVINGS.iter().filter_map(|(name, ours)| {
      theirs
        .metric_savings
        .get(*name)
        .and_then(|v| *v)
        .map(|v| (*name, *ours, v))
    });

    for (theirs_name, ours_name, theirs_value) in scalars.chain(counts).chain(savings) {
      match numbers.get(ours_name) {
        None => differences.push(format!(
          "insight {key}: upstream {theirs_name} is {theirs_value}, we report no {ours_name}"
        )),
        Some(mine) if !agrees(*mine, theirs_value) => {
          differences.push(format!(
            "insight {key} {theirs_name}: upstream {theirs_value}, ours {ours_name} {mine}"
          ));
        },
        Some(_) => {},
      }
    }
  }

  compare_lantern(fixture, &upstream, &mut differences);

  assert!(
    differences.is_empty(),
    "{fixture} disagrees with {} on {} point(s):\n  {}\n\nRun `just perf-diff` to see it against the live engine.",
    upstream.engine,
    differences.len(),
    differences.join("\n  ")
  );
}

/// The simulator's own inputs and outputs, which no insight exposes.
///
/// Every predicted saving is a difference of two simulations, so a
/// round trip that is wrong in the same direction on both sides cancels
/// and the insight comparison stays green while the model underneath is
/// out. Comparing the analysis directly is what catches that.
fn compare_lantern(fixture: &str, upstream: &Upstream, differences: &mut Vec<String>) {
  let Some(lantern) = upstream.lantern.as_ref() else {
    return;
  };
  let (Some(rtt), Some(throughput)) = (lantern.rtt, lantern.throughput) else {
    // Upstream could not build a context for this trace. Neither should
    // we, and `the_recorded_traces_produce_a_full_report` already
    // asserts the report survives it.
    return;
  };
  let Some(ours) = lantern_context(fixture) else {
    differences.push("lantern: upstream built a context, we did not".into());
    return;
  };
  let analysis = ours.network_analysis();
  if !agrees(analysis.rtt, rtt) {
    differences.push(format!("lantern rtt: upstream {rtt}, ours {}", analysis.rtt));
  }
  if (analysis.throughput - throughput).abs() > throughput.abs() * THROUGHPUT_TOLERANCE {
    differences.push(format!(
      "lantern throughput: upstream {throughput}, ours {}",
      analysis.throughput
    ));
  }

  let Some(paints) = lantern.observed.as_ref() else {
    return;
  };
  if PAINT_ESTIMATE_GAPS.contains(&fixture) {
    return;
  }
  let observed = Throttling::observed(analysis.rtt, analysis.throughput);
  let Some(estimate) = ours.paint_estimate(observed) else {
    differences.push("lantern: upstream estimated the paints, we did not".into());
    return;
  };
  for (name, theirs, mine) in [
    (
      "FCP",
      paints.first_contentful_paint_ms,
      estimate.first_contentful_paint_ms,
    ),
    (
      "LCP",
      paints.largest_contentful_paint_ms,
      estimate.largest_contentful_paint_ms,
    ),
  ] {
    if !agrees(mine, theirs) {
      differences.push(format!("lantern {name} estimate: upstream {theirs}, ours {mine}"));
    }
  }
}

/// Rebuild the graph the way `analyze` does, so the comparison above
/// measures what the crate actually produces.
fn lantern_context(fixture: &str) -> Option<Context> {
  let bytes = read_trace(fixture);
  let events = ferridriver_perf::event::parse(&bytes).ok()?;
  let meta = Meta::from_events(&events);
  let requests = ferridriver_perf::handlers::network::from_events(&events);
  let metrics = PageLoadMetrics::from_events(&events, &meta);
  let at = |ms: Option<f64>| ms.map(|ms| meta.time_origin() + ferridriver_perf::units::ms_to_micros(ms));
  Context::build(
    &requests,
    &meta.main_frame_url,
    &events,
    at(metrics.first_contentful_paint),
    at(metrics.largest_contentful_paint),
  )
}

/// Our metrics under the names upstream gives them.
fn page_metrics(metrics: &Metrics) -> BTreeMap<&'static str, f64> {
  let mut named: BTreeMap<&'static str, f64> = BTreeMap::new();
  let mut insert = |name: &'static str, value: Option<f64>| {
    if let Some(value) = value {
      named.insert(name, value);
    }
  };
  insert("FP", metrics.first_paint);
  insert("FCP", metrics.first_contentful_paint);
  insert("LCP", metrics.largest_contentful_paint);
  insert("DCL", metrics.dom_content_loaded);
  insert("L", metrics.load);
  insert("CLS", Some(metrics.cumulative_layout_shift));
  insert(
    "requests",
    Some(ferridriver_perf::units::len_to_f64(metrics.total_requests)),
  );
  named
}

fn severity_name(severity: Severity) -> &'static str {
  match severity {
    Severity::Pass => "pass",
    Severity::Informative => "informative",
    Severity::Fail => "fail",
  }
}

fn agrees(ours: f64, theirs: f64) -> bool {
  (ours - theirs).abs() <= TOLERANCE
}

fn fixture_path(name: &str, extension: &str) -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    .join("tests/fixtures")
    .join(format!("{name}.{extension}"))
}

fn read_trace(name: &str) -> Vec<u8> {
  let path = fixture_path(name, "json.gz");
  let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display()));
  let mut trace = Vec::new();
  flate2::read::GzDecoder::new(file)
    .read_to_end(&mut trace)
    .unwrap_or_else(|e| panic!("cannot decompress {}: {e}", path.display()));
  trace
}

fn analyze(fixture: &str) -> Report {
  ferridriver_perf::analyze_json(&read_trace(fixture)).unwrap_or_else(|e| panic!("{fixture} is not a trace: {e}"))
}

fn read_upstream(name: &str) -> Upstream {
  let path = fixture_path(name, "upstream.json");
  let recorded = std::fs::read(&path).unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display()));
  serde_json::from_slice(&recorded).unwrap_or_else(|e| panic!("{} is not a recording: {e}", path.display()))
}

/// A trace read from disk and the same trace handed over as already-
/// parsed values must analyse identically.
///
/// They take different routes through `TraceEvent`: one borrows unparsed
/// slices out of the file buffer and parses each event's `args` on
/// demand, the other borrows a `Value` tree the caller already built and
/// never parses anything. The MCP server only ever uses the second, so
/// a divergence here would be invisible to every other test in the crate
/// and visible to every user of the tool.
#[test]
fn a_trace_analyses_the_same_from_bytes_as_from_values() {
  for fixture in FIXTURES {
    let bytes = read_trace(fixture);
    let values: Vec<serde_json::Value> = match serde_json::from_slice(&bytes).expect("fixture is JSON") {
      serde_json::Value::Array(events) => events,
      serde_json::Value::Object(mut file) => match file.remove("traceEvents") {
        Some(serde_json::Value::Array(events)) => events,
        _ => panic!("{fixture} has no traceEvents"),
      },
      _ => panic!("{fixture} is not a trace"),
    };
    let from_bytes = serde_json::to_value(analyze(fixture)).expect("report serializes");
    let from_values = serde_json::to_value(ferridriver_perf::analyze_values(&values)).expect("report serializes");
    assert_eq!(from_bytes, from_values, "{fixture} analysed differently from values");
  }
}

/// The report is what everything above reads; if its shape drifts the
/// comparison silently stops covering things.
#[test]
fn the_recorded_traces_produce_a_full_report() {
  for fixture in FIXTURES {
    let report = analyze(fixture);
    assert_eq!(
      report.insights.len(),
      19,
      "{fixture} should produce all nineteen insights"
    );
    assert!(!report.requests.is_empty(), "{fixture} should resolve a waterfall");
    assert!(
      report.url.starts_with("http"),
      "{fixture} should name the page it loaded"
    );
  }
}
