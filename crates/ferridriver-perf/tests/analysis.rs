//! Analysis over hand-built traces.
//!
//! Every case here is a shape that a real capture produced at least
//! once, kept as a fixture so the browser is not needed to prove the
//! arithmetic still holds.

use ferridriver_perf::insights::Severity;
use ferridriver_perf::units::micros_to_ms;
use serde_json::json;

/// Minimum viable trace: one navigation, one document request, the
/// paint markers. Timestamps are microseconds.
fn trace(extra: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
  let frame = "FRAME1";
  let mut events = vec![
    json!({"name":"TracingStartedInBrowser","ph":"I","ts":1000,"pid":1,"tid":1,
           "args":{"data":{"frames":[{"frame":frame,"url":"about:blank","isInPrimaryMainFrame":true,"isOutermostMainFrame":true}]}}}),
    json!({"name":"navigationStart","ph":"I","ts":2000,"pid":1,"tid":1,
           "args":{"frame":frame,"data":{"navigationId":"N1","documentLoaderURL":"https://site.example/","frame":frame}}}),
    json!({"name":"firstContentfulPaint","ph":"I","ts":102_000,"pid":1,"tid":1,"args":{"frame":frame,"data":{"frame":frame}}}),
    json!({"name":"MarkLoad","ph":"I","ts":152_000,"pid":1,"tid":1,"args":{"data":{"frame":frame}}}),
  ];
  events.extend(extra);
  events
}

/// A request triple as Chrome emits it. `timing` values are ms relative
/// to `requestTime` (seconds), which is the unit trap this pins down.
fn request(id: &str, url: &str, mime: &str, blocking: &str, start_us: i64, body_len: i64) -> Vec<serde_json::Value> {
  // `requestTime` is SECONDS; the ts fields around it are microseconds.
  let request_time = micros_to_ms(start_us) / 1000.0;
  vec![
    json!({"name":"ResourceSendRequest","ph":"I","ts":start_us,"pid":1,"tid":1,
           "args":{"data":{"requestId":id,"url":url,"requestMethod":"GET","priority":"VeryHigh",
                           "frame":"FRAME1","renderBlocking":blocking,"resourceType":"Stylesheet"}}}),
    json!({"name":"ResourceReceiveResponse","ph":"I","ts":start_us + 10_000,"pid":1,"tid":1,
           "args":{"data":{"requestId":id,"statusCode":200,"mimeType":mime,"protocol":"http/1.1",
                           "encodedDataLength":body_len,
                           "timing":{"requestTime":request_time,"sendStart":0.0,"sendEnd":1.0,
                                     "receiveHeadersStart":51.0,"receiveHeadersEnd":61.0,
                                     "dnsStart":0.0,"dnsEnd":0.0,"connectStart":0.0,"connectEnd":0.0,
                                     "sslStart":0.0,"sslEnd":0.0,"proxyStart":0.0,"proxyEnd":0.0}}}}),
    json!({"name":"ResourceFinish","ph":"I","ts":start_us + 80_000,"pid":1,"tid":1,
           "args":{"data":{"requestId":id,"finishTime":micros_to_ms(start_us + 80_000) / 1000.0,
                           "encodedDataLength":body_len,"decodedBodyLength":body_len,"didFail":false}}}),
  ]
}

fn insight<'a>(report: &'a ferridriver_perf::Report, key: &str) -> &'a ferridriver_perf::insights::Insight {
  report
    .insights
    .iter()
    .find(|i| i.key == key)
    .unwrap_or_else(|| panic!("no {key} insight"))
}

#[test]
fn metrics_are_measured_from_the_navigation_not_the_trace_start() {
  let events = trace(request(
    "1",
    "https://site.example/",
    "text/html",
    "blocking",
    3000,
    500,
  ));
  let report = ferridriver_perf::analyze_values(&events);

  // navigationStart is at 2000us, FCP at 102000us, so FCP is 100ms.
  // Measuring from trace_start (1000us) would give 101ms.
  assert_eq!(report.metrics.first_contentful_paint, Some(100.0));
  assert_eq!(report.metrics.load, Some(150.0));
  assert_eq!(report.url, "https://site.example/");
}

/// The frame tree records `about:blank` because tracing starts before
/// the navigation. Leaving it there made every request third-party.
#[test]
fn the_page_url_comes_from_the_navigation_not_the_initial_frame_tree() {
  let report = ferridriver_perf::analyze_values(&trace(vec![]));
  assert_eq!(report.url, "https://site.example/");
  assert_ne!(report.url, "about:blank");
}

