//! HTML reporter: one self-contained report file for a whole run.
//!
//! Collects every attempt of every test, serializes to JSON, and embeds it in
//! a template with inline CSS/JS. No external dependencies, no build step, and
//! nothing to serve — the file opens from disk.
//!
//! Artifacts travel with the report when they are small enough (images and
//! anything else under [`INLINE_LIMIT`] become `data:` URLs); larger ones —
//! videos, trace zips — are referenced by path, with the command that opens
//! them.

use std::path::PathBuf;

use crate::model::{Attachment, AttachmentBody, TestAnnotation, TestOutcome, TestStep};
use crate::reporter::base::{ResultCollector, TestRecord};
use crate::reporter::{Reporter, ReporterEvent};

/// Biggest artifact that still travels inside the report. Past this a
/// self-contained file stops being openable, so the report links instead.
const INLINE_LIMIT: u64 = 2 * 1024 * 1024;

#[derive(serde::Serialize)]
struct HtmlAttachment {
  name: String,
  content_type: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  path: Option<String>,
  /// `data:` URL for an artifact small enough to travel with the report.
  #[serde(skip_serializing_if = "Option::is_none")]
  data_url: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  size: Option<u64>,
}

#[derive(serde::Serialize)]
struct HtmlAnnotation {
  #[serde(rename = "type")]
  kind: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  description: Option<String>,
}

/// One run of a test. A retried test has several, oldest first.
#[derive(serde::Serialize)]
struct HtmlAttempt {
  attempt: u32,
  /// 0-based retry index, the number every other report calls `retry`.
  retry: u32,
  status: String,
  duration_ms: u64,
  #[serde(skip_serializing_if = "Option::is_none")]
  error: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  stack: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  diff: Option<String>,
  /// Every error of the attempt, soft assertions included. `error` is
  /// the first of them, kept for readers that only show one.
  #[serde(skip_serializing_if = "Vec::is_empty")]
  errors: Vec<HtmlError>,
  #[serde(skip_serializing_if = "str::is_empty")]
  stdout: String,
  #[serde(skip_serializing_if = "str::is_empty")]
  stderr: String,
  worker: u32,
  #[serde(skip_serializing_if = "Option::is_none")]
  started_at_ms: Option<i64>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  steps: Vec<HtmlStep>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  attachments: Vec<HtmlAttachment>,
}

#[derive(serde::Serialize)]
struct HtmlError {
  message: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  stack: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  diff: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  location: Option<String>,
}

#[derive(serde::Serialize)]
struct HtmlTest {
  #[serde(skip_serializing_if = "Option::is_none")]
  project: Option<String>,
  file: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  suite: Option<String>,
  name: String,
  /// Stable identity — the name a trace file carries on disk, and what
  /// a UI asks to re-run by.
  id: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  location: Option<String>,
  /// Outcome of the last attempt, or `flaky` when an earlier one failed.
  status: String,
  duration_ms: u64,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  tags: Vec<String>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  annotations: Vec<HtmlAnnotation>,
  attempts: Vec<HtmlAttempt>,
}

#[derive(serde::Serialize)]
struct HtmlStep {
  title: String,
  status: String,
  duration_ms: u64,
  #[serde(skip_serializing_if = "Option::is_none")]
  error: Option<String>,
  /// The step's own file — a `.feature` line for BDD, the `test.step`
  /// call site (or its `{ location }`) otherwise.
  #[serde(skip_serializing_if = "Option::is_none")]
  location: Option<String>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  steps: Vec<HtmlStep>,
}

#[derive(serde::Serialize)]
struct HtmlReport {
  tests: Vec<HtmlTest>,
  /// Every project that produced a test, in first-seen order — the report
  /// of a multi-project run has to say which one a result came from.
  projects: Vec<String>,
  /// Failures that belonged to no test (config, global setup, a worker
  /// that died). Without them a report can show zero failures for a run
  /// that never got off the ground.
  #[serde(skip_serializing_if = "Vec::is_empty")]
  errors: Vec<HtmlError>,
  total: usize,
  passed: usize,
  failed: usize,
  skipped: usize,
  flaky: usize,
  duration_ms: u64,
  started_at_ms: i64,
  generated_at_ms: u64,
  #[serde(skip_serializing_if = "serde_json::Value::is_null")]
  metadata: serde_json::Value,
}

