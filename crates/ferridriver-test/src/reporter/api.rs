//! Playwright's reporter API, as data.
//!
//! A reporter written against Playwright is handed objects — a
//! `FullConfig`, a `Suite` tree, and per attempt a `TestCase`,
//! `TestResult` and `TestStep`. This module is the ONE lowering from the
//! runner's model into those shapes. A host that speaks to reporters in
//! another language builds its objects from these structs and decides
//! nothing itself, so a Rust reporter and a JS reporter cannot disagree
//! about what a title path, an expected status or an attempt's errors
//! are.
//!
//! Playwright ref: `packages/playwright/types/testReporter.d.ts`.

use std::path::Path;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::config::{ProjectConfig, TestConfig};
use crate::model::{
  Attachment as ModelAttachment, AttachmentBody, ExpectedStatus, TestAnnotation, TestFailure, TestId, TestOutcome,
  TestPlan, TestStep as ModelStep,
};
use crate::reporter::base;

/// Milliseconds since the Unix epoch — what a JS `Date` is built from.
///
/// Integer, like every other time on ferridriver's wire: serde's
/// internally-tagged buffering turns a float into a map under
/// `serde_json/arbitrary_precision` (which a transitive dependency
/// force-enables workspace-wide), and the blob reader then cannot read
/// its own writer's output.
#[must_use]
pub fn epoch_ms(time: SystemTime) -> i64 {
  time
    .duration_since(SystemTime::UNIX_EPOCH)
    .ok()
    .and_then(|d| i64::try_from(d.as_millis()).ok())
    .unwrap_or_default()
}

/// A duration in whole milliseconds — see [`epoch_ms`] on why not a
/// float.
#[must_use]
pub fn ms(duration: Duration) -> i64 {
  i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
  pub file: String,
  pub line: usize,
  pub column: usize,
}

/// `{ type, description }` — the shape both `TestCase.annotations` and
/// `TestResult.annotations` carry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Annotation {
  #[serde(rename = "type")]
  pub kind: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub description: Option<String>,
}

/// Playwright's `TestError`. `snippet` is the rendered assertion diff —
/// the slot every consumer prints verbatim under the message.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportedError {
  pub message: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub stack: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub location: Option<Location>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub snippet: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
  pub name: String,
  pub content_type: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub path: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub body: Option<Vec<u8>>,
}

/// Playwright's `TestStep`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
  pub id: String,
  pub title: String,
  pub category: String,
  pub duration: i64,
  pub start_time: i64,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub error: Option<ReportedError>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub location: Option<Location>,
  pub annotations: Vec<Annotation>,
  pub attachments: Vec<Attachment>,
  pub steps: Vec<Step>,
}

/// Playwright's `TestResult` — one attempt.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attempt {
  pub retry: u32,
  pub worker_index: u32,
  pub parallel_index: u32,
  pub duration: i64,
  pub start_time: i64,
  /// `None` while the attempt is still running — Playwright's
  /// `TestResult.status` is undefined between `onTestBegin` and
  /// `onTestEnd`.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub status: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub error: Option<ReportedError>,
  pub errors: Vec<ReportedError>,
  pub stdout: Vec<String>,
  pub stderr: Vec<String>,
  pub attachments: Vec<Attachment>,
  pub annotations: Vec<Annotation>,
  pub steps: Vec<Step>,
}

/// Playwright's `TestCase`, without its results — those arrive as the
/// run goes and are appended by whoever drives the reporter.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Case {
  /// `TestId::stable_id` under this test's project — the id a reporter
  /// keys by and the one the HTML report links to.
  pub id: String,
  pub title: String,
  pub title_path: Vec<String>,
  pub location: Location,
  pub expected_status: String,
  pub timeout: i64,
  pub retries: u32,
  pub repeat_each_index: u32,
  pub tags: Vec<String>,
  pub annotations: Vec<Annotation>,
  /// The project this case belongs to. Not part of Playwright's
  /// `TestCase` (there it is reached through `parent.project()`); kept
  /// here so a driver can route an event to the right case without
  /// walking the tree.
  #[serde(default)]
  pub project_name: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SuiteKind {
  #[default]
  Root,
  Project,
  File,
  Describe,
}

impl SuiteKind {
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Root => "root",
      Self::Project => "project",
      Self::File => "file",
      Self::Describe => "describe",
    }
  }
}