#[test]
fn server_response_time_is_receive_headers_start_minus_send_end() {
  let events = trace(request(
    "1",
    "https://site.example/",
    "text/html",
    "blocking",
    3000,
    500,
  ));
  let report = ferridriver_perf::analyze_values(&events);
  // receiveHeadersStart 51ms - sendEnd 1ms = 50ms.
  let doc = insight(&report, "DocumentLatency");
  let (_, value) = doc.metrics.iter().find(|(k, _)| k == "serverResponseTimeMs").unwrap();
  assert!((value - 50.0).abs() < 1.0, "expected ~50ms, got {value}");
}

#[test]
fn a_slow_server_fails_the_document_latency_check() {
  let mut events = trace(vec![]);
  let mut req = request("1", "https://site.example/", "text/html", "blocking", 3000, 500);
  // receiveHeadersStart 700ms past sendEnd: over the 600ms line.
  req[1]["args"]["data"]["timing"]["receiveHeadersStart"] = json!(701.0);
  req[1]["args"]["data"]["timing"]["receiveHeadersEnd"] = json!(710.0);
  events.extend(req);

  let report = ferridriver_perf::analyze_values(&events);
  let doc = insight(&report, "DocumentLatency");
  assert_eq!(doc.severity, Severity::Fail);
  let check = doc.checks.iter().find(|c| c.name == "serverResponseIsFast").unwrap();
  assert!(!check.passed, "{}", check.detail);
}

#[test]
fn an_uncompressed_text_document_is_reported_with_its_estimated_saving() {
  let mut events = trace(vec![]);
  // 100 KB of HTML with no content-encoding: 67% of it is the estimate.
  events.extend(request(
    "1",
    "https://site.example/",
    "text/html",
    "blocking",
    3000,
    100_000,
  ));
  let report = ferridriver_perf::analyze_values(&events);

  let doc = insight(&report, "DocumentLatency");
  let check = doc.checks.iter().find(|c| c.name == "usesCompression").unwrap();
  assert!(!check.passed, "{}", check.detail);
  let (_, savings) = doc
    .metrics
    .iter()
    .find(|(k, _)| k == "uncompressedResponseBytes")
    .unwrap();
  assert!((savings - 67_000.0).abs() < 1.0, "expected ~67000, got {savings}");
}

/// Under the 1400-byte floor there is nothing worth telling anyone.
#[test]
fn a_tiny_uncompressed_document_is_not_flagged() {
  let mut events = trace(vec![]);
  events.extend(request(
    "1",
    "https://site.example/",
    "text/html",
    "blocking",
    3000,
    200,
  ));
  let report = ferridriver_perf::analyze_values(&events);
  let doc = insight(&report, "DocumentLatency");
  assert!(doc.checks.iter().find(|c| c.name == "usesCompression").unwrap().passed);
}

#[test]
fn a_compressed_document_passes() {
  let mut events = trace(vec![]);
  let mut req = request("1", "https://site.example/", "text/html", "blocking", 3000, 100_000);
  req[1]["args"]["data"]["headers"] = json!([{"name":"content-encoding","value":"gzip"}]);
  events.extend(req);
  let report = ferridriver_perf::analyze_values(&events);
  let doc = insight(&report, "DocumentLatency");
  assert!(doc.checks.iter().find(|c| c.name == "usesCompression").unwrap().passed);
}

#[test]
fn a_stylesheet_finishing_before_first_paint_is_render_blocking() {
  let mut events = trace(vec![]);
  events.extend(request(
    "1",
    "https://site.example/",
    "text/html",
    "blocking",
    3000,
    500,
  ));
  // Starts at 5ms, finishes at 85ms, before FCP at 100ms.
  events.extend(request(
    "2",
    "https://site.example/a.css",
    "text/css",
    "blocking",
    5000,
    9000,
  ));
  let report = ferridriver_perf::analyze_values(&events);

  let rb = insight(&report, "RenderBlocking");
  assert_eq!(rb.severity, Severity::Fail);
  assert!(rb.items.iter().any(|i| i.label.ends_with("a.css")), "{:?}", rb.items);
}

/// A request that lands after the paint it supposedly blocked did not
/// block it.
#[test]
fn a_stylesheet_finishing_after_first_paint_is_not_render_blocking() {
  let mut events = trace(vec![]);
  events.extend(request(
    "1",
    "https://site.example/",
    "text/html",
    "blocking",
    3000,
    500,
  ));
  events.extend(request(
    "2",
    "https://site.example/late.css",
    "text/css",
    "blocking",
    200_000,
    9000,
  ));
  let report = ferridriver_perf::analyze_values(&events);
  assert!(
    !insight(&report, "RenderBlocking")
      .items
      .iter()
      .any(|i| i.label.contains("late"))
  );
}

