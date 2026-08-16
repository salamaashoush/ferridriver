#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `ferridriver test --ui` speaking Playwright's test-server protocol.
//!
//! The UI is Playwright's own app, so this test is that app's side of the
//! conversation: connect to the websocket it would connect to, make the
//! calls it makes (`initialize`, `runGlobalSetup`, `listTests`,
//! `runTests`, `stopTests`), and check the reporter events that come
//! back rebuild a real run — including the live trace the viewer reads
//! while a test is still going.
//!
//! Requires a built `ferridriver` binary and a Chromium.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

fn bin() -> String {
  std::env::var("FERRIDRIVER_BIN").unwrap_or_else(|_| {
    let base = format!("{}/../../target", env!("CARGO_MANIFEST_DIR"));
    let debug = format!("{base}/debug/ferridriver");
    if std::path::Path::new(&debug).exists() {
      debug
    } else {
      format!("{base}/release/ferridriver")
    }
  })
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
  fn drop(&mut self) {
    let _ = self.0.kill();
    let _ = self.0.wait();
  }
}

/// A scratch project: two specs, one of which fails, so the stream has
/// both outcomes in it.
fn write_project(root: &std::path::Path) {
  std::fs::create_dir_all(root.join("specs")).expect("mkdir");
  std::fs::write(
    root.join("specs/pass.spec.ts"),
    "import { test, expect } from '@ferridriver/test';\n\
     test('renders the heading', async ({ page }) => {\n\
     \x20 await test.step('open the page', async () => {\n\
     \x20   await page.setContent('<h1>hello</h1>');\n\
     \x20 });\n\
     \x20 await expect(page.locator('h1')).toHaveText('hello');\n\
     });\n",
  )
  .expect("write pass spec");
  std::fs::write(
    root.join("specs/fail.spec.ts"),
    "import { test, expect } from '@ferridriver/test';\n\
     test('notices the wrong title', async ({ page }) => {\n\
     \x20 await page.setContent('<h1>hello</h1>');\n\
     \x20 expect(await page.title()).toBe('never');\n\
     });\n",
  )
  .expect("write fail spec");
  std::fs::write(
    root.join("ferridriver.toml"),
    "[test]\n\
     testDir = \"specs\"\n\
     testMatch = [\"**/*.spec.ts\"]\n\
     workers = 1\n\
     retries = 0\n\
     reporter = []\n\
     name = \"cdp-pipe\"\n\
     \n[test.browser]\n\
     headless = true\n",
  )
  .expect("write config");
}

/// The same scratch project across two backends, so the UI has two
/// projects to keep apart.
fn write_two_project_config(root: &std::path::Path) {
  std::fs::write(
    root.join("ferridriver.toml"),
    "[test]\n\
     testDir = \"specs\"\n\
     testMatch = [\"**/*.spec.ts\"]\n\
     workers = 1\n\
     retries = 0\n\
     reporter = []\n\
     maxParallelProjects = 1\n\
     \n[test.browser]\n\
     headless = true\n\
     \n[[test.projects]]\n\
     name = \"cdp-pipe\"\n\
     \n[test.projects.browser]\n\
     browser = \"chromium\"\n\
     backend = \"cdp-pipe\"\n\
     headless = true\n\
     \n[[test.projects]]\n\
     name = \"cdp-raw\"\n\
     \n[test.projects.browser]\n\
     browser = \"chromium\"\n\
     backend = \"cdp-raw\"\n\
     headless = true\n",
  )
  .expect("write config");
}

struct Ui {
  ws: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
  next_id: u64,
  /// Events that arrived while waiting for a reply.
  events: Vec<Value>,
}

impl Ui {
  async fn connect(url: &str) -> Self {
    // `http://host/trace/uiMode.html?ws=<guid>&…` -> `ws://host/<guid>`
    let (base, query) = url.split_once("/trace/").expect("app url");
    let guid = query
      .split("ws=")
      .nth(1)
      .and_then(|rest| rest.split('&').next())
      .expect("ws parameter");
    let ws_url = format!("{}/{guid}", base.replace("http://", "ws://"));
    let (ws, _) = tokio_tungstenite::connect_async(&ws_url).await.expect("connect");
    Self {
      ws,
      next_id: 0,
      events: Vec::new(),
    }
  }

