//! Core automation primitives -- shared by MCP tools, BDD steps, and library consumers.
//!
//! All functions take `&AnyPage` or `&AnyElement` and return [`crate::error::Result<T>`].
//! No MCP types, no server state -- pure browser automation logic.
//!
//! All JS operations go through the unified `window.__fd` runtime
//! (injected automatically via addScriptToEvaluateOnNewDocument on CDP,
//! or after navigation on `WebKit`).

use crate::backend::{AnyElement, AnyPage};
use crate::error::{FerriError, Result};
use crate::selectors;
mod inspect;
mod pointer;

pub use inspect::{
  DropdownOption, FindElementsOptions, FindResult, FoundElement, ScrollInfo, SearchMatch, SearchOptions, SearchResult,
  SelectResult, console_error_count, extract_markdown, find_elements, format_find_results, format_search_results,
  get_dropdown_options, navigate_with_health_check, scroll_info, search_page, select_option,
};
pub(crate) use pointer::{DragStep, drag_step, hold_modifiers, restore_modifiers};
pub use pointer::{InstallOutcome, click_with_opts, hover_with_opts, resolve_click_point, tap_with_opts};

/// What a locator's retry loop tells the pointer action it runs.
#[derive(Clone)]
struct PointerContext {
  /// How many times the loop has already tried the action. Playwright's
  /// `_retryPointerAction` picks each try's scroll alignment from this
  /// count, and the count spans the tries a covering element sent back
  /// to the loop, which is how a target under a `position: sticky`
  /// header is finally revealed.
  retry: usize,
  /// The frame the element lives in, when it is not the main frame.
  frame: Option<crate::frame::Frame>,
}

tokio::task_local! {
  static POINTER: PointerContext;
}

/// Run `action` as try number `retry` of its locator's retry loop, on an
/// element of `frame`.
pub(crate) async fn with_pointer_context<F: std::future::Future>(
  retry: usize,
  frame: Option<crate::frame::Frame>,
  action: F,
) -> F::Output {
  POINTER.scope(PointerContext { retry, frame }, action).await
}

pub(super) fn pointer_retry() -> usize {
  POINTER.try_with(|context| context.retry).unwrap_or(0)
}

/// How far the viewport of the action's frame sits from the top page's,
/// beyond what the injected script's `frameElement` walk adds to a click
/// point: that walk stops at the first cross-origin frame, whose
/// `frameElement` it cannot read. Each such boundary adds its `<iframe>`'s
/// content box, measured in the parent frame, as Playwright's
/// `crPage._framePosition` does. Same-origin boundaries add nothing here,
/// the walk having counted them already.
pub(crate) async fn cross_origin_frame_offset() -> Result<(f64, f64)> {
  /// Run on the owner `<iframe>`: `null` across a same-origin boundary,
  /// otherwise its content box's position in the top-most document its
  /// own same-origin ancestors reach.
  const OWNER_POSITION: &str = "function() { \
    if (this.contentDocument) return null; \
    var r = this.getBoundingClientRect(); var s = getComputedStyle(this); \
    var x = r.x + parseFloat(s.borderLeftWidth) + parseFloat(s.paddingLeft); \
    var y = r.y + parseFloat(s.borderTopWidth) + parseFloat(s.paddingTop); \
    var win = this.ownerDocument.defaultView; \
    while (win && win !== win.parent && win.frameElement) { \
      var fr = win.frameElement.getBoundingClientRect(); x += fr.x; y += fr.y; win = win.parent; \
    } \
    return { x: x, y: y }; }";
  let Some(mut frame) = POINTER.try_with(|context| context.frame.clone()).ok().flatten() else {
    return Ok((0.0, 0.0));
  };
  let (mut dx, mut dy) = (0.0, 0.0);
  while let Some(parent) = frame.parent_frame() {
    let owner = frame.frame_element().await?;
    if let Some(position) = owner.any_element().call_js_fn_value(OWNER_POSITION).await?
      && let (Some(x), Some(y)) = (position["x"].as_f64(), position["y"].as_f64())
    {
      dx += x;
      dy += y;
    }
    frame = parent;
  }
  Ok((dx, dy))
}

