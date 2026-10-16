//! Chromium's JavaScript and CSS coverage. Playwright: `class Coverage`
//! (`client/coverage.ts`), backed by `server/chromium/crCoverage.ts`.
//!
//! Each kind runs on a CDP session of its own attached to the page, so
//! enabling `Debugger` or `CSS` here leaves the page's own session as it
//! was. Other engines have no such domains; Playwright offers coverage on
//! Chromium only.

use std::sync::{Arc, Mutex};

use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::{Value, json};

use crate::cdp_session::{CdpListenerId, CdpSession};
use crate::error::{FerriError, Result};

/// Options for `coverage.startJSCoverage(options?)`.
#[derive(Debug, Clone, Default)]
pub struct JsCoverageOptions {
  /// Forget scripts when a navigation clears the execution contexts.
  /// Defaults to `true`.
  pub reset_on_navigation: Option<bool>,
  /// Report scripts that have no URL. Defaults to `false`.
  pub report_anonymous_scripts: Option<bool>,
}

/// Options for `coverage.startCSSCoverage(options?)`.
#[derive(Debug, Clone, Default)]
pub struct CssCoverageOptions {
  /// Forget stylesheets when a navigation clears the execution contexts.
  /// Defaults to `true`.
  pub reset_on_navigation: Option<bool>,
}

/// One script's coverage: V8's `ScriptCoverage` plus its source.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JsCoverageEntry {
  pub url: String,
  pub script_id: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub source: Option<String>,
  /// V8's `FunctionCoverage` list, as the protocol reports it.
  pub functions: Vec<Value>,
}

/// A used range of a stylesheet, as offsets into its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct CoverageRange {
  pub start: u64,
  pub end: u64,
}

/// One stylesheet's coverage.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CssCoverageEntry {
  pub url: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub text: Option<String>,
  pub ranges: Vec<CoverageRange>,
}

/// What a coverage run has collected so far, filled from event callbacks.
#[derive(Default)]
struct Collected {
  ids: FxHashSet<String>,
  sources: FxHashMap<String, String>,
  urls: FxHashMap<String, String>,
}

struct Run {
  session: CdpSession,
  listeners: Vec<CdpListenerId>,
  collected: Arc<Mutex<Collected>>,
  fetches: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
  report_anonymous: bool,
}

impl Run {
  fn lock(&self) -> std::sync::MutexGuard<'_, Collected> {
    self.collected.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  /// Let every event that arrived before the last reply reach its
  /// listener, then wait for the source fetches those started, so an
  /// entry does not lose its text to a race with `stop`.
  async fn settle(&self) {
    self.session.flush().await;
    let fetches = std::mem::take(&mut *self.fetches.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    for fetch in fetches {
      let _ = fetch.await;
    }
  }

  async fn finish(self) {
    for id in &self.listeners {
      self.session.off(*id);
    }
    let _ = self.session.detach().await;
  }
}

/// A page's coverage state: one run of each kind at a time.
#[derive(Default)]
pub(crate) struct CoverageState {
  js: Option<Run>,
  css: Option<Run>,
}

/// Playwright: `page.coverage`.
#[derive(Clone)]
pub struct Coverage {
  page: Arc<crate::page::Page>,
}

impl Coverage {
  pub(crate) fn new(page: Arc<crate::page::Page>) -> Self {
    Self { page }
  }

  fn state(&self) -> &tokio::sync::Mutex<CoverageState> {
    self.page.coverage_state()
  }

  /// Fetch `method` on `session` for an event, without blocking the
  /// event pump, remembering the task so `stop` can wait for it.
  fn fetch_later(
    run_session: &CdpSession,
    fetches: &Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    work: impl FnOnce(CdpSession) -> futures::future::BoxFuture<'static, ()>,
  ) {
    let task = tokio::spawn(work(run_session.clone()));
    let mut fetches = fetches.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    fetches.retain(|fetch| !fetch.is_finished());
    fetches.push(task);
  }