  /// One call, awaiting its reply; events that arrive meanwhile are kept.
  async fn call(&mut self, method: &str, params: Value) -> Value {
    self.next_id += 1;
    let id = self.next_id;
    let message = json!({ "id": id, "method": method, "params": params });
    self
      .ws
      .send(Message::Text(message.to_string().into()))
      .await
      .expect("send");
    loop {
      let message = tokio::time::timeout(Duration::from_mins(3), self.ws.next())
        .await
        .expect("test server answered in time")
        .expect("socket open")
        .expect("frame");
      let Message::Text(text) = message else { continue };
      let value: Value = serde_json::from_str(&text).expect("json");
      if value.get("id").and_then(Value::as_u64) == Some(id) {
        assert!(value.get("error").is_none(), "{method} failed: {value}");
        return value.get("result").cloned().unwrap_or(Value::Null);
      }
      if value.get("method").is_some() {
        self.events.push(value);
      }
    }
  }

  /// Every teleReporter event seen so far, in order.
  fn reports(&self) -> Vec<&Value> {
    self
      .events
      .iter()
      .filter(|event| event["method"] == "report")
      .map(|event| &event["params"])
      .collect()
  }

  fn reports_of(&self, method: &str) -> Vec<&Value> {
    self
      .reports()
      .into_iter()
      .filter(|report| report["method"] == method)
      .collect()
  }
}

/// Start `ferridriver test --ui` on a port and return its URL. A port
/// makes it serve rather than open a window, which is what a test wants
/// (and what `--ui-port` means in Playwright too).
fn start_ui(root: &std::path::Path) -> (KillOnDrop, String) {
  let mut child = Command::new(bin())
    .args(["test", "--ui", "--ui-port", "0"])
    .current_dir(root)
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit())
    .spawn()
    .expect("spawn ui");
  let stdout = child.stdout.take().expect("stdout");
  let guard = KillOnDrop(child);

  let (tx, rx) = std::sync::mpsc::channel();
  // The pipe has to keep being read for the whole session: a server that
  // blocks writing to a full (or closed) stdout stops answering.
  std::thread::spawn(move || {
    let mut sent = false;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
      let trimmed = line.trim().to_string();
      if !sent && trimmed.starts_with("http://") {
        sent = true;
        let _ = tx.send(trimmed);
      }
    }
  });
  let url = rx
    .recv_timeout(Duration::from_mins(2))
    .expect("ui server never printed a URL");
  (guard, url)
}