/// The scroll alignment of a pointer action's `retry`-th try, in
/// Playwright's order: the precise reveal first (`None`), then the
/// element's end, center and start.
pub(crate) fn scroll_alignment(retry: usize) -> Option<&'static str> {
  match retry % 4 {
    0 => None,
    1 => Some("{block:'end',inline:'end'}"),
    2 => Some("{block:'center',inline:'center'}"),
    _ => Some("{block:'start',inline:'start'}"),
  }
}

pub(super) async fn ensure_mcp_support(page: &AnyPage) -> Result<String> {
  let fd = page.injected_script().await?;
  page.evaluate(selectors::MCP_SUPPORT_JS).await?;
  Ok(fd)
}

#[derive(Debug)]
pub enum ClickGuardError {
  IsSelect,
  IsFileInput,
}

impl std::fmt::Display for ClickGuardError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::IsSelect => write!(
        f,
        "Cannot click <select> directly. Use select_option or get_dropdown_options instead."
      ),
      Self::IsFileInput => write!(
        f,
        "Cannot click file input directly. Use evaluate() to set files programmatically."
      ),
    }
  }
}

// ─── Runtime helper ─────────────────────────────────────────────────────────

/// Ensure the unified runtime is injected, then evaluate JS.
pub(super) async fn rt_eval(page: &AnyPage, js: &str) -> Result<Option<serde_json::Value>> {
  page.evaluate(js).await
}

/// Ensure runtime + evaluate, return string result.
pub(super) async fn rt_eval_str(page: &AnyPage, js: &str) -> Result<String> {
  let val = rt_eval(page, js).await?;
  Ok(
    val
      .and_then(|v| v.as_str().map(std::string::ToString::to_string))
      .unwrap_or_default(),
  )
}

// ─── Element Resolution ─────────────────────────────────────────────────────

/// Resolve an element by ref (from snapshot) or selector.
///
/// ALL selectors go through the unified selector engine -- no dual path.
/// Plain CSS like `#id` or `.class` works (default engine is CSS).
/// Rich selectors like `role=button[name="Save"]` also work.
///
/// # Errors
///
/// Returns an error if the ref is unknown, the selector is missing, or no element is found.
#[expect(
  clippy::implicit_hasher,
  reason = "the hasher-generic form needs std::collections::HashMap, which disallowed-types bans; FxHashMap is the only map callers hold"
)]
pub async fn resolve_element(
  page: &AnyPage,
  ref_map: &rustc_hash::FxHashMap<String, i64>,
  r#ref: Option<&str>,
  selector: Option<&str>,
) -> Result<AnyElement> {
  if let Some(r) = r#ref {
    let backend_id = ref_map
      .get(r)
      .ok_or_else(|| FerriError::invalid_argument("ref", format!("unknown ref '{r}'. Take a new snapshot.")))?;
    return page.resolve_backend_node(*backend_id, r).await;
  }

  let sel = selector
    .ok_or_else(|| FerriError::invalid_argument("ref-or-selector", "provide 'ref' (from snapshot) or 'selector'"))?;

  // ALL selectors go through the engine (treats bare CSS as default).
  // `actions::resolve_element` is the page-level escape hatch used by
  // MCP's snapshot-ref bridge — it always queries the main frame.
  selectors::query_one(page, sel, false, None).await
}

/// Suggest available selectors on the page.
pub async fn suggest_selectors(page: &AnyPage) -> Vec<String> {
  let Ok(fd) = ensure_mcp_support(page).await else {
    return Vec::new();
  };
  let json_str = rt_eval_str(page, &format!("{fd}.suggestSelectors()"))
    .await
    .unwrap_or_default();
  if let Ok(data) = serde_json::from_str::<serde_json::Value>(&json_str) {
    let mut suggestions = Vec::new();
    if let Some(ids) = data["ids"].as_array() {
      for id in ids.iter().filter_map(|v| v.as_str()) {
        suggestions.push(id.to_string());
      }
    }
    if let Some(inputs) = data["inputs"].as_array() {
      for input in inputs.iter().filter_map(|v| v.as_str()) {
        suggestions.push(input.to_string());
      }
    }
    suggestions
  } else {
    Vec::new()
  }
}