/// Playwright's `Suite`. The tree is root → project → file → describe…,
/// exactly the nesting `onBegin(suite)` hands a reporter.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Suite {
  pub title: String,
  #[serde(rename = "type")]
  pub kind: SuiteKind,
  pub title_path: Vec<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub location: Option<Location>,
  /// The `FullProject` a project-level suite answers `project()` with.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub project: Option<serde_json::Value>,
  pub suites: Vec<Suite>,
  pub tests: Vec<Case>,
}

impl Suite {
  /// Every case in this subtree, in the order a reporter walks them.
  pub fn all_cases(&self) -> Vec<&Case> {
    let mut out = Vec::new();
    self.collect_cases(&mut out);
    out
  }

  /// Fold another tree of the same run into this one — the shape a
  /// sharded run needs when its blobs are merged, where each shard
  /// carries only the slice of the tree it ran. Suites match on
  /// (title, kind); a case already present by id is not duplicated.
  pub fn merge_from(&mut self, other: Self) {
    for suite in other.suites {
      match self
        .suites
        .iter_mut()
        .find(|existing| existing.title == suite.title && existing.kind == suite.kind)
      {
        Some(existing) => existing.merge_from(suite),
        None => self.suites.push(suite),
      }
    }
    for case in other.tests {
      if !self.tests.iter().any(|existing| existing.id == case.id) {
        self.tests.push(case);
      }
    }
  }

  fn collect_cases<'a>(&'a self, out: &mut Vec<&'a Case>) {
    for suite in &self.suites {
      suite.collect_cases(out);
    }
    out.extend(self.tests.iter());
  }
}

/// Playwright's `FullResult` — how the whole run ended.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FullResult {
  pub status: String,
  pub start_time: i64,
  pub duration: i64,
}

/// Everything a reporter is told before the first test runs:
/// Playwright's `onConfigure(config)` argument and its `onBegin(suite)`
/// argument, resolved once by the runner and carried on
/// [`crate::reporter::ReporterEvent::RunStarted`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunPreamble {
  pub config: serde_json::Value,
  pub suite: Suite,
}

impl RunPreamble {
  /// The preamble for a run whose plan is not known — the replay paths
  /// that predate one, and the tests that only assert on counters.
  #[must_use]
  pub fn empty() -> Self {
    Self {
      config: serde_json::json!({}),
      suite: Suite::default(),
    }
  }

  /// Fold another shard's preamble into this one. The first non-empty
  /// `config` wins — every shard of a run resolved the same one — and
  /// the suite trees union.
  pub fn merge_from(&mut self, other: Self) {
    if self.config.as_object().is_none_or(serde_json::Map::is_empty) {
      self.config = other.config;
    }
    self.suite.merge_from(other.suite);
  }

  /// Build from the projects a run covers. Each entry is a project's
  /// resolved name, the config it runs under, and the plan already
  /// narrowed to it.
  #[must_use]
  pub fn build(config: &TestConfig, projects: &[ProjectPlan<'_>]) -> Self {
    Self {
      config: full_config(config),
      suite: root_suite(config, projects),
    }
  }
}

/// One project of a run: its name, its merged config, and its plan.
pub struct ProjectPlan<'a> {
  pub name: &'a str,
  pub config: &'a TestConfig,
  pub project: Option<&'a ProjectConfig>,
  pub plan: &'a TestPlan,
}

// ── Config ──

/// Playwright's `FullConfig`. Field names and shapes mirror it, so a
/// consumer keying off `config.projects[].name` or `config.rootDir`
/// works unchanged.
#[must_use]
pub fn full_config(config: &TestConfig) -> serde_json::Value {
  let root_dir = root_dir(config);
  let projects: Vec<serde_json::Value> = if config.projects.is_empty() {
    vec![full_project(config, None)]
  } else {
    config.projects.iter().map(|p| full_project(config, Some(p))).collect()
  };

  serde_json::json!({
    "rootDir": root_dir,
    "forbidOnly": config.forbid_only,
    "fullyParallel": config.fully_parallel,
    "globalSetup": config.global_setup,
    "globalTeardown": config.global_teardown,
    "globalTimeout": config.global_timeout,
    "grep": config.config_grep,
    "grepInvert": config.config_grep_invert,
    "maxFailures": config.max_failures,
    "metadata": config.metadata,
    "preserveOutput": config.preserve_output,
    "projects": projects,
    "quiet": config.quiet,
    "reporter": config
      .reporter
      .iter()
      .map(|r| serde_json::json!([r.name, r.options]))
      .collect::<Vec<_>>(),
    "reportSlowTests": config.report_slow_tests,
    "shard": serde_json::Value::Null,
    "updateSnapshots": config.update_snapshots,
    "version": env!("CARGO_PKG_VERSION"),
    "workers": config.workers,
    "webServer": config.web_server,
  })
}

