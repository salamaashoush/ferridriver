//! Reading a Playwright trace back into a model.
//!
//! The viewer does this in TypeScript (`isomorphic/trace/traceModel.ts`);
//! this is the same shape in Rust, so a trace can be inspected without a
//! browser — `ferridriver trace show` renders from it, and tests assert
//! against it instead of grepping JSON lines.
//!
//! A trace is a set of named entries, either zipped (`trace.zip`) or loose
//! on disk (a recording in progress). Inside them, `<prefix>.trace` and
//! `<prefix>.network` are JSONL event streams; one `<prefix>` is one browser
//! context, and an archive can hold several (the test runner merges its own
//! step stream in beside the browser's).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where a trace's entries are read from.
pub enum TraceSource {
  /// A `trace.zip` archive.
  Zip(PathBuf),
  /// A directory of loose trace files — what a live recording writes.
  Dir(PathBuf),
}

impl TraceSource {
  /// Classify `path`: a directory is read loose, anything else as an archive.
  ///
  /// # Errors
  ///
  /// Errors when `path` does not exist.
  pub fn open(path: &Path) -> Result<Self, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.is_dir() {
      Ok(Self::Dir(path.to_path_buf()))
    } else {
      Ok(Self::Zip(path.to_path_buf()))
    }
  }

  /// Names of every entry, in the form the trace stream references them
  /// (`trace.trace`, `resources/<sha1>`, …).
  ///
  /// # Errors
  ///
  /// Errors when the archive or directory cannot be read.
  pub fn entry_names(&self) -> Result<Vec<String>, String> {
    match self {
      Self::Zip(path) => {
        let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let zip = zip::ZipArchive::new(file).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(zip.file_names().map(ToString::to_string).collect())
      },
      Self::Dir(dir) => {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(dir)
          .map_err(|e| format!("{}: {e}", dir.display()))?
          .flatten()
        {
          let name = entry.file_name().to_string_lossy().into_owned();
          if entry.path().is_dir() {
            if let Ok(nested) = std::fs::read_dir(entry.path()) {
              names.extend(
                nested
                  .flatten()
                  .map(|e| format!("{name}/{}", e.file_name().to_string_lossy())),
              );
            }
          } else {
            names.push(name);
          }
        }
        Ok(names)
      },
    }
  }

  /// Read one entry, `None` when the trace has no such entry.
  ///
  /// # Errors
  ///
  /// Errors when the entry exists but cannot be read.
  pub fn read(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
    match self {
      Self::Zip(path) => {
        let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("{}: {e}", path.display()))?;
        let Ok(mut entry) = zip.by_name(name) else {
          return Ok(None);
        };
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes).map_err(|e| format!("{name}: {e}"))?;
        Ok(Some(bytes))
      },
      Self::Dir(dir) => match std::fs::read(dir.join(name)) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{name}: {e}")),
      },
    }
  }

  fn read_text(&self, name: &str) -> Result<Option<String>, String> {
    Ok(
      self
        .read(name)?
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
    )
  }
}

/// One call recorded in the trace: a browser action, an expect, or a test
/// step, assembled from its `before` / `input` / `after` lines.
#[derive(Debug, Clone, Default)]
pub struct Action {
  pub call_id: String,
  pub parent_id: Option<String>,
  pub step_id: Option<String>,
  pub class: String,
  pub method: String,
  pub title: String,
  pub params: serde_json::Value,
  pub page_id: Option<String>,
  pub start_time: f64,
  /// `None` while the action is still running (a live trace, or a crash).
  pub end_time: Option<f64>,
  pub error: Option<ActionError>,
  pub attachments: Vec<Attachment>,
  pub logs: Vec<String>,
  pub stack: Vec<StackFrame>,
  pub point: Option<(f64, f64)>,
}

impl Action {
  /// Wall duration in milliseconds, `None` while still running.
  #[must_use]
  pub fn duration_ms(&self) -> Option<f64> {
    self.end_time.map(|end| (end - self.start_time).max(0.0))
  }