#[tokio::test]
async fn the_ui_lists_runs_and_watches_a_suite() {
  let root = tempfile::tempdir().expect("tempdir");
  write_project(root.path());
  let (_server, url) = start_ui(root.path());
  assert!(url.contains("/trace/uiMode.html?ws="), "not the UI app: {url}");

  let mut ui = Ui::connect(&url).await;
  ui.call("initialize", json!({ "interceptStdio": true, "watchTestDirs": true }))
    .await;

  // Global setup: the UI will not render anything without a config.
  let setup = ui.call("runGlobalSetup", json!({})).await;
  assert_eq!(setup["status"], "passed");
  let configure = &setup["report"][0];
  assert_eq!(configure["method"], "onConfigure");
  assert_eq!(configure["params"]["config"]["projects"][0]["name"], "cdp-pipe");

  // Listing: both specs, as one file suite each.
  let listed = ui.call("listTests", json!({})).await;
  assert_eq!(listed["status"], "passed");
  let report = listed["report"].as_array().expect("report");
  let project = report
    .iter()
    .find(|event| event["method"] == "onProject")
    .expect("onProject");
  let suites = project["params"]["project"]["suites"].as_array().expect("suites");
  assert_eq!(suites.len(), 2, "one suite per file: {suites:#?}");
  assert!(
    report.iter().any(|event| event["method"] == "onBegin"),
    "the receiver only builds its tree on onBegin"
  );

  let mut ids: Vec<(String, String)> = Vec::new();
  for suite in suites {
    for entry in suite["entries"].as_array().expect("entries") {
      ids.push((
        entry["title"].as_str().expect("title").to_string(),
        entry["testId"].as_str().expect("testId").to_string(),
      ));
    }
  }
  assert_eq!(ids.len(), 2, "{ids:?}");

  // Run one test by id — the UI's "run this test" button.
  let (_, passing_id) = ids
    .iter()
    .find(|(title, _)| title.contains("renders"))
    .expect("passing test")
    .clone();
  let run = ui
    .call("runTests", json!({ "testIds": [passing_id], "trace": "on" }))
    .await;
  assert_eq!(run["status"], "passed", "run failed: {run}");

  let begins = ui.reports_of("onTestBegin");
  assert_eq!(begins.len(), 1, "exactly the test we asked for ran");
  assert_eq!(begins[0]["params"]["testId"], passing_id.as_str());
  let result_id = begins[0]["params"]["result"]["id"].as_str().expect("result id");

  let ends = ui.reports_of("onTestEnd");
  assert_eq!(ends.len(), 1);
  assert_eq!(ends[0]["params"]["result"]["status"], "passed");
  assert_eq!(ends[0]["params"]["result"]["id"], result_id);

  // The trace the viewer opens for a finished test is an attachment.
  let attachments: Vec<&Value> = ui.reports_of("onAttach");
  let trace = attachments
    .iter()
    .flat_map(|event| event["params"]["attachments"].as_array().cloned().unwrap_or_default())
    .find(|attachment| attachment["name"] == "trace")
    .expect("a trace attachment");
  let trace_path = trace["path"].as_str().expect("trace path");
  assert!(
    std::path::Path::new(trace_path).exists(),
    "trace attachment points nowhere: {trace_path}"
  );

  // Steps of the run are reported too — the UI's step list and the
  // trace's actions are the same thing keyed by the same ids.
  assert!(
    !ui.reports_of("onStepBegin").is_empty(),
    "no steps reported: {:#?}",
    ui.reports()
  );
}