/// When `finalize` opens the finished report in a browser. Mirrors
/// Playwright's `open` option on the HTML reporter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpenMode {
  Always,
  #[default]
  Never,
  OnFailure,
}

impl OpenMode {
  #[must_use]
  pub fn parse(value: &str) -> Self {
    match value {
      "always" => Self::Always,
      "on-failure" | "on_failure" => Self::OnFailure,
      _ => Self::Never,
    }
  }
}

pub struct HtmlReporter {
  output_path: PathBuf,
  collector: ResultCollector,
  open: OpenMode,
}

impl HtmlReporter {
  pub fn new(output_path: PathBuf) -> Self {
    Self {
      output_path,
      collector: ResultCollector::new(),
      open: OpenMode::Never,
    }
  }

  /// Whether the finished report opens in a browser. `never` by
  /// default: a CI job must not try to launch one.
  #[must_use]
  pub fn with_open_mode(mut self, open: OpenMode) -> Self {
    self.open = open;
    self
  }

  fn report(&self) -> HtmlReport {
    let counts = self.collector.counts();
    let tests: Vec<HtmlTest> = self.collector.records().iter().map(html_test).collect();
    HtmlReport {
      projects: self
        .collector
        .projects()
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect(),
      errors: self
        .collector
        .errors
        .iter()
        .map(|e| html_error(e, &crate::model::TestId::default()))
        .collect(),
      total: self.collector.run.total_tests.max(tests.len()),
      passed: counts.expected + counts.flaky,
      failed: counts.unexpected,
      skipped: counts.skipped,
      flaky: counts.flaky,
      tests,
      duration_ms: self.collector.run.duration.as_millis() as u64,
      started_at_ms: self
        .collector
        .run
        .start_time
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or_default(),
      generated_at_ms: std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64,
      metadata: self.collector.run.metadata.clone(),
    }
  }

  /// Hand the finished file to the platform's opener. Best effort: a
  /// headless CI box has no opener and must not fail the run for it.
  fn open_in_browser(&self) {
    let opener = if cfg!(target_os = "macos") {
      "open"
    } else if cfg!(target_os = "windows") {
      "explorer"
    } else {
      "xdg-open"
    };
    match std::process::Command::new(opener).arg(&self.output_path).spawn() {
      Ok(_) => {},
      Err(e) => tracing::warn!("could not open the HTML report: {e}"),
    }
  }
}

#[async_trait::async_trait]
impl Reporter for HtmlReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    self.collector.observe(event);
  }

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    let report = self.report();
    let failed = report.failed > 0 || !report.errors.is_empty();

    let json = serde_json::to_string(&report)?;
    // The payload rides in a `application/json` block, so the only thing
    // that could end it early is a literal `</script>` inside a test name
    // or a captured console line.
    let json = json.replace("</", "<\\/");
    let html = HTML_TEMPLATE.replace("/*REPORT_DATA*/", &json);

    if let Some(parent) = self.output_path.parent() {
      std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&self.output_path, html)?;

    tracing::info!("HTML report: {}", self.output_path.display());
    if self.open == OpenMode::Always || (self.open == OpenMode::OnFailure && failed) {
      self.open_in_browser();
    }
    Ok(())
  }
}

fn html_test(record: &TestRecord) -> HtmlTest {
  let last = record.last();
  let id = record.id();
  HtmlTest {
    project: (!record.key.project.is_empty()).then(|| record.key.project.clone()),
    file: id.file.clone(),
    suite: id.suite.clone(),
    name: id.name.clone(),
    id: record.stable_id(),
    location: id.line.map(|line| format!("{}:{line}", id.file)),
    status: match record.outcome_kind() {
      crate::model::TestOutcomeKind::Expected => last.status.as_str().to_string(),
      crate::model::TestOutcomeKind::Flaky => "flaky".to_string(),
      crate::model::TestOutcomeKind::Skipped => "skipped".to_string(),
      crate::model::TestOutcomeKind::Unexpected => last.status.as_str().to_string(),
    },
    duration_ms: record.total_duration().as_millis() as u64,
    tags: tags_of(&last.annotations),
    annotations: annotations_of(&last.annotations),
    attempts: record.attempts.iter().map(|a| attempt_of(a)).collect(),
  }
}

