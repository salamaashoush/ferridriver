//! Rendering a trace as text.
//!
//! The trace viewer answers "what happened?" with a browser; this answers it
//! with a terminal. Same model, no GUI: what ran, in what order, how long it
//! took, what failed and what the failing call was waiting for — plus the
//! console, network and attachment summaries that usually explain it.
//!
//! Playwright has no equivalent, and the absence is felt exactly where a
//! browser is least available: over ssh, in CI logs, and by an agent reading
//! a failed run.

use std::fmt::Write as _;

use crate::model::{Action, ContextEntry, TraceModel};

/// How much of a trace to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
  /// Every call, message and request.
  #[default]
  Everything,
  /// Only what went wrong: failing calls, console errors, failed requests.
  Failures,
}

/// The optional sections below the call tree.
#[derive(Debug, Clone, Copy)]
pub struct Sections {
  /// Per-call log — what an auto-retrying call was waiting for.
  pub logs: bool,
  /// Console messages from the page.
  pub console: bool,
  /// Network requests.
  pub network: bool,
}

impl Default for Sections {
  fn default() -> Self {
    Self {
      logs: true,
      console: true,
      network: true,
    }
  }
}

/// What to include in a rendered trace.
#[derive(Debug, Clone, Default)]
pub struct DumpOptions {
  pub scope: Scope,
  pub sections: Sections,
  /// Truncate call lists to this many entries per context.
  pub limit: Option<usize>,
  /// ANSI colors.
  pub color: bool,
}

impl DumpOptions {
  fn failures_only(&self) -> bool {
    self.scope == Scope::Failures
  }
}

/// Render `model` as a human-readable report.
#[must_use]
pub fn render(model: &TraceModel, options: &DumpOptions) -> String {
  let mut out = String::new();
  for (index, context) in model.contexts.iter().enumerate() {
    if index > 0 {
      out.push('\n');
    }
    render_context(&mut out, context, options);
  }
  if model.contexts.is_empty() {
    out.push_str("empty trace\n");
  }
  out
}

fn render_context(out: &mut String, context: &ContextEntry, options: &DumpOptions) {
  let paint = |text: &str, code: &str| paint_if(options.color, text, code);

  render_heading(out, context, options);

  let shown = render_actions(out, context, options);
  if options.failures_only() {
    // The calls left out here are the ones that passed — counting them
    // as "more" would read like something was truncated.
    if shown == 0 {
      let _ = writeln!(out, "{}", paint("  no failing calls", DIM));
    }
  } else if let Some(hidden) = context.actions.len().checked_sub(shown).filter(|hidden| *hidden > 0) {
    let _ = writeln!(out, "{}", paint(&format!("  … {hidden} more"), DIM));
  }

  render_run_errors(out, context, options);
  if options.sections.console {
    render_console(out, context, options);
  }
  if options.sections.network {
    render_network(out, context, options);
  }

  if !context.stdout.is_empty() || !context.stderr.is_empty() {
    let _ = writeln!(
      out,
      "\n{} {} stdout, {} stderr chunk(s)",
      paint("output", BOLD),
      context.stdout.len(),
      context.stderr.len()
    );
  }

  let failed = context.actions.iter().filter(|action| action.error.is_some()).count();
  if failed > 0 {
    let _ = writeln!(out, "\n{}", paint(&format!("{failed} failed action(s)"), RED));
  }
}

fn render_heading(out: &mut String, context: &ContextEntry, options: &DumpOptions) {
  let paint = |text: &str, code: &str| paint_if(options.color, text, code);
  let title = context.title.clone().unwrap_or_else(|| context.prefix.clone());
  let _ = writeln!(out, "{}", paint(&title, BOLD));

  let mut facts = Vec::new();
  if !context.browser_name.is_empty() {
    facts.push(context.browser_name.clone());
  }
  if !context.platform.is_empty() {
    facts.push(context.platform.clone());
  }
  if let Some(version) = &context.recorder_version {
    facts.push(version.clone());
  }
  facts.push(format!(
    "{} action{}",
    context.actions.len(),
    plural(context.actions.len())
  ));
  if !context.pages.is_empty() {
    facts.push(format!("{} page{}", context.pages.len(), plural(context.pages.len())));
  }
  if let Some(duration) = context_duration_ms(context) {
    facts.push(format_ms(duration));
  }
  if context.screencast_frames > 0 {
    facts.push(format!(
      "{} frame{}",
      context.screencast_frames,
      plural(context.screencast_frames)
    ));
  }
  let _ = writeln!(out, "{}", paint(&facts.join(" · "), DIM));
}