  /// Playwright: `coverage.startJSCoverage(options?)`.
  ///
  /// # Errors
  ///
  /// JavaScript coverage already running, a browser that is not
  /// Chromium, or a protocol failure.
  pub async fn start_js_coverage(&self, options: JsCoverageOptions) -> Result<()> {
    let mut state = self.state().lock().await;
    if state.js.is_some() {
      return Err(FerriError::invalid_argument(
        "coverage",
        "JSCoverage is already enabled",
      ));
    }
    let session = self.page.inner().new_cdp_session().await?;
    let reset = options.reset_on_navigation.unwrap_or(true);
    let report_anonymous = options.report_anonymous_scripts.unwrap_or(false);
    let collected = Arc::new(Mutex::new(Collected::default()));
    let fetches = Arc::new(Mutex::new(Vec::new()));
    let on_script = {
      let collected = Arc::clone(&collected);
      let fetches = Arc::clone(&fetches);
      let session_for_fetch = session.clone();
      session.on(
        "Debugger.scriptParsed",
        Arc::new(move |params: Value| {
          let Some(id) = params.get("scriptId").and_then(Value::as_str).map(str::to_string) else {
            return;
          };
          collected
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ids
            .insert(id.clone());
          let anonymous = params.get("url").and_then(Value::as_str).is_none_or(str::is_empty);
          if anonymous && !report_anonymous {
            return;
          }
          let collected = Arc::clone(&collected);
          Self::fetch_later(&session_for_fetch, &fetches, move |session| {
            Box::pin(async move {
              // The page may already have navigated away from the script.
              if let Ok(reply) = session
                .send("Debugger.getScriptSource", json!({ "scriptId": id }))
                .await
                && let Some(source) = reply.get("scriptSource").and_then(Value::as_str)
              {
                collected
                  .lock()
                  .unwrap_or_else(std::sync::PoisonError::into_inner)
                  .sources
                  .insert(id, source.to_string());
              }
            })
          });
        }),
      )
    };
    let cleared = Self::on_contexts_cleared(&session, &collected, reset);
    let on_pause = {
      let session_for_resume = session.clone();
      let resume_fetches = Arc::clone(&fetches);
      session.on(
        "Debugger.paused",
        Arc::new(move |_| {
          Self::fetch_later(&session_for_resume, &resume_fetches, |session| {
            Box::pin(async move {
              let _ = session.send("Debugger.resume", json!({})).await;
            })
          });
        }),
      )
    };
    let started = async {
      session.send("Runtime.enable", json!({})).await?;
      session.send("Profiler.enable", json!({})).await?;
      session
        .send(
          "Profiler.startPreciseCoverage",
          json!({ "callCount": true, "detailed": true }),
        )
        .await?;
      session.send("Debugger.enable", json!({})).await?;
      session.send("Debugger.setSkipAllPauses", json!({ "skip": true })).await
    }
    .await;
    let run = Run {
      session,
      listeners: vec![on_script, cleared, on_pause],
      collected,
      fetches,
      report_anonymous,
    };
    if let Err(error) = started {
      run.finish().await;
      return Err(error);
    }
    state.js = Some(run);
    Ok(())
  }

  /// Playwright: `coverage.stopJSCoverage(): Promise<JSCoverageEntry[]>`.
  /// Empty when coverage was not running.
  ///
  /// # Errors
  ///
  /// A protocol failure while collecting.
  pub async fn stop_js_coverage(&self) -> Result<Vec<JsCoverageEntry>> {
    let Some(run) = self.state().lock().await.js.take() else {
      return Ok(Vec::new());
    };
    let profile = run.session.send("Profiler.takePreciseCoverage", json!({})).await;
    // Sources are fetched through the Debugger, so they are collected
    // before it is turned off.
    run.settle().await;
    let _ = run.session.send("Profiler.stopPreciseCoverage", json!({})).await;
    let _ = run.session.send("Profiler.disable", json!({})).await;
    let _ = run.session.send("Debugger.disable", json!({})).await;
    let entries = profile.map(|profile| {
      let collected = run.lock();
      profile
        .get("result")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|script| {
          let script_id = script.get("scriptId").and_then(Value::as_str)?;
          let url = script.get("url").and_then(Value::as_str).unwrap_or_default();
          if !collected.ids.contains(script_id) || (url.is_empty() && !run.report_anonymous) {
            return None;
          }
          Some(JsCoverageEntry {
            url: url.to_string(),
            script_id: script_id.to_string(),
            source: collected.sources.get(script_id).cloned(),
            functions: script
              .get("functions")
              .and_then(Value::as_array)
              .cloned()
              .unwrap_or_default(),
          })
        })
        .collect()
    });
    run.finish().await;
    entries
  }