/// Playwright's `FullProject`. `project` is `None` for a config that
/// declares no `[[test.projects]]` — the config itself is then the one
/// project, as it is in Playwright.
#[must_use]
pub fn full_project(config: &TestConfig, project: Option<&ProjectConfig>) -> serde_json::Value {
  let root_dir = root_dir(config);
  match project {
    None => serde_json::json!({
      "outputDir": config.output_dir,
      "repeatEach": config.repeat_each,
      "retries": config.retries,
      "metadata": config.metadata,
      "id": config.name.clone().unwrap_or_default(),
      "name": config.name.clone().unwrap_or_default(),
      "testDir": root_dir,
      "testIgnore": config.test_ignore,
      "testMatch": config.test_match,
      "timeout": config.timeout,
    }),
    Some(project) => serde_json::json!({
      "outputDir": project.output_dir.clone().unwrap_or_else(|| config.output_dir.display().to_string()),
      "repeatEach": project.repeat_each.unwrap_or(config.repeat_each),
      "retries": project.retries.unwrap_or(config.retries),
      "metadata": serde_json::json!({ "project": project.name }),
      "id": project.name,
      "name": project.name,
      "testDir": project.test_dir.clone().unwrap_or_else(|| root_dir.clone()),
      "testIgnore": project.test_ignore.clone().unwrap_or_else(|| config.test_ignore.clone()),
      "testMatch": project.test_match.clone().unwrap_or_else(|| config.test_match.clone()),
      "timeout": project.timeout.unwrap_or(config.timeout),
    }),
  }
}

fn root_dir(config: &TestConfig) -> String {
  config.test_dir.clone().unwrap_or_else(|| ".".to_string())
}

// ── Suite tree ──

fn root_suite(config: &TestConfig, projects: &[ProjectPlan<'_>]) -> Suite {
  let root = Path::new(config.test_dir.as_deref().unwrap_or("."));
  Suite {
    title: String::new(),
    kind: SuiteKind::Root,
    title_path: vec![String::new()],
    location: None,
    project: None,
    suites: projects.iter().map(|p| project_suite(root, config, p)).collect(),
    tests: Vec::new(),
  }
}

fn project_suite(root: &Path, config: &TestConfig, project: &ProjectPlan<'_>) -> Suite {
  // Playwright's root title is the empty string and every title path
  // starts with it, so a project's path is ["", "<project>"].
  let title_path = vec![String::new(), project.name.to_string()];
  let mut suite = Suite {
    title: project.name.to_string(),
    kind: SuiteKind::Project,
    title_path: title_path.clone(),
    location: None,
    project: Some(full_project(config, project.project)),
    suites: Vec::new(),
    tests: Vec::new(),
  };
  for file in file_nodes(project.plan) {
    suite.suites.push(file.into_suite(root, project.name, &title_path));
  }
  suite
}

/// A suite while it is being built. Grouped by each test's own title
/// path rather than by the plan's suite list, because a `describe`
/// arrives there as a separate flat suite and a reporter wants it as a
/// child of its file.
struct Node<'a> {
  title: String,
  file: &'a str,
  children: Vec<Node<'a>>,
  cases: Vec<&'a crate::model::TestCase>,
}

impl Node<'_> {
  fn into_suite(self, root: &Path, project_name: &str, parent_path: &[String]) -> Suite {
    let mut title_path = parent_path.to_vec();
    title_path.push(self.title.clone());
    let location = Location {
      file: relative(root, self.file),
      line: 0,
      column: 0,
    };
    let kind = if parent_path.len() == 2 {
      SuiteKind::File
    } else {
      SuiteKind::Describe
    };
    Suite {
      title: self.title,
      kind,
      location: Some(location),
      project: None,
      suites: self
        .children
        .into_iter()
        .map(|child| child.into_suite(root, project_name, &title_path))
        .collect(),
      tests: self
        .cases
        .into_iter()
        .map(|test| case(root, project_name, test))
        .collect(),
      title_path,
    }
  }
}

