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
  // A breakdown reports where the time went; the verdict on whether
  // 300ms is too slow belongs to the INP metric, not to this insight.
  // Upstream marks it informative for the same reason.
  assert_eq!(inp.severity, Severity::Informative);
  let (_, inp_ms) = inp.metrics.iter().find(|(k, _)| k == "inpMs").unwrap();
  assert!((inp_ms - 300.0).abs() < 0.01);
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

// ── images, dependency chains, layout shift causes ─────────────────────

fn image_request(id: &str, url: &str, mime: &str, bytes: i64, start_us: i64) -> Vec<serde_json::Value> {
  let mut req = request(id, url, mime, "none", start_us, bytes);
  req[0]["args"]["data"]["resourceType"] = json!("Image");
  req
}

fn paint_image(url: &str, src: (i64, i64), displayed: (i64, i64), is_css: bool) -> serde_json::Value {
  json!({"name":"PaintImage","ph":"I","ts":200_000,"pid":1,"tid":1,
         "args":{"data":{"url":url,"srcWidth":src.0,"srcHeight":src.1,
                         "width":displayed.0,"height":displayed.1,"isCSS":is_css}}})
}

#[test]
fn an_image_heavier_than_its_pixels_justify_is_flagged() {
  let mut events = trace(vec![paint_image(
    "https://site.example/big.png",
    (600, 400),
    (600, 400),
    false,
  )]);
  events.extend(request(
    "1",
    "https://site.example/",
    "text/html",
    "blocking",
    3000,
    500,
  ));
  // 240k pixels at the 2/12 target is 40k bytes; 100k is well over.
  events.extend(image_request(
    "2",
    "https://site.example/big.png",
    "image/png",
    100_000,
    5000,
  ));

  let report = ferridriver_perf::analyze_values(&events);
  let img = insight(&report, "ImageDelivery");
  assert_eq!(img.severity, Severity::Fail);
  assert!(img.items[0].label.contains("modern format"), "{}", img.items[0].label);
}

/// A well-compressed image is not a finding, however large the picture.
#[test]
fn a_well_compressed_image_is_not_flagged() {
  let mut events = trace(vec![paint_image(
    "https://site.example/ok.png",
    (600, 400),
    (600, 400),
    false,
  )]);
  events.extend(request(
    "1",
    "https://site.example/",
    "text/html",
    "blocking",
    3000,
    500,
  ));
  events.extend(image_request(
    "2",
    "https://site.example/ok.png",
    "image/png",
    4_660,
    5000,
  ));
  assert!(
    insight(&ferridriver_perf::analyze_values(&events), "ImageDelivery")
      .items
      .is_empty()
  );
}

/// An image served far larger than it is drawn wastes the difference.
#[test]
fn an_oversized_image_is_flagged_for_responsive_serving() {
  let mut events = trace(vec![paint_image(
    "https://site.example/huge.png",
    (2000, 2000),
    (100, 100),
    false,
  )]);
  events.extend(request(
    "1",
    "https://site.example/",
    "text/html",
    "blocking",
    3000,
    500,
  ));
  // Cheap per pixel, so only the responsive rule can fire.
  events.extend(image_request(
    "2",
    "https://site.example/huge.png",
    "image/png",
    200_000,
    5000,
  ));

  let report = ferridriver_perf::analyze_values(&events);
  let img = insight(&report, "ImageDelivery");
  assert!(
    img.items.iter().any(|i| i.label.contains("responsive images")),
    "{:?}",
    img.items
  );
}

/// CSS backgrounds are exempt from the responsive advice upstream.
#[test]
fn an_oversized_css_background_is_not_flagged_for_responsive_serving() {
  let mut events = trace(vec![paint_image(
    "https://site.example/bg.png",
    (2000, 2000),
    (100, 100),
    true,
  )]);
  events.extend(request(
    "1",
    "https://site.example/",
    "text/html",
    "blocking",
    3000,
    500,
  ));
  events.extend(image_request(
    "2",
    "https://site.example/bg.png",
    "image/png",
    200_000,
    5000,
  ));
  let report = ferridriver_perf::analyze_values(&events);
  let img = insight(&report, "ImageDelivery");
  assert!(!img.items.iter().any(|i| i.label.contains("responsive images")));
}