/// Failures the runner recorded outside any call (assertion messages,
/// unhandled errors) — the trace's `error` events.
fn render_run_errors(out: &mut String, context: &ContextEntry, options: &DumpOptions) {
  let paint = |text: &str, code: &str| paint_if(options.color, text, code);
  for error in &context.errors {
    let _ = writeln!(out, "\n{} {}", paint("error", RED), first_line(&error.message));
    for line in error.message.lines().skip(1).take(20) {
      let _ = writeln!(out, "  {line}");
    }
    for frame in error.stack.iter().take(5) {
      let _ = writeln!(out, "  {}", paint(&format!("at {frame}"), DIM));
    }
  }
}

fn render_console(out: &mut String, context: &ContextEntry, options: &DumpOptions) {
  if context.console.is_empty() {
    return;
  }
  let paint = |text: &str, code: &str| paint_if(options.color, text, code);
  let errors = context
    .console
    .iter()
    .filter(|message| message.message_type == "error")
    .count();
  let _ = writeln!(
    out,
    "\n{} {} message{}{}",
    paint("console", BOLD),
    context.console.len(),
    plural(context.console.len()),
    if errors > 0 {
      format!(", {errors} error(s)")
    } else {
      String::new()
    }
  );
  for message in context
    .console
    .iter()
    .filter(|message| !options.failures_only() || message.message_type == "error")
  {
    let tag = match message.message_type.as_str() {
      "error" => paint("error", RED),
      "warning" => paint("warn ", YELLOW),
      other => paint(&format!("{other:<5}"), DIM),
    };
    let _ = writeln!(out, "  {tag} {}", first_line(&message.text));
  }
}

fn render_network(out: &mut String, context: &ContextEntry, options: &DumpOptions) {
  if context.network.is_empty() {
    return;
  }
  let paint = |text: &str, code: &str| paint_if(options.color, text, code);
  let failed = context.network.iter().filter(|entry| entry.failed()).count();
  let _ = writeln!(
    out,
    "\n{} {} request{}{}",
    paint("network", BOLD),
    context.network.len(),
    plural(context.network.len()),
    if failed > 0 {
      format!(", {failed} failed")
    } else {
      String::new()
    }
  );
  for entry in context
    .network
    .iter()
    .filter(|entry| !options.failures_only() || entry.failed())
  {
    let status = if entry.status == 0 {
      paint("---", RED)
    } else if entry.failed() {
      paint(&entry.status.to_string(), RED)
    } else {
      entry.status.to_string()
    };
    let _ = writeln!(out, "  {status} {:<6} {}", entry.method, entry.url);
  }
}

/// Render the action tree; returns how many actions were printed.
fn render_actions(out: &mut String, context: &ContextEntry, options: &DumpOptions) -> usize {
  let mut printed = 0;
  let mut depth_of: rustc_hash::FxHashMap<&str, usize> = rustc_hash::FxHashMap::default();
  for action in &context.actions {
    let depth = action
      .parent_id
      .as_deref()
      .and_then(|parent| depth_of.get(parent).copied())
      .map_or(0, |parent_depth| parent_depth + 1);
    depth_of.insert(action.call_id.as_str(), depth);

    if options.failures_only() && action.error.is_none() {
      continue;
    }
    if options.limit.is_some_and(|limit| printed >= limit) {
      break;
    }
    printed += 1;
    render_action(out, action, depth, options);
  }
  printed
}

