//! Chrome performance trace analysis in Rust.
//!
//! Takes the events `Page::stop_tracing` returns and produces Core Web
//! Vitals, a resolved network waterfall, and the insights `DevTools` shows
//! in its Performance panel. Nothing here drives a browser or evaluates
//! JavaScript: input is a trace, output is a report, so an analysis is
//! reproducible from a saved trace file and testable without a browser.
//!
//! Ported from `devtools-frontend`'s `models/trace` (`handlers/` and
//! `insights/`). Where a number is a threshold or a magic multiplier, it
//! is carried over verbatim and its origin named, so our output stays
//! comparable with what `DevTools` reports for the same page.
//!
//! ```no_run
//! # fn main() -> Result<(), ferridriver_perf::Error> {
//! let bytes = std::fs::read("trace.json").unwrap();
//! let report = ferridriver_perf::analyze_json(&bytes)?;
//! println!("LCP {:?} ms", report.metrics.largest_contentful_paint);
//! # Ok(())
//! # }
//! ```

pub mod event;
pub mod handlers;
pub mod insights;
pub mod lantern;
pub mod units;

use serde::Serialize;

use handlers::meta::Meta;
use handlers::network::NetworkRequest;
use handlers::page_load::PageLoadMetrics;
use insights::Insight;

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("trace is not valid JSON: {0}")]
  Json(#[from] serde_json::Error),
  #[error("input is neither a trace-event array nor an object with `traceEvents`")]
  NotATrace,
}

/// Everything derived from one trace.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
  pub url: String,
  /// Wall time the trace covers, in milliseconds.
  pub duration_ms: f64,
  pub event_count: usize,
  pub metrics: Metrics,
  pub insights: Vec<Insight>,
  /// The resolved waterfall, slowest first.
  pub requests: Vec<RequestSummary>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
  pub first_paint: Option<f64>,
  pub first_contentful_paint: Option<f64>,
  pub largest_contentful_paint: Option<f64>,
  pub dom_content_loaded: Option<f64>,
  pub load: Option<f64>,
  pub cumulative_layout_shift: f64,
  /// Longest interaction, which is what INP reports.
  pub interaction_to_next_paint: Option<f64>,
  pub total_requests: usize,
  pub total_transfer_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestSummary {
  pub url: String,
  pub status: i64,
  pub mime_type: String,
  pub protocol: String,
  pub transfer_bytes: i64,
  pub duration_ms: f64,
  pub server_response_ms: f64,
  pub render_blocking: bool,
}

/// Analyse a trace read from disk, or as Chrome's `DevTools` saves it.
///
/// # Errors
///
/// [`Error::Json`] or [`Error::NotATrace`] when the input is not a trace.
pub fn analyze_json(bytes: &[u8]) -> Result<Report, Error> {
  Ok(analyze(&event::parse(bytes)?))
}

/// Analyse the events `Page::stop_tracing` returned.
///
/// An unrecognisable trace yields an empty report rather than an error:
/// a caller that traced a page which did nothing should see zero
/// insights, not a failure.
#[must_use]
pub fn analyze_values(values: &[serde_json::Value]) -> Report {
  analyze(&event::from_values(values))
}