// ─── Click Guard ────────────────────────────────────────────────────────────

/// Check element tag/type before clicking.
///
/// # Errors
///
/// Returns `ClickGuardError::IsSelect` or `ClickGuardError::IsFileInput` if the element
/// should not be clicked directly.
pub async fn check_click_guard(element: &AnyElement, page: &AnyPage) -> std::result::Result<(), ClickGuardError> {
  let _ = page.ensure_engine_injected().await;
  let fd = "window.__fd";
  let guard = element
    .call_js_fn_value(&format!("function() {{ return {fd} ? {fd}.clickGuard(this) : ''; }}"))
    .await
    .ok()
    .flatten()
    .and_then(|v| v.as_str().map(std::string::ToString::to_string))
    .unwrap_or_default();

  match guard.as_str() {
    "select" => Err(ClickGuardError::IsSelect),
    "file" => Err(ClickGuardError::IsFileInput),
    _ => Ok(()),
  }
}

// ─── Click Pre-flight (combined) ────────────────────────────────────────────

/// Outcome of [`click_prep`] — the merged `isActionable` +
/// `scrollIntoView` + `clickPoint` pre-flight in one
/// `Runtime.callFunctionOn`. Mirrors Playwright's `evaluateInUtility`
/// pattern in `/tmp/playwright/packages/playwright-core/src/server/dom.ts`.
#[derive(Debug)]
pub enum ClickPrep {
  /// Element is actionable; cursor target is `(x, y)`. `armed` is the
  /// outcome of arming the hit-target interceptor in the same call, when
  /// one was asked for: `None` when none was, or when the page could not
  /// arm it (the action then dispatches uninstrumented).
  Ready {
    x: f64,
    y: f64,
    armed: Option<InstallOutcome>,
  },
  /// Element exists but is not currently actionable. Carries the
  /// reason marker (`notvisible` / `notconnected` / `disabled`),
  /// formatted as a Playwright-style `error:not<state>` string so
  /// the retry loop treats it as retriable.
  NotActionable { reason: String },
}

/// Combined pre-click pre-flight. ONE `Runtime.callFunctionOn`
/// returning `{actionable, reason, point}`. Replaces 3 separate calls
/// (`isActionable`, `scrollIntoView`, `getBoundingClientRect`). Caller
/// branches on the [`ClickPrep`] variant. The injected helper
/// (`__fd.clickPrep`) lives in
/// `crates/ferridriver/src/injected/index.ts::clickPrep`.
///
/// # Errors
///
/// Returns an error only on protocol-level failure (the engine is
/// not injected, the element handle is no longer connected, JSON
/// parse failure on the response). Element-state failures
/// (not-visible, disabled) are encoded as
/// [`ClickPrep`] variants, not errors.
pub async fn click_prep(
  element: &AnyElement,
  page: &AnyPage,
  position: Option<crate::options::Point>,
  scroll_align: Option<&str>,
  hit_action: Option<&str>,
) -> Result<ClickPrep> {
  click_prep_with_states(
    element,
    page,
    position,
    &["visible", "enabled"],
    scroll_align,
    hit_action,
  )
  .await
}