fn html_error(failure: &crate::model::TestFailure, test_id: &crate::model::TestId) -> HtmlError {
  let location = failure
    .stack
    .as_deref()
    .and_then(crate::reporter::base::parse_error_location);
  HtmlError {
    message: failure.message.clone(),
    stack: failure.stack.clone(),
    diff: failure.diff.clone(),
    location: location
      .map(|l| format!("{}:{}:{}", l.file, l.line, l.column))
      .or_else(|| test_id.line.map(|line| format!("{}:{line}", test_id.file))),
  }
}

fn attempt_of(outcome: &TestOutcome) -> HtmlAttempt {
  HtmlAttempt {
    attempt: outcome.attempt,
    retry: outcome.attempt.saturating_sub(1),
    status: outcome.status.as_str().to_string(),
    duration_ms: outcome.duration.as_millis() as u64,
    error: outcome.error.as_ref().map(|failure| failure.message.clone()),
    stack: outcome.error.as_ref().and_then(|failure| failure.stack.clone()),
    diff: outcome.error.as_ref().and_then(|failure| failure.diff.clone()),
    errors: crate::reporter::base::attempt_errors(outcome)
      .into_iter()
      .map(|e| html_error(e, &outcome.test_id))
      .collect(),
    stdout: outcome.stdout.clone(),
    stderr: outcome.stderr.clone(),
    worker: outcome.worker_index,
    started_at_ms: {
      let ms = outcome.start_epoch_ms();
      (ms > 0).then_some(ms)
    },
    steps: serialize_html_steps(&outcome.steps),
    attachments: attachments_of(outcome),
  }
}

fn tags_of(annotations: &[TestAnnotation]) -> Vec<String> {
  annotations
    .iter()
    .filter_map(|annotation| match annotation {
      TestAnnotation::Tag(tag) => Some(if tag.starts_with('@') {
        tag.clone()
      } else {
        format!("@{tag}")
      }),
      _ => None,
    })
    .collect()
}

fn annotations_of(annotations: &[TestAnnotation]) -> Vec<HtmlAnnotation> {
  annotations
    .iter()
    .filter_map(|annotation| {
      let (kind, description) = match annotation {
        TestAnnotation::Skip { reason, .. } => ("skip", reason.clone()),
        TestAnnotation::Fixme { reason, .. } => ("fixme", reason.clone()),
        TestAnnotation::Fail { reason, .. } => ("fail", reason.clone()),
        TestAnnotation::Slow { reason, .. } => ("slow", reason.clone()),
        TestAnnotation::Info { type_name, description } => {
          return Some(HtmlAnnotation {
            kind: type_name.clone(),
            description: Some(description.clone()),
          });
        },
        TestAnnotation::Tag(_) | TestAnnotation::Only => return None,
      };
      Some(HtmlAnnotation {
        kind: kind.to_string(),
        description,
      })
    })
    .collect()
}

/// The attempt's artifacts, plus the failure screenshot the runner captures
/// outside the attachment list.
fn attachments_of(outcome: &TestOutcome) -> Vec<HtmlAttachment> {
  let mut out: Vec<HtmlAttachment> = outcome.attachments.iter().map(html_attachment).collect();
  // The runner attaches its own failure screenshot; only synthesize one
  // when it did not, or the report shows the same picture twice.
  if let Some(screenshot) = outcome.error.as_ref().and_then(|failure| failure.screenshot.as_ref())
    && !out
      .iter()
      .any(|attachment| attachment.content_type.starts_with("image/"))
  {
    out.push(HtmlAttachment {
      name: "screenshot".to_string(),
      content_type: "image/png".to_string(),
      path: None,
      data_url: data_url("image/png", screenshot),
      size: Some(screenshot.len() as u64),
    });
  }
  out
}