  /// Playwright: `coverage.startCSSCoverage(options?)`.
  ///
  /// # Errors
  ///
  /// CSS coverage already running, a browser that is not Chromium, or a
  /// protocol failure.
  pub async fn start_css_coverage(&self, options: CssCoverageOptions) -> Result<()> {
    let mut state = self.state().lock().await;
    if state.css.is_some() {
      return Err(FerriError::invalid_argument(
        "coverage",
        "CSSCoverage is already enabled",
      ));
    }
    let session = self.page.inner().new_cdp_session().await?;
    let reset = options.reset_on_navigation.unwrap_or(true);
    let collected = Arc::new(Mutex::new(Collected::default()));
    let fetches = Arc::new(Mutex::new(Vec::new()));
    let added = {
      let collected = Arc::clone(&collected);
      let fetches = Arc::clone(&fetches);
      let session_for_fetch = session.clone();
      session.on(
        "CSS.styleSheetAdded",
        Arc::new(move |params: Value| {
          let header = params.get("header").cloned().unwrap_or(Value::Null);
          let (Some(id), Some(url)) = (
            header.get("styleSheetId").and_then(Value::as_str).map(str::to_string),
            header
              .get("sourceURL")
              .and_then(Value::as_str)
              .filter(|url| !url.is_empty())
              .map(str::to_string),
          ) else {
            return;
          };
          let collected = Arc::clone(&collected);
          Self::fetch_later(&session_for_fetch, &fetches, move |session| {
            Box::pin(async move {
              if let Ok(reply) = session
                .send("CSS.getStyleSheetText", json!({ "styleSheetId": id }))
                .await
                && let Some(text) = reply.get("text").and_then(Value::as_str)
              {
                let mut collected = collected.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                collected.urls.insert(id.clone(), url);
                collected.sources.insert(id, text.to_string());
              }
            })
          });
        }),
      )
    };
    let cleared = Self::on_contexts_cleared(&session, &collected, reset);
    let started = async {
      session.send("Runtime.enable", json!({})).await?;
      session.send("DOM.enable", json!({})).await?;
      session.send("CSS.enable", json!({})).await?;
      session.send("CSS.startRuleUsageTracking", json!({})).await
    }
    .await;
    let run = Run {
      session,
      listeners: vec![added, cleared],
      collected,
      fetches,
      report_anonymous: false,
    };
    if let Err(error) = started {
      run.finish().await;
      return Err(error);
    }
    state.css = Some(run);
    Ok(())
  }

