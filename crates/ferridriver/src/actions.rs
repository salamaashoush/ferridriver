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
use rustc_hash::FxHashMap;

// ─── Types ──────────────────────────────────────────────────────────────────

pub struct SearchOptions {
  pub pattern: String,
  pub regex: bool,
  pub case_sensitive: bool,
  pub context_chars: usize,
  pub css_scope: Option<String>,
  pub max_results: usize,
}

impl Default for SearchOptions {
  fn default() -> Self {
    Self {
      pattern: String::new(),
      regex: false,
      case_sensitive: false,
      context_chars: 150,
      css_scope: None,
      max_results: 25,
    }
  }
}

#[derive(Debug, Clone)]
pub struct SearchMatch {
  pub match_text: String,
  pub context: String,
  pub element_path: String,
  pub char_position: usize,
}

#[derive(Debug)]
pub struct SearchResult {
  pub matches: Vec<SearchMatch>,
  pub total: usize,
  pub has_more: bool,
}

pub struct FindElementsOptions {
  pub selector: String,
  pub attributes: Option<Vec<String>>,
  pub max_results: usize,
  pub include_text: bool,
}

impl Default for FindElementsOptions {
  fn default() -> Self {
    Self {
      selector: String::new(),
      attributes: None,
      max_results: 50,
      include_text: true,
    }
  }
}

#[derive(Debug, Clone)]
pub struct FoundElement {
  pub index: usize,
  pub tag: String,
  pub text: Option<String>,
  pub attrs: FxHashMap<String, String>,
  pub children_count: usize,
}

#[derive(Debug)]
pub struct FindResult {
  pub elements: Vec<FoundElement>,
  pub total: usize,
}

#[derive(Debug, Clone)]
pub struct SelectResult {
  pub selected_text: String,
  pub selected_value: String,
}