#[test]
fn a_long_chain_of_critical_requests_is_reported_with_its_depth() {
  let mut events = trace(vec![]);
  events.extend(request(
    "1",
    "https://site.example/",
    "text/html",
    "blocking",
    3000,
    500,
  ));

  // Each script is initiated by the one before it, so none can start
  // until its parent finished.
  let mut chained = |id: &str, url: &str, parent: &str, start: i64| {
    let mut req = request(id, url, "text/javascript", "blocking", start, 1000);
    req[0]["args"]["data"]["resourceType"] = json!("Script");
    req[0]["args"]["data"]["initiator"] = json!({"type":"script","url":parent});
    events.extend(req);
  };
  chained("2", "https://site.example/a.js", "https://site.example/", 10_000);
  chained("3", "https://site.example/b.js", "https://site.example/a.js", 100_000);
  chained("4", "https://site.example/c.js", "https://site.example/b.js", 200_000);

  let report = ferridriver_perf::analyze_values(&events);
  let tree = insight(&report, "NetworkDependencyTree");
  assert_eq!(tree.severity, Severity::Fail);
  let (_, depth) = tree.metrics.iter().find(|(k, _)| k == "maxChainLength").unwrap();
  assert!(
    (depth - 4.0).abs() < f64::EPSILON,
    "expected a 4-deep chain, got {depth}"
  );
  // Root first, so the document heads the list.
  assert_eq!(tree.items[0].label.trim(), "https://site.example/");
}

/// Upstream fails at a chain of two, so the document plus one critical
/// resource already counts. What separates a healthy page from a bad one
/// is the DEPTH, so that is what this pins.
#[test]
fn parser_found_requests_stay_at_depth_two_however_many_there_are() {
  let mut events = trace(vec![]);
  events.extend(request(
    "1",
    "https://site.example/",
    "text/html",
    "blocking",
    3000,
    500,
  ));
  for (n, id) in ["2", "3", "4", "5"].iter().enumerate() {
    let mut req = request(
      id,
      &format!("https://site.example/{n}.js"),
      "text/javascript",
      "blocking",
      10_000,
      1000,
    );
    req[0]["args"]["data"]["resourceType"] = json!("Script");
    req[0]["args"]["data"]["initiator"] = json!({"type":"parser","url":"https://site.example/"});
    events.extend(req);
  }
  let report = ferridriver_perf::analyze_values(&events);
  let tree = insight(&report, "NetworkDependencyTree");
  let (_, depth) = tree.metrics.iter().find(|(k, _)| k == "maxChainLength").unwrap();
  assert!(
    (depth - 2.0).abs() < f64::EPSILON,
    "four parser-found scripts should stay two deep, got {depth}"
  );
}

fn shift(ts: i64, score: f64) -> serde_json::Value {
  json!({"name":"LayoutShift","ph":"I","ts":ts,"pid":1,"tid":1,
         "args":{"data":{"frame":"FRAME1","weighted_score_delta":score,"had_recent_input":false}}})
}

#[test]
fn a_shift_is_blamed_on_a_cause_that_finished_just_before_it() {
  let events = trace(vec![
    json!({"name":"LayoutImageUnsized","ph":"I","ts":900_000,"pid":1,"tid":1,
           "args":{"data":{"nodeName":"IMG"}}}),
    shift(1_000_000, 0.3),
  ]);
  let report = ferridriver_perf::analyze_values(&events);
  let cls = insight(&report, "CLSCulprits");
  assert_eq!(cls.severity, Severity::Fail);
  assert!(
    cls.items[0].label.starts_with("Unsized image"),
    "{}",
    cls.items[0].label
  );
}