/// Like [`click_prep`], parameterized by the non-stable states to gate on
/// (e.g. `["visible"]` for `hover` and `drag`, `["visible", "enabled"]` for
/// `click` and `tap`). Collapses actionability + scroll + point + hit-target
/// interceptor arming into one round trip across all pointer actions.
///
/// # Errors
///
/// Returns an error if the JS bridge call fails or returns an unparseable payload.
pub async fn click_prep_with_states(
  element: &AnyElement,
  page: &AnyPage,
  position: Option<crate::options::Point>,
  base_states: &[&str],
  scroll_align: Option<&str>,
  hit_action: Option<&str>,
) -> Result<ClickPrep> {
  let _ = page.ensure_engine_injected().await;
  let position_lit = match position {
    Some(p) => format!("{{x:{},y:{}}}", p.x, p.y),
    None => "null".to_string(),
  };
  let scroll_lit = scroll_align.unwrap_or("null");
  let stable = stable_check().state();
  let mut states_js = String::from("[");
  for s in base_states {
    states_js.push('\'');
    states_js.push_str(s);
    states_js.push_str("',");
  }
  states_js.push('\'');
  states_js.push_str(stable);
  states_js.push_str("']");
  let hit_lit = hit_action.map_or_else(|| "null".to_string(), |action| format!("'{action}'"));
  let js = format!(
    "async function() {{ return JSON.stringify(await window.__fd.clickPrep(this, {position_lit}, {states_js}, {scroll_lit}, {hit_lit})); }}"
  );
  let raw = element
    .call_js_fn_value(&js)
    .await?
    .and_then(|v| v.as_str().map(std::string::ToString::to_string))
    .unwrap_or_default();
  if raw.is_empty() {
    return Err(FerriError::backend("click_prep: empty response from page-side helper"));
  }
  let parsed: serde_json::Value =
    serde_json::from_str(&raw).map_err(|e| FerriError::Backend(format!("click_prep: parse: {e}")))?;
  let actionable = parsed
    .get("actionable")
    .and_then(serde_json::Value::as_bool)
    .unwrap_or(false);
  if !actionable {
    let reason = parsed
      .get("reason")
      .and_then(|v| v.as_str())
      .unwrap_or("notconnected")
      .to_string();
    return Ok(ClickPrep::NotActionable {
      reason: format!("error:not{reason}"),
    });
  }
  let point = parsed
    .get("point")
    .ok_or_else(|| FerriError::evaluation("click_prep: missing point"))?;
  let x = point
    .get("x")
    .and_then(serde_json::Value::as_f64)
    .ok_or_else(|| FerriError::evaluation("click_prep: bad x"))?;
  let y = point
    .get("y")
    .and_then(serde_json::Value::as_f64)
    .ok_or_else(|| FerriError::evaluation("click_prep: bad y"))?;
  let armed = match parsed.get("hit").and_then(serde_json::Value::as_str) {
    None | Some("error:notconnected") => None,
    Some("ok") => Some(InstallOutcome::Armed),
    Some(description) => Some(InstallOutcome::Miss(description.to_string())),
  };
  Ok(ClickPrep::Ready { x, y, armed })
}

/// How long an action watches an element's box before trusting that it
/// has stopped moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StableCheck {
  /// Playwright's check: the box sampled in one animation frame equals
  /// the box in the next. About 33ms at 60Hz for an element at rest.
  #[default]
  Playwright,
  /// The box sampled when the action starts equals the box in the next
  /// animation frame, watched for at least 8ms: about 10ms on average and
  /// 17ms at most at 60Hz. A document with an animation in flight gets
  /// the Playwright check. An element moved from a timer slower than the
  /// watch can pass here where Playwright would have caught it.
  Frame,
}

impl StableCheck {
  /// The state name the injected `awaitStates` runs this check under.
  pub(crate) fn state(self) -> &'static str {
    match self {
      Self::Playwright => "stable",
      Self::Frame => "stable-frame",
    }
  }
}

static STABLE_CHECK_FRAME: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Choose the stability check every later pointer action in this process
/// runs. Process-wide, as the selector registry is: the check is a
/// property of how the suite is run, not of a page.
pub fn set_stable_check(check: StableCheck) {
  STABLE_CHECK_FRAME.store(check == StableCheck::Frame, std::sync::atomic::Ordering::Relaxed);
}

/// The stability check pointer actions currently run.
#[must_use]
pub fn stable_check() -> StableCheck {
  if STABLE_CHECK_FRAME.load(std::sync::atomic::Ordering::Relaxed) {
    StableCheck::Frame
  } else {
    StableCheck::Playwright
  }
}

// ─── Fill ───────────────────────────────────────────────────────────────────