  /// What the viewer shows as the action's name.
  #[must_use]
  pub fn display_title(&self) -> &str {
    if self.title.is_empty() {
      &self.method
    } else {
      &self.title
    }
  }
}

#[derive(Debug, Clone)]
pub struct ActionError {
  pub name: String,
  pub message: String,
}

#[derive(Debug, Clone)]
pub struct Attachment {
  pub name: String,
  pub content_type: String,
  /// Entry name inside the trace, when the body travels with it.
  pub sha1: Option<String>,
  /// On-disk path, for attachments left outside the trace.
  pub path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StackFrame {
  pub file: String,
  pub line: u32,
  pub column: u32,
}

impl std::fmt::Display for StackFrame {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}:{}", self.file, self.line)
  }
}

/// A console message recorded from the page.
#[derive(Debug, Clone)]
pub struct ConsoleMessage {
  pub time: f64,
  pub message_type: String,
  pub text: String,
  pub url: String,
  pub line_number: u32,
}

/// A page lifecycle event (`page`, `dialog`, `download`, `pageError`, …).
#[derive(Debug, Clone)]
pub struct PageEvent {
  pub time: f64,
  pub method: String,
  pub params: serde_json::Value,
}

/// A test-level failure written into the trace by the runner.
#[derive(Debug, Clone)]
pub struct TraceError {
  pub message: String,
  pub stack: Vec<StackFrame>,
}

/// One request from the `.network` stream.
#[derive(Debug, Clone)]
pub struct NetworkEntry {
  pub method: String,
  pub url: String,
  pub status: u32,
  pub mime_type: String,
  pub duration_ms: f64,
}

impl NetworkEntry {
  /// Whether the request failed outright or came back 4xx/5xx.
  #[must_use]
  pub fn failed(&self) -> bool {
    self.status == 0 || self.status >= 400
  }
}

/// Everything one browser context contributed to the trace.
#[derive(Debug, Clone, Default)]
pub struct ContextEntry {
  /// The `<prefix>` its streams were named after.
  pub prefix: String,
  pub title: Option<String>,
  pub origin: String,
  pub browser_name: String,
  pub platform: String,
  pub recorder_version: Option<String>,
  pub sdk_language: Option<String>,
  pub wall_time: f64,
  pub monotonic_time: f64,
  pub options: serde_json::Value,
  pub actions: Vec<Action>,
  pub console: Vec<ConsoleMessage>,
  pub events: Vec<PageEvent>,
  pub errors: Vec<TraceError>,
  pub stdout: Vec<String>,
  pub stderr: Vec<String>,
  pub network: Vec<NetworkEntry>,
  pub screencast_frames: usize,
  /// Pages seen in the trace, in first-seen order.
  pub pages: Vec<String>,
}

/// A whole trace: one entry per recorded context.
#[derive(Debug, Clone, Default)]
pub struct TraceModel {
  pub contexts: Vec<ContextEntry>,
  /// Whether the trace carries the sources its stacks point at.
  pub has_sources: bool,
}

impl TraceModel {
  /// Parse every context in `source`.
  ///
  /// # Errors
  ///
  /// Errors when the trace holds no `.trace` stream, or an entry cannot be
  /// read.
  pub fn load(source: &TraceSource) -> Result<Self, String> {
    let names = source.entry_names()?;
    let mut prefixes: Vec<String> = names
      .iter()
      .filter_map(|name| name.strip_suffix(".trace").map(ToString::to_string))
      .collect();
    prefixes.sort();
    if prefixes.is_empty() {
      return Err("not a Playwright trace: no .trace stream inside".to_string());
    }
    let has_sources = names
      .iter()
      .any(|name| name.starts_with("src/") || name.contains("src@"));

    let mut contexts = Vec::with_capacity(prefixes.len());
    for prefix in prefixes {
      let trace = source.read_text(&format!("{prefix}.trace"))?.unwrap_or_default();
      let network = source.read_text(&format!("{prefix}.network"))?.unwrap_or_default();
      let mut context = parse_context(&prefix, &trace);
      context.network = parse_network(&network);
      contexts.push(context);
    }
    contexts.sort_by(|a, b| a.wall_time.total_cmp(&b.wall_time));
    Ok(Self { contexts, has_sources })
  }