/// The half-second window is what stops an unrelated event elsewhere in
/// the load being blamed for a shift.
#[test]
fn a_cause_outside_the_root_cause_window_is_not_blamed() {
  let events = trace(vec![
    json!({"name":"LayoutImageUnsized","ph":"I","ts":100_000,"pid":1,"tid":1,
           "args":{"data":{"nodeName":"IMG"}}}),
    shift(2_000_000, 0.3),
  ]);
  let report = ferridriver_perf::analyze_values(&events);
  let cls = insight(&report, "CLSCulprits");
  assert_eq!(cls.severity, Severity::Fail, "the page still moved");
  assert!(cls.items.is_empty(), "but nothing should be blamed: {:?}", cls.items);
  assert!(cls.checks[0].detail.contains("no cause identified"));
}

#[test]
fn a_page_that_never_moved_passes_cls_culprits() {
  let report = ferridriver_perf::analyze_values(&trace(vec![]));
  assert_eq!(insight(&report, "CLSCulprits").severity, Severity::Pass);
}

// ── charset, scripts, selectors ────────────────────────────────────────

/// Source text only arrives on `ScriptCatchup` under the
/// v8-source-rundown-sources category; the same event name under the
/// plain rundown category carries metadata only.
fn script_source(script_id: i64, url: &str, source: &str) -> serde_json::Value {
  json!({"name":"ScriptCatchup","ph":"X","ts":60_000,"dur":10,"pid":1,"tid":1,
         "cat":"disabled-by-default-devtools.v8-source-rundown-sources",
         "args":{"data":{"isolate":"ISO1","scriptId":script_id,"url":url,"sourceText":source}}})
}

#[test]
fn a_charset_in_the_content_type_header_passes() {
  let mut events = trace(vec![]);
  let mut req = request("1", "https://site.example/", "text/html", "blocking", 3000, 500);
  req[1]["args"]["data"]["headers"] = json!([{"name":"content-type","value":"text/html; charset=utf-8"}]);
  events.extend(req);

  let report = ferridriver_perf::analyze_values(&events);
  let cs = insight(&report, "CharacterSet");
  assert_eq!(cs.severity, Severity::Pass);
  assert!(cs.checks.iter().find(|c| c.name == "httpCharset").unwrap().passed);
}

/// An early meta tag is enough on its own.
#[test]
fn an_early_meta_charset_passes_without_the_header() {
  let mut events = trace(vec![
    json!({"name":"MetaCharsetCheck","ph":"I","ts":50_000,"pid":1,"tid":1,
                                     "args":{"data":{"disposition":"found-in-first-1024-bytes"}}}),
  ]);
  events.extend(request(
    "1",
    "https://site.example/",
    "text/html",
    "blocking",
    3000,
    500,
  ));
  let report = ferridriver_perf::analyze_values(&events);
  assert_eq!(insight(&report, "CharacterSet").severity, Severity::Pass);
}

#[test]
fn a_late_meta_charset_and_no_header_fails() {
  let mut events = trace(vec![
    json!({"name":"MetaCharsetCheck","ph":"I","ts":50_000,"pid":1,"tid":1,
                                     "args":{"data":{"disposition":"found-after-first-1024-bytes"}}}),
  ]);
  events.extend(request(
    "1",
    "https://site.example/",
    "text/html",
    "blocking",
    3000,
    500,
  ));
  let report = ferridriver_perf::analyze_values(&events);
  let cs = insight(&report, "CharacterSet");
  assert_eq!(cs.severity, Severity::Fail);
  assert!(cs.checks.iter().any(|c| c.detail.contains("after the first 1024")));
}

/// Detection only runs on scripts over the 5000-byte floor, so the
/// fixture is padded past it. A smaller script is covered separately.
#[test]
fn core_js_polyfills_and_babel_transforms_are_detected() {
  let source = format!(
    "function _classCallCheck(a,n){{if(!(a instanceof n))throw new TypeError(\
     \"Cannot call a class as a function\");}}\n\
     Array.prototype.at = function(i){{return this[i];}};\n\
     Object.fromEntries = function(e){{return {{}};}};\n\
     Array.prototype.flat = function(){{return this;}};\n\
     Array.prototype.includes = function(x){{return false;}};\n\
     String.prototype.padStart = function(n){{return this;}};\n\
     Promise.allSettled = function(p){{return p;}};\n\
     Object.entries = function(o){{return [];}};\n{}",
    "// pad\n".repeat(1200)
  );
  let source = source.as_str();
  let events = trace(vec![script_source(1, "https://site.example/bundle.js", source)]);
  let report = ferridriver_perf::analyze_values(&events);

  let legacy = insight(&report, "LegacyJavaScript");
  assert_eq!(legacy.severity, Severity::Fail);
  let label = &legacy.items[0].label;
  assert!(label.contains("Array.prototype.at"), "{label}");
  assert!(label.contains("Object.fromEntries"), "{label}");
  assert!(label.contains("@babel/plugin-transform-classes"), "{label}");
}

