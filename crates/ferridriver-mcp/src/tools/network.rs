use crate::params::{DiagnosticsKind, DiagnosticsParams};
use crate::server::{McpServer, sess};
use rmcp::{
  ErrorData,
  handler::server::wrapper::Parameters,
  model::{CallToolResult, ContentBlock},
  tool, tool_router,
};
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
    let s = sess(p.session.as_opt());
    match p.r#type {
      DiagnosticsKind::Console => {
        let _guard = self.session_guard(s).await;
        let handles = self
          .state
          .log_handles_for(s)
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
        Ok(CallToolResult::success(vec![ContentBlock::text(
          serde_json::to_string_pretty(&msgs).unwrap_or_default(),
        )]))
      },
      DiagnosticsKind::Network => {
        let _guard = self.session_guard(s).await;
        let handles = self
          .state
          .log_handles_for(s)
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
        Ok(CallToolResult::success(vec![ContentBlock::text(
          serde_json::to_string_pretty(&snapshots).unwrap_or_default(),
        )]))
      },
      DiagnosticsKind::TraceStart => {
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        page.start_tracing().await.map_err(Self::err)?;
        Ok(CallToolResult::success(vec![ContentBlock::text("Trace started.")]))
      },
      DiagnosticsKind::TraceStop => {
        let _guard = self.session_guard(s).await;
        let page = Box::pin(self.page(s)).await?;
        page.stop_tracing().await.map_err(Self::err)?;
        let metrics = page.metrics().await.map_err(Self::err)?;
        let mut out = String::from("Trace stopped.\n\n### Performance Metrics\n");
        for m in &metrics {
          if m.value > 0.0 {
            let _ = writeln!(out, "- {}: {:.2}", m.name, m.value);
          }
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(out)]))
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use super::newest_matching;

  const URLS: [&str; 6] = [
    "https://cdn.example.com/assets/main.js",
    "https://localhost:3000/remote.js",
    "https://app.example.com/api/one",
    "https://app.example.com/api/two",
    "https://app.example.com/api/three",
    "https://app.example.com/api/four",
  ];

  fn select(needle: Option<&str>, limit: usize) -> Vec<&'static str> {
    newest_matching(URLS.iter(), needle, limit, |u| u)
      .into_iter()
      .copied()
      .collect()
  }

  #[test]
  fn no_filter_returns_the_newest_entries_in_order() {
    assert_eq!(
      select(None, 2),
      vec!["https://app.example.com/api/three", "https://app.example.com/api/four"]
    );
  }

  // The whole point of the filter: the interesting request is the oldest one
  // here, so limiting first would drop it.
  #[test]
  fn filter_is_applied_before_the_limit() {
    assert_eq!(select(Some("cdn.example"), 2), vec![URLS[0]]);
    assert_eq!(select(Some("localhost:3000"), 2), vec![URLS[1]]);
  }

  #[test]
  fn filter_is_case_insensitive() {
    // `needle` arrives pre-lowercased from the caller.
    assert_eq!(select(Some("cdn.example"), 5), vec![URLS[0]]);
    assert_eq!(
      newest_matching(["HTTPS://CDN.EXAMPLE.COM/x"].iter(), Some("cdn.example"), 5, |u| u).len(),
      1
    );
  }

  #[test]
  fn filter_with_no_match_returns_nothing() {
    assert!(select(Some("nonesuch"), 50).is_empty());
  }

  #[test]
  fn limit_zero_returns_nothing() {
    assert!(select(None, 0).is_empty());
  }

  #[test]
  fn limit_above_the_count_returns_everything_in_order() {
    assert_eq!(select(None, 100), URLS.to_vec());
  }
}