/// Fill an input, a textarea or a contenteditable, or the control a label
/// names.
///
/// Unless `force` is set, the element must first be visible, enabled and
/// editable; an `error:not<state>` answer goes back to the retry loop,
/// which keeps polling. Then the injected `fill` either sets the value of
/// an input that takes no typing (a date, a colour, a range) and fires
/// `input` and `change`, or focuses the element and selects its content.
/// What it selected is then replaced the way typing replaces it: the text
/// goes in through the backend's input domain, or a Delete empties it, so
/// the page gets trusted `beforeinput` and `input` events and a framework
/// that tracks the value (a React-controlled input) sees the edit.
///
/// # Errors
///
/// Returns `error:not<state>` when the pre-check fails (retriable), the
/// injected script's own error for an element that cannot be filled (an
/// input of a type that takes no text, a value a date input rejects), or
/// what inserting the text failed with.
pub async fn fill(element: &AnyElement, page: &AnyPage, value: &str, force: bool) -> Result<()> {
  // A JSON string is a JavaScript string literal with every line break,
  // quote and control character escaped.
  let literal = serde_json::to_string(value)?;
  let check = if force {
    String::new()
  } else {
    "var state = fd.checkElementStates(this, ['visible', 'enabled', 'editable']); \
     if (state !== 'done') return JSON.stringify({ result: state }); "
      .to_string()
  };
  // In the utility world, as the engine runs there in Playwright: a value
  // the fill sets (a date, a colour) goes through the element's own
  // setter, so a framework tracking the value it set itself sees a change.
  let fill = format!(
    "function() {{ var fd = window.__fd; \
       if (!(fd instanceof Object)) return JSON.stringify({{ result: 'inject' }}); \
       {check}\
       try {{ return JSON.stringify({{ result: fd.fill(this, {literal}) }}); }} \
       catch (e) {{ return JSON.stringify({{ error: String(e && e.message || e) }}); }} }}"
  );
  let call = || async {
    Ok::<_, FerriError>(
      page
        .call_on_element_in_utility(element, &fill)
        .await?
        .and_then(|v| v.as_str().map(std::string::ToString::to_string))
        .unwrap_or_default(),
    )
  };
  let mut raw = call().await?;
  if raw.contains("\"inject\"") {
    let inject = format!(
      "async function() {{ await {}; return true; }}",
      crate::selectors::build_lazy_inject_js()
    );
    page.call_on_element_in_utility(element, &inject).await?;
    raw = call().await?;
  }
  let answer: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
  if let Some(error) = answer.get("error").and_then(serde_json::Value::as_str) {
    return Err(FerriError::backend(error));
  }
  match answer.get("result").and_then(serde_json::Value::as_str) {
    Some("done") => Ok(()),
    Some("needsinput") if value.is_empty() => page.press_key("Delete", 0).await,
    Some("needsinput") => page.insert_text(value).await,
    // `error:notvisible`, `error:notenabled`, `error:noteditable` or
    // `error:notconnected`: `retry_resolve!` polls on `error:not*`.
    Some(state) => Err(FerriError::backend(state)),
    None => Err(FerriError::backend("error:notconnected")),
  }
}

/// Select options on a `<select>` element. Each
/// [`crate::options::SelectOptionValue`] descriptor matches options by
/// `value`, `label`, or `index` per Playwright's
/// `injected/selectOptions` (see
/// `/tmp/playwright/packages/injected/src/injectedScript.ts`); arrays
/// select every matching option for multi-selects.
///
/// Returns the final list of selected `option.value`s, matching
/// Playwright's `selectOption` return shape.
///
/// # Errors
///
/// Returns an error if the element is not a `<select>`, or no option
/// matches any of the provided descriptors.
pub async fn select_options(
  element: &AnyElement,
  page: &AnyPage,
  values: &[crate::options::SelectOptionValue],
) -> Result<Vec<String>> {
  let fd = page.injected_script().await?;
  let values_json =
    serde_json::to_string(values).map_err(|e| FerriError::Backend(format!("select_option serialize: {e}")))?;
  // The injected `selectOptions` wrapper takes `(el, ...descriptors)`
  // via rest args — spread the descriptor array into positional args.
  let js = format!(
    "function() {{ \
      var descriptors = {values_json}; \
      var result = {fd}.selectOptions.apply(null, [this].concat(descriptors)); \
      if (typeof result === 'string') return JSON.stringify({{ error: result }}); \
      return JSON.stringify({{ selected: result }}); \
    }}"
  );
  let raw = element
    .call_js_fn_value(&js)
    .await?
    .and_then(|v| v.as_str().map(std::string::ToString::to_string))
    .unwrap_or_default();
  let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({}));
  if let Some(err) = parsed.get("error").and_then(|v| v.as_str()) {
    return Err(FerriError::Backend(err.to_string()));
  }
  Ok(
    parsed
      .get("selected")
      .and_then(|v| v.as_array())
      .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
      .unwrap_or_default(),
  )
}