#[tokio::test]
async fn a_failing_test_reports_its_error_and_the_run_fails() {
  let root = tempfile::tempdir().expect("tempdir");
  write_project(root.path());
  let (_server, url) = start_ui(root.path());

  let mut ui = Ui::connect(&url).await;
  ui.call("initialize", json!({})).await;
  let listed = ui.call("listTests", json!({})).await;
  let failing_id = listed["report"]
    .as_array()
    .expect("report")
    .iter()
    .filter(|event| event["method"] == "onProject")
    .flat_map(|event| {
      event["params"]["project"]["suites"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    })
    .flat_map(|suite| suite["entries"].as_array().cloned().unwrap_or_default())
    .find(|entry| entry["title"].as_str().is_some_and(|title| title.contains("notices")))
    .and_then(|entry| entry["testId"].as_str().map(ToString::to_string))
    .expect("failing test id");

  let run = ui
    .call("runTests", json!({ "testIds": [failing_id], "trace": "on" }))
    .await;
  assert_eq!(run["status"], "failed");

  let ends = ui.reports_of("onTestEnd");
  assert_eq!(ends.len(), 1);
  assert_eq!(ends[0]["params"]["result"]["status"], "failed");
  let message = ends[0]["params"]["result"]["errors"][0]["message"]
    .as_str()
    .expect("error message");
  assert!(message.contains("never") || message.contains("expect"), "{message}");
}

#[tokio::test]
async fn a_live_trace_is_readable_while_the_test_runs() {
  let root = tempfile::tempdir().expect("tempdir");
  write_project(root.path());
  // A spec slow enough to be observed mid-flight.
  std::fs::write(
    root.path().join("specs/slow.spec.ts"),
    "import { test, expect } from '@ferridriver/test';\n\
     test('takes its time', async ({ page }) => {\n\
     \x20 await page.setContent('<h1>slow</h1>');\n\
     \x20 for (let i = 0; i < 6; i++) {\n\
     \x20   await expect(page.locator('h1')).toHaveText('slow');\n\
     \x20   await page.waitForTimeout(500);\n\
     \x20 }\n\
     });\n",
  )
  .expect("write slow spec");

  let (_server, url) = start_ui(root.path());
  let mut ui = Ui::connect(&url).await;
  ui.call("initialize", json!({})).await;

  let run_url = url.clone();
  let runner = tokio::spawn(async move {
    let mut ui = Ui::connect(&run_url).await;
    ui.call("runTests", json!({ "grep": "takes its time", "trace": "on" }))
      .await
  });

  // While it runs, the loose trace files are on disk under the worker's
  // artifacts directory — that is what the viewer polls.
  let traces_dir = root.path().join("test-results");
  let deadline = std::time::Instant::now() + Duration::from_mins(1) + Duration::from_secs(30);
  let mut live_trace = None;
  while std::time::Instant::now() < deadline {
    if let Some(found) = find_live_trace(&traces_dir) {
      live_trace = Some(found);
      break;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  let live_trace = live_trace.expect("no live trace appeared while a test was running");

  let body = std::fs::read_to_string(&live_trace).expect("read live trace");
  let first: Value = serde_json::from_str(body.lines().next().expect("a line")).expect("json");
  assert_eq!(first["type"], "context-options", "{first}");
  assert_eq!(first["version"], 8);

  let result = runner.await.expect("run finished");
  assert_eq!(result["status"], "passed", "{result}");
  let _ = ui.call("ping", json!({})).await;
}

#[tokio::test]
async fn every_project_is_listed_and_runs_under_its_own_name() {
  let root = tempfile::tempdir().expect("tempdir");
  write_project(root.path());
  // One spec is enough here, and it has to pass on both backends.
  std::fs::remove_file(root.path().join("specs/fail.spec.ts")).expect("drop the failing spec");
  write_two_project_config(root.path());
  let (_server, url) = start_ui(root.path());

  let mut ui = Ui::connect(&url).await;
  ui.call("initialize", json!({})).await;

  let setup = ui.call("runGlobalSetup", json!({})).await;
  let names: Vec<String> = setup["report"][0]["params"]["config"]["projects"]
    .as_array()
    .expect("projects")
    .iter()
    .map(|project| project["name"].as_str().unwrap_or_default().to_string())
    .collect();
  assert_eq!(names, ["cdp-pipe", "cdp-raw"], "every project is in the config");

  let listed = ui.call("listTests", json!({})).await;
  let projects: Vec<&Value> = listed["report"]
    .as_array()
    .expect("report")
    .iter()
    .filter(|event| event["method"] == "onProject")
    .collect();
  assert_eq!(projects.len(), 2, "one onProject per project: {projects:#?}");
  let ids: Vec<String> = projects
    .iter()
    .map(|project| {
      project["params"]["project"]["suites"][0]["entries"][0]["testId"]
        .as_str()
        .expect("testId")
        .to_string()
    })
    .collect();
  assert_ne!(ids[0], ids[1], "the same test in two projects is two ids");

  // Run everything: both projects report, each under its own id.
  let run = ui.call("runTests", json!({ "trace": "on" })).await;
  assert_eq!(run["status"], "passed", "run failed: {run}");
  let ran: Vec<String> = ui
    .reports_of("onTestBegin")
    .iter()
    .map(|event| event["params"]["testId"].as_str().unwrap_or_default().to_string())
    .collect();
  assert_eq!(ran.len(), 2, "both projects ran the spec: {ran:?}");
  assert!(ran.contains(&ids[0]) && ran.contains(&ids[1]), "{ran:?} vs {ids:?}");

  // Narrow to one project: the UI's project filter.
  let mut second = Ui::connect(&url).await;
  let run = second
    .call("runTests", json!({ "projects": ["cdp-raw"], "trace": "on" }))
    .await;
  assert_eq!(run["status"], "passed", "run failed: {run}");
  let ran: Vec<String> = second
    .reports_of("onTestBegin")
    .iter()
    .map(|event| event["params"]["testId"].as_str().unwrap_or_default().to_string())
    .collect();
  assert_eq!(ran, vec![ids[1].clone()], "only the project asked for ran");

  // An option the runner cannot honour fails the call and says why,
  // rather than running as if it had been applied.
  let mut third = Ui::connect(&url).await;
  let refused = third.call("runTests", json!({ "reuseContext": true })).await;
  assert_eq!(refused["status"], "failed");
  assert!(
    refused["error"]
      .as_str()
      .is_some_and(|error| error.contains("reuseContext")),
    "{refused}"
  );
  let errors = third.reports_of("onError");
  assert!(!errors.is_empty(), "the refusal reaches the Errors tab too");
}

#[tokio::test]
async fn run_options_change_the_run_they_are_sent_with() {
  let root = tempfile::tempdir().expect("tempdir");
  write_project(root.path());
  // A second failure, so "stop on first failure" has something to stop.
  std::fs::write(
    root.path().join("specs/fail2.spec.ts"),
    "import { test, expect } from '@ferridriver/test';\n\
     test('notices the other wrong title', async ({ page }) => {\n\
     \x20 await page.setContent('<h1>hello</h1>');\n\
     \x20 expect(await page.title()).toBe('never either');\n\
     });\n",
  )
  .expect("write second failing spec");
  std::fs::write(
    root.path().join("specs/fail3.spec.ts"),
    "import { test, expect } from '@ferridriver/test';\n\
     test('notices a third wrong title', async ({ page }) => {\n\
     \x20 await page.setContent('<h1>hello</h1>');\n\
     \x20 expect(await page.title()).toBe('never at all');\n\
     });\n",
  )
  .expect("write third failing spec");
  std::fs::write(
    root.path().join("specs/slow.spec.ts"),
    "import { test } from '@ferridriver/test';\n\
     test('waits around', async ({ page }) => {\n\
     \x20 await page.setContent('<h1>slow</h1>');\n\
     \x20 await page.waitForTimeout(5000);\n\
     });\n",
  )
  .expect("write slow spec");
  std::fs::write(
    root.path().join("specs/snap.spec.ts"),
    "import { test, expect } from '@ferridriver/test';\n\
     test('keeps its shape', async ({ page }) => {\n\
     \x20 await page.setContent('<h1>snapshot</h1>');\n\
     \x20 await expect(page.locator('h1')).toMatchSnapshot('heading');\n\
     });\n",
  )
  .expect("write snapshot spec");
  // Four workers in the config, so a run asking for one has something to
  // narrow: without the option these specs spread across workers.
  std::fs::write(
    root.path().join("ferridriver.toml"),
    "[test]\n\
     testDir = \"specs\"\n\
     testMatch = [\"**/*.spec.ts\"]\n\
     workers = 4\n\
     retries = 0\n\
     reporter = []\n\
     name = \"cdp-pipe\"\n\
     \n[test.browser]\n\
     headless = true\n",
  )
  .expect("rewrite config");

  let (_server, url) = start_ui(root.path());

  // workers + maxFailures: one worker takes the failures in order, and
  // the second one never gets to run.
  let mut stop_on_failure = Ui::connect(&url).await;
  let run = stop_on_failure
    .call(
      "runTests",
      json!({ "grep": "notices", "workers": 1, "maxFailures": 1, "trace": "on" }),
    )
    .await;
  assert_eq!(run["status"], "failed", "{run}");
  // Three tests match, all failing; stopping after the first leaves at
  // most the one the worker had already picked up.
  let ended = stop_on_failure.reports_of("onTestEnd").len();
  assert!(
    ended < 3,
    "the run went on past the first failure: {:#?}",
    stop_on_failure.reports_of("onTestEnd")
  );
  assert!(
    stop_on_failure
      .reports_of("onTestBegin")
      .iter()
      .all(|event| event["params"]["result"]["workerIndex"] == 0),
    "one worker means worker 0 only: {:#?}",
    stop_on_failure.reports_of("onTestBegin")
  );

  // trace: "off" — the run records nothing for the viewer to open.
  let mut untraced = Ui::connect(&url).await;
  let run = untraced
    .call("runTests", json!({ "grep": "renders the heading", "trace": "off" }))
    .await;
  assert_eq!(run["status"], "passed", "{run}");
  let traces: Vec<Value> = untraced
    .reports_of("onAttach")
    .iter()
    .flat_map(|event| event["params"]["attachments"].as_array().cloned().unwrap_or_default())
    .filter(|attachment| attachment["name"] == "trace")
    .collect();
  assert!(traces.is_empty(), "trace:off still recorded one: {traces:#?}");

  // timeout: the run's, not the config's 30s default.
  let mut impatient = Ui::connect(&url).await;
  let run = impatient
    .call(
      "runTests",
      json!({ "grep": "waits around", "timeout": 1000, "trace": "on" }),
    )
    .await;
  assert_eq!(run["status"], "failed", "{run}");
  let ends = impatient.reports_of("onTestEnd");
  assert_eq!(ends.len(), 1);
  assert_eq!(
    ends[0]["params"]["result"]["status"], "timedOut",
    "the run's timeout is what the test was held to: {:#?}",
    ends[0]
  );
  assert_eq!(
    ends[0]["params"]["test"]["timeout"], 1000,
    "and the UI is told which timeout that was"
  );

  // video: "on" — the run records one, and the UI is told where it is.
  let mut recorded = Ui::connect(&url).await;
  let run = recorded
    .call(
      "runTests",
      json!({ "grep": "renders the heading", "video": "on", "trace": "off" }),
    )
    .await;
  assert_eq!(run["status"], "passed", "{run}");
  let videos: Vec<Value> = recorded
    .reports_of("onAttach")
    .iter()
    .flat_map(|event| event["params"]["attachments"].as_array().cloned().unwrap_or_default())
    .filter(|attachment| attachment["name"] == "video")
    .collect();
  assert_eq!(videos.len(), 1, "video:on recorded nothing: {:#?}", recorded.reports());

  // updateSnapshots: "none" refuses to write the missing baseline,
  // "missing" writes it — the UI's Update snapshots setting.
  let mut strict = Ui::connect(&url).await;
  let run = strict
    .call(
      "runTests",
      json!({ "grep": "keeps its shape", "updateSnapshots": "none", "trace": "off" }),
    )
    .await;
  assert_eq!(
    run["status"], "failed",
    "a missing baseline is a failure under none: {run}"
  );
  let mut writing = Ui::connect(&url).await;
  let run = writing
    .call(
      "runTests",
      json!({ "grep": "keeps its shape", "updateSnapshots": "missing", "trace": "off" }),
    )
    .await;
  assert_eq!(run["status"], "passed", "missing writes the baseline: {run}");

  // reporters: the run's own, on top of whatever the config has.
  let mut reported = Ui::connect(&url).await;
  let run = reported
    .call(
      "runTests",
      json!({ "grep": "renders the heading", "reporters": ["json"], "trace": "off" }),
    )
    .await;
  assert_eq!(run["status"], "passed", "{run}");
  let results = root.path().join("test-results/results.json");
  assert!(
    results.exists(),
    "the json reporter the run asked for wrote nothing to {}",
    results.display()
  );
}

#[tokio::test]
async fn a_discovery_failure_reaches_the_ui_as_an_error() {
  let root = tempfile::tempdir().expect("tempdir");
  write_project(root.path());
  std::fs::write(
    root.path().join("specs/broken.spec.ts"),
    "import { test } from '@ferridriver/test';\ntest('never closes', async ({ page }) => {\n",
  )
  .expect("write broken spec");

  let (_server, url) = start_ui(root.path());
  let mut ui = Ui::connect(&url).await;
  ui.call("initialize", json!({})).await;

  let listed = ui.call("listTests", json!({})).await;
  assert_eq!(
    listed["status"], "failed",
    "a tree that could not be built is not a pass"
  );
  let error = listed["report"]
    .as_array()
    .expect("report")
    .iter()
    .find(|event| event["method"] == "onError")
    .expect("an onError explaining the empty tree");
  let message = error["params"]["error"]["message"].as_str().unwrap_or_default();
  assert!(!message.is_empty(), "an error with no message explains nothing");
}

/// A `<testId>.trace` under any `.playwright-artifacts-*/traces`.
fn find_live_trace(output_dir: &std::path::Path) -> Option<std::path::PathBuf> {
  for entry in std::fs::read_dir(output_dir).ok()?.flatten() {
    let name = entry.file_name().to_string_lossy().into_owned();
    if !name.starts_with(".playwright-artifacts-") {
      continue;
    }
    let Ok(traces) = std::fs::read_dir(entry.path().join("traces")) else {
      continue;
    };
    for trace in traces.flatten() {
      let path = trace.path();
      if path.extension().is_some_and(|ext| ext == "trace") && std::fs::metadata(&path).is_ok_and(|meta| meta.len() > 0)
      {
        return Some(path);
      }
    }
  }
  None
}
