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

  // ALL selectors go through the engine (treats bare CSS as default)
  selectors::query_one(page, sel, false).await
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
  page.goto(url, crate::backend::NavLifecycle::Load, 30_000).await?;

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
    let matched = selectors::query_all(page, &opts.selector).await?;
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

/// Wait for an element to be actionable (visible, enabled).
/// Polls from Rust side with short sleeps -- no blocking JS promises.
/// This allows parallel pages to make progress concurrently.
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