const ACTIONABLE_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// A retry's pause that ends early when the page's main document changes
/// (`window.__fd.waitForChange`), so an element that becomes actionable
/// 5ms into a 50ms pause is acted on then. Never longer than `pause`: a
/// document that cannot be watched (navigating, no engine, the element's
/// own frame is a child) is slept on for all of it.
pub(super) async fn pause_for_change(page: &AnyPage, pause: std::time::Duration) {
  if pause.is_zero() {
    return;
  }
  let started = tokio::time::Instant::now();
  let call = format!(
    "window.__fd instanceof Object ? window.__fd.waitForChange({}) : null",
    pause.as_millis()
  );
  let floor = match tokio::time::timeout(pause, page.evaluate(&call)).await {
    Ok(Ok(Some(serde_json::Value::Bool(_)))) => crate::expect::CHANGE_FLOOR.min(pause),
    _ => pause,
  };
  tokio::time::sleep(floor.saturating_sub(started.elapsed())).await;
}

/// Wait for an element to be actionable (visible, enabled, stable).
/// Polls from the Rust side with short sleeps — no blocking JS promises
/// — so parallel pages can make progress concurrently.
///
/// # Errors
///
/// Returns an error if the element is not actionable within the timeout.
pub async fn wait_for_actionable(element: &AnyElement, page: &AnyPage) -> Result<()> {
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
  let _ = page.ensure_engine_injected().await;
  let fd = "window.__fd";

  loop {
    if tokio::time::Instant::now() >= deadline {
      return Err(FerriError::timeout("element not actionable", 5_000));
    }

    let val = element
      .call_js_fn_value(&format!(
        "function() {{ \
            return JSON.stringify({fd}.isActionable(this)); \
        }}"
      ))
      .await
      .ok()
      .flatten()
      .and_then(|v| v.as_str().map(std::string::ToString::to_string))
      .unwrap_or_default();

    if let Ok(result) = serde_json::from_str::<serde_json::Value>(&val)
      && result["actionable"].as_bool() == Some(true)
    {
      return Ok(());
    }

    pause_for_change(page, ACTIONABLE_POLL).await;
  }
}

/// Wait for an element to satisfy a specific Playwright actionability state
/// list (e.g. `["visible", "enabled", "stable"]` for click/tap, or
/// `["visible", "stable"]` for hover). Delegates to the page-side
/// `awaitStates` helper, which runs the vendored `checkElementStates` —
/// including the async rAF `stable` (bounding-box-steady) sampling — in a
/// single round-trip per poll. Retries until the states pass or the 5s
/// actionability deadline elapses.
///
/// # Errors
///
/// Returns a timeout error if the states are not all satisfied in time.
pub async fn wait_for_states(element: &AnyElement, page: &AnyPage, states: &[&str]) -> Result<()> {
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
  let _ = page.ensure_engine_injected().await;
  let stable = stable_check().state();
  let states_lit = states
    .iter()
    .map(|s| format!("'{}'", if *s == "stable" { stable } else { s }))
    .collect::<Vec<_>>()
    .join(",");
  // `awaitStates` resolves to `undefined` when every state passes (so
  // `?? 'ok'` yields `"ok"`), or the failing state name otherwise.
  let js = format!(
    "async function() {{ return JSON.stringify((await window.__fd.awaitStates(this, [{states_lit}])) ?? 'ok'); }}"
  );

  loop {
    if tokio::time::Instant::now() >= deadline {
      return Err(FerriError::timeout("element not actionable", 5_000));
    }

    let val = element
      .call_js_fn_value(&js)
      .await
      .ok()
      .flatten()
      .and_then(|v| v.as_str().map(std::string::ToString::to_string))
      .unwrap_or_default();

    if val == "\"ok\"" {
      return Ok(());
    }

    pause_for_change(page, ACTIONABLE_POLL).await;
  }
}
