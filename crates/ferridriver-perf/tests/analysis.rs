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

// ── LCP, cache, fonts, viewport ────────────────────────────────────────

/// An image LCP candidate as current Chrome writes it: no `imageUrl`,
/// but `imageLoadStart` / `imageLoadEnd` in ms from the navigation.
fn image_lcp(load_start_ms: f64, load_end_ms: f64, loading_attr: &str) -> serde_json::Value {
  json!({"name":"largestContentfulPaint::Candidate","ph":"I","ts":420_000,"pid":1,"tid":1,
         "args":{"frame":"FRAME1","data":{"frame":"FRAME1","type":"image","size":240_000,
                 "loadingAttr":loading_attr,
                 "imageLoadStart":load_start_ms,"imageLoadEnd":load_end_ms}}})
}

fn text_lcp() -> serde_json::Value {
  json!({"name":"largestContentfulPaint::Candidate","ph":"I","ts":300_000,"pid":1,"tid":1,
         "args":{"frame":"FRAME1","data":{"frame":"FRAME1","type":"text","size":6048}}})
}

#[test]
fn a_text_lcp_breaks_into_ttfb_and_render_delay() {
  let mut events = trace(vec![text_lcp()]);
  events.extend(request(
    "1",
    "https://site.example/",
    "text/html",
    "blocking",
    3000,
    500,
  ));
  let report = ferridriver_perf::analyze_values(&events);

  let lcp = insight(&report, "LCPBreakdown");
  let labels: Vec<&str> = lcp.items.iter().map(|i| i.label.as_str()).collect();
  assert_eq!(labels, vec!["Time to first byte", "Element render delay"]);
  assert!(lcp.checks[0].detail.starts_with("Text LCP"), "{}", lcp.checks[0].detail);
}

/// The image request is recovered from the candidate's load timings,
/// because Chrome no longer puts the URL on the candidate.
#[test]
fn an_image_lcp_breaks_into_four_parts_and_finds_its_request() {
  let mut events = trace(vec![]);
  events.extend(request(
    "1",
    "https://site.example/",
    "text/html",
    "blocking",
    3000,
    500,
  ));
  // Image runs 100ms..400ms after the navigation at ts=2000.
  let mut img = request(
    "2",
    "https://site.example/hero.png",
    "image/png",
    "none",
    102_000,
    40_000,
  );
  img[0]["args"]["data"]["resourceType"] = json!("Image");
  events.extend(img);
  events.push(image_lcp(100.0, 180.0, "eager"));

  let report = ferridriver_perf::analyze_values(&events);
  let lcp = insight(&report, "LCPBreakdown");
  let labels: Vec<&str> = lcp.items.iter().map(|i| i.label.as_str()).collect();
  assert_eq!(
    labels,
    vec![
      "Time to first byte",
      "Resource load delay",
      "Resource load duration",
      "Element render delay"
    ]
  );
  assert!(
    lcp.checks[0].detail.starts_with("Image LCP"),
    "{}",
    lcp.checks[0].detail
  );
}

#[test]
fn a_lazy_loaded_lcp_image_without_a_priority_hint_is_flagged() {
  let mut events = trace(vec![]);
  events.extend(request(
    "1",
    "https://site.example/",
    "text/html",
    "blocking",
    3000,
    500,
  ));
  let mut img = request(
    "2",
    "https://site.example/hero.png",
    "image/png",
    "none",
    102_000,
    40_000,
  );
  img[0]["args"]["data"]["resourceType"] = json!("Image");
  img[0]["args"]["data"]["initiator"] = json!({"type":"parser","url":"https://site.example/"});
  events.extend(img);
  events.push(image_lcp(100.0, 180.0, "lazy"));

  let report = ferridriver_perf::analyze_values(&events);
  let d = insight(&report, "LCPDiscovery");
  let check = |name: &str| d.checks.iter().find(|c| c.name == name).unwrap();
  assert!(!check("priorityHinted").passed);
  // Parser-initiated from the main document, so the scanner did see it.
  assert!(check("requestDiscoverable").passed);
  assert!(!check("eagerlyLoaded").passed, "lazy should be flagged");
}

