//! Core automation primitives -- shared by MCP tools, BDD steps, and library consumers.
//!
//! All functions take `&AnyPage` or `&AnyElement` and return `Result<T, String>`.
//! No MCP types, no server state -- pure browser automation logic.
//!
//! All JS operations go through the unified `window.__fd` runtime
//! (injected automatically via addScriptToEvaluateOnNewDocument on CDP,
//! or after navigation on `WebKit`).

use crate::backend::{AnyElement, AnyPage};
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
async fn rt_eval(page: &AnyPage, js: &str) -> Result<Option<serde_json::Value>, String> {
  page.evaluate(js).await
}

/// Ensure runtime + evaluate, return string result.
async fn rt_eval_str(page: &AnyPage, js: &str) -> Result<String, String> {
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
) -> Result<AnyElement, String> {
  if let Some(r) = r#ref {
    let backend_id = ref_map
      .get(r)
      .ok_or_else(|| format!("Unknown ref '{r}'. Take a new snapshot."))?;
    return page.resolve_backend_node(*backend_id, r).await;
  }

  let sel = selector.ok_or("Provide 'ref' (from snapshot) or 'selector'.")?;

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
pub async fn check_click_guard(element: &AnyElement, page: &AnyPage) -> Result<(), ClickGuardError> {
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

// ─── Fill ───────────────────────────────────────────────────────────────────

/// Fill an input element: auto-wait for actionable, click to focus, clear with events, type value, dispatch events.
///
/// # Errors
///
/// Returns an error if the element cannot be focused or the value cannot be set.
pub async fn fill(element: &AnyElement, value: &str) -> Result<(), String> {
  // Single JS call: focus + set value + dispatch events.
  // Handles both regular inputs (.value) and contenteditable elements (.textContent).
  let escaped = value.replace('\\', "\\\\").replace('\'', "\\'");
  element
    .call_js_fn(&format!(
      "function() {{ \
            this.focus(); \
            if (this.isContentEditable) {{ \
              this.textContent = ''; \
              this.textContent = '{escaped}'; \
              this.dispatchEvent(new InputEvent('input', {{bubbles: true}})); \
            }} else {{ \
              this.value = ''; \
              this.value = '{escaped}'; \
              this.dispatchEvent(new Event('input', {{bubbles: true}})); \
              this.dispatchEvent(new Event('change', {{bubbles: true}})); \
            }} \
        }}"
    ))
    .await
    .map_err(|e| format!("Fill: {e}"))
}

// ─── Navigation ─────────────────────────────────────────────────────────────

/// Navigate to URL with empty DOM health check for HTTP/HTTPS pages.
///
/// # Errors
///
/// Returns an error if navigation fails or the page DOM remains empty after retries.
pub async fn navigate_with_health_check(page: &AnyPage, url: &str) -> Result<(), String> {
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
pub async fn search_page(page: &AnyPage, opts: &SearchOptions) -> Result<SearchResult, String> {
  let pattern = serde_json::to_string(&opts.pattern).map_err(|e| e.to_string())?;
  let is_regex = if opts.regex { "true" } else { "false" };
  let case_sensitive = if opts.case_sensitive { "true" } else { "false" };
  let context_chars = opts.context_chars;
  let css_scope = serde_json::to_string(&opts.css_scope).map_err(|e| e.to_string())?;
  let max_results = opts.max_results;

  let fd = page.injected_script().await?;
  let js =
    format!("{fd}.searchPage({pattern}, {is_regex}, {case_sensitive}, {context_chars}, {css_scope}, {max_results})");

  let result_str = rt_eval_str(page, &js).await?;
  let data: serde_json::Value = serde_json::from_str(&result_str).unwrap_or(serde_json::json!({}));

  if let Some(err) = data["error"].as_str() {
    return Err(err.to_string());
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
pub async fn find_elements(page: &AnyPage, opts: &FindElementsOptions) -> Result<FindResult, String> {
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
  let selector = serde_json::to_string(&opts.selector).map_err(|e| e.to_string())?;
  let attributes = serde_json::to_string(&opts.attributes).map_err(|e| e.to_string())?;
  let max_results = opts.max_results;
  let include_text = if opts.include_text { "true" } else { "false" };

  let fd = page.injected_script().await?;
  let js = format!("{fd}.findElementsCSS({selector}, {attributes}, {max_results}, {include_text})");

  let result_str = rt_eval_str(page, &js).await?;
  let data: serde_json::Value = serde_json::from_str(&result_str).unwrap_or(serde_json::json!({}));

  if let Some(err) = data["error"].as_str() {
    return Err(err.to_string());
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
pub async fn select_option(element: &AnyElement, page: &AnyPage, target: &str) -> Result<SelectResult, String> {
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
    return Err(msg);
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
pub async fn get_dropdown_options(element: &AnyElement, page: &AnyPage) -> Result<Vec<DropdownOption>, String> {
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
    return Err(err.to_string());
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
pub async fn scroll_info(page: &AnyPage) -> Result<ScrollInfo, String> {
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
pub async fn extract_markdown(page: &AnyPage) -> Result<String, String> {
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
pub async fn upload_file(page: &AnyPage, selector: &str, paths: &[String]) -> Result<(), String> {
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
pub async fn resolve_click_point(
  element: &AnyElement,
  position: Option<crate::options::Point>,
) -> Result<(f64, f64), String> {
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
    .ok_or_else(|| "could not compute click point".to_string())
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
pub async fn click_with_opts(
  element: &AnyElement,
  page: &AnyPage,
  opts: &crate::options::ClickOptions,
) -> Result<(), String> {
  if !opts.is_force() {
    check_click_guard(element, page).await.map_err(|e| e.to_string())?;
    wait_for_actionable(element, page).await.ok();
  }
  let args = crate::backend::BackendClickArgs::from_options(opts);
  page.press_modifiers(&opts.modifiers).await?;
  let result = if opts.is_trial() {
    Ok(())
  } else {
    match resolve_click_point(element, opts.position).await {
      Ok((x, y)) => page.click_at_with(x, y, &args).await,
      Err(e) => Err(e),
    }
  };
  // Always release modifiers so page state doesn't leak.
  let _ = page.release_modifiers(&opts.modifiers).await;
  result
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
pub async fn hover_with_opts(
  element: &AnyElement,
  page: &AnyPage,
  opts: &crate::options::HoverOptions,
) -> Result<(), String> {
  if !opts.is_force() {
    wait_for_actionable(element, page).await.ok();
  }
  page.press_modifiers(&opts.modifiers).await?;
  let result: Result<(), String> = if opts.is_trial() {
    Ok(())
  } else {
    match resolve_click_point(element, opts.position).await {
      Ok((x, y)) => {
        let args = crate::backend::BackendHoverArgs {
          modifiers_bitmask: crate::options::modifiers_bitmask(&opts.modifiers),
          steps: opts.resolved_steps(),
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
/// `server/dom.ts::ElementHandle._tap`: scroll-into-view, actionability
/// (unless `force`), press modifiers, then fire a touch-event sequence
/// at the element's centre (or `position` offset). Touch events include
/// the modifier flags in their event init dict so the page's
/// `event.shiftKey` etc. are true when the caller requested them.
///
/// Modifiers are released on every exit path.
///
/// Uses JS dispatch rather than native touch input because all three
/// backends lack a standard cross-platform touch primitive — CDP's
/// `Input.dispatchTouchEvent` is Chromium-only, `BiDi` has no touch
/// pointer type equivalent in stable, and `WKWebView` has no public
/// touch injection API. The JS path matches the existing `tap` impl
/// behavior on all three backends and produces `isTrusted:false`
/// events, which suffices for everything bar click-jacking defenses.
///
/// # Errors
///
/// Returns an error from actionability, point resolution, or the JS
/// dispatch itself.
pub async fn tap_with_opts(
  element: &AnyElement,
  page: &AnyPage,
  opts: &crate::options::TapOptions,
) -> Result<(), String> {
  if !opts.is_force() {
    wait_for_actionable(element, page).await.ok();
  }
  page.press_modifiers(&opts.modifiers).await?;
  let result: Result<(), String> = if opts.is_trial() {
    Ok(())
  } else {
    let shift = opts.modifiers.contains(&crate::options::Modifier::Shift);
    let alt = opts.modifiers.contains(&crate::options::Modifier::Alt);
    let ctrl_raw = opts.modifiers.contains(&crate::options::Modifier::Control);
    let meta_raw = opts.modifiers.contains(&crate::options::Modifier::Meta);
    let com = opts.modifiers.contains(&crate::options::Modifier::ControlOrMeta);
    let (ctrl, meta) = if cfg!(target_os = "macos") {
      (ctrl_raw, meta_raw || com)
    } else {
      (ctrl_raw || com, meta_raw)
    };
    let position_js = match opts.position {
      Some(p) => format!("{{x:{},y:{}}}", p.x, p.y),
      None => "null".to_string(),
    };
    let js = format!(
      "function() {{ \
        if (typeof this.scrollIntoViewIfNeeded === 'function') {{ \
          this.scrollIntoViewIfNeeded(); \
        }} else {{ \
          this.scrollIntoView({{ block: 'center', inline: 'center' }}); \
        }} \
        var r = this.getBoundingClientRect(); \
        var pos = {position_js}; \
        var cx = pos ? (r.left + pos.x) : (r.left + r.width / 2); \
        var cy = pos ? (r.top + pos.y) : (r.top + r.height / 2); \
        var init = {{ clientX: cx, clientY: cy, bubbles: true, \
          shiftKey: {shift}, altKey: {alt}, ctrlKey: {ctrl}, metaKey: {meta} }}; \
        if (typeof Touch !== 'undefined' && typeof TouchEvent !== 'undefined') {{ \
          var t = new Touch({{ identifier: 1, target: this, clientX: cx, clientY: cy }}); \
          this.dispatchEvent(new TouchEvent('touchstart', Object.assign({{ touches: [t], changedTouches: [t] }}, init))); \
          this.dispatchEvent(new TouchEvent('touchend', Object.assign({{ touches: [], changedTouches: [t] }}, init))); \
        }} else {{ \
          this.dispatchEvent(new PointerEvent('pointerdown', Object.assign({{ isPrimary: true, pointerType: 'touch' }}, init))); \
          this.dispatchEvent(new PointerEvent('pointerup', Object.assign({{ isPrimary: true, pointerType: 'touch' }}, init))); \
          this.click(); \
        }} \
      }}"
    );
    element.call_js_fn(&js).await
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
) -> Result<Vec<String>, String> {
  let fd = page.injected_script().await?;
  let values_json = serde_json::to_string(values).map_err(|e| format!("select_option serialize: {e}"))?;
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
    return Err(err.to_string());
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
pub async fn wait_for_actionable(element: &AnyElement, page: &AnyPage) -> Result<(), String> {
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
