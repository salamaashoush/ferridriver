//! Browser calls as `pw:api` steps of the test that made them.
//!
//! Playwright's runner listens to every API call a test makes and opens a
//! step for it (`packages/playwright/src/index.ts`, `onApiCallBegin`), titled
//! from the protocol metadata (`packages/isomorphic/protocolMetainfo.ts`,
//! rendered by `protocolFormatter.ts`). A call whose metadata names a group
//! (`configuration`, `route`, `getter`) is still traced but never reported,
//! which is why `page.title()` does not show up as a step and `page.goto()`
//! does.
//!
//! ferridriver's calls arrive through core's action observer, named by the
//! client class and method (`Locator.click`), where Playwright's metadata is
//! keyed by the protocol call the client method makes (`Frame.click`).
//! [`metainfo`] is that mapping. An assertion is not reported from here: the
//! expect layer opens its own step for every matcher, the ones that never
//! touch a browser included.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferridriver::trace::{ActionInfo, ActionObserver};
use serde_json::Value;

use crate::model::{StepCategory, StepLocation, StepStatus, TestInfo};
use crate::step::{StepFrame, StepOptions, StepOutcome, StepSpec};

/// What Playwright's metadata says about one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Metainfo {
  title: &'static str,
  subtitle: Option<&'static str>,
  /// A grouped call is hidden from reporters.
  group: Option<&'static str>,
  /// Parameters reported on the step (`renderParams`), as `[key=]path`.
  params: &'static [&'static str],
}

const fn meta(title: &'static str) -> Metainfo {
  Metainfo {
    title,
    subtitle: None,
    group: None,
    params: &[],
  }
}

const fn on_selector(title: &'static str) -> Metainfo {
  Metainfo {
    subtitle: Some("{selector}"),
    ..meta(title)
  }
}

const fn grouped(title: &'static str, group: &'static str) -> Metainfo {
  Metainfo {
    group: Some(group),
    ..meta(title)
  }
}

const fn getter_on_selector(title: &'static str) -> Metainfo {
  Metainfo {
    group: Some("getter"),
    ..on_selector(title)
  }
}

const fn with_params(info: Metainfo, params: &'static [&'static str]) -> Metainfo {
  Metainfo { params, ..info }
}