fn html_attachment(attachment: &Attachment) -> HtmlAttachment {
  match &attachment.body {
    AttachmentBody::Bytes(bytes) => HtmlAttachment {
      name: attachment.name.clone(),
      content_type: attachment.content_type.clone(),
      path: None,
      data_url: data_url(&attachment.content_type, bytes),
      size: Some(bytes.len() as u64),
    },
    AttachmentBody::Path(path) => {
      let size = std::fs::metadata(path).ok().map(|meta| meta.len());
      // Only images are worth carrying: a video or a trace zip would
      // bloat the file past what a browser opens comfortably.
      let data_url = (attachment.content_type.starts_with("image/") && size.is_some_and(|size| size <= INLINE_LIMIT))
        .then(|| {
          std::fs::read(path)
            .ok()
            .and_then(|bytes| data_url(&attachment.content_type, &bytes))
        })
        .flatten();
      HtmlAttachment {
        name: attachment.name.clone(),
        content_type: attachment.content_type.clone(),
        path: Some(path.display().to_string()),
        data_url,
        size,
      }
    },
  }
}

fn data_url(content_type: &str, bytes: &[u8]) -> Option<String> {
  if bytes.len() as u64 > INLINE_LIMIT {
    return None;
  }
  use base64::Engine as _;
  let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
  Some(format!("data:{content_type};base64,{encoded}"))
}

fn serialize_html_steps(steps: &[TestStep]) -> Vec<HtmlStep> {
  steps
    .iter()
    .filter(|step| step.category.is_visible())
    .map(|step| HtmlStep {
      title: step.title.clone(),
      status: format!("{:?}", step.status).to_ascii_lowercase(),
      duration_ms: step.duration.as_millis() as u64,
      error: step.error.clone(),
      location: step.location.as_ref().map(ToString::to_string),
      steps: serialize_html_steps(&step.steps),
    })
    .collect()
}

