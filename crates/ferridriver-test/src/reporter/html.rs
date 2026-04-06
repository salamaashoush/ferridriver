//! HTML reporter: generates a single self-contained HTML report file.
//!
//! Collects all test outcomes, serializes to JSON, embeds into an HTML template
//! with inline CSS/JS. No external dependencies, no build step.

use std::path::PathBuf;
use std::time::Duration;

use crate::model::{StepCategory, TestStep};
use crate::reporter::{Reporter, ReporterEvent};

/// Serializable test result for the HTML report.
#[derive(serde::Serialize)]
struct HtmlTestResult {
  file: String,
  suite: Option<String>,
  name: String,
  status: String,
  duration_ms: u64,
  attempt: u32,
  max_attempts: u32,
  error: Option<String>,
  diff: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  screenshot_base64: Option<String>,
  /// Step hierarchy (user-defined steps only).
  #[serde(skip_serializing_if = "Vec::is_empty")]
  steps: Vec<HtmlStep>,
}

#[derive(serde::Serialize)]
struct HtmlStep {
  title: String,
  status: String,
  duration_ms: u64,
  #[serde(skip_serializing_if = "Option::is_none")]
  error: Option<String>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  steps: Vec<HtmlStep>,
}

#[derive(serde::Serialize)]
struct HtmlReport {
  tests: Vec<HtmlTestResult>,
  total: usize,
  passed: usize,
  failed: usize,
  skipped: usize,
  flaky: usize,
  duration_ms: u64,
}

pub struct HtmlReporter {
  output_path: PathBuf,
  tests: Vec<HtmlTestResult>,
  total: usize,
  passed: usize,
  failed: usize,
  skipped: usize,
  flaky: usize,
  duration: Duration,
}

impl HtmlReporter {
  pub fn new(output_path: PathBuf) -> Self {
    Self {
      output_path,
      tests: Vec::new(),
      total: 0,
      passed: 0,
      failed: 0,
      skipped: 0,
      flaky: 0,
      duration: Duration::ZERO,
    }
  }
}

#[async_trait::async_trait]
impl Reporter for HtmlReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    match event {
      ReporterEvent::TestFinished { outcome, .. } => {
        let screenshot_base64 = outcome.attachments.iter().find_map(|a| {
          if a.content_type == "image/png" {
            if let crate::model::AttachmentBody::Bytes(ref data) = a.body {
              use base64::Engine;
              return Some(base64::engine::general_purpose::STANDARD.encode(data));
            }
          }
          None
        });

        self.tests.push(HtmlTestResult {
          file: outcome.test_id.file.clone(),
          suite: outcome.test_id.suite.clone(),
          name: outcome.test_id.name.clone(),
          status: outcome.status.to_string(),
          duration_ms: outcome.duration.as_millis() as u64,
          attempt: outcome.attempt,
          max_attempts: outcome.max_attempts,
          error: outcome.error.as_ref().map(|e| e.message.clone()),
          diff: outcome.error.as_ref().and_then(|e| e.diff.clone()),
          screenshot_base64,
          steps: serialize_html_steps(&outcome.steps),
        });
      }
      ReporterEvent::RunFinished {
        total,
        passed,
        failed,
        skipped,
        flaky,
        duration,
      } => {
        self.total = *total;
        self.passed = *passed;
        self.failed = *failed;
        self.skipped = *skipped;
        self.flaky = *flaky;
        self.duration = *duration;
      }
      _ => {}
    }
  }

  async fn finalize(&mut self) -> Result<(), String> {
    let report = HtmlReport {
      tests: std::mem::take(&mut self.tests),
      total: self.total,
      passed: self.passed,
      failed: self.failed,
      skipped: self.skipped,
      flaky: self.flaky,
      duration_ms: self.duration.as_millis() as u64,
    };

    let json = serde_json::to_string(&report).map_err(|e| format!("JSON serialize: {e}"))?;
    let html = HTML_TEMPLATE.replace("/*REPORT_DATA*/", &json);

    if let Some(parent) = self.output_path.parent() {
      std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
    }
    std::fs::write(&self.output_path, html)
      .map_err(|e| format!("write HTML report: {e}"))?;

    tracing::info!("HTML report: {}", self.output_path.display());
    Ok(())
  }
}