  /// Actions of every context, ordered by start time — what a reader wants
  /// when the split into contexts does not matter.
  #[must_use]
  pub fn all_actions(&self) -> Vec<&Action> {
    let mut actions: Vec<&Action> = self.contexts.iter().flat_map(|c| c.actions.iter()).collect();
    actions.sort_by(|a, b| a.start_time.total_cmp(&b.start_time));
    actions
  }

  /// Every failure in the trace: failed actions first, then errors the
  /// runner recorded on their own.
  #[must_use]
  pub fn failures(&self) -> Vec<String> {
    let mut failures: Vec<String> = self
      .all_actions()
      .into_iter()
      .filter_map(|action| {
        action
          .error
          .as_ref()
          .map(|e| format!("{}: {}", action.display_title(), e.message))
      })
      .collect();
    failures.extend(
      self
        .contexts
        .iter()
        .flat_map(|c| c.errors.iter().map(|e| e.message.clone())),
    );
    failures
  }
}

/// Assemble one context from its JSONL stream.
fn parse_context(prefix: &str, trace: &str) -> ContextEntry {
  let mut context = ContextEntry {
    prefix: prefix.to_string(),
    ..ContextEntry::default()
  };
  // Actions arrive as separate before / input / after lines that have to be
  // joined by callId; insertion order is kept so a live trace (missing its
  // `after` lines) still reads in call order.
  let mut actions: BTreeMap<String, usize> = BTreeMap::new();
  let mut ordered: Vec<Action> = Vec::new();

  for line in trace.lines().filter(|line| !line.trim().is_empty()) {
    let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
      continue;
    };
    match event
      .get("type")
      .and_then(serde_json::Value::as_str)
      .unwrap_or_default()
    {
      "context-options" => apply_context_options(&mut context, &event),
      "before" => {
        let action = parse_before(&event);
        note_page(&mut context, action.page_id.clone());
        actions.insert(action.call_id.clone(), ordered.len());
        ordered.push(action);
      },
      "input" => {
        if let Some(action) = action_mut(&mut actions, &mut ordered, &event) {
          action.point = parse_point(&event);
        }
      },
      "after" => {
        if let Some(action) = action_mut(&mut actions, &mut ordered, &event) {
          apply_after(action, &event);
        }
      },
      "log" => {
        if let Some(action) = action_mut(&mut actions, &mut ordered, &event) {
          action.logs.push(string(&event, "message"));
        }
      },
      "console" => context.console.push(parse_console(&event)),
      "event" => {
        let method = string(&event, "method");
        if method == "page" {
          note_page(&mut context, opt_string(&event, "pageId"));
        }
        context.events.push(PageEvent {
          time: number(&event, "time"),
          method,
          params: event.get("params").cloned().unwrap_or(serde_json::Value::Null),
        });
      },
      "error" => context.errors.push(TraceError {
        message: string(&event, "message"),
        stack: parse_stack(event.get("stack")),
      }),
      "stdout" => context.stdout.push(string(&event, "text")),
      "stderr" => context.stderr.push(string(&event, "text")),
      "screencast-frame" => context.screencast_frames += 1,
      _ => {},
    }
  }

  context.actions = ordered;
  context
}

fn note_page(context: &mut ContextEntry, page_id: Option<String>) {
  if let Some(page) = page_id
    && !context.pages.contains(&page)
  {
    context.pages.push(page);
  }
}

fn parse_before(event: &serde_json::Value) -> Action {
  Action {
    call_id: string(event, "callId"),
    parent_id: opt_string(event, "parentId"),
    step_id: opt_string(event, "stepId"),
    class: string(event, "class"),
    method: string(event, "method"),
    title: string(event, "title"),
    params: event.get("params").cloned().unwrap_or(serde_json::Value::Null),
    page_id: opt_string(event, "pageId"),
    start_time: number(event, "startTime"),
    stack: parse_stack(event.get("stack")),
    ..Action::default()
  }
}