fn render_action(out: &mut String, action: &Action, depth: usize, options: &DumpOptions) {
  let paint = |text: &str, code: &str| paint_if(options.color, text, code);
  let indent = "  ".repeat(depth + 1);
  let duration = action
    .duration_ms()
    .map_or_else(|| paint("running", YELLOW), |ms| paint(&format_ms(ms), DIM));
  let summary = params_summary(action);
  let mark = if action.error.is_some() {
    paint("x ", RED)
  } else {
    String::new()
  };
  let _ = writeln!(
    out,
    "{indent}{mark}{}{} {duration}",
    action.display_title(),
    if summary.is_empty() {
      String::new()
    } else {
      format!(" {}", paint(&summary, CYAN))
    }
  );

  if let Some(error) = &action.error {
    let _ = writeln!(
      out,
      "{indent}  {} {}",
      paint(&error.name, RED),
      first_line(&error.message)
    );
    for line in error.message.lines().skip(1).take(8) {
      let _ = writeln!(out, "{indent}  {}", paint(line, DIM));
    }
  }
  if options.sections.logs && (action.error.is_some() || !options.failures_only()) {
    for log in action.logs.iter().take(if action.error.is_some() { 12 } else { 3 }) {
      let _ = writeln!(out, "{indent}  {}", paint(log, DIM));
    }
  }
  for attachment in &action.attachments {
    let _ = writeln!(
      out,
      "{indent}  {} {} ({})",
      paint("attachment", DIM),
      attachment.name,
      attachment.content_type
    );
  }
  if let Some(frame) = action.stack.first() {
    let _ = writeln!(out, "{indent}  {}", paint(&format!("at {frame}"), DIM));
  }
}

/// The parameter a reader actually wants next to the call name.
fn params_summary(action: &Action) -> String {
  let object = action.params.as_object();
  for key in ["url", "selector", "value", "text", "name", "key", "state", "expression"] {
    if let Some(value) = object.and_then(|params| params.get(key)) {
      let rendered = match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
      };
      if !rendered.is_empty() {
        return truncate(&rendered, 100);
      }
    }
  }
  String::new()
}

fn context_duration_ms(context: &ContextEntry) -> Option<f64> {
  let start = context
    .actions
    .iter()
    .map(|a| a.start_time)
    .fold(f64::INFINITY, f64::min);
  let end = context
    .actions
    .iter()
    .filter_map(Action::duration_ms)
    .zip(context.actions.iter().map(|a| a.start_time))
    .map(|(duration, start)| start + duration)
    .fold(f64::NEG_INFINITY, f64::max);
  (start.is_finite() && end.is_finite() && end >= start).then_some(end - start)
}