fn serialize_html_steps(steps: &[TestStep]) -> Vec<HtmlStep> {
  steps
    .iter()
    .filter(|s| s.category == StepCategory::TestStep)
    .map(|s| HtmlStep {
      title: s.title.clone(),
      status: format!("{:?}", s.status),
      duration_ms: s.duration.as_millis() as u64,
      error: s.error.clone(),
      steps: serialize_html_steps(&s.steps),
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
.header{background:#161b22;border-bottom:1px solid #30363d;padding:16px 24px;display:flex;align-items:center;gap:24px}
.header h1{font-size:18px;color:#58a6ff}
.stats{display:flex;gap:16px;font-size:14px}
.stats .pass{color:#3fb950}.stats .fail{color:#f85149}.stats .skip{color:#d29922}.stats .flaky{color:#db6d28}
.filters{display:flex;gap:8px;margin-left:auto}
.filters button{background:#21262d;border:1px solid #30363d;color:#c9d1d9;padding:4px 12px;border-radius:6px;cursor:pointer;font-size:12px}
.filters button.active{background:#388bfd;border-color:#388bfd;color:#fff}
.content{flex:1;padding:16px 24px;overflow-y:auto}
.test{border:1px solid #30363d;border-radius:8px;margin-bottom:8px;overflow:hidden}
.test-header{padding:10px 16px;display:flex;align-items:center;gap:12px;cursor:pointer;background:#161b22}
.test-header:hover{background:#1c2128}
.badge{padding:2px 8px;border-radius:12px;font-size:11px;font-weight:600}
.badge.passed{background:#238636;color:#fff}.badge.failed{background:#da3633;color:#fff}
.badge.skipped{background:#9e6a03;color:#fff}.badge.flaky{background:#db6d28;color:#fff}
.badge.timed{background:#da3633;color:#fff}
.test-name{flex:1;font-size:13px}.test-file{color:#8b949e;font-size:12px}.test-dur{color:#8b949e;font-size:12px}
.test-details{display:none;padding:12px 16px;background:#0d1117;border-top:1px solid #30363d}
.test.open .test-details{display:block}
.error{background:#1c0c0c;border:1px solid #f85149;border-radius:6px;padding:12px;margin-top:8px;font-family:monospace;font-size:12px;white-space:pre-wrap;color:#f85149}
.diff{background:#161b22;border:1px solid #30363d;border-radius:6px;padding:12px;margin-top:8px;font-family:monospace;font-size:12px;white-space:pre-wrap}
.diff .del{color:#f85149}.diff .ins{color:#3fb950}
.screenshot{margin-top:8px;max-width:100%;border-radius:6px;border:1px solid #30363d}
.empty{text-align:center;padding:48px;color:#8b949e}
</style>
</head>
<body>
<div class="header">
  <h1>ferridriver</h1>
  <div class="stats">
    <span class="pass" id="s-pass"></span>
    <span class="fail" id="s-fail"></span>
    <span class="skip" id="s-skip"></span>
    <span class="flaky" id="s-flaky"></span>
    <span id="s-dur" style="color:#8b949e"></span>
  </div>
  <div class="filters">
    <button class="active" onclick="filter('all')">All</button>
    <button onclick="filter('failed')">Failed</button>
    <button onclick="filter('flaky')">Flaky</button>
  </div>
</div>
<div class="content" id="content"></div>
<script>
const R=/*REPORT_DATA*/null;
const $=s=>document.getElementById(s);
$('s-pass').textContent=R.passed+' passed';
$('s-fail').textContent=R.failed+' failed';
$('s-skip').textContent=R.skipped+' skipped';
$('s-flaky').textContent=R.flaky+' flaky';
$('s-dur').textContent=(R.duration_ms/1000).toFixed(1)+'s';

function render(tests){
  const c=$('content');
  if(!tests.length){c.innerHTML='<div class="empty">No tests match filter</div>';return}
  c.innerHTML=tests.map((t,i)=>{
    const badge=t.status==='timed out'?'timed':t.status;
    const file=t.suite?t.file+' > '+t.suite:t.file;
    const dur=t.duration_ms<1000?t.duration_ms+'ms':(t.duration_ms/1000).toFixed(1)+'s';
    let details='';
    if(t.error)details+=`<div class="error">${esc(t.error)}</div>`;
    if(t.diff)details+=`<div class="diff">${diffHtml(t.diff)}</div>`;
    if(t.screenshot_base64)details+=`<img class="screenshot" src="data:image/png;base64,${t.screenshot_base64}">`;
    if(t.steps&&t.steps.length)details+=renderSteps(t.steps);
    return `<div class="test" id="t${i}">
      <div class="test-header" onclick="toggle(${i})">
        <span class="badge ${badge}">${t.status}</span>
        <span class="test-name">${esc(t.name)}</span>
        <span class="test-file">${esc(file)}</span>
        <span class="test-dur">${dur}</span>
      </div>
      <div class="test-details">${details||'<span style="color:#8b949e">No details</span>'}</div>
    </div>`;
  }).join('');
}

function toggle(i){document.getElementById('t'+i).classList.toggle('open')}
function filter(f){
  document.querySelectorAll('.filters button').forEach(b=>b.classList.remove('active'));
  event.target.classList.add('active');
  const tests=f==='all'?R.tests:R.tests.filter(t=>t.status===f);
  render(tests);
}
function renderSteps(steps,depth=0){
  const pad='  '.repeat(depth);
  return '<div class="steps" style="margin-top:8px;font-family:monospace;font-size:12px">'+
    steps.map(s=>{
      const ic=s.status==='Passed'?'<span style="color:#3fb950">v</span>':s.status==='Failed'?'<span style="color:#f85149">x</span>':'<span style="color:#d29922">-</span>';
      const d=s.duration_ms<1000?s.duration_ms+'ms':(s.duration_ms/1000).toFixed(1)+'s';
      let line=pad+ic+' '+esc(s.title)+' <span style="color:#8b949e">('+d+')</span>';
      if(s.error)line+='<div style="color:#f85149;margin-left:16px">'+esc(s.error)+'</div>';
      if(s.steps&&s.steps.length)line+=renderSteps(s.steps,depth+1);
      return '<div>'+line+'</div>';
    }).join('')+'</div>';
}
function esc(s){return s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;')}
function diffHtml(d){return d.split('\n').map(l=>{
  if(l.startsWith('-'))return'<span class="del">'+esc(l)+'</span>';
  if(l.startsWith('+'))return'<span class="ins">'+esc(l)+'</span>';
  return esc(l);
}).join('\n')}
render(R.tests);
</script>
</body>
</html>
"##;