/// A text LCP has no request to discover, so the insight does not apply.
#[test]
fn lcp_discovery_is_absent_for_a_text_lcp() {
  let mut events = trace(vec![text_lcp()]);
  events.extend(request(
    "1",
    "https://site.example/",
    "text/html",
    "blocking",
    3000,
    500,
  ));
  let report = ferridriver_perf::analyze_values(&events);
  assert!(report.insights.iter().all(|i| i.key != "LCPDiscovery"));
}

fn static_asset(id: &str, url: &str, cache_control: Option<&str>, bytes: i64) -> Vec<serde_json::Value> {
  let mut req = request(id, url, "text/css", "none", 5000, bytes);
  req[0]["args"]["data"]["resourceType"] = json!("Stylesheet");
  if let Some(cc) = cache_control {
    req[1]["args"]["data"]["headers"] = json!([{"name":"cache-control","value":cc}]);
  } else {
    req[1]["args"]["data"]["headers"] = json!([{"name":"content-type","value":"text/css"}]);
  }
  req
}

#[test]
fn a_short_cache_lifetime_is_flagged_and_a_long_one_is_not() {
  let mut short = trace(vec![]);
  short.extend(static_asset(
    "1",
    "https://site.example/a.css",
    Some("max-age=60"),
    50_000,
  ));
  assert_eq!(
    insight(&ferridriver_perf::analyze_values(&short), "Cache").items.len(),
    1
  );

  let mut long = trace(vec![]);
  long.extend(static_asset(
    "1",
    "https://site.example/a.css",
    Some("max-age=31536000"),
    50_000,
  ));
  assert!(
    insight(&ferridriver_perf::analyze_values(&long), "Cache")
      .items
      .is_empty()
  );
}

/// Opting out of caching is a decision, not a defect.
#[test]
fn a_no_store_response_is_not_a_cache_finding() {
  let mut events = trace(vec![]);
  events.extend(static_asset(
    "1",
    "https://site.example/a.css",
    Some("no-store"),
    50_000,
  ));
  assert!(
    insight(&ferridriver_perf::analyze_values(&events), "Cache")
      .items
      .is_empty()
  );
}

/// Documents and XHR are not static assets, so their lifetime is not
/// this insight's business.
#[test]
fn a_non_static_resource_is_not_a_cache_finding() {
  let mut events = trace(vec![]);
  events.extend(request(
    "1",
    "https://site.example/",
    "text/html",
    "blocking",
    3000,
    50_000,
  ));
  assert!(
    insight(&ferridriver_perf::analyze_values(&events), "Cache")
      .items
      .is_empty()
  );
}

fn font(url: &str, display: &str) -> serde_json::Value {
  json!({"name":"BeginRemoteFontLoad","ph":"I","ts":6000,"pid":1,"tid":1,
         "args":{"data":{"url":url,"display":display}}})
}

#[test]
fn a_blocking_font_display_is_flagged_and_swap_is_not() {
  let mut blocking = trace(vec![font("https://site.example/f.woff2", "block")]);
  blocking.extend(request(
    "1",
    "https://site.example/f.woff2",
    "font/woff2",
    "none",
    5000,
    20_000,
  ));
  assert_eq!(
    insight(&ferridriver_perf::analyze_values(&blocking), "FontDisplay")
      .items
      .len(),
    1
  );

  let mut swap = trace(vec![font("https://site.example/f.woff2", "swap")]);
  swap.extend(request(
    "1",
    "https://site.example/f.woff2",
    "font/woff2",
    "none",
    5000,
    20_000,
  ));
  assert!(
    insight(&ferridriver_perf::analyze_values(&swap), "FontDisplay")
      .items
      .is_empty()
  );
}