fn apply_after(action: &mut Action, event: &serde_json::Value) {
  action.end_time = Some(number(event, "endTime"));
  action.error = event.get("error").and_then(|error| {
    Some(ActionError {
      name: error.get("name").and_then(serde_json::Value::as_str)?.to_string(),
      message: error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string(),
    })
  });
  if action.point.is_none() {
    action.point = parse_point(event);
  }
  for attachment in event
    .get("attachments")
    .and_then(serde_json::Value::as_array)
    .into_iter()
    .flatten()
  {
    action.attachments.push(Attachment {
      name: string(attachment, "name"),
      content_type: string(attachment, "contentType"),
      sha1: opt_string(attachment, "sha1").or_else(|| opt_string(attachment, "file")),
      path: opt_string(attachment, "path"),
    });
  }
}

fn parse_point(event: &serde_json::Value) -> Option<(f64, f64)> {
  let point = event.get("point")?;
  Some((
    point.get("x").and_then(serde_json::Value::as_f64)?,
    point.get("y").and_then(serde_json::Value::as_f64)?,
  ))
}

fn parse_console(event: &serde_json::Value) -> ConsoleMessage {
  let location = event.get("location");
  ConsoleMessage {
    time: number(event, "time"),
    message_type: string(event, "messageType"),
    text: string(event, "text"),
    url: location.map(|location| string(location, "url")).unwrap_or_default(),
    line_number: location
      .map(|location| integer(location, "lineNumber"))
      .unwrap_or_default(),
  }
}

fn action_mut<'a>(
  index: &mut BTreeMap<String, usize>,
  actions: &'a mut [Action],
  event: &serde_json::Value,
) -> Option<&'a mut Action> {
  let call_id = event.get("callId")?.as_str()?;
  let position = *index.get(call_id)?;
  actions.get_mut(position)
}

fn apply_context_options(context: &mut ContextEntry, event: &serde_json::Value) {
  context.title = opt_string(event, "title").filter(|t| !t.is_empty());
  context.origin = string(event, "origin");
  context.browser_name = string(event, "browserName");
  context.platform = string(event, "platform");
  context.recorder_version = opt_string(event, "playwrightVersion");
  context.sdk_language = opt_string(event, "sdkLanguage");
  context.wall_time = number(event, "wallTime");
  context.monotonic_time = number(event, "monotonicTime");
  context.options = event.get("options").cloned().unwrap_or(serde_json::Value::Null);
}

fn parse_network(network: &str) -> Vec<NetworkEntry> {
  let mut entries = Vec::new();
  for line in network.lines().filter(|line| !line.trim().is_empty()) {
    let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
      continue;
    };
    let Some(snapshot) = event.get("snapshot") else {
      continue;
    };
    let request = snapshot.get("request");
    let response = snapshot.get("response");
    entries.push(NetworkEntry {
      method: request.map(|r| string(r, "method")).unwrap_or_default(),
      url: request.map(|r| string(r, "url")).unwrap_or_default(),
      status: response.map(|r| integer(r, "status")).unwrap_or_default(),
      mime_type: response
        .and_then(|r| r.get("content"))
        .map(|c| string(c, "mimeType"))
        .unwrap_or_default(),
      duration_ms: number(snapshot, "time"),
    });
  }
  entries
}

fn parse_stack(value: Option<&serde_json::Value>) -> Vec<StackFrame> {
  value
    .and_then(serde_json::Value::as_array)
    .map(|frames| {
      frames
        .iter()
        .map(|frame| StackFrame {
          file: string(frame, "file"),
          line: integer(frame, "line"),
          column: integer(frame, "column"),
        })
        .collect()
    })
    .unwrap_or_default()
}

fn string(value: &serde_json::Value, key: &str) -> String {
  value
    .get(key)
    .and_then(serde_json::Value::as_str)
    .unwrap_or_default()
    .to_string()
}

fn opt_string(value: &serde_json::Value, key: &str) -> Option<String> {
  value
    .get(key)
    .and_then(serde_json::Value::as_str)
    .map(ToString::to_string)
}