fn file_nodes(plan: &TestPlan) -> Vec<Node<'_>> {
  let mut files: Vec<Node<'_>> = Vec::new();
  for suite in &plan.suites {
    for test in &suite.tests {
      let titles = test.id.title_path();
      let node = node_for(&mut files, &suite.file, &suite.file);
      // titles = [file, ...describes, test]: the file is the node above
      // and the test is the leaf.
      let describes: Vec<String> = titles[1..titles.len().saturating_sub(1)].to_vec();
      let mut target = node;
      for describe in &describes {
        target = node_for(&mut target.children, describe, &suite.file);
      }
      target.cases.push(test);
    }
  }
  files
}

fn node_for<'a, 'p>(nodes: &'a mut Vec<Node<'p>>, title: &str, file: &'p str) -> &'a mut Node<'p> {
  if let Some(index) = nodes.iter().position(|node| node.title == title) {
    return &mut nodes[index];
  }
  nodes.push(Node {
    title: title.to_string(),
    file,
    children: Vec::new(),
    cases: Vec::new(),
  });
  nodes.last_mut().unwrap_or_else(|| unreachable!("just pushed"))
}

/// One `TestCase`, as the tree carries it before it has run.
#[must_use]
pub fn case(root: &Path, project_name: &str, test: &crate::model::TestCase) -> Case {
  let titles = test.id.title_path();
  Case {
    id: test.id.stable_id(project_name),
    title: titles.last().cloned().unwrap_or_default(),
    // Playwright's TestCase.titlePath() is the enclosing suite's path
    // plus its own title, and every path starts at the root's "".
    title_path: std::iter::once(String::new())
      .chain(std::iter::once(project_name.to_string()))
      .chain(titles.iter().cloned())
      .collect(),
    location: Location {
      file: relative(root, &test.id.file),
      line: test.id.line.unwrap_or(0),
      column: test.id.column.unwrap_or(0),
    },
    expected_status: base::expected_status_str(test.expected_status).to_string(),
    timeout: test.timeout.map(ms).unwrap_or_default(),
    retries: test.retries.unwrap_or_default(),
    repeat_each_index: 0,
    tags: tags(&test.annotations),
    annotations: annotations(&test.annotations),
    project_name: project_name.to_string(),
  }
}

fn relative(root: &Path, file: &str) -> String {
  Path::new(file)
    .strip_prefix(root)
    .map_or_else(|_| file.to_string(), |p| p.display().to_string())
}

// ── Attempts ──

/// One `TestResult`, filled from a finished attempt.
#[must_use]
pub fn attempt(outcome: &TestOutcome) -> Attempt {
  let errors: Vec<ReportedError> = base::attempt_errors(outcome)
    .into_iter()
    .map(|e| error(e, Some(&outcome.test_id)))
    .collect();
  Attempt {
    retry: outcome.attempt.saturating_sub(1),
    worker_index: outcome.worker_index,
    parallel_index: outcome.parallel_index,
    duration: ms(outcome.duration),
    start_time: epoch_ms(outcome.start_time),
    status: Some(outcome.status.as_str().to_string()),
    error: errors.first().cloned(),
    errors,
    stdout: stdio(&outcome.stdout),
    stderr: stdio(&outcome.stderr),
    attachments: outcome.attachments.iter().map(attachment).collect(),
    annotations: annotations(&outcome.annotations),
    steps: outcome.steps.iter().map(step).collect(),
  }
}

/// The result object `onTestBegin` is handed: everything already known
/// when an attempt starts, with `status` still undefined.
#[must_use]
pub fn started_attempt(attempt_number: u32, worker_index: u32, start: SystemTime) -> Attempt {
  Attempt {
    retry: attempt_number.saturating_sub(1),
    worker_index,
    parallel_index: worker_index,
    start_time: epoch_ms(start),
    ..Attempt::default()
  }
}