  /// Playwright: `coverage.stopCSSCoverage(): Promise<CSSCoverageEntry[]>`.
  /// Empty when coverage was not running.
  ///
  /// # Errors
  ///
  /// A protocol failure while collecting.
  pub async fn stop_css_coverage(&self) -> Result<Vec<CssCoverageEntry>> {
    let Some(run) = self.state().lock().await.css.take() else {
      return Ok(Vec::new());
    };
    let usage = run.session.send("CSS.stopRuleUsageTracking", json!({})).await;
    // Stylesheet text is fetched through the CSS domain, which forgets
    // every sheet once disabled.
    run.settle().await;
    let _ = run.session.send("CSS.disable", json!({})).await;
    let _ = run.session.send("DOM.disable", json!({})).await;
    let entries = usage.map(|usage| {
      let mut by_sheet: FxHashMap<String, Vec<(u64, u64, u64)>> = FxHashMap::default();
      for rule in usage.get("ruleUsage").and_then(Value::as_array).into_iter().flatten() {
        let (Some(id), Some(start), Some(end)) = (
          rule.get("styleSheetId").and_then(Value::as_str),
          rule.get("startOffset").and_then(Value::as_u64),
          rule.get("endOffset").and_then(Value::as_u64),
        ) else {
          continue;
        };
        let used = rule.get("used").and_then(Value::as_bool) == Some(true);
        by_sheet
          .entry(id.to_string())
          .or_default()
          .push((start, end, u64::from(used)));
      }
      let collected = run.lock();
      collected
        .urls
        .iter()
        .map(|(id, url)| CssCoverageEntry {
          url: url.clone(),
          text: collected.sources.get(id).cloned(),
          ranges: disjoint_ranges(by_sheet.remove(id).unwrap_or_default()),
        })
        .collect()
    });
    run.finish().await;
    entries
  }

  fn on_contexts_cleared(session: &CdpSession, collected: &Arc<Mutex<Collected>>, reset: bool) -> CdpListenerId {
    let collected = Arc::clone(collected);
    session.on(
      "Runtime.executionContextsCleared",
      Arc::new(move |_| {
        if reset {
          *collected.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Collected::default();
        }
      }),
    )
  }
}

/// Playwright's `convertToDisjointRanges`: nested `(start, end, count)`
/// rule ranges flattened into the disjoint ranges where the innermost
/// rule was used, dropping ranges of one character or less.
fn disjoint_ranges(nested: Vec<(u64, u64, u64)>) -> Vec<CoverageRange> {
  // (offset, is_end, length, count)
  let mut points: Vec<(u64, bool, u64, u64)> = Vec::with_capacity(nested.len() * 2);
  for (start, end, count) in nested {
    let length = end.saturating_sub(start);
    points.push((start, false, length, count));
    points.push((end, true, length, count));
  }
  points.sort_by(|a, b| {
    a.0
      .cmp(&b.0)
      // Ends go before starts at the same offset.
      .then_with(|| b.1.cmp(&a.1))
      .then_with(|| {
        if a.1 {
          // Two ends: the shorter range closes first.
          a.2.cmp(&b.2)
        } else {
          // Two starts: the longer range opens first.
          b.2.cmp(&a.2)
        }
      })
  });
  let mut stack: Vec<u64> = Vec::new();
  let mut results: Vec<CoverageRange> = Vec::new();
  let mut last = 0;
  for (offset, is_end, _, count) in points {
    if stack.last().is_some_and(|top| *top > 0) && last < offset {
      match results.last_mut() {
        Some(previous) if previous.end == last => previous.end = offset,
        _ => results.push(CoverageRange {
          start: last,
          end: offset,
        }),
      }
    }
    last = offset;
    if is_end {
      stack.pop();
    } else {
      stack.push(count);
    }
  }
  results.retain(|range| range.end - range.start > 1);
  results
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn nested_rule_ranges_flatten_to_where_the_innermost_rule_was_used() {
    // An unused rule [0, 50) holding a used rule [10, 20), next to a
    // used rule [20, 30): the used parts merge, the rest is dropped.
    let ranges = disjoint_ranges(vec![(0, 50, 0), (10, 20, 1), (20, 30, 1)]);
    assert_eq!(ranges, vec![CoverageRange { start: 10, end: 30 }]);
    // A used rule holding an unused one leaves a hole.
    let ranges = disjoint_ranges(vec![(0, 40, 1), (10, 20, 0)]);
    assert_eq!(
      ranges,
      vec![
        CoverageRange { start: 0, end: 10 },
        CoverageRange { start: 20, end: 40 }
      ]
    );
    assert!(disjoint_ranges(vec![(5, 6, 1)]).is_empty(), "one character is dropped");
  }
}