const HTML_TEMPLATE: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>ferridriver test report</title>
<style>
*{margin:0;padding:0;box-sizing:border-box}
body{font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;background:#0d1117;color:#c9d1d9;display:flex;flex-direction:column;min-height:100vh}
a{color:#58a6ff}
.header{background:#161b22;border-bottom:1px solid #30363d;padding:14px 24px;display:flex;align-items:center;gap:20px;flex-wrap:wrap}
.header h1{font-size:17px;color:#58a6ff}
.stats{display:flex;gap:14px;font-size:13px;align-items:center}
.stats button{background:none;border:none;color:inherit;cursor:pointer;font-size:13px;padding:2px 6px;border-radius:6px}
.stats button:hover{background:#21262d}
.stats button.active{background:#21262d;outline:1px solid #30363d}
.pass{color:#3fb950}.fail{color:#f85149}.skip{color:#d29922}.flaky{color:#db6d28}.muted{color:#8b949e}
.toolbar{margin-left:auto;display:flex;gap:8px;align-items:center}
.toolbar input,.toolbar select{background:#0d1117;border:1px solid #30363d;color:#c9d1d9;border-radius:6px;padding:5px 10px;font-size:12px}
.toolbar input{width:220px}
.content{flex:1;padding:16px 24px}
.file{margin-bottom:14px}
.file-name{font-size:12px;color:#8b949e;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;margin-bottom:6px}
.test{border:1px solid #30363d;border-radius:8px;margin-bottom:6px;overflow:hidden}
.test-header{padding:9px 14px;display:flex;align-items:center;gap:10px;cursor:pointer;background:#161b22}
.test-header:hover{background:#1c2128}
.badge{padding:2px 8px;border-radius:12px;font-size:11px;font-weight:600;white-space:nowrap}
.badge.passed{background:#238636;color:#fff}.badge.failed{background:#da3633;color:#fff}
.badge.skipped{background:#9e6a03;color:#fff}.badge.flaky{background:#db6d28;color:#fff}
.badge.timedout,.badge.interrupted{background:#da3633;color:#fff}
.chip{padding:1px 7px;border-radius:10px;font-size:11px;background:#21262d;border:1px solid #30363d;color:#8b949e;white-space:nowrap}
.test-name{flex:1;font-size:13px}
.test-suite{color:#8b949e;font-size:12px}
.test-dur{color:#8b949e;font-size:12px;font-variant-numeric:tabular-nums}
.test-details{display:none;padding:12px 14px;background:#0d1117;border-top:1px solid #30363d}
.test.open .test-details{display:block}
.attempt{border-top:1px dashed #30363d;margin-top:10px;padding-top:10px}
.attempt:first-child{border-top:none;margin-top:0;padding-top:0}
.attempt-title{font-size:12px;color:#8b949e;margin-bottom:6px}
.error{background:#1c0c0c;border:1px solid #f85149;border-radius:6px;padding:10px;margin-top:6px;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:12px;white-space:pre-wrap;color:#f85149}
.stack{color:#8b949e;font-size:11px;white-space:pre-wrap;margin-top:6px;font-family:ui-monospace,SFMono-Regular,Menlo,monospace}
.diff,.io{background:#161b22;border:1px solid #30363d;border-radius:6px;padding:10px;margin-top:6px;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:12px;white-space:pre-wrap;max-height:340px;overflow:auto}
.diff .del{color:#f85149}.diff .ins{color:#3fb950}
.label{font-size:11px;text-transform:uppercase;letter-spacing:.04em;color:#8b949e;margin-top:10px}
.steps{margin-top:6px;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:12px}
.steps .step{padding-left:14px;border-left:1px solid #21262d}
.attachments{display:flex;flex-direction:column;gap:8px;margin-top:6px}
.attachment{border:1px solid #30363d;border-radius:6px;padding:8px 10px;font-size:12px}
.attachment img,.attachment video{max-width:100%;max-height:320px;width:auto;border-radius:6px;margin-top:8px;display:block}
.attachment a.zoom{display:inline-block}
.attachment code{background:#161b22;border:1px solid #30363d;border-radius:4px;padding:2px 6px;font-size:11px}
.empty{text-align:center;padding:48px;color:#8b949e}
</style>
</head>
<body>
<div class="header">
  <h1>ferridriver</h1>
  <div class="stats" id="stats"></div>
  <div class="toolbar">
    <input id="search" type="search" placeholder="Filter by name or file" autocomplete="off">
    <select id="project"></select>
    <select id="sort">
      <option value="order">Run order</option>
      <option value="slowest">Slowest first</option>
      <option value="name">Name</option>
    </select>
  </div>
</div>
<div class="content" id="content"></div>
<script id="report-data" type="application/json">/*REPORT_DATA*/</script>
<script>
const R = JSON.parse(document.getElementById('report-data').textContent);
const byId = id => document.getElementById(id);
const esc = s => String(s == null ? '' : s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
const dur = ms => ms < 1000 ? ms + 'ms' : (ms / 1000).toFixed(1) + 's';
const size = n => n == null ? '' : n < 1024 ? n + ' B' : n < 1048576 ? (n / 1024).toFixed(0) + ' KB' : (n / 1048576).toFixed(1) + ' MB';
const state = { status: 'all', project: 'all', search: '', sort: 'order' };

function statLabel(key, count, cls) {
  const active = state.status === key ? ' active' : '';
  return `<button class="${cls}${active}" data-status="${key}">${count} ${key === 'all' ? 'total' : key}</button>`;
}

function renderStats() {
  const when = R.generated_at_ms ? new Date(R.generated_at_ms).toLocaleString() : '';
  byId('stats').innerHTML =
    statLabel('all', R.total, 'muted') +
    statLabel('passed', R.passed, 'pass') +
    statLabel('failed', R.failed, 'fail') +
    statLabel('flaky', R.flaky, 'flaky') +
    statLabel('skipped', R.skipped, 'skip') +
    `<span class="muted">${dur(R.duration_ms)}</span>` +
    (when ? `<span class="muted">${esc(when)}</span>` : '');
  for (const button of byId('stats').querySelectorAll('button')) {
    button.onclick = () => { state.status = button.dataset.status; renderStats(); render(); };
  }
}

function renderProjects() {
  const select = byId('project');
  if (!R.projects.length) { select.style.display = 'none'; return; }
  select.innerHTML = ['all', ...R.projects].map(p => `<option value="${esc(p)}">${p === 'all' ? 'All projects' : esc(p)}</option>`).join('');
  select.onchange = () => { state.project = select.value; render(); };
}

function visible() {
  const needle = state.search.toLowerCase();
  let tests = R.tests.filter(t => {
    if (state.status !== 'all' && t.status !== state.status) return false;
    if (state.project !== 'all' && (t.project || '') !== state.project) return false;
    if (!needle) return true;
    return (t.name + ' ' + t.file + ' ' + (t.suite || '')).toLowerCase().includes(needle);
  });
  if (state.sort === 'slowest') tests = tests.slice().sort((a, b) => b.duration_ms - a.duration_ms);
  if (state.sort === 'name') tests = tests.slice().sort((a, b) => a.name.localeCompare(b.name));
  return tests;
}

// Failures that belonged to no test — a config error, a dead worker, a
// global setup that threw. A run that never started has zero failing
// tests and everything to explain.
function runErrorsHtml() {
  if (!R.errors || !R.errors.length) return '';
  return '<div class="file"><div class="file-name">Errors outside any test</div>' +
    R.errors.map(err => {
      const where = err.location ? `<div class="muted">${esc(err.location)}</div>` : '';
      const stack = err.stack ? `<div class="stack">${esc(err.stack)}</div>` : '';
      return `<div class="test"><div class="test-details"><div class="error">${esc(err.message)}${where}${stack}</div></div></div>`;
    }).join('') + '</div>';
}

function render() {
  const tests = visible();
  const content = byId('content');
  const errors = runErrorsHtml();
  if (!tests.length) {
    content.innerHTML = errors || '<div class="empty">No tests match</div>';
    return;
  }
  let html = errors;
  if (state.sort === 'order') {
    const files = [];
    for (const test of tests) {
      let group = files.find(f => f.file === test.file);
      if (!group) { group = { file: test.file, tests: [] }; files.push(group); }
      group.tests.push(test);
    }
    for (const group of files) {
      html += `<div class="file"><div class="file-name">${esc(group.file)}</div>` +
        group.tests.map(renderTest).join('') + '</div>';
    }
  } else {
    html = tests.map(renderTest).join('');
  }
  content.innerHTML = html;
  for (const header of content.querySelectorAll('.test-header')) {
    header.onclick = () => header.parentElement.classList.toggle('open');
  }
}

function renderTest(test) {
  const badge = test.status.replace(/\s+/g, '');
  const chips = [];
  if (test.project) chips.push(`<span class="chip">${esc(test.project)}</span>`);
  for (const tag of test.tags || []) chips.push(`<span class="chip">${esc(tag)}</span>`);
  if (test.attempts.length > 1) chips.push(`<span class="chip">${test.attempts.length} attempts</span>`);
  for (const a of test.annotations || []) {
    chips.push(`<span class="chip">${esc(a.type)}${a.description ? ': ' + esc(a.description) : ''}</span>`);
  }
  const suite = test.suite && test.suite !== test.file ? `<span class="test-suite">${esc(test.suite)}</span>` : '';
  const details = test.attempts.map(a => renderAttempt(a, test.attempts.length > 1)).join('');
  return `<div class="test">
    <div class="test-header">
      <span class="badge ${esc(badge)}">${esc(test.status)}</span>
      <span class="test-name">${esc(test.name)}</span>
      ${suite}
      ${chips.join('')}
      <span class="test-dur">${dur(test.duration_ms)}</span>
    </div>
    <div class="test-details">${details || '<span class="muted">No details</span>'}</div>
  </div>`;
}

function renderAttempt(attempt, showTitle) {
  let html = '<div class="attempt">';
  if (showTitle) {
    const when = attempt.started_at_ms ? ' · ' + new Date(attempt.started_at_ms).toLocaleTimeString() : '';
    html += `<div class="attempt-title">Attempt ${attempt.attempt} — ${esc(attempt.status)} (${dur(attempt.duration_ms)}) · worker ${attempt.worker ?? 0}${when}</div>`;
  }
  // Every error of the attempt: a soft-assertion run fails on several at
  // once, and showing only the first hides the rest.
  const errors = attempt.errors && attempt.errors.length
    ? attempt.errors
    : (attempt.error ? [{ message: attempt.error, stack: attempt.stack, diff: attempt.diff }] : []);
  for (const err of errors) {
    const where = err.location ? `<div class="muted">${esc(err.location)}</div>` : '';
    html += `<div class="error">${esc(err.message)}${where}${err.stack ? `<div class="stack">${esc(err.stack)}</div>` : ''}</div>`;
    if (err.diff) html += `<div class="diff">${diffHtml(err.diff)}</div>`;
  }
  if (attempt.steps && attempt.steps.length) html += `<div class="label">Steps</div>${renderSteps(attempt.steps)}`;
  if (attempt.attachments && attempt.attachments.length) {
    html += `<div class="label">Attachments</div><div class="attachments">${attempt.attachments.map(renderAttachment).join('')}</div>`;
  }
  if (attempt.stdout) html += `<div class="label">stdout</div><div class="io">${esc(attempt.stdout)}</div>`;
  if (attempt.stderr) html += `<div class="label">stderr</div><div class="io">${esc(attempt.stderr)}</div>`;
  return html + '</div>';
}

function renderAttachment(attachment) {
  const meta = [attachment.content_type, size(attachment.size)].filter(Boolean).join(' · ');
  let body = `<div><strong>${esc(attachment.name)}</strong> <span class="muted">${esc(meta)}</span></div>`;
  if (attachment.data_url && attachment.content_type.startsWith('image/')) {
    // Thumbnail in the row, full size in a tab — a failing run attaches a
    // screenshot per attempt and full-width images bury everything else.
    body += `<a class="zoom" href="${attachment.data_url}" target="_blank" rel="noreferrer"><img src="${attachment.data_url}" alt="${esc(attachment.name)}"></a>`;
  } else if (attachment.data_url && attachment.content_type.startsWith('video/')) {
    body += `<video controls src="${attachment.data_url}"></video>`;
  } else if (attachment.path) {
    body += `<div class="muted">${esc(attachment.path)}</div>`;
    if (attachment.name === 'trace' || attachment.content_type === 'application/zip') {
      body += `<div style="margin-top:6px"><code>ferridriver trace view ${esc(attachment.path)}</code></div>`;
    } else {
      body += `<div style="margin-top:6px"><a href="file://${esc(attachment.path)}">open</a></div>`;
    }
  }
  return `<div class="attachment">${body}</div>`;
}

function renderSteps(steps) {
  return '<div class="steps">' + steps.map(step => {
    const mark = step.status === 'passed' ? '<span class="pass">v</span>'
      : step.status === 'failed' ? '<span class="fail">x</span>'
      : '<span class="skip">-</span>';
    const where = step.location ? ` <span class="muted">${esc(step.location)}</span>` : '';
    let line = `<div>${mark} ${esc(step.title)} <span class="muted">(${dur(step.duration_ms)})</span>${where}</div>`;
    if (step.error) line += `<div class="fail" style="margin-left:16px">${esc(step.error)}</div>`;
    if (step.steps && step.steps.length) line += `<div class="step">${renderSteps(step.steps)}</div>`;
    return line;
  }).join('') + '</div>';
}

function diffHtml(diff) {
  return diff.split('\n').map(line => {
    if (line.startsWith('-')) return `<span class="del">${esc(line)}</span>`;
    if (line.startsWith('+')) return `<span class="ins">${esc(line)}</span>`;
    return esc(line);
  }).join('\n');
}

byId('search').oninput = event => { state.search = event.target.value; render(); };
byId('sort').onchange = event => { state.sort = event.target.value; render(); };
renderStats();
renderProjects();
render();
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::time::Duration;

  use super::*;
  use crate::model::{TestFailure, TestId, TestStatus};
  use crate::reporter::ReporterEvent;

  fn outcome(name: &str, status: TestStatus, attempt: u32) -> TestOutcome {
    TestOutcome {
      test_id: TestId {
        file: "specs/a.spec.ts".into(),
        suite: None,
        name: name.into(),
        line: Some(1),
        column: None,
      },
      status,
      duration: Duration::from_millis(120),
      attempt,
      max_attempts: 2,
      project_name: "webkit".into(),
      ..Default::default()
    }
  }

  async fn record(reporter: &mut HtmlReporter, outcome: TestOutcome) {
    reporter
      .on_event(&ReporterEvent::TestFinished {
        outcome: Arc::new(outcome),
      })
      .await;
  }

  #[tokio::test]
  async fn a_retried_test_keeps_every_attempt_and_reads_as_flaky() {
    let mut reporter = HtmlReporter::new(PathBuf::from("report.html"));
    let mut first = outcome("logs in", TestStatus::Failed, 1);
    first.error = Some(TestFailure {
      message: "boom".into(),
      stack: None,
      diff: None,
      screenshot: None,
    });
    record(&mut reporter, first).await;
    record(&mut reporter, outcome("logs in", TestStatus::Passed, 2)).await;

    let report = reporter.report();
    assert_eq!(report.tests.len(), 1, "attempts fold into one test");
    let test = &report.tests[0];
    assert_eq!(test.attempts.len(), 2);
    assert_eq!(test.status, "flaky", "failed then passed");
    assert_eq!(test.duration_ms, 240, "the test's time is all of its attempts");
    assert_eq!(test.project.as_deref(), Some("webkit"));
    assert_eq!(report.flaky, 1);
  }

  #[tokio::test]
  async fn tests_of_different_projects_stay_apart() {
    let mut reporter = HtmlReporter::new(PathBuf::from("report.html"));
    record(&mut reporter, outcome("logs in", TestStatus::Passed, 1)).await;
    let mut other = outcome("logs in", TestStatus::Passed, 1);
    other.project_name = "cdp-pipe".into();
    record(&mut reporter, other).await;
    let report = reporter.report();
    assert_eq!(report.tests.len(), 2, "same test, two projects, two rows");
    assert_eq!(report.projects, vec!["webkit", "cdp-pipe"]);
  }

  #[test]
  fn a_failure_screenshot_travels_with_the_report() {
    let mut failed = outcome("logs in", TestStatus::Failed, 1);
    failed.error = Some(TestFailure {
      message: "boom".into(),
      stack: None,
      diff: None,
      screenshot: Some(vec![1, 2, 3]),
    });
    let attachments = attachments_of(&failed);
    assert_eq!(attachments.len(), 1);
    assert!(
      attachments[0]
        .data_url
        .as_deref()
        .is_some_and(|url| url.starts_with("data:image/png;base64,")),
      "{:?}",
      attachments[0].data_url
    );
  }

  #[test]
  fn a_big_artifact_is_linked_rather_than_embedded() {
    let big = vec![0u8; (INLINE_LIMIT + 1) as usize];
    assert!(data_url("video/webm", &big).is_none(), "too big to travel inline");
    assert!(data_url("image/png", &[1, 2, 3]).is_some());
  }

  #[tokio::test]
  async fn a_run_error_reaches_the_report() {
    let mut reporter = HtmlReporter::new(PathBuf::from("report.html"));
    reporter
      .on_event(&ReporterEvent::RunError {
        error: Box::new(TestFailure {
          message: "global setup failed".into(),
          stack: Some("at setup.ts:3:1".into()),
          diff: None,
          screenshot: None,
        }),
      })
      .await;
    let report = reporter.report();
    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.errors[0].location.as_deref(), Some("setup.ts:3:1"));
  }
}