#[must_use]
pub fn step(step: &ModelStep) -> Step {
  Step {
    id: step.step_id.clone(),
    title: step.title.clone(),
    category: step.category.to_string(),
    duration: ms(step.duration),
    start_time: 0,
    error: step.error.as_ref().map(|message| ReportedError {
      message: base::strip_ansi(message).into_owned(),
      ..ReportedError::default()
    }),
    location: step.location.as_ref().map(|l| Location {
      file: l.file.clone(),
      line: usize::try_from(l.line).unwrap_or(0),
      column: usize::try_from(l.column).unwrap_or(0),
    }),
    annotations: annotations(&step.annotations),
    attachments: Vec::new(),
    steps: step.steps.iter().map(self::step).collect(),
  }
}

#[must_use]
pub fn attachment(a: &ModelAttachment) -> Attachment {
  Attachment {
    name: a.name.clone(),
    content_type: a.content_type.clone(),
    path: match &a.body {
      AttachmentBody::Path(p) => Some(p.display().to_string()),
      AttachmentBody::Bytes(_) => None,
    },
    body: match &a.body {
      AttachmentBody::Bytes(bytes) => Some(bytes.clone()),
      AttachmentBody::Path(_) => None,
    },
  }
}

#[must_use]
pub fn error(failure: &TestFailure, test_id: Option<&TestId>) -> ReportedError {
  let location = failure
    .stack
    .as_deref()
    .and_then(base::parse_error_location)
    .or_else(|| {
      test_id.map(|id| base::ErrorLocation {
        file: id.file.clone(),
        line: id.line.unwrap_or(0),
        column: id.column.unwrap_or(0),
      })
    })
    .map(|loc| Location {
      file: loc.file,
      line: loc.line,
      column: loc.column,
    });
  ReportedError {
    message: base::strip_ansi(&failure.message).into_owned(),
    stack: failure.stack.clone(),
    location,
    snippet: failure
      .diff
      .as_ref()
      .map(|d| base::strip_ansi(d).into_owned())
      .filter(|d| !d.trim().is_empty()),
  }
}

fn stdio(text: &str) -> Vec<String> {
  if text.is_empty() {
    return Vec::new();
  }
  vec![text.to_string()]
}

#[must_use]
pub fn annotations(annotations: &[TestAnnotation]) -> Vec<Annotation> {
  annotations
    .iter()
    .map(|a| match a {
      TestAnnotation::Skip { reason, .. } => Annotation {
        kind: "skip".into(),
        description: reason.clone(),
      },
      TestAnnotation::Slow { reason, .. } => Annotation {
        kind: "slow".into(),
        description: reason.clone(),
      },
      TestAnnotation::Fixme { reason, .. } => Annotation {
        kind: "fixme".into(),
        description: reason.clone(),
      },
      TestAnnotation::Fail { reason, .. } => Annotation {
        kind: "fail".into(),
        description: reason.clone(),
      },
      TestAnnotation::Only => Annotation {
        kind: "only".into(),
        description: None,
      },
      TestAnnotation::Tag(tag) => Annotation {
        kind: "tag".into(),
        description: Some(tag.clone()),
      },
      TestAnnotation::Info { type_name, description } => Annotation {
        kind: type_name.clone(),
        description: Some(description.clone()),
      },
    })
    .collect()
}

/// Tags as Playwright reports them — with the leading `@`, which is how
/// they are written in a title and how `--grep` matches them.
#[must_use]
pub fn tags(annotations: &[TestAnnotation]) -> Vec<String> {
  annotations
    .iter()
    .filter_map(|a| match a {
      TestAnnotation::Tag(tag) => Some(if tag.starts_with('@') {
        tag.clone()
      } else {
        format!("@{tag}")
      }),
      _ => None,
    })
    .collect()
}

/// Playwright's `TestCase.outcome()`, over the statuses an attempt
/// list produced. The rule is [`crate::model::outcome_kind`]'s — this
/// is the spelling a reporter API sees, in the strings a host already
/// holds, so a binding decides nothing.
#[must_use]
pub fn outcome_of(statuses: &[String], expected_status: &str) -> &'static str {
  let statuses: Vec<crate::model::TestStatus> = statuses.iter().map(|s| crate::model::TestStatus::parse(s)).collect();
  let expected = if expected_status == "failed" {
    ExpectedStatus::Fail
  } else {
    ExpectedStatus::Pass
  };
  crate::model::outcome_kind(&statuses, expected).as_str()
}

/// Playwright's `TestCase.ok()`: everything but `unexpected`.
#[must_use]
pub fn ok_of(statuses: &[String], expected_status: &str) -> bool {
  outcome_of(statuses, expected_status) != "unexpected"
}