#[test]
fn requests_on_the_page_own_registrable_domain_are_not_third_party() {
  let mut events = trace(vec![]);
  events.extend(request(
    "1",
    "https://site.example/",
    "text/html",
    "blocking",
    3000,
    500,
  ));
  events.extend(request(
    "2",
    "https://cdn.site.example/a.css",
    "text/css",
    "none",
    5000,
    9000,
  ));
  events.extend(request(
    "3",
    "https://tracker.other/t.js",
    "text/javascript",
    "none",
    6000,
    4000,
  ));

  let report = ferridriver_perf::analyze_values(&events);
  let tp = insight(&report, "ThirdParties");
  assert_eq!(tp.items.len(), 1, "{:?}", tp.items);
  assert_eq!(tp.items[0].label, "tracker.other");
}

/// CLS is the worst session window, not the sum. Three shifts spaced
/// more than a second apart are three windows, so the score is the
/// largest single one.
#[test]
fn cumulative_layout_shift_takes_the_worst_session_window() {
  let shift = |ts: i64, score: f64| {
    json!({"name":"LayoutShift","ph":"I","ts":ts,"pid":1,"tid":1,
           "args":{"data":{"frame":"FRAME1","weighted_score_delta":score,"had_recent_input":false}}})
  };
  let report = ferridriver_perf::analyze_values(&trace(vec![
    shift(10_000, 0.05),
    shift(20_000, 0.06),
    // >1s later: a new window.
    shift(3_000_000, 0.20),
    shift(9_000_000, 0.03),
  ]));
  // Windows are 0.11, 0.20, 0.03. Summing all four would give 0.34.
  assert!(
    (report.metrics.cumulative_layout_shift - 0.20).abs() < 1e-9,
    "got {}",
    report.metrics.cumulative_layout_shift
  );
}

#[test]
fn a_shift_after_user_input_is_excluded() {
  let report = ferridriver_perf::analyze_values(&trace(vec![json!({
    "name":"LayoutShift","ph":"I","ts":10_000,"pid":1,"tid":1,
    "args":{"data":{"frame":"FRAME1","weighted_score_delta":0.5,"had_recent_input":true}}})]));
  assert!(report.metrics.cumulative_layout_shift.abs() < f64::EPSILON);
}

#[test]
fn an_origin_needs_six_http1_requests_before_it_is_flagged() {
  let mut few = trace(vec![]);
  for i in 0..5 {
    few.extend(request(
      &format!("r{i}"),
      &format!("https://slow.example/{i}.css"),
      "text/css",
      "none",
      5000 + i * 100,
      100,
    ));
  }
  assert!(
    insight(&ferridriver_perf::analyze_values(&few), "ModernHTTP")
      .items
      .is_empty()
  );

  let mut many = trace(vec![]);
  for i in 0..6 {
    many.extend(request(
      &format!("r{i}"),
      &format!("https://slow.example/{i}.css"),
      "text/css",
      "none",
      5000 + i * 100,
      100,
    ));
  }
  let many_report = ferridriver_perf::analyze_values(&many);
  let flagged = insight(&many_report, "ModernHTTP");
  assert_eq!(flagged.severity, Severity::Fail);
  assert_eq!(flagged.items[0].label, "https://slow.example");
}

#[test]
fn an_empty_trace_analyses_to_an_empty_report_rather_than_failing() {
  let report = ferridriver_perf::analyze_values(&[]);
  assert_eq!(report.event_count, 0);
  assert_eq!(report.metrics.total_requests, 0);
  assert!(report.metrics.first_contentful_paint.is_none());
}

#[test]
fn a_saved_trace_file_object_is_accepted_as_well_as_a_bare_array() {
  let events = trace(vec![]);
  let wrapped = serde_json::to_vec(&serde_json::json!({ "traceEvents": events })).unwrap();
  let report = ferridriver_perf::analyze_json(&wrapped).unwrap();
  assert_eq!(report.url, "https://site.example/");

  let bare = serde_json::to_vec(&events).unwrap();
  assert_eq!(
    ferridriver_perf::analyze_json(&bare).unwrap().url,
    "https://site.example/"
  );
}

#[test]
fn a_non_trace_document_is_rejected() {
  assert!(ferridriver_perf::analyze_json(b"{\"nope\":1}").is_err());
  assert!(ferridriver_perf::analyze_json(b"not json").is_err());
}
