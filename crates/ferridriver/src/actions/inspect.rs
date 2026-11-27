//! Page inspection, search, find, dropdown, scroll, and markdown action helpers.

use rustc_hash::FxHashMap;

use super::{ensure_mcp_support, rt_eval, rt_eval_str};
use crate::backend::{AnyElement, AnyPage};
use crate::error::{FerriError, Result};
use crate::selectors;

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

#[derive(Debug, Clone)]
pub struct ScrollInfo {
  pub scroll_y: i64,
  pub scroll_height: i64,
  pub viewport_height: i64,
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

  let fd = ensure_mcp_support(page).await?;
  let js =
    format!("{fd}.searchPage({pattern}, {is_regex}, {case_sensitive}, {context_chars}, {css_scope}, {max_results})");

  let result_str = rt_eval_str(page, &js).await?;
  let data: serde_json::Value = serde_json::from_str(&result_str).unwrap_or_else(|_| serde_json::json!({}));

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

  let fd = ensure_mcp_support(page).await?;
  let js = format!("{fd}.findElementsCSS({selector}, {attributes}, {max_results}, {include_text})");

  let result_str = rt_eval_str(page, &js).await?;
  let data: serde_json::Value = serde_json::from_str(&result_str).unwrap_or_else(|_| serde_json::json!({}));

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
    if let Some(text) = &el.text
      && !text.is_empty()
    {
      let mut display: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
      if let Some((cut, _)) = display.char_indices().nth(120) {
        display.truncate(cut);
        display.push_str("...");
      }
      parts.push(format!("\"{display}\""));
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
  // A JSON string is a JS string literal whatever it holds.
  let target_json =
    serde_json::to_string(target).map_err(|e| FerriError::Backend(format!("select_option serialize: {e}")))?;
  let fd = ensure_mcp_support(page).await?;
  let result_json = element
    .call_js_fn_value(&format!(
      "function() {{ return JSON.stringify({fd}.selectOption(this, {target_json})); }}"
    ))
    .await
    .ok()
    .flatten()
    .and_then(|v| v.as_str().map(std::string::ToString::to_string))
    .unwrap_or_else(|| "{}".into());

  let result: serde_json::Value = serde_json::from_str(&result_json).unwrap_or_else(|_| serde_json::json!({}));

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

  let result: serde_json::Value = serde_json::from_str(&result_json).unwrap_or_else(|_| serde_json::json!({}));

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
  let parsed: serde_json::Value = serde_json::from_str(&result).unwrap_or_else(|_| serde_json::json!({}));

  Ok(ScrollInfo {
    scroll_y: parsed["scrollY"].as_i64().unwrap_or(0),
    scroll_height: parsed["scrollHeight"].as_i64().unwrap_or(0),
    viewport_height: parsed["viewportHeight"].as_i64().unwrap_or(0),
  })
}

// ─── Console Errors ─────────────────────────────────────────────────────────

/// Get console error count (installs interceptor on first call). Uses runtime.
pub async fn console_error_count(page: &AnyPage) -> i64 {
  let Ok(fd) = ensure_mcp_support(page).await else {
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
  let fd = ensure_mcp_support(page).await?;
  rt_eval_str(page, &format!("{fd}.extractMarkdown()")).await
}