/// The metadata of the protocol call a ferridriver client method makes.
fn metainfo(class: &str, method: &str) -> Option<Metainfo> {
  const CONFIGURATION: &str = "configuration";
  const GETTER: &str = "getter";
  const ROUTE: &str = "route";
  Some(match (class, method) {
    ("Page" | "Frame", "goto") => with_params(
      Metainfo {
        subtitle: Some("{url}"),
        ..meta("Navigate")
      },
      &["url"],
    ),
    ("Page", "reload") => meta("Reload"),
    ("Page", "goBack") => meta("Go back"),
    ("Page", "goForward") => meta("Go forward"),
    ("Page" | "Frame", "setContent") => meta("Set content"),
    ("Page" | "Frame", "content") => meta("Get content"),
    ("Page" | "Frame", "title") => grouped("Get page title", GETTER),
    ("Page" | "Frame", "evaluate" | "evaluateHandle") => meta("Evaluate"),
    ("Page" | "Frame", "$eval" | "$$eval") => on_selector("Evaluate"),
    ("Page" | "Frame", "$") => on_selector("Query selector"),
    ("Page" | "Frame", "$$") => on_selector("Query selector all"),
    ("Page" | "Frame", "waitForLoadState") => meta("Wait for load state"),
    ("Page" | "Frame", "waitForURL" | "waitForNavigation") => meta("Wait for navigation"),
    ("Page" | "Frame", "waitForTimeout") => with_params(meta("Wait for timeout"), &["timeout"]),
    ("Page" | "Frame", "ariaSnapshot") => grouped("Aria snapshot", GETTER),
    ("Page" | "Frame", "ariaSnapshotJSON") => grouped("Aria snapshot JSON", GETTER),
    ("Frame", "addScriptTag") => with_params(meta("Add script tag"), &["url"]),
    ("Frame", "addStyleTag") => with_params(meta("Add style tag"), &["url"]),
    ("Page", "screenshot") => meta("Screenshot"),
    ("Page", "pdf") => meta("PDF"),
    ("Page", "emulateMedia") => meta("Emulate media"),
    ("Page", "setViewportSize") => with_params(
      meta("Set viewport size"),
      &["viewportSize.width", "viewportSize.height"],
    ),
    ("Page", "bringToFront") => meta("Bring to front"),
    ("Page", "close") => meta("Close page"),
    ("Page" | "BrowserContext", "setExtraHTTPHeaders") => grouped("Set extra HTTP headers", CONFIGURATION),
    ("Page" | "BrowserContext", "addInitScript") => grouped("Add init script", CONFIGURATION),
    ("Page" | "BrowserContext", "route") => grouped("Route requests", ROUTE),
    ("Page" | "BrowserContext", "unroute") => grouped("Unroute requests", ROUTE),

    ("Locator", "click") => on_selector("Click"),
    ("Locator", "dblclick") => on_selector("Double click"),
    ("Locator", "hover") => on_selector("Hover"),
    ("Locator", "tap") => on_selector("Tap"),
    ("Locator", "check") => on_selector("Check"),
    ("Locator", "uncheck") => on_selector("Uncheck"),
    ("Locator", "fill") => with_params(on_selector("Fill \"{value}\""), &["value"]),
    ("Locator", "clear") => on_selector("Clear"),
    ("Locator", "type" | "pressSequentially") => with_params(on_selector("Type \"{text}\""), &["text"]),
    ("Locator", "press") => with_params(on_selector("Press \"{key}\""), &["key"]),
    ("Locator", "focus") => on_selector("Focus"),
    ("Locator", "blur") => on_selector("Blur"),
    ("Locator", "selectOption") => on_selector("Select option"),
    ("Locator", "setInputFiles") => on_selector("Set input files"),
    ("Locator", "dispatchEvent") => with_params(on_selector("Dispatch \"{type}\""), &["type"]),
    ("Locator", "dragTo") => meta("Drag and drop"),
    ("Locator", "scrollIntoViewIfNeeded") => on_selector("Scroll into view"),
    ("Locator", "selectText") => on_selector("Select text"),
    ("Locator", "screenshot") => on_selector("Screenshot"),
    ("Locator", "boundingBox") => on_selector("Bounding box"),
    ("Locator", "evaluate" | "evaluateHandle" | "evaluateAll") => on_selector("Evaluate"),
    ("Locator", "waitFor") => on_selector("Wait for selector"),
    ("Locator", "count") => on_selector("Query count"),
    ("Locator", "ariaSnapshot") => getter_on_selector("Aria snapshot"),
    ("Locator", "textContent") => getter_on_selector("Get text content"),
    ("Locator", "innerText") => getter_on_selector("Get inner text"),
    ("Locator", "innerHTML") => getter_on_selector("Get HTML"),
    ("Locator", "inputValue") => getter_on_selector("Get input value"),
    ("Locator", "getAttribute") => getter_on_selector("Get attribute \"{name}\""),
    ("Locator", "isChecked") => getter_on_selector("Is checked"),
    ("Locator", "isDisabled") => getter_on_selector("Is disabled"),
    ("Locator", "isEnabled") => getter_on_selector("Is enabled"),
    ("Locator", "isEditable") => getter_on_selector("Is editable"),
    ("Locator", "isHidden") => getter_on_selector("Is hidden"),
    ("Locator", "isVisible") => getter_on_selector("Is visible"),

    ("ElementHandle", "click") => meta("Click"),
    ("ElementHandle", "dblclick") => meta("Double click"),
    ("ElementHandle", "hover") => meta("Hover"),
    ("ElementHandle", "tap") => meta("Tap"),
    ("ElementHandle", "check") => meta("Check"),
    ("ElementHandle", "uncheck") => meta("Uncheck"),
    ("ElementHandle", "fill") => with_params(meta("Fill \"{value}\""), &["value"]),
    ("ElementHandle", "type") => with_params(meta("Type"), &["text"]),
    ("ElementHandle", "press") => meta("Press \"{key}\""),
    ("ElementHandle", "focus") => meta("Focus"),
    ("ElementHandle", "scrollIntoViewIfNeeded") => meta("Scroll into view"),
    ("ElementHandle", "selectOption") => meta("Select option"),
    ("ElementHandle", "setInputFiles") => meta("Set input files"),
    ("ElementHandle", "screenshot") => meta("Screenshot"),
    ("ElementHandle", "boundingBox") => meta("Get bounding box"),

    ("Keyboard", "down") => with_params(meta("Key down \"{key}\""), &["key"]),
    ("Keyboard", "up") => with_params(meta("Key up \"{key}\""), &["key"]),
    ("Keyboard", "press") => with_params(meta("Press \"{key}\""), &["key"]),
    ("Keyboard", "type") => with_params(meta("Type \"{text}\""), &["text"]),
    ("Keyboard", "insertText") => with_params(meta("Insert \"{text}\""), &["text"]),
    ("Mouse", "move") => with_params(meta("Mouse move"), &["x", "y"]),
    ("Mouse", "down") => meta("Mouse down"),
    ("Mouse", "up") => meta("Mouse up"),
    ("Mouse", "click") => with_params(meta("Click"), &["x", "y"]),
    ("Mouse", "dblclick") => with_params(meta("Double click"), &["x", "y"]),
    ("Mouse", "wheel") => with_params(meta("Mouse wheel"), &["deltaX", "deltaY"]),
    ("Touchscreen", "tap") => with_params(meta("Tap"), &["x", "y"]),

    ("BrowserContext", "newPage") => meta("Create page"),
    ("BrowserContext", "close") => meta("Close context"),
    ("BrowserContext", "cookies") => grouped("Get cookies", GETTER),
    ("BrowserContext", "addCookies") => grouped("Add cookies", CONFIGURATION),
    ("BrowserContext", "clearCookies") => grouped("Clear cookies", CONFIGURATION),
    ("BrowserContext", "grantPermissions") => grouped("Grant permissions", CONFIGURATION),
    ("BrowserContext", "clearPermissions") => grouped("Clear permissions", CONFIGURATION),
    ("BrowserContext", "setGeolocation") => grouped("Set geolocation", CONFIGURATION),
    ("BrowserContext", "setHTTPCredentials") => grouped("Set HTTP credentials", CONFIGURATION),
    ("BrowserContext", "storageState") => grouped("Get storage state", CONFIGURATION),
    ("BrowserContext", "setOffline") => with_params(meta("Set offline mode"), &["offline"]),

    ("WebMCP", "tools") => grouped("List WebMCP tools", GETTER),
    ("WebMCP", "callTool") => meta("Call WebMCP tool"),
    _ => return None,
  })
}