/// Machine-readable form of the same model.
#[must_use]
pub fn to_json(model: &TraceModel) -> serde_json::Value {
  let contexts: Vec<serde_json::Value> = model
    .contexts
    .iter()
    .map(|context| {
      serde_json::json!({
        "prefix": context.prefix,
        "title": context.title,
        "origin": context.origin,
        "browserName": context.browser_name,
        "platform": context.platform,
        "recorderVersion": context.recorder_version,
        "wallTime": context.wall_time,
        "pages": context.pages,
        "screencastFrames": context.screencast_frames,
        "actions": context.actions.iter().map(action_json).collect::<Vec<_>>(),
        "console": context.console.iter().map(|message| serde_json::json!({
          "time": message.time,
          "type": message.message_type,
          "text": message.text,
          "url": message.url,
          "lineNumber": message.line_number,
        })).collect::<Vec<_>>(),
        "events": context.events.iter().map(|event| serde_json::json!({
          "time": event.time,
          "method": event.method,
          "params": event.params,
        })).collect::<Vec<_>>(),
        "errors": context.errors.iter().map(|error| serde_json::json!({
          "message": error.message,
          "stack": error.stack.iter().map(|frame| serde_json::json!({
            "file": frame.file, "line": frame.line, "column": frame.column,
          })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "network": context.network.iter().map(|entry| serde_json::json!({
          "method": entry.method,
          "url": entry.url,
          "status": entry.status,
          "mimeType": entry.mime_type,
          "durationMs": entry.duration_ms,
        })).collect::<Vec<_>>(),
        "stdout": context.stdout,
        "stderr": context.stderr,
      })
    })
    .collect();
  serde_json::json!({ "hasSources": model.has_sources, "contexts": contexts })
}

fn action_json(action: &Action) -> serde_json::Value {
  serde_json::json!({
    "callId": action.call_id,
    "parentId": action.parent_id,
    "stepId": action.step_id,
    "class": action.class,
    "method": action.method,
    "title": action.display_title(),
    "params": action.params,
    "pageId": action.page_id,
    "startTime": action.start_time,
    "endTime": action.end_time,
    "durationMs": action.duration_ms(),
    "error": action.error.as_ref().map(|error| serde_json::json!({
      "name": error.name, "message": error.message,
    })),
    "logs": action.logs,
    "attachments": action.attachments.iter().map(|attachment| serde_json::json!({
      "name": attachment.name,
      "contentType": attachment.content_type,
      "sha1": attachment.sha1,
      "path": attachment.path,
    })).collect::<Vec<_>>(),
    "stack": action.stack.iter().map(|frame| serde_json::json!({
      "file": frame.file, "line": frame.line, "column": frame.column,
    })).collect::<Vec<_>>(),
    "point": action.point.map(|(x, y)| serde_json::json!({ "x": x, "y": y })),
  })
}

/// One line summarizing a whole trace — what a listing prints per file.
#[must_use]
pub fn one_line_summary(model: &TraceModel) -> String {
  let actions: usize = model.contexts.iter().map(|context| context.actions.len()).sum();
  let failures = model.failures().len();
  let duration: f64 = model.contexts.iter().filter_map(context_duration_ms).sum();
  let title = model
    .contexts
    .iter()
    .find_map(|context| context.title.clone())
    .unwrap_or_default();
  let browser = model
    .contexts
    .first()
    .map(|context| context.browser_name.clone())
    .unwrap_or_default();
  let mut parts = vec![format!("{actions} actions"), format_ms(duration)];
  if !browser.is_empty() {
    parts.insert(0, browser);
  }
  if failures > 0 {
    parts.push(format!("{failures} failed"));
  }
  if title.is_empty() {
    parts.join(" · ")
  } else {
    format!("{title} — {}", parts.join(" · "))
  }
}

const BOLD: &str = "1";
const DIM: &str = "2";
const RED: &str = "31";
const YELLOW: &str = "33";
const CYAN: &str = "36";

fn paint_if(color: bool, text: &str, code: &str) -> String {
  if color {
    format!("\u{1b}[{code}m{text}\u{1b}[0m")
  } else {
    text.to_string()
  }
}

fn plural(count: usize) -> &'static str {
  if count == 1 { "" } else { "s" }
}

fn first_line(text: &str) -> String {
  truncate(text.lines().next().unwrap_or_default(), 200)
}

fn truncate(text: &str, max: usize) -> String {
  if text.chars().count() <= max {
    return text.to_string();
  }
  let kept: String = text.chars().take(max.saturating_sub(1)).collect();
  format!("{kept}…")
}

/// Milliseconds as a person reads them.
#[must_use]
pub fn format_ms(ms: f64) -> String {
  if ms >= 60_000.0 {
    format!("{}m{:02}s", (ms / 60_000.0).floor(), ((ms % 60_000.0) / 1000.0).floor())
  } else if ms >= 1000.0 {
    format!("{:.1}s", ms / 1000.0)
  } else {
    format!("{}ms", ms.round())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{ActionError, Attachment, ConsoleMessage, NetworkEntry, StackFrame};

  fn model() -> TraceModel {
    let goto = Action {
      call_id: "call@1".into(),
      class: "Page".into(),
      method: "goto".into(),
      title: "page.goto".into(),
      params: serde_json::json!({ "url": "http://app.local" }),
      start_time: 0.0,
      end_time: Some(412.0),
      stack: vec![StackFrame {
        file: "/spec.ts".into(),
        line: 4,
        column: 3,
      }],
      ..Action::default()
    };
    let click = Action {
      call_id: "call@2".into(),
      parent_id: Some("call@1".into()),
      class: "Locator".into(),
      method: "click".into(),
      title: "locator.click".into(),
      params: serde_json::json!({ "selector": "#submit" }),
      start_time: 500.0,
      end_time: Some(1700.0),
      error: Some(ActionError {
        name: "TimeoutError".into(),
        message: "Timeout 1000ms exceeded\nwaiting for locator('#submit')".into(),
      }),
      logs: vec!["waiting for locator('#submit')".into()],
      attachments: vec![Attachment {
        name: "screenshot".into(),
        content_type: "image/png".into(),
        sha1: Some("abc.png".into()),
        path: None,
      }],
      ..Action::default()
    };
    TraceModel {
      has_sources: true,
      contexts: vec![ContextEntry {
        prefix: "trace".into(),
        title: Some("checkout".into()),
        browser_name: "chromium".into(),
        platform: "darwin".into(),
        actions: vec![goto, click],
        console: vec![ConsoleMessage {
          time: 10.0,
          message_type: "error".into(),
          text: "boom".into(),
          url: "http://app.local".into(),
          line_number: 1,
        }],
        network: vec![
          NetworkEntry {
            method: "GET".into(),
            url: "http://app.local/app.js".into(),
            status: 200,
            mime_type: "text/javascript".into(),
            duration_ms: 12.0,
          },
          NetworkEntry {
            method: "POST".into(),
            url: "http://app.local/api".into(),
            status: 500,
            mime_type: "application/json".into(),
            duration_ms: 3.0,
          },
        ],
        pages: vec!["page@1".into()],
        ..ContextEntry::default()
      }],
    }
  }

  #[test]
  fn renders_calls_with_params_timings_and_failure_detail() {
    let text = render(&model(), &DumpOptions::default());
    assert!(text.contains("page.goto http://app.local"), "{text}");
    assert!(text.contains("412ms"), "{text}");
    assert!(text.contains("locator.click #submit"), "{text}");
    assert!(text.contains("1.2s"), "{text}");
    assert!(text.contains("TimeoutError"), "{text}");
    assert!(text.contains("waiting for locator('#submit')"), "{text}");
    assert!(text.contains("attachment screenshot"), "{text}");
    assert!(text.contains("at /spec.ts:4"), "{text}");
  }

  #[test]
  fn nests_child_calls_under_their_parent() {
    let text = render(&model(), &DumpOptions::default());
    let click_line = text
      .lines()
      .find(|line| line.contains("locator.click"))
      .expect("click line");
    let goto_line = text.lines().find(|line| line.contains("page.goto")).expect("goto line");
    assert!(
      click_line.len() - click_line.trim_start().len() > goto_line.len() - goto_line.trim_start().len(),
      "child not indented:\n{text}"
    );
  }

  #[test]
  fn errors_only_on_a_clean_trace_says_so() {
    let mut clean = model();
    clean.contexts[0].actions.retain(|action| action.error.is_none());
    let text = render(
      &clean,
      &DumpOptions {
        scope: Scope::Failures,
        ..DumpOptions::default()
      },
    );
    assert!(text.contains("no failing calls"), "{text}");
    assert!(
      !text.contains("more"),
      "passing calls must not read as truncation:\n{text}"
    );
  }

  #[test]
  fn errors_only_drops_passing_calls_and_healthy_requests() {
    let options = DumpOptions {
      scope: Scope::Failures,
      ..DumpOptions::default()
    };
    let text = render(&model(), &options);
    assert!(!text.contains("page.goto"), "{text}");
    assert!(text.contains("locator.click"), "{text}");
    assert!(!text.contains("app.js"), "{text}");
    assert!(text.contains("/api"), "{text}");
  }

  #[test]
  fn summaries_count_network_failures_and_console_errors() {
    let text = render(&model(), &DumpOptions::default());
    assert!(text.contains("2 requests, 1 failed"), "{text}");
    assert!(text.contains("1 message, 1 error(s)"), "{text}");
  }

  #[test]
  fn color_is_opt_in() {
    let plain = render(&model(), &DumpOptions::default());
    assert!(!plain.contains('\u{1b}'));
    let colored = render(
      &model(),
      &DumpOptions {
        color: true,
        ..DumpOptions::default()
      },
    );
    assert!(colored.contains('\u{1b}'));
  }

  #[test]
  fn json_form_carries_the_same_facts() {
    let json = to_json(&model());
    let context = &json["contexts"][0];
    assert_eq!(context["browserName"], "chromium");
    assert_eq!(context["actions"][1]["error"]["name"], "TimeoutError");
    assert_eq!(context["actions"][1]["durationMs"], 1200.0);
    assert_eq!(context["network"][1]["status"], 500);
  }

  #[test]
  fn one_line_summary_reads_as_a_listing_row() {
    assert_eq!(
      one_line_summary(&model()),
      "checkout — chromium · 2 actions · 1.7s · 1 failed"
    );
  }

  #[test]
  fn durations_scale_with_magnitude() {
    assert_eq!(format_ms(412.0), "412ms");
    assert_eq!(format_ms(1234.0), "1.2s");
    assert_eq!(format_ms(75_000.0), "1m15s");
  }
}