fn number(value: &serde_json::Value, key: &str) -> f64 {
  value.get(key).and_then(serde_json::Value::as_f64).unwrap_or_default()
}

/// Integer fields (line numbers, status codes) as written — never rounded
/// off a float, so a malformed value reads as absent instead of as zero-ish.
fn integer(value: &serde_json::Value, key: &str) -> u32 {
  value
    .get(key)
    .and_then(serde_json::Value::as_u64)
    .and_then(|number| u32::try_from(number).ok())
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
  use super::*;

  const TRACE: &str = concat!(
    r#"{"version":8,"type":"context-options","origin":"library","browserName":"chromium","platform":"darwin","wallTime":1000,"monotonicTime":0,"title":"smoke","options":{},"sdkLanguage":"javascript"}"#,
    "\n",
    r#"{"type":"before","callId":"call@1","startTime":10,"class":"Page","method":"goto","title":"page.goto","params":{"url":"http://a"},"pageId":"page@1","stack":[{"file":"/spec.ts","line":4,"column":3}]}"#,
    "\n",
    r#"{"type":"after","callId":"call@1","endTime":60}"#,
    "\n",
    r##"{"type":"before","callId":"call@2","startTime":70,"class":"Locator","method":"click","title":"locator.click","params":{"selector":"#go"},"pageId":"page@1","parentId":"call@1"}"##,
    "\n",
    r##"{"type":"log","callId":"call@2","time":75,"message":"waiting for locator('#go')"}"##,
    "\n",
    r#"{"type":"input","callId":"call@2","point":{"x":12,"y":34}}"#,
    "\n",
    r#"{"type":"after","callId":"call@2","endTime":1070,"error":{"name":"TimeoutError","message":"Timeout 1000ms exceeded"},"attachments":[{"name":"screenshot","contentType":"image/png","sha1":"abc.png"}]}"#,
    "\n",
    r#"{"type":"console","time":80,"messageType":"error","text":"boom","pageId":"page@1","location":{"url":"http://a","lineNumber":2,"columnNumber":1}}"#,
    "\n",
    r#"{"type":"event","time":5,"class":"BrowserContext","method":"page","params":{"pageId":"page@1"},"pageId":"page@1"}"#,
    "\n",
    r#"{"type":"error","message":"expect(received).toBe(expected)","stack":[{"file":"/spec.ts","line":9,"column":1}]}"#,
    "\n",
    r#"{"type":"stdout","timestamp":90,"text":"hello"}"#,
    "\n",
    r#"{"type":"screencast-frame","pageId":"page@1","sha1":"page@1-1.jpeg","width":800,"height":600,"timestamp":20}"#,
    "\n",
  );

  const NETWORK: &str = concat!(
    r#"{"type":"resource-snapshot","snapshot":{"time":12.5,"request":{"method":"GET","url":"http://a/app.js"},"response":{"status":200,"content":{"mimeType":"text/javascript"}}}}"#,
    "\n",
    r#"{"type":"resource-snapshot","snapshot":{"time":3,"request":{"method":"POST","url":"http://a/api"},"response":{"status":500,"content":{"mimeType":"application/json"}}}}"#,
    "\n",
  );

  fn write_trace(dir: &Path, prefix: &str) {
    std::fs::write(dir.join(format!("{prefix}.trace")), TRACE).expect("write trace");
    std::fs::write(dir.join(format!("{prefix}.network")), NETWORK).expect("write network");
  }

  #[test]
  fn reads_a_loose_recording_as_one_context() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_trace(dir.path(), "trace");
    let model = TraceModel::load(&TraceSource::Dir(dir.path().to_path_buf())).expect("model");

    assert_eq!(model.contexts.len(), 1);
    let context = &model.contexts[0];
    assert_eq!(context.browser_name, "chromium");
    assert_eq!(context.title.as_deref(), Some("smoke"));
    assert_eq!(context.pages, vec!["page@1"]);
    assert_eq!(context.screencast_frames, 1);
    assert_eq!(context.console.len(), 1);
    assert_eq!(context.stdout, vec!["hello"]);
    assert_eq!(context.errors.len(), 1);
    assert_eq!(context.network.len(), 2);
    assert!(context.network[1].failed());
  }

  #[test]
  fn joins_action_lines_into_calls() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_trace(dir.path(), "trace");
    let model = TraceModel::load(&TraceSource::Dir(dir.path().to_path_buf())).expect("model");
    let actions = &model.contexts[0].actions;

    assert_eq!(actions.len(), 2);
    assert_eq!(actions[0].display_title(), "page.goto");
    assert_eq!(actions[0].duration_ms(), Some(50.0));
    assert_eq!(
      actions[0].stack.first().map(ToString::to_string).as_deref(),
      Some("/spec.ts:4")
    );

    let click = &actions[1];
    assert_eq!(click.parent_id.as_deref(), Some("call@1"));
    assert_eq!(click.point, Some((12.0, 34.0)));
    assert_eq!(click.logs, vec!["waiting for locator('#go')"]);
    assert_eq!(click.error.as_ref().map(|e| e.name.as_str()), Some("TimeoutError"));
    assert_eq!(click.attachments.first().map(|a| a.name.as_str()), Some("screenshot"));
  }

  #[test]
  fn failures_cover_actions_and_runner_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_trace(dir.path(), "trace");
    let model = TraceModel::load(&TraceSource::Dir(dir.path().to_path_buf())).expect("model");
    let failures = model.failures();
    assert_eq!(failures.len(), 2);
    assert!(failures[0].starts_with("locator.click: Timeout"));
    assert!(failures[1].starts_with("expect("));
  }

  #[test]
  fn an_unfinished_action_has_no_duration() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
      dir.path().join("live.trace"),
      concat!(
        r#"{"version":8,"type":"context-options","origin":"library","browserName":"chromium","platform":"darwin","wallTime":1,"monotonicTime":0,"options":{}}"#,
        "\n",
        r#"{"type":"before","callId":"call@1","startTime":10,"class":"Locator","method":"click","title":"locator.click","params":{}}"#,
        "\n",
      ),
    )
    .expect("write");
    let model = TraceModel::load(&TraceSource::Dir(dir.path().to_path_buf())).expect("model");
    assert_eq!(model.contexts[0].actions[0].duration_ms(), None);
  }

  #[test]
  fn several_prefixes_are_several_contexts() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_trace(dir.path(), "browser");
    write_trace(dir.path(), "runner");
    let model = TraceModel::load(&TraceSource::Dir(dir.path().to_path_buf())).expect("model");
    assert_eq!(model.contexts.len(), 2);
    assert_eq!(model.all_actions().len(), 4);
  }

  #[test]
  fn a_zip_reads_the_same_as_a_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let zip_path = dir.path().join("trace.zip");
    {
      let file = std::fs::File::create(&zip_path).expect("create");
      let mut writer = zip::ZipWriter::new(file);
      let options = zip::write::SimpleFileOptions::default();
      writer.start_file("trace.trace", options).expect("entry");
      std::io::Write::write_all(&mut writer, TRACE.as_bytes()).expect("write");
      writer.start_file("trace.network", options).expect("entry");
      std::io::Write::write_all(&mut writer, NETWORK.as_bytes()).expect("write");
      writer.start_file("resources/src@1.txt", options).expect("entry");
      std::io::Write::write_all(&mut writer, b"source").expect("write");
      writer.finish().expect("finish");
    }
    let model = TraceModel::load(&TraceSource::open(&zip_path).expect("open")).expect("model");
    assert!(model.has_sources);
    assert_eq!(model.contexts[0].actions.len(), 2);
    assert_eq!(model.contexts[0].network.len(), 2);
  }

  #[test]
  fn a_non_trace_file_is_reported_as_such() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, b"nope").expect("write");
    let error = TraceModel::load(&TraceSource::Dir(dir.path().to_path_buf())).expect_err("must fail");
    assert!(error.contains("no .trace stream"), "{error}");
  }
}