/// The analysis proper, over already-typed events.
#[must_use]
pub fn analyze(events: &[event::TraceEvent<'_>]) -> Report {
  let meta = Meta::from_events(events);
  let requests = handlers::network::from_events(events);
  let metrics = PageLoadMetrics::from_events(events, &meta);

  // The main document is the first request on the main frame that the
  // navigation actually loaded; falling back to the first request at all
  // keeps insights working for a trace with no navigation in it.
  let document = requests
    .iter()
    .find(|r| !meta.main_frame_url.is_empty() && r.url == meta.main_frame_url)
    .or_else(|| requests.first());

  let first_paint_ts = metrics
    .first_contentful_paint
    .map(|ms| meta.time_origin() + units::ms_to_micros(ms));

  let paint = handlers::paint::LargestPaint::from_events(events, &meta, &requests, meta.time_origin());
  let signals = handlers::page_signals::PageSignals::from_events(events, &meta);
  let renderer = handlers::renderer::Renderer::from_events(events);
  let interactions = handlers::interactions::from_events(events);
  let painted_images = handlers::paint::painted_images(events);
  let culprits = handlers::page_signals::layout_shift_culprits(events);
  let scripts = handlers::scripts::from_events(events);
  let lcp_ts = metrics
    .largest_contentful_paint
    .map(|ms| meta.time_origin() + units::ms_to_micros(ms));

  // One graph for the whole report. Every predicted saving below is
  // measured against it, and building it per insight would collect the
  // main thread's tasks eight times over.
  let lantern = lantern::Context::build(&requests, &meta.main_frame_url, events, first_paint_ts, lcp_ts);
  let lantern = lantern.as_ref();

  let mut insights = Vec::new();
  if let Some(insight) = insights::document_latency::run(document) {
    insights.push(insight);
  }
  insights.push(insights::lcp_breakdown::run(
    document,
    &requests,
    &paint,
    lcp_ts,
    meta.time_origin(),
  ));
  insights.push(insights::lcp_discovery::run(document, &requests, &paint));
  if let Some(insight) = insights::render_blocking::run(&requests, first_paint_ts, lantern, paint.request.is_some()) {
    insights.push(insight);
  }
  insights.push(insights::inp_breakdown::run(&interactions));
  insights.push(insights::dom_size::run(&renderer));
  insights.push(insights::forced_reflow::run(&renderer));
  insights.push(insights::cls_culprits::run(
    &metrics.layout_shifts,
    &culprits,
    metrics.cumulative_layout_shift,
  ));
  insights.push(insights::image_delivery::run(&requests, &painted_images, lantern));
  insights.push(insights::network_dependency_tree::run(
    &requests,
    &meta.main_frame_url,
    lantern,
  ));
  if let Some(insight) = insights::character_set::run(document, signals.meta_charset) {
    insights.push(insight);
  }
  insights.push(insights::duplicated_javascript::run(&scripts, lantern));
  insights.push(insights::legacy_javascript::run(&scripts, lantern));
  insights.push(insights::slow_css_selector::run(&signals.selector_timings));
  insights.push(insights::cache::run(&requests, lantern));
  insights.push(insights::font_display::run(&signals.fonts, &requests));
  insights.push(insights::viewport::run(&signals.viewport));
  insights.push(insights::modern_http::run(&requests, lantern));
  insights.push(insights::third_parties::run(&requests, &meta.main_frame_url));

  let mut summaries: Vec<RequestSummary> = requests.iter().map(summarize).collect();
  summaries.sort_by(|a, b| b.duration_ms.total_cmp(&a.duration_ms));

  Report {
    url: meta.main_frame_url.clone(),
    duration_ms: units::micros_to_ms(meta.trace_end - meta.trace_start),
    event_count: events.len(),
    metrics: Metrics {
      first_paint: metrics.first_paint,
      first_contentful_paint: metrics.first_contentful_paint,
      largest_contentful_paint: metrics.largest_contentful_paint,
      dom_content_loaded: metrics.dom_content_loaded,
      load: metrics.load,
      cumulative_layout_shift: metrics.cumulative_layout_shift,
      interaction_to_next_paint: interactions
        .first()
        .map(handlers::interactions::Interaction::duration_ms),
      total_requests: requests.len(),
      total_transfer_bytes: requests.iter().map(|r| r.encoded_data_length).sum(),
    },
    insights,
    requests: summaries,
  }
}

fn summarize(r: &NetworkRequest) -> RequestSummary {
  RequestSummary {
    url: r.url.clone(),
    status: r.status_code,
    mime_type: r.mime_type.clone(),
    protocol: r.protocol.clone(),
    transfer_bytes: r.encoded_data_length,
    duration_ms: crate::units::micros_to_ms(r.end_time - r.start_time),
    server_response_ms: crate::units::micros_to_ms(r.timing.server_response_time),
    render_blocking: matches!(r.render_blocking.as_str(), "blocking" | "in_body_parser_blocking"),
  }
}