/// A browser call as a step: what reporters are told about it.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiStep {
  pub title: String,
  pub subtitle: Option<String>,
  pub params: Option<Value>,
}

/// The step a call is reported as, or `None` for a call Playwright keeps
/// out of the report (a grouped call, an assertion).
#[must_use]
pub fn api_step(class: &str, method: &str, params: &Value) -> Option<ApiStep> {
  if class == "Expect" {
    return None;
  }
  let Some(info) = metainfo(class, method) else {
    // `renderTitleForCall` falls back to the method name for a call its
    // metadata gives no title.
    return Some(ApiStep {
      title: method.to_string(),
      subtitle: None,
      params: rendered_params(params, &[]),
    });
  };
  if info.group.is_some() {
    return None;
  }
  Some(ApiStep {
    title: render_template(info.title, params).0,
    subtitle: info
      .subtitle
      .and_then(|template| match render_template(template, params) {
        (subtitle, true) => Some(truncate(&subtitle)),
        (_, false) => None,
      }),
    params: rendered_params(params, info.params),
  })
}

/// `renderTitleForCall`: every `{name}` (or `{a|b}`, first one present)
/// replaced by the parameter. An absent parameter leaves the placeholder
/// as written; the flag says whether every one resolved, which is what
/// decides whether a subtitle is shown at all.
fn render_template(template: &str, params: &Value) -> (String, bool) {
  let mut out = String::with_capacity(template.len());
  let mut resolved = true;
  let mut rest = template;
  while let Some(open) = rest.find('{') {
    let Some(close) = rest[open..].find('}').map(|at| open + at) else {
      break;
    };
    out.push_str(&rest[..open]);
    let alternatives = &rest[open + 1..close];
    if let Some(value) = alternatives.split('|').find_map(|name| format_param(params, name)) {
      out.push_str(&value.replace('\n', "\\n"));
    } else {
      resolved = false;
      out.push_str(&rest[open..=close]);
    }
    rest = &rest[close + 1..];
  }
  out.push_str(rest);
  (out, resolved)
}