/// A few hundred bytes of hand-written compatibility code is not a
/// bundling problem, and reporting it as one is a false positive. Both
/// the script size and the estimated saving have to clear 5000 bytes.
#[test]
fn a_small_script_with_a_polyfill_is_below_the_reporting_floor() {
  let source = "Array.prototype.at = function(i){return this[i];};\n";
  let events = trace(vec![script_source(1, "https://site.example/tiny.js", source)]);
  let report = ferridriver_perf::analyze_values(&events);
  assert_eq!(insight(&report, "LegacyJavaScript").severity, Severity::Pass);
}

/// Modern source must not be flagged; a false positive here tells people
/// to remove code they need.
#[test]
fn modern_javascript_is_not_flagged_as_legacy() {
  let source = "const x = [1,2,3].at(-1);\nconst y = Object.fromEntries(new Map());\nclass A { b() {} }\n";
  let events = trace(vec![script_source(1, "https://site.example/modern.js", source)]);
  let report = ferridriver_perf::analyze_values(&events);
  assert_eq!(insight(&report, "LegacyJavaScript").severity, Severity::Pass);
}

#[test]
fn the_same_bundle_served_from_two_urls_is_duplicated() {
  let source = "x".repeat(4096);
  let events = trace(vec![
    script_source(1, "https://site.example/a/vendor.js", &source),
    script_source(2, "https://site.example/b/vendor.js", &source),
  ]);
  let report = ferridriver_perf::analyze_values(&events);
  let dup = insight(&report, "DuplicatedJavaScript");
  assert_eq!(dup.severity, Severity::Fail);
  // One redundant copy, so one bundle's worth of bytes.
  let (_, wasted) = dup.metrics.iter().find(|(k, _)| k == "wastedBytes").unwrap();
  assert!((wasted - 4096.0).abs() < f64::EPSILON, "got {wasted}");
}

/// One script fetched twice is a caching question, not duplication.
#[test]
fn the_same_url_seen_twice_is_not_duplicated() {
  let source = "y".repeat(4096);
  let events = trace(vec![
    script_source(1, "https://site.example/vendor.js", &source),
    script_source(2, "https://site.example/vendor.js", &source),
  ]);
  let report = ferridriver_perf::analyze_values(&events);
  assert_eq!(insight(&report, "DuplicatedJavaScript").severity, Severity::Pass);
}

/// Selector stats need profiling turned on. Absent them the answer is
/// "not measured", which is not the same claim as "fast".
#[test]
fn selector_costs_are_reported_as_unmeasured_without_stats() {
  let report = ferridriver_perf::analyze_values(&trace(vec![]));
  let sel = insight(&report, "SlowCSSSelector");
  assert_eq!(sel.severity, Severity::Informative);
  assert!(sel.checks[0].detail.contains("Not measured"));
}

#[test]
fn a_slow_selector_is_reported_when_stats_are_present() {
  let events = trace(vec![
    json!({"name":"SelectorStats","ph":"X","ts":60_000,"dur":100,"pid":1,"tid":1,
    "args":{"selector_stats":{"selector_timings":[
      {"selector":"div > .a *","elapsed (us)":2500,"match_attempts":9000,"match_count":12},
      {"selector":".b","elapsed (us)":10,"match_attempts":5,"match_count":5}
    ]}}}),
  ]);
  let report = ferridriver_perf::analyze_values(&events);
  let sel = insight(&report, "SlowCSSSelector");
  assert_eq!(sel.severity, Severity::Fail);
  assert_eq!(sel.items.len(), 1, "only the slow one: {:?}", sel.items);
  assert!(sel.items[0].label.starts_with("div > .a *"));
}
