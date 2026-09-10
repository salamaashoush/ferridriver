use crate::params::{DiagnosticsKind, DiagnosticsParams};
use crate::server::McpServer;
use rmcp::{ErrorData, handler::server::wrapper::Parameters, model::CallToolResult, tool, tool_router};
use std::fmt::Write;

/// The newest `limit` items whose projected text contains `needle`, in original
/// order.
///
/// `needle` must already be lowercased. Filtering happens *before* the limit is
/// applied: limiting first would return whatever happens to match inside the
/// newest `limit` entries, which on a busy page is usually nothing.
fn newest_matching<I, T, F>(items: I, needle: Option<&str>, limit: usize, text_of: F) -> Vec<T>
where
  I: DoubleEndedIterator<Item = T>,
  F: Fn(&T) -> &str,
{
  let mut selected: Vec<T> = items
    .filter(|item| needle.is_none_or(|needle| text_of(item).to_lowercase().contains(needle)))
    .rev()
    .take(limit)
    .collect();
  selected.reverse();
  selected
}

#[tool_router(router = network_router, vis = "pub")]
impl McpServer {
  #[tool(
    name = "diagnostics",
    title = "Page Diagnostics",
    description = "Page diagnostics. REQUIRED param `type`, one of: console (log/warn/error messages), \
    network (HTTP requests since load), trace_start (begin perf tracing), trace_stop (end tracing + metrics). \
    Narrow the output before reading it: `filter` is a case-insensitive substring match (request URL for network, \
    message text for console) applied before `limit` (default 50), and `summary: true` reduces each network entry to \
    { method, status, url }. A full network read includes every request and response header and can exceed 200 KB on \
    a real page, so pass `summary: true` and/or `filter` unless you specifically need headers.",
    annotations(read_only_hint = true, open_world_hint = false)
  )]
  async fn diagnostics(&self, Parameters(p): Parameters<DiagnosticsParams>) -> Result<CallToolResult, ErrorData> {
    match p.r#type {
      DiagnosticsKind::Console => {
        self
          .on_session(p.session.as_opt(), async |s| {
            let handles = self
              .state
              .log_handles_for(&s)
              .await
              .ok_or_else(|| Self::err(format!("Context '{s}' not found")))?;
            let limit = p.limit.unwrap_or(50);
            let level = p.level.unwrap_or_default();
            let needle = p.filter.as_deref().map(str::to_lowercase);
            let log = handles.console.read().await;
            let matched = newest_matching(
              log.iter().filter(|m| level.accepts(m.type_str())),
              needle.as_deref(),
              limit,
              |m| m.text(),
            );
            let msgs: Vec<serde_json::Value> = matched
              .into_iter()
              .map(|m| {
                serde_json::json!({
                  "type": m.type_str(),
                  "text": m.text(),
                })
              })
              .collect();
            drop(log);
            Ok(self.ok_text(serde_json::to_string_pretty(&msgs).unwrap_or_default()))
          })
          .await
      },
      DiagnosticsKind::Network => {
        self
          .on_session(p.session.as_opt(), async |s| {
            let handles = self
              .state
              .log_handles_for(&s)
              .await
              .ok_or_else(|| Self::err(format!("Context '{s}' not found")))?;
            let limit = p.limit.unwrap_or(50);
            let needle = p.filter.as_deref().map(str::to_lowercase);
            let log = handles.network.read().await;
            let reqs: Vec<_> = newest_matching(log.iter(), needle.as_deref(), limit, |req| req.url())
              .into_iter()
              .cloned()
              .collect();
            drop(log);

            // Full records carry every request and response header, which runs to
            // hundreds of KB on a real page.
            let summary = p.summary.unwrap_or(false);
            let mut snapshots = Vec::with_capacity(reqs.len());
            for req in &reqs {
              if summary {
                snapshots.push(req.to_summary_json().await);
              } else {
                snapshots.push(req.to_diagnostic_json().await);
              }
            }
            Ok(self.ok_text(serde_json::to_string_pretty(&snapshots).unwrap_or_default()))
          })
          .await
      },
      DiagnosticsKind::TraceStart => {
        self
          .on_page(p.session.as_opt(), async |page, _s| {
            page.start_tracing(None).await.map_err(Self::err)?;
            Ok(self.ok_text("Trace started. Navigate or interact, then call trace_stop."))
          })
          .await
      },
      DiagnosticsKind::TraceStop => {
        self
          .on_page(p.session.as_opt(), async |page, _s| {
            // The paint markers (first paint, LCP candidates) are emitted
            // when the compositor commits a frame, which happens after
            // `load`. Ending the trace the moment the caller asks yields a
            // report with no Core Web Vitals in it at all, which is the
            // main thing they were tracing for. Two frames, because the
            // first rAF can run before the commit that carries the paint.
            let _ = page
              .evaluate(
                "new Promise(r => requestAnimationFrame(() => requestAnimationFrame(() => r(1))))",
                ferridriver::protocol::serializers::SerializedArgument::default(),
                None,
              )
              .await;
            let events = page.stop_tracing().await.map_err(Self::err)?;
            let report = ferridriver_perf::analyze_values(&events);
            Ok(self.ok_text(render_trace_report(&report)))
          })
          .await
      },
    }
  }
}

/// Render a trace report as the compact markdown an agent reads, rather
/// than the full JSON: a real trace carries hundreds of requests, and
/// the point of the analysis is that the caller does not have to wade
/// through them.
fn render_trace_report(report: &ferridriver_perf::Report) -> String {
  let mut out = String::from("Trace stopped.\n\n");
  let _ = writeln!(
    out,
    "{} events over {:.0} ms{}",
    report.event_count,
    report.duration_ms,
    if report.url.is_empty() {
      String::new()
    } else {
      format!(" on {}", report.url)
    }
  );

  out.push_str("\n### Metrics\n");
  let m = &report.metrics;
  let row = |out: &mut String, label: &str, value: Option<f64>| {
    if let Some(v) = value {
      let _ = writeln!(out, "- {label}: {v:.0} ms");
    }
  };
  row(&mut out, "First Contentful Paint", m.first_contentful_paint);
  row(&mut out, "Largest Contentful Paint", m.largest_contentful_paint);
  row(&mut out, "DOM Content Loaded", m.dom_content_loaded);
  row(&mut out, "Load", m.load);
  let _ = writeln!(out, "- Cumulative Layout Shift: {:.4}", m.cumulative_layout_shift);
  let _ = writeln!(
    out,
    "- Requests: {} ({:.0} KB transferred)",
    m.total_requests,
    f64::from(u32::try_from(m.total_transfer_bytes.max(0)).unwrap_or(u32::MAX)) / 1024.0
  );

  out.push_str("\n### Insights\n");
  for insight in &report.insights {
    let mark = match insight.severity {
      ferridriver_perf::insights::Severity::Pass => "PASS",
      ferridriver_perf::insights::Severity::Informative => "INFO",
      ferridriver_perf::insights::Severity::Fail => "FAIL",
    };
    let _ = writeln!(out, "\n**[{mark}] {}**", insight.title);
    for check in &insight.checks {
      let _ = writeln!(out, "- {} {}", if check.passed { "ok" } else { "!!" }, check.detail);
    }
    // Only the worst few: the tail of a render-blocking list is noise
    // once the caller knows what the top offenders are.
    for item in insight.items.iter().take(5) {
      let _ = writeln!(out, "  - {} ({:.0} {})", item.label, item.value, item.unit);
    }
  }
  out
}