/// `formatProtocolParam`: a URL reads as host, path and query (a `data:`
/// URL as its scheme), a selector as the locator it describes, anything
/// else as itself.
fn format_param(params: &Value, name: &str) -> Option<String> {
  let value = deep_param(params, name)?;
  if name == "url" {
    let text = scalar(value)?;
    return Some(match url::Url::parse(&text) {
      Ok(url) if url.scheme() == "data" => "data:".to_string(),
      Ok(url) if matches!(url.scheme(), "about" | "chrome" | "edge") => text,
      Ok(url) => {
        let mut shown = url.host_str().unwrap_or_default().to_string();
        if let Some(port) = url.port() {
          shown.push_str(&format!(":{port}"));
        }
        shown.push_str(url.path());
        if let Some(query) = url.query().filter(|q| !q.is_empty()) {
          shown.push('?');
          shown.push_str(query);
        }
        shown
      },
      Err(_) => text,
    });
  }
  let text = scalar(value)?;
  if name == "selector" || name.ends_with(".selector") {
    return Some(ferridriver::locator_generator::as_locator_description(&text));
  }
  Some(text)
}

fn deep_param<'a>(params: &'a Value, path: &str) -> Option<&'a Value> {
  path
    .split('.')
    .try_fold(params, |current, token| current.get(token))
    .filter(|v| !v.is_null())
}

/// JavaScript's `String(value)` for the values a title can show.
fn scalar(value: &Value) -> Option<String> {
  match value {
    Value::String(s) => Some(s.clone()),
    Value::Number(n) => Some(n.to_string()),
    Value::Bool(b) => Some(b.to_string()),
    _ => None,
  }
}

const MAX_PARAM_LENGTH: usize = 200;

/// `truncateParam`.
fn truncate(value: &str) -> String {
  match value.char_indices().nth(MAX_PARAM_LENGTH) {
    Some((cut, _)) => format!("{}…", &value[..cut]),
    None => value.to_string(),
  }
}

/// `renderParamsForCall`: the call's locator, then each listed parameter
/// that is present, long strings truncated.
fn rendered_params(params: &Value, listed: &[&str]) -> Option<Value> {
  let mut out = serde_json::Map::new();
  if let Some(selector) = params.get("selector").and_then(Value::as_str) {
    out.insert(
      "locator".to_string(),
      Value::String(truncate(&ferridriver::locator_generator::as_locator_description(
        selector,
      ))),
    );
  }
  for entry in listed {
    let (key, path) = match entry.split_once('=') {
      Some((key, path)) => (key, path),
      None => (entry.rsplit('.').next().unwrap_or(entry), *entry),
    };
    let Some(value) = deep_param(params, path) else {
      continue;
    };
    let value = match value {
      Value::String(s) => Value::String(truncate(s)),
      other => other.clone(),
    };
    out.insert(key.to_string(), value);
  }
  (!out.is_empty()).then_some(Value::Object(out))
}

/// Reports one test's browser calls as its steps, for as long as the
/// guard [`observe`] returns is held.
struct ApiSteps {
  test_info: Arc<TestInfo>,
  /// Locations are shown relative to where the run started, as a
  /// `test.step`'s are.
  cwd: PathBuf,
  /// Call id -> the step reporting it.
  open: Mutex<rustc_hash::FxHashMap<String, String>>,
}

impl ApiSteps {
  fn location(&self, action: &ActionInfo) -> Option<StepLocation> {
    let frame = action.location.as_ref()?;
    let path = std::path::Path::new(&frame.file);
    let file = path
      .strip_prefix(&self.cwd)
      .map_or_else(|_| frame.file.clone(), |relative| relative.display().to_string());
    Some(StepLocation {
      file,
      line: frame.line,
      column: frame.column,
    })
  }
}

impl ActionObserver for ApiSteps {
  fn action_begin(&self, action: &ActionInfo) {
    let Some(step) = api_step(&action.class, &action.method, &action.params) else {
      return;
    };
    let mut spec = StepSpec::new(step.title).with_options(StepOptions {
      subtitle: step.subtitle,
      params: step.params,
      ..StepOptions::default()
    });
    spec.category = StepCategory::PwApi;
    spec.frames = self.location(action).map(StepFrame::Source).into_iter().collect();
    let started = self.test_info.open_step_now(spec, false);
    self
      .open
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .insert(action.call_id.clone(), started.step_id);
  }