#[derive(Debug, Clone)]
pub struct DropdownOption {
  pub index: usize,
  pub text: String,
  pub value: String,
  pub selected: bool,
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

#[derive(Debug, Clone)]
pub struct ScrollInfo {
  pub scroll_y: i64,
  pub scroll_height: i64,
  pub viewport_height: i64,
}

// ─── Runtime helper ─────────────────────────────────────────────────────────

/// Ensure the unified runtime is injected, then evaluate JS.
async fn rt_eval(page: &AnyPage, js: &str) -> Result<Option<serde_json::Value>> {
  page.evaluate(js).await
}

/// Ensure runtime + evaluate, return string result.
async fn rt_eval_str(page: &AnyPage, js: &str) -> Result<String> {
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
pub async fn resolve_element<S: std::hash::BuildHasher>(
  page: &AnyPage,
  ref_map: &std::collections::HashMap<String, i64, S>,
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
  let Ok(fd) = page.injected_script().await else {
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

/// Outcome of [`click_prep`] — the merged
/// `clickGuard` + `isActionable` + `scrollIntoView` + `clickPoint`
/// pre-flight that replaces 4 sequential CDP RTTs with one
/// `Runtime.callFunctionOn`. Mirrors Playwright's
/// `evaluateInUtility` pattern in
/// `/tmp/playwright/packages/playwright-core/src/server/dom.ts`.
#[derive(Debug)]
pub enum ClickPrep {
  /// Element is actionable; cursor target is `(x, y)`.
  Ready { x: f64, y: f64 },
  /// Element is `<select>` — caller should redispatch via select-option helper.
  IsSelect,
  /// Element is `<input type=file>` — caller should redispatch via file-chooser path.
  IsFileInput,
  /// Element exists but is not currently actionable. Carries the
  /// reason marker (`notvisible` / `notconnected` / `disabled`),
  /// formatted as a Playwright-style `error:not<state>` string so
  /// the retry loop treats it as retriable.
  NotActionable { reason: String },
}

/// Combined pre-click pre-flight. ONE `Runtime.callFunctionOn`
/// returning `{guard, actionable, reason, point}`. Replaces 4
/// separate calls (`clickGuard`, `isActionable`, `scrollIntoView`,
/// `getBoundingClientRect`) — saves 3 CDP RTTs per click. Caller
/// branches on the [`ClickPrep`] variant. The injected helper
/// (`__fd.clickPrep`) lives in
/// `crates/ferridriver/src/injected/index.ts::clickPrep`.
///
/// # Errors
///
/// Returns an error only on protocol-level failure (the engine is
/// not injected, the element handle is no longer connected, JSON
/// parse failure on the response). Element-state failures
/// (not-visible, disabled, file/select) are encoded as
/// [`ClickPrep`] variants, not errors.
pub async fn click_prep(
  element: &AnyElement,
  page: &AnyPage,
  position: Option<crate::options::Point>,
) -> Result<ClickPrep> {
  let _ = page.ensure_engine_injected().await;
  let position_lit = match position {
    Some(p) => format!("{{x:{},y:{}}}", p.x, p.y),
    None => "null".to_string(),
  };
  let js = format!("function() {{ return JSON.stringify(window.__fd.clickPrep(this, {position_lit})); }}");
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
  let guard = parsed.get("guard").and_then(|v| v.as_str()).unwrap_or("");
  match guard {
    "select" => return Ok(ClickPrep::IsSelect),
    "file" => return Ok(ClickPrep::IsFileInput),
    _ => {},
  }
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
  let point = parsed.get("point").ok_or("click_prep: missing point")?;
  let x = point
    .get("x")
    .and_then(serde_json::Value::as_f64)
    .ok_or("click_prep: bad x")?;
  let y = point
    .get("y")
    .and_then(serde_json::Value::as_f64)
    .ok_or("click_prep: bad y")?;
  Ok(ClickPrep::Ready { x, y })
}

// ─── Fill ───────────────────────────────────────────────────────────────────

/// Fill an input element: when `force` is `false`, run Playwright's
/// `['visible', 'enabled', 'editable']` pre-check via the injected
/// `checkElementStates` helper and propagate the `error:not<state>`
/// signal back to the retry loop so callers keep polling. With
/// `force = true` both checks are skipped and the value is set
/// immediately, matching Playwright's `_fill(force)` short-circuit in
/// `/tmp/playwright/packages/playwright-core/src/server/dom.ts:615`.
///
/// Handles both regular inputs (`.value`) and contenteditable elements
/// (`.textContent`) with the accompanying `input` / `change` events.
///
/// # Errors
///
/// Returns `error:not<state>` when the non-force pre-check fails (the
/// retry loop treats it as retriable), or a generic fill error when
/// the JS `.value` / `.textContent` assignment fails.
pub async fn fill(element: &AnyElement, page: &AnyPage, value: &str, force: bool) -> Result<()> {
  if !force {
    let fd = page.injected_script().await?;
    let state_raw = element
      .call_js_fn_value(&format!(
        "function() {{ return {fd}.checkElementStates(this, ['visible', 'enabled', 'editable']); }}"
      ))
      .await?
      .and_then(|v| v.as_str().map(std::string::ToString::to_string))
      .unwrap_or_else(|| "error:notconnected".to_string());
    if state_raw != "done" {
      // Playwright-style retriable marker: `error:notvisible`,
      // `error:notenabled`, `error:noteditable`, or `error:notconnected`.
      // `retry_resolve!` treats `error:not*` as retry-until-deadline.
      return Err(FerriError::backend(state_raw));
    }
  }
  let escaped = value.replace('\\', "\\\\").replace('\'', "\\'");
  // Use the prototype's native `value` setter — React (and other
  // controlled-input frameworks) wrap the descriptor on the element
  // instance to track changes. Setting `this.value = '…'` directly
  // bypasses React's tracker and the controlled component never sees
  // the new value, so onChange never fires and component state stays
  // stale (the input visually shows the new text but useState() keeps
  // the old). Calling the prototype setter via `setter.call(el, v)`
  // restores the change-detection signal — this is the same trick
  // Playwright uses (`packages/injected/src/injectedScript.ts::fill`
  // → keyboard typing path) and React DevTools.
  element
    .call_js_fn(&format!(
      "function() {{ \
            this.focus(); \
            if (this.isContentEditable) {{ \
              this.textContent = ''; \
              this.textContent = '{escaped}'; \
              this.dispatchEvent(new InputEvent('input', {{bubbles: true}})); \
            }} else {{ \
              var proto = Object.getPrototypeOf(this); \
              var desc = Object.getOwnPropertyDescriptor(proto, 'value'); \
              var setter = desc && desc.set; \
              if (setter) {{ \
                setter.call(this, ''); \
                setter.call(this, '{escaped}'); \
              }} else {{ \
                this.value = ''; \
                this.value = '{escaped}'; \
              }} \
              this.dispatchEvent(new Event('input', {{bubbles: true}})); \
              this.dispatchEvent(new Event('change', {{bubbles: true}})); \
            }} \
        }}"
    ))
    .await
    .map_err(|e| FerriError::Backend(format!("Fill: {e}")))
}

// ─── Navigation ─────────────────────────────────────────────────────────────

/// Navigate to URL with empty DOM health check for HTTP/HTTPS pages.
///
/// # Errors
///
/// Returns an error if navigation fails or the page DOM remains empty after retries.
pub async fn navigate_with_health_check(page: &AnyPage, url: &str) -> Result<()> {
  page.goto(url, crate::backend::NavLifecycle::Load, 30_000, None).await?;

  let url_lower = url.to_lowercase();
  if url_lower.starts_with("http://") || url_lower.starts_with("https://") {
    let check_js = "document.body ? document.body.children.length : 0";
    let is_empty = || async {
      page
        .evaluate(check_js)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        == 0
    };
    if is_empty().await {
      tokio::time::sleep(std::time::Duration::from_secs(2)).await;
      if is_empty().await {
        let _ = page.reload(crate::backend::NavLifecycle::Load, 30_000).await;
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        if is_empty().await {
          return Err(
            "Page loaded but DOM is empty. The page may need JS rendering, \
                         have anti-bot protection, or the URL may be wrong. \
                         Try wait_for with a selector, or try a different URL."
              .into(),
          );
        }
      }
    }
  }
  Ok(())
}

// ─── Search Page ────────────────────────────────────────────────────────────

/// Search page text for a pattern (like grep). Uses injected runtime.
///
/// # Errors
///
/// Returns an error if JS evaluation fails or the search pattern is invalid.
///
pub async fn search_page(page: &AnyPage, opts: &SearchOptions) -> Result<SearchResult> {
  let pattern = serde_json::to_string(&opts.pattern)?;
  let is_regex = if opts.regex { "true" } else { "false" };
  let case_sensitive = if opts.case_sensitive { "true" } else { "false" };
  let context_chars = opts.context_chars;
  let css_scope = serde_json::to_string(&opts.css_scope)?;
  let max_results = opts.max_results;

  let fd = page.injected_script().await?;
  let js =
    format!("{fd}.searchPage({pattern}, {is_regex}, {case_sensitive}, {context_chars}, {css_scope}, {max_results})");

  let result_str = rt_eval_str(page, &js).await?;
  let data: serde_json::Value = serde_json::from_str(&result_str).unwrap_or(serde_json::json!({}));

  if let Some(err) = data["error"].as_str() {
    return Err(FerriError::Backend(err.to_string()));
  }

  let total = usize::try_from(data["total"].as_u64().unwrap_or(0)).unwrap_or(0);
  let has_more = data["has_more"].as_bool().unwrap_or(false);
  let matches = data["matches"]
    .as_array()
    .map(|arr| {
      arr
        .iter()
        .map(|m| SearchMatch {
          match_text: m["match_text"].as_str().unwrap_or("").to_string(),
          context: m["context"].as_str().unwrap_or("").to_string(),
          element_path: m["element_path"].as_str().unwrap_or("").to_string(),
          char_position: usize::try_from(m["char_position"].as_u64().unwrap_or(0)).unwrap_or(0),
        })
        .collect()
    })
    .unwrap_or_default();

  Ok(SearchResult {
    matches,
    total,
    has_more,
  })
}

/// Format search results into human-readable text.
#[must_use]
pub fn format_search_results(result: &SearchResult, pattern: &str) -> String {
  if result.total == 0 {
    return format!("No matches found for \"{pattern}\" on page.");
  }

  let mut lines = vec![format!(
    "Found {} match{} for \"{pattern}\" on page:\n",
    result.total,
    if result.total == 1 { "" } else { "es" }
  )];
  for (i, m) in result.matches.iter().enumerate() {
    let loc = if m.element_path.is_empty() {
      String::new()
    } else {
      format!(" (in {})", m.element_path)
    };
    lines.push(format!("[{}] {}{loc}", i + 1, m.context));
  }
  if result.has_more {
    lines.push(format!(
      "\n... showing {} of {} total. Increase max_results to see more.",
      result.matches.len(),
      result.total
    ));
  }
  lines.join("\n")
}

// ─── Find Elements ──────────────────────────────────────────────────────────

/// Query DOM elements by selector. Uses injected runtime.
///
/// # Errors
///
/// Returns an error if JS evaluation fails or the selector is invalid.
///
pub async fn find_elements(page: &AnyPage, opts: &FindElementsOptions) -> Result<FindResult> {
  // Rich selectors go through the selector engine
  if selectors::is_rich_selector(&opts.selector) {
    let matched = selectors::query_all(page, &opts.selector, None).await?;
    selectors::cleanup_tags(page).await;
    let total = matched.len();
    let elements = matched
      .into_iter()
      .take(opts.max_results)
      .map(|m| FoundElement {
        index: m.index,
        tag: m.tag,
        text: if opts.include_text { Some(m.text) } else { None },
        attrs: FxHashMap::default(),
        children_count: 0,
      })
      .collect();
    return Ok(FindResult { elements, total });
  }

  // Plain CSS: use runtime's findElementsCSS
  let selector = serde_json::to_string(&opts.selector)?;
  let attributes = serde_json::to_string(&opts.attributes)?;
  let max_results = opts.max_results;
  let include_text = if opts.include_text { "true" } else { "false" };

  let fd = page.injected_script().await?;
  let js = format!("{fd}.findElementsCSS({selector}, {attributes}, {max_results}, {include_text})");

  let result_str = rt_eval_str(page, &js).await?;
  let data: serde_json::Value = serde_json::from_str(&result_str).unwrap_or(serde_json::json!({}));

  if let Some(err) = data["error"].as_str() {
    return Err(FerriError::Backend(err.to_string()));
  }

  let total = usize::try_from(data["total"].as_u64().unwrap_or(0)).unwrap_or(0);
  let elements = data["elements"]
    .as_array()
    .map(|arr| {
      arr
        .iter()
        .map(|el| {
          let mut attrs = FxHashMap::default();
          if let Some(obj) = el["attrs"].as_object() {
            for (k, v) in obj {
              attrs.insert(k.clone(), v.as_str().unwrap_or("").to_string());
            }
          }
          FoundElement {
            index: usize::try_from(el["index"].as_u64().unwrap_or(0)).unwrap_or(0),
            tag: el["tag"].as_str().unwrap_or("?").to_string(),
            text: el["text"].as_str().map(std::string::ToString::to_string),
            attrs,
            children_count: usize::try_from(el["children_count"].as_u64().unwrap_or(0)).unwrap_or(0),
          }
        })
        .collect()
    })
    .unwrap_or_default();

  Ok(FindResult { elements, total })
}

/// Format `find_elements` results into human-readable text.
#[must_use]
pub fn format_find_results(result: &FindResult, selector: &str) -> String {
  if result.total == 0 {
    return format!("No elements found matching \"{selector}\".");
  }

  let mut lines = vec![format!(
    "Found {} element{} matching \"{selector}\":\n",
    result.total,
    if result.total == 1 { "" } else { "s" }
  )];
  for el in &result.elements {
    let mut parts = vec![format!("[{}] <{}>", el.index, el.tag)];
    if let Some(text) = &el.text {
      if !text.is_empty() {
        let display: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let display = if display.len() > 120 {
          format!("{}...", &display[..120])
        } else {
          display
        };
        parts.push(format!("\"{display}\""));
      }
    }
    if !el.attrs.is_empty() {
      let attr_strs: Vec<String> = el.attrs.iter().map(|(k, v)| format!("{k}=\"{v}\"")).collect();
      parts.push(format!("{{{}}}", attr_strs.join(", ")));
    }
    parts.push(format!("({} children)", el.children_count));
    lines.push(parts.join(" "));
  }
  if result.elements.len() < result.total {
    lines.push(format!(
      "\nShowing {} of {} total. Increase max_results to see more.",
      result.elements.len(),
      result.total
    ));
  }
  lines.join("\n")
}

// ─── Select Option ──────────────────────────────────────────────────────────

/// Select a dropdown option by value or label text.
///
/// # Errors
///
/// Returns an error if the element is not a select or the target option is not found.
pub async fn select_option(element: &AnyElement, page: &AnyPage, target: &str) -> Result<SelectResult> {
  let escaped = target.replace('\\', "\\\\").replace('\'', "\\'");
  let fd = page.injected_script().await?;
  let result_json = element
    .call_js_fn_value(&format!(
      "function() {{ return JSON.stringify({fd}.selectOption(this, '{escaped}')); }}"
    ))
    .await
    .ok()
    .flatten()
    .and_then(|v| v.as_str().map(std::string::ToString::to_string))
    .unwrap_or_else(|| "{}".into());

  let result: serde_json::Value = serde_json::from_str(&result_json).unwrap_or(serde_json::json!({}));

  if let Some(err) = result["error"].as_str() {
    let mut msg = format!("{err}.");
    if let Some(avail) = result["available"].as_array() {
      let opts: Vec<&str> = avail.iter().filter_map(|v| v.as_str()).collect();
      let _ = std::fmt::Write::write_fmt(&mut msg, format_args!(" Available options: {}", opts.join(", ")));
    }
    return Err(FerriError::Backend(msg));
  }

  Ok(SelectResult {
    selected_text: result["selected"].as_str().unwrap_or(target).to_string(),
    selected_value: result["value"].as_str().unwrap_or("").to_string(),
  })
}

/// Get all options from a `<select>` dropdown.
///
/// # Errors
///
/// Returns an error if the element is not a select or options cannot be retrieved.
pub async fn get_dropdown_options(element: &AnyElement, page: &AnyPage) -> Result<Vec<DropdownOption>> {
  let fd = page.injected_script().await?;
  let result_json = element
    .call_js_fn_value(&format!(
      "function() {{ return JSON.stringify({fd}.getOptions(this)); }}"
    ))
    .await
    .ok()
    .flatten()
    .and_then(|v| v.as_str().map(std::string::ToString::to_string))
    .unwrap_or_else(|| "{}".into());

  let result: serde_json::Value = serde_json::from_str(&result_json).unwrap_or(serde_json::json!({}));

  if let Some(err) = result["error"].as_str() {
    return Err(FerriError::Backend(err.to_string()));
  }

  let opts = result["options"]
    .as_array()
    .map(|arr| {
      arr
        .iter()
        .map(|o| DropdownOption {
          index: usize::try_from(o["index"].as_u64().unwrap_or(0)).unwrap_or(0),
          text: o["text"].as_str().unwrap_or("").to_string(),
          value: o["value"].as_str().unwrap_or("").to_string(),
          selected: o["selected"].as_bool().unwrap_or(false),
        })
        .collect()
    })
    .unwrap_or_default();

  Ok(opts)
}

// ─── Scroll Info ────────────────────────────────────────────────────────────

/// Get current scroll position, page height, and viewport height.
///
/// # Errors
///
/// Returns an error if JS evaluation fails.
pub async fn scroll_info(page: &AnyPage) -> Result<ScrollInfo> {
  let fd = page.injected_script().await?;
  let result = rt_eval_str(page, &format!("{fd}.scrollInfo()")).await?;
  let parsed: serde_json::Value = serde_json::from_str(&result).unwrap_or(serde_json::json!({}));

  Ok(ScrollInfo {
    scroll_y: parsed["scrollY"].as_i64().unwrap_or(0),
    scroll_height: parsed["scrollHeight"].as_i64().unwrap_or(0),
    viewport_height: parsed["viewportHeight"].as_i64().unwrap_or(0),
  })
}

// ─── Console Errors ─────────────────────────────────────────────────────────

/// Get console error count (installs interceptor on first call). Uses runtime.
pub async fn console_error_count(page: &AnyPage) -> i64 {
  let Ok(fd) = page.injected_script().await else {
    return 0;
  };
  rt_eval(page, &format!("{fd}.consoleErrors()"))
    .await
    .ok()
    .flatten()
    .and_then(|v| v.as_i64())
    .unwrap_or(0)
}

// ─── Markdown Extraction ────────────────────────────────────────────────────

/// Extract page content as clean markdown. Uses the injected runtime.
///
/// # Errors
///
/// Returns an error if JS evaluation fails.
pub async fn extract_markdown(page: &AnyPage) -> Result<String> {
  let fd = page.injected_script().await?;
  rt_eval_str(page, &format!("{fd}.extractMarkdown()")).await
}

// ─── File Upload ────────────────────────────────────────────────────────────

/// Upload file(s) to a file input element.
/// Uses CDP `DOM.setFileInputFiles` with the element's `backendNodeId`.
///
/// # Errors
///
/// Returns an error if the file input element is not found or the files cannot be set.
pub async fn upload_file(page: &AnyPage, selector: &str, paths: &[String]) -> Result<()> {
  page.set_file_input(selector, paths).await
}

// ─── Auto-waiting ───────────────────────────────────────────────────────────

/// Resolve the click dispatch point for an element, in top-level page
/// (viewport) coordinates. Mirrors Playwright's
/// `/tmp/playwright/packages/playwright-core/src/server/dom.ts` click-point
/// logic: scroll-into-view, then take the element's padding-box + an
/// optional `position` offset, then walk the frame chain accumulating
/// `window.frameElement.getBoundingClientRect()` offsets so an iframe
/// element resolves to the right top-level coords.
///
/// All runs in a single JS round-trip; the per-backend `click_at_with`
/// receives ready-to-dispatch viewport coords.
///
/// # Errors
///
/// Returns an error if the JS evaluation fails or the element has no box.
pub async fn resolve_click_point(element: &AnyElement, position: Option<crate::options::Point>) -> Result<(f64, f64)> {
  let position_js = match position {
    Some(p) => format!("{{x:{},y:{}}}", p.x, p.y),
    None => "null".to_string(),
  };
  // Prefer the non-standard Chromium/WebKit `scrollIntoViewIfNeeded` —
  // a single native call that measures viewport + scroll in one step.
  // Fall back to the W3C `scrollIntoView({block:'center'})` on Firefox
  // (BiDi), which lacks the non-standard variant. Both paths are
  // genuine implementations; this is feature detection for the
  // best-available primitive, not a workaround.
  let js = format!(
    "function() {{ \
      if (typeof this.scrollIntoViewIfNeeded === 'function') {{ \
        this.scrollIntoViewIfNeeded(); \
      }} else {{ \
        this.scrollIntoView({{ block: 'center', inline: 'center' }}); \
      }} \
      var r = this.getBoundingClientRect(); \
      var pos = {position_js}; \
      var x = pos ? (r.x + pos.x) : (r.x + r.width / 2); \
      var y = pos ? (r.y + pos.y) : (r.y + r.height / 2); \
      var win = this.ownerDocument.defaultView; \
      while (win && win !== win.parent && win.frameElement) {{ \
        var fr = win.frameElement.getBoundingClientRect(); \
        x += fr.x; \
        y += fr.y; \
        win = win.parent; \
      }} \
      return {{ x: x, y: y }}; \
    }}"
  );
  let val = element.call_js_fn_value(&js).await?;
  val
    .and_then(|v| {
      let x = v.get("x").and_then(serde_json::Value::as_f64)?;
      let y = v.get("y").and_then(serde_json::Value::as_f64)?;
      Some((x, y))
    })
    .ok_or_else(|| FerriError::backend("could not compute click point"))
}

/// Dispatch a click with the full Playwright [`crate::options::ClickOptions`]
/// surface. Mirrors Playwright's `/tmp/playwright/packages/playwright-core/
/// src/server/dom.ts::ElementHandle._click`:
///
/// 1. If `!force`, run the actionability checks (visibility / attached /
///    stable / enabled / editable where applicable). On `force=true`,
///    skip them entirely and rely on the element's current state.
/// 2. Press the modifier keys (keydown) — even in `trial` mode; per
///    Playwright, modifiers are pressed so callers can test
///    modifier-visible UI without firing the actual mouse event.
/// 3. If `trial`, release modifiers and return `Ok(())` without firing
///    any mouse event.
/// 4. Otherwise resolve the click point (element center or padding-box
///    offset by `position`, with iframe-chain accumulation), then
///    dispatch via the backend's `click_at_with(x, y, args)`.
/// 5. Release modifiers on all paths (error or success) so page state
///    doesn't leak between actions.
///
/// `click_count`, `delay`, `button`, and `steps` are honored in
/// [`crate::backend::BackendClickArgs`] — each backend's `click_at_with`
/// is responsible for emitting the right wire events with those values.
///
/// # Errors
///
/// Returns an error from actionability, point resolution, or the
/// per-backend click dispatch. Modifier release is best-effort and does
/// not override the primary error.
pub async fn click_with_opts(element: &AnyElement, page: &AnyPage, opts: &crate::options::ClickOptions) -> Result<()> {
  let args = crate::backend::BackendClickArgs::from_options(opts);
  // Retry the entire pointer action when the hit-target interceptor
  // reports another element captured the click — mirrors Playwright's
  // `_retryAction` loop in
  // `/tmp/playwright/packages/playwright-core/src/server/dom.ts:310`.
  // The most common reason in headed-mode tests: the page mutates
  // (onload `setTimeout` fires, dynamic ad slot lays out) between
  // `clickPrep` resolving the point and Chrome dispatching the
  // queued `Input.dispatchMouseEvent`, so the click lands on whatever
  // element flowed into that pixel position. Re-resolving the point
  // on retry resyncs us to the current layout.
  let retry_backoff_ms: [u64; 5] = [0, 20, 100, 100, 500];
  let mut attempt: usize = 0;
  loop {
    if attempt > 0 {
      let wait_ms = retry_backoff_ms[attempt.min(retry_backoff_ms.len() - 1)];
      if wait_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
      }
    }

    // Single combined pre-flight: `clickGuard` + `isActionable` +
    // `scrollIntoView` + `clickPoint` in ONE callFunctionOn.
    let (x, y) = if opts.is_force() {
      resolve_click_point(element, opts.position).await?
    } else {
      match click_prep(element, page, opts.position).await? {
        ClickPrep::Ready { x, y } => (x, y),
        ClickPrep::IsSelect => return Err(FerriError::Backend(ClickGuardError::IsSelect.to_string())),
        ClickPrep::IsFileInput => return Err(FerriError::Backend(ClickGuardError::IsFileInput.to_string())),
        ClickPrep::NotActionable { reason } => return Err(FerriError::Backend(reason)),
      }
    };

    // Install Playwright's `setupHitTargetInterceptor` page-side
    // BEFORE dispatching the mouse events. Skipped on force=true and
    // trial=true to match Playwright's `_performPointerAction`
    // (force bypasses actionability checks; trial does not click at
    // all). Skipped also when the backend has no functioning JS
    // injection bridge — `install_hit_interceptor` falls back to
    // `'ok'` in that case so the click still attempts.
    let interceptor_installed = if opts.is_force() || opts.is_trial() {
      false
    } else {
      // Either branch (preliminary miss OR protocol failure) skips
      // arming so we don't gate the click on a finalize that has no
      // listener to wake. `Ok(false)` lets the retry loop re-resolve
      // the point; `Err` falls through to the dispatch which surfaces
      // the underlying CDP / BiDi error on its own.
      matches!(install_hit_interceptor(element, x, y).await, Ok(true))
    };

    page.press_modifiers(&opts.modifiers).await?;
    let dispatch = if opts.is_trial() {
      Ok(())
    } else {
      page.click_at_with(x, y, &args).await
    };
    let _ = page.release_modifiers(&opts.modifiers).await;

    if let Err(e) = dispatch {
      // Clean up any installed interceptor on dispatch failure so the
      // next attempt isn't poisoned by a stale listener.
      if interceptor_installed {
        let _ = finalize_hit_interceptor(element).await;
      }
      return Err(e);
    }

    if !interceptor_installed {
      return Ok(());
    }

    // `Err` from `finalize_hit_interceptor` means the page-side
    // helper round-trip itself failed (target navigated away, JS
    // engine torn down, ...). The mouse events have already
    // dispatched at that point; treat it as success so the action
    // doesn't error on a teardown race. A real action-result failure
    // would have surfaced from `click_at_with` earlier.
    let Ok(hit) = finalize_hit_interceptor(element).await else {
      return Ok(());
    };
    match hit {
      HitResult::Done => return Ok(()),
      HitResult::Missed { description } => {
        attempt += 1;
        if attempt >= retry_backoff_ms.len() + 2 {
          return Err(FerriError::Backend(format!(
            "{description} intercepts pointer events after {attempt} attempts"
          )));
        }
        // Loop continues — re-resolve + retry.
      },
    }
  }
}

/// Outcome of the page-side hit-target interceptor — see
/// `crates/ferridriver/src/injected/index.ts::finalizeHitInterceptor`.
enum HitResult {
  /// The captured mousedown / mouseup events landed on the target
  /// element (or one of its descendants); the click succeeded.
  Done,
  /// Another element (or no element) was at the hit point when the
  /// mouse events fired. The retry loop in [`click_with_opts`]
  /// recomputes the click point and tries again.
  Missed { description: String },
}

/// Install Playwright's `setupHitTargetInterceptor` for the click
/// about to be dispatched. Returns `Ok(true)` when the interceptor is
/// armed, `Ok(false)` when the preliminary hit-target check already
/// reports a miss (the caller should retry without dispatching), and
/// `Err(_)` on protocol failure.
async fn install_hit_interceptor(element: &AnyElement, x: f64, y: f64) -> Result<bool> {
  let js = format!("function() {{ return window.__fd.installHitInterceptor(this, {{x: {x}, y: {y}}}, 'mouse'); }}");
  let val = element.call_js_fn_value(&js).await?;
  let s = val.and_then(|v| v.as_str().map(std::string::ToString::to_string));
  Ok(matches!(s.as_deref(), Some("ok")))
}

/// Tear down the interceptor and read the captured outcome.
async fn finalize_hit_interceptor(element: &AnyElement) -> Result<HitResult> {
  let js = "function() { return JSON.stringify(window.__fd.finalizeHitInterceptor()); }";
  let val = element.call_js_fn_value(js).await?;
  let raw = val
    .and_then(|v| v.as_str().map(std::string::ToString::to_string))
    .unwrap_or_default();
  if raw.is_empty() || raw == "\"done\"" {
    return Ok(HitResult::Done);
  }
  // Object form: { hitTargetDescription: "..." }.
  if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) {
    if let Some(desc) = parsed.get("hitTargetDescription").and_then(|v| v.as_str()) {
      return Ok(HitResult::Missed {
        description: desc.to_string(),
      });
    }
  }
  Ok(HitResult::Done)
}

/// Dispatch a hover with the full [`crate::options::HoverOptions`]
/// surface. Mirrors Playwright's `server/dom.ts::ElementHandle._hover`:
/// scroll-into-view, actionability (unless `force`), press modifiers,
/// then emit `steps` interpolated `mousemove` events ending at the
/// element's centre (or the `position` offset). No `mousedown` /
/// `mouseup` is fired — hover is the bare pointer move. Modifiers are
/// released on every exit path so page state doesn't leak.
///
/// # Errors
///
/// Returns an error from actionability, point resolution, or the
/// per-backend mouse-move dispatch.
pub async fn hover_with_opts(element: &AnyElement, page: &AnyPage, opts: &crate::options::HoverOptions) -> Result<()> {
  if !opts.is_force() {
    wait_for_actionable(element, page).await.ok();
  }
  page.press_modifiers(&opts.modifiers).await?;
  let result: Result<()> = if opts.is_trial() {
    Ok(())
  } else {
    match resolve_click_point(element, opts.position).await {
      Ok((x, y)) => {
        let args = crate::backend::BackendHoverArgs {
          modifiers_bitmask: crate::options::modifiers_bitmask(&opts.modifiers),
          steps: 1,
        };
        page.hover_at_with(x, y, &args).await
      },
      Err(e) => Err(e),
    }
  };
  let _ = page.release_modifiers(&opts.modifiers).await;
  result
}

/// Dispatch a tap (touch event) with the full Playwright
/// [`crate::options::TapOptions`] surface. Mirrors
/// `server/dom.ts::ElementHandle._tap` + `input.ts::Touchscreen::tap`:
/// scroll-into-view, actionability (unless `force`), press modifiers,
/// then emit a real `Input.dispatchTouchEvent { touchStart; touchEnd }`
/// pair at the element's centre (or `position` offset).
///
/// CDP (`cdp-pipe`, `cdp-raw`) uses the native
/// `Input.dispatchTouchEvent` protocol command so dispatched touch
/// events have `isTrusted === true` and fire through the full
/// hit-testing + pointer event pipeline. `BiDi` and `WebKit` have no
/// public touch-injection primitive; both emit a backend-level
/// `unsupported:` error that the Locator layer surfaces as
/// [`crate::error::FerriError::Unsupported`]. Matches Playwright's
/// `server/chromium/crInput.ts::RawTouchscreenImpl::tap` (CDP) plus
/// the explicit absence of `Touchscreen` in the `BiDi` backend.
///
/// Modifiers are pressed before dispatch and released on every exit
/// path so page state never leaks.
///
/// # Errors
///
/// Returns an error from actionability, point resolution, or the
/// backend's native `tap_at_with` dispatch.
pub async fn tap_with_opts(element: &AnyElement, page: &AnyPage, opts: &crate::options::TapOptions) -> Result<()> {
  if !opts.is_force() {
    wait_for_actionable(element, page).await.ok();
  }
  page.press_modifiers(&opts.modifiers).await?;
  let result: Result<()> = if opts.is_trial() {
    Ok(())
  } else {
    match resolve_click_point(element, opts.position).await {
      Ok((x, y)) => {
        let args = crate::backend::BackendTapArgs {
          modifiers_bitmask: crate::options::modifiers_bitmask(&opts.modifiers),
        };
        page.tap_at_with(x, y, &args).await
      },
      Err(e) => Err(e),
    }
  };
  let _ = page.release_modifiers(&opts.modifiers).await;
  result
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
  let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::json!({}));
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
      return Err("Timeout: element not actionable".into());
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

    if let Ok(result) = serde_json::from_str::<serde_json::Value>(&val) {
      if result["actionable"].as_bool() == Some(true) {
        return Ok(());
      }
    }

    // Yield to other tasks (allows parallel pages to make progress)
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
  }
}