#[test]
fn a_non_mobile_optimized_frame_fails_the_viewport_check() {
  let frame = |optimized: bool| {
    json!({"name":"BeginCommitCompositorFrame","ph":"I","ts":50_000,"pid":1,"tid":1,
           "args":{"frame":"FRAME1","is_mobile_optimized":optimized}})
  };
  let bad = ferridriver_perf::analyze_values(&trace(vec![frame(false)]));
  assert_eq!(insight(&bad, "Viewport").severity, Severity::Fail);

  let good = ferridriver_perf::analyze_values(&trace(vec![frame(true)]));
  assert_eq!(insight(&good, "Viewport").severity, Severity::Pass);
}

/// Without a committed frame the trace simply does not say.
#[test]
fn viewport_is_not_judged_without_a_compositor_frame() {
  let report = ferridriver_perf::analyze_values(&trace(vec![]));
  let v = insight(&report, "Viewport");
  assert_eq!(v.severity, Severity::Informative);
  assert!(v.checks[0].detail.contains("Not evaluated"));
}

// ── interactions, DOM size, forced reflow ──────────────────────────────

/// `EventTiming` as Chrome writes it: a begin/end pair, with the timing
/// fields in `performance.now()` milliseconds rather than trace
/// microseconds, and several events sharing one `interactionId`.
fn event_timing(
  id: i64,
  kind: &str,
  ts: i64,
  time_stamp: f64,
  proc_start: f64,
  proc_end: f64,
  duration: f64,
) -> Vec<serde_json::Value> {
  vec![
    json!({"name":"EventTiming","ph":"b","ts":ts,"pid":1,"tid":1,
           "args":{"data":{"interactionId":id,"type":kind,"timeStamp":time_stamp,
                           "processingStart":proc_start,"processingEnd":proc_end,"duration":duration}}}),
    json!({"name":"EventTiming","ph":"e","ts":ts + 1000,"pid":1,"tid":1,"args":{"data":{"interactionId":id}}}),
  ]
}

#[test]
fn inp_breaks_the_longest_interaction_into_three_phases() {
  let mut events = trace(vec![]);
  // timeStamp 100, handler runs 110..150, total 300ms.
  events.extend(event_timing(7, "click", 500_000, 100.0, 110.0, 150.0, 300.0));
  let report = ferridriver_perf::analyze_values(&events);

  assert_eq!(report.metrics.interaction_to_next_paint, Some(300.0));
  let inp = insight(&report, "INPBreakdown");
  let by = |label: &str| inp.items.iter().find(|i| i.label == label).unwrap().value;
  assert!((by("Input delay") - 10.0).abs() < 0.01);
  assert!((by("Processing duration") - 40.0).abs() < 0.01);
  // 100 + 300 - 150 = 250.
  assert!((by("Presentation delay") - 250.0).abs() < 0.01);
  assert_eq!(inp.severity, Severity::Fail, "300ms is over the 200ms bar");
}

/// A tap emits pointerdown, pointerup and click under one interactionId.
/// Treating them as separate interactions, or letting the last overwrite
/// the first, both give the wrong breakdown.
#[test]
fn events_sharing_an_interaction_id_merge_into_one_interaction() {
  let mut events = trace(vec![]);
  events.extend(event_timing(9, "pointerdown", 500_000, 100.0, 110.0, 115.0, 80.0));
  events.extend(event_timing(9, "pointerup", 501_000, 102.0, 116.0, 120.0, 80.0));
  events.extend(event_timing(9, "click", 502_000, 102.0, 120.0, 160.0, 80.0));

  let report = ferridriver_perf::analyze_values(&events);
  let inp = insight(&report, "INPBreakdown");
  // One interaction, named for the click, spanning the widest window:
  // earliest timeStamp 100, earliest processingStart 110, latest
  // processingEnd 160.
  assert!(inp.checks[0].detail.contains("click"), "{}", inp.checks[0].detail);
  let by = |label: &str| inp.items.iter().find(|i| i.label == label).unwrap().value;
  assert!((by("Input delay") - 10.0).abs() < 0.01);
  assert!((by("Processing duration") - 50.0).abs() < 0.01);
}