  fn action_end(&self, action: &ActionInfo, _elapsed: Duration, error: Option<&str>) {
    let step_id = self
      .open
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .remove(&action.call_id);
    let Some(step_id) = step_id else { return };
    self.test_info.close_step_now(
      &step_id,
      StepOutcome {
        status: if error.is_some() {
          StepStatus::Failed
        } else {
          StepStatus::Passed
        },
        error: error.map(ToString::to_string),
        annotations: Vec::new(),
      },
    );
  }

  fn action_log(&self, _action: &ActionInfo, _message: &str) {}
}

/// Report every browser call `test_info`'s test makes as a `pw:api` step,
/// on whichever context or browser it makes it, until the guard drops.
#[must_use]
pub fn observe(test_info: &Arc<TestInfo>) -> ferridriver::trace::OwnerObserverGuard {
  let observer = ApiSteps {
    test_info: Arc::clone(test_info),
    cwd: std::env::current_dir().unwrap_or_default(),
    open: Mutex::default(),
  };
  ferridriver::trace::observe_owner_actions(&test_info.call_owner(), Arc::new(observer))
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;

  fn step(class: &str, method: &str, params: Value) -> Option<ApiStep> {
    api_step(class, method, &params)
  }

  #[test]
  fn titles_follow_playwright_protocol_metainfo() {
    let goto = step("Page", "goto", json!({ "url": "https://example.com/a/b?q=1#frag" })).expect("reported");
    assert_eq!(goto.title, "Navigate");
    assert_eq!(goto.subtitle.as_deref(), Some("example.com/a/b?q=1"));
    assert_eq!(goto.params, Some(json!({ "url": "https://example.com/a/b?q=1#frag" })));

    let data = step("Page", "goto", json!({ "url": "data:text/html,<h1>hi</h1>" })).expect("reported");
    assert_eq!(data.subtitle.as_deref(), Some("data:"));

    let click = step("Locator", "click", json!({ "selector": "internal:role=heading" })).expect("reported");
    assert_eq!(click.title, "Click");
    assert_eq!(click.subtitle.as_deref(), Some("getByRole('heading')"));
    assert_eq!(click.params, Some(json!({ "locator": "getByRole('heading')" })));

    let fill = step("Locator", "fill", json!({ "selector": "#name", "value": "a\nb" })).expect("reported");
    assert_eq!(fill.title, "Fill \"a\\nb\"");
    assert_eq!(fill.subtitle.as_deref(), Some("locator('#name')"));

    let press = step("Keyboard", "press", json!({ "key": "Enter" })).expect("reported");
    assert_eq!(press.title, "Press \"Enter\"");
    assert_eq!(press.subtitle, None);
  }

  #[test]
  fn grouped_calls_and_assertions_are_not_reported() {
    assert_eq!(step("Page", "title", json!({})), None, "a getter is grouped");
    assert_eq!(step("Locator", "textContent", json!({ "selector": "b" })), None);
    assert_eq!(step("BrowserContext", "addCookies", json!({})), None, "configuration");
    assert_eq!(step("Page", "route", json!({})), None, "route");
    assert_eq!(
      step("Expect", "toHaveTitle", json!({})),
      None,
      "expect has its own step"
    );
  }

  #[test]
  fn a_call_without_metadata_is_titled_by_its_method() {
    let other = step("Page", "snapshotForAI", json!({})).expect("reported");
    assert_eq!(other.title, "snapshotForAI");
    assert_eq!(other.subtitle, None);
  }

  fn action(call_id: &str, class: &str, method: &str, params: Value) -> ActionInfo {
    ActionInfo {
      call_id: call_id.to_string(),
      class: class.to_string(),
      method: method.to_string(),
      title: format!("{}.{method}", class.to_lowercase()),
      params,
      location: Some(ferridriver::trace::StackFrame {
        file: "spec.ts".to_string(),
        line: 7,
        column: 3,
      }),
      script: None,
    }
  }

  #[test]
  fn calls_become_leaf_steps_of_the_step_they_ran_in() {
    let test_info = Arc::new(TestInfo::new_anonymous());
    let observer = ApiSteps {
      test_info: Arc::clone(&test_info),
      cwd: PathBuf::new(),
      open: Mutex::default(),
    };
    let outer = test_info.open_step_now(StepSpec::new("open the site"), true);
    let click = action("call@1", "Locator", "click", json!({ "selector": "#a" }));
    let hover = action("call@2", "Locator", "hover", json!({ "selector": "#b" }));
    let title = action("call@3", "Page", "title", json!({}));
    // Two calls in flight at once are siblings, not one inside the other.
    observer.action_begin(&click);
    observer.action_begin(&hover);
    observer.action_begin(&title);
    observer.action_end(&hover, Duration::ZERO, None);
    observer.action_end(&click, Duration::ZERO, Some("element is detached"));
    observer.action_end(&title, Duration::ZERO, None);
    test_info.close_step_now(
      &outer.step_id,
      StepOutcome {
        status: StepStatus::Passed,
        error: None,
        annotations: Vec::new(),
      },
    );

    let steps = crate::model::nest_steps(
      test_info
        .steps
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone(),
    );
    assert_eq!(steps.len(), 1, "a getter is not a step: {steps:?}");
    let children: Vec<(&str, StepStatus, Option<&str>)> = steps[0]
      .steps
      .iter()
      .map(|s| (s.title.as_str(), s.status, s.error.as_deref()))
      .collect();
    assert_eq!(
      children,
      vec![
        ("Click", StepStatus::Failed, Some("element is detached")),
        ("Hover", StepStatus::Passed, None),
      ]
    );
    assert!(steps[0].steps.iter().all(|s| s.category == StepCategory::PwApi));
    assert_eq!(
      steps[0].steps[0].location,
      Some(StepLocation {
        file: "spec.ts".to_string(),
        line: 7,
        column: 3
      })
    );
  }

  #[test]
  fn a_call_made_in_a_hook_belongs_to_the_hook() {
    let test_info = Arc::new(TestInfo::new_anonymous());
    let observer = ApiSteps {
      test_info: Arc::clone(&test_info),
      cwd: PathBuf::new(),
      open: Mutex::default(),
    };
    let hook = futures::executor::block_on(test_info.begin_step("beforeEach", StepCategory::Hook));
    let goto = action("call@1", "Page", "goto", json!({ "url": "https://example.com/" }));
    observer.action_begin(&goto);
    observer.action_end(&goto, Duration::ZERO, None);
    futures::executor::block_on(hook.end(None));
    let after = action("call@2", "Page", "reload", json!({}));
    observer.action_begin(&after);
    observer.action_end(&after, Duration::ZERO, None);

    let steps = crate::model::nest_steps(
      test_info
        .steps
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone(),
    );
    let tree: Vec<(&str, Vec<&str>)> = steps
      .iter()
      .map(|s| (s.title.as_str(), s.steps.iter().map(|c| c.title.as_str()).collect()))
      .collect();
    assert_eq!(tree, vec![("beforeEach", vec!["Navigate"]), ("Reload", vec![])]);
  }

  #[test]
  fn a_call_that_never_ends_is_closed_with_the_test() {
    let test_info = Arc::new(TestInfo::new_anonymous());
    let observer = ApiSteps {
      test_info: Arc::clone(&test_info),
      cwd: PathBuf::new(),
      open: Mutex::default(),
    };
    observer.action_begin(&action(
      "call@1",
      "Page",
      "goto",
      json!({ "url": "https://example.com/" }),
    ));
    test_info.close_open_steps("step never completed (test aborted)");
    let steps = test_info
      .steps
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].title, "Navigate");
    assert_eq!(steps[0].status, StepStatus::Failed);
  }

  #[test]
  fn an_unresolved_subtitle_is_dropped_and_long_values_truncated() {
    let click = step("Locator", "click", json!({})).expect("reported");
    assert_eq!(click.subtitle, None);
    let long = "x".repeat(250);
    let fill = step("Locator", "fill", json!({ "selector": "#a", "value": long })).expect("reported");
    let value = fill.params.and_then(|p| p["value"].as_str().map(ToString::to_string));
    assert_eq!(value.map(|v| v.chars().count()), Some(201));
  }
}