#[test]
fn an_interaction_chrome_did_not_count_is_ignored() {
  let mut events = trace(vec![]);
  events.extend(event_timing(0, "click", 500_000, 100.0, 110.0, 150.0, 300.0));
  let report = ferridriver_perf::analyze_values(&events);
  assert!(report.metrics.interaction_to_next_paint.is_none());
}

/// `Layout` hides its size under `args.beginData` and `UpdateLayoutTree`
/// puts it directly on `args`. Reading `args.data` for either, as every
/// other event in the trace does, silently finds nothing.
#[test]
fn large_layout_and_style_updates_are_read_from_their_own_arg_shapes() {
  let events = trace(vec![
    json!({"name":"Layout","ph":"X","ts":50_000,"dur":60_000,"pid":1,"tid":1,
           "args":{"beginData":{"dirtyObjects":500}}}),
    json!({"name":"UpdateLayoutTree","ph":"X","ts":120_000,"dur":50_000,"pid":1,"tid":1,
           "args":{"elementCount":900}}),
    json!({"name":"DOMStats","ph":"I","ts":150_000,"pid":1,"tid":1,
           "args":{"data":{"totalElements":4200,"maxDepth":18,"maxChildren":90}}}),
  ]);
  let report = ferridriver_perf::analyze_values(&events);

  let dom = insight(&report, "DOMSize");
  assert_eq!(dom.severity, Severity::Fail);
  assert_eq!(dom.items.len(), 2, "{:?}", dom.items);
  let (_, total) = dom.metrics.iter().find(|(k, _)| k == "totalElements").unwrap();
  assert!((total - 4200.0).abs() < f64::EPSILON);
}

/// Small or quick updates are not a DOM-size problem.
#[test]
fn a_fast_layout_is_not_a_dom_size_finding() {
  let events = trace(vec![
    json!({"name":"Layout","ph":"X","ts":50_000,"dur":5000,"pid":1,"tid":1,
                                 "args":{"beginData":{"dirtyObjects":5000}}}),
  ]);
  assert!(
    insight(&ferridriver_perf::analyze_values(&events), "DOMSize")
      .items
      .is_empty()
  );
}

/// A layout nested inside a script was forced by that script. Below the
/// 30ms per-task threshold it is noise, above it is a finding.
#[test]
fn reflow_inside_a_script_is_forced_only_once_it_crosses_the_threshold() {
  let build = |reflow_us: i64| {
    trace(vec![
      json!({"name":"RunTask","ph":"X","ts":50_000,"dur":100_000,"pid":1,"tid":1,"args":{}}),
      json!({"name":"FunctionCall","ph":"X","ts":51_000,"dur":90_000,"pid":1,"tid":1,"args":{}}),
      json!({"name":"Layout","ph":"X","ts":52_000,"dur":reflow_us,"pid":1,"tid":1,
             "args":{"beginData":{"dirtyObjects":10}}}),
    ])
  };
  let quiet = ferridriver_perf::analyze_values(&build(5_000));
  assert_eq!(insight(&quiet, "ForcedReflow").severity, Severity::Pass);

  let loud = ferridriver_perf::analyze_values(&build(40_000));
  assert_eq!(insight(&loud, "ForcedReflow").severity, Severity::Fail);
}

/// The same layout outside any script is the browser doing its normal
/// work, not a forced reflow.
#[test]
fn reflow_outside_a_script_is_not_forced() {
  let events = trace(vec![
    json!({"name":"RunTask","ph":"X","ts":50_000,"dur":100_000,"pid":1,"tid":1,"args":{}}),
    json!({"name":"Layout","ph":"X","ts":52_000,"dur":40_000,"pid":1,"tid":1,
           "args":{"beginData":{"dirtyObjects":10}}}),
  ]);
  assert_eq!(
    insight(&ferridriver_perf::analyze_values(&events), "ForcedReflow").severity,
    Severity::Pass
  );
}
