//! Playwright-style selector engine.
//!
//! Parses rich selector strings (role=, text=, testid=, css=, etc.) in Rust,
//! then builds a self-contained JS IIFE that executes the query pipeline
//! in the browser context for maximum performance.
//!
//! # Selector Format
//!
//! ```text
//! css=div.container >> role=button[name="Submit"]
//! text="Hello World"
//! role=heading[level=1]
//! testid=login-form
//! label="Email"
//! ```
//!
//! Chaining with `>>` narrows scope: each part's results become the next part's search roots.

use crate::backend::{AnyElement, AnyPage};

// ─── Types ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Selector {
  pub parts: Vec<SelectorPart>,
}

#[derive(Debug, Clone)]
pub struct SelectorPart {
  pub engine: Engine,
  pub body: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Engine {
  Css,
  Text,
  Role,
  TestId,
  Label,
  Placeholder,
  Alt,
  Title,
  XPath,
  Id,
  Nth,
  Visible,
  Has,
  HasText,
  HasNot,
  HasNotText,
  /// Playwright-compatible `internal:and` engine. Intersects the current
  /// scope with another locator's selector; both must match the same
  /// element. The body is the JSON-encoded inner selector string.
  InternalAnd,
  /// Playwright-compatible `internal:or` engine. Union of two locators;
  /// matches elements resolved by either selector.
  InternalOr,
}

/// Bootstrap JS script to be evaluated on new document.
/// Resets the injection promise so we know we need to re-inject after navigation.
pub const ENGINE_BOOTSTRAP_JS: &str = "window.__fd_promise = null;";

/// Unified lazy-injection IIFE.
/// 1. Checks if already ready.
/// 2. Checks if currently injecting.
/// 3. Otherwise, starts injection and stores the promise.
///
/// Returns the `InjectedScript` instance.
#[must_use]
pub fn build_lazy_inject_js() -> String {
  let engine_js = build_inject_js();
  // The engine JS directly creates window.__fd at the end of its IIFE.
  // We just need to run it once and return the result.
  format!(
    r"(async () => {{
      if (window.__fd) return window.__fd;
      if (window.__fd_promise) return await window.__fd_promise;
      window.__fd_promise = (async () => {{
        try {{
          {engine_js}
          return window.__fd;
        }} catch (e) {{
          window.__fd_promise = null;
          throw e;
        }}
      }})();
      return await window.__fd_promise;
    }})()"
  )
}

/// Result of a selector query -- lightweight info returned from JS.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct MatchedElement {
  pub index: usize,
  pub tag: String,
  pub text: String,
}

// ─── Detector ───────────────────────────────────────────────────────────────

/// Check if a selector string uses the rich engine format (not plain CSS).
#[must_use]
pub fn is_rich_selector(s: &str) -> bool {
  let prefixes = [
    "role=",
    "text=",
    "testid=",
    "label=",
    "placeholder=",
    "alt=",
    "title=",
    "xpath=",
    "id=",
    "css=",
    "nth=",
    "visible=",
    "has=",
    "has-text=",
    "has-not=",
    "has-not-text=",
    "internal:and=",
    "internal:or=",
  ];
  let trimmed = s.trim();
  // Has explicit engine prefix
  if prefixes.iter().any(|p| trimmed.starts_with(p)) {
    return true;
  }
  // Has chaining operator
  if trimmed.contains(" >> ") {
    return true;
  }
  false
}

// ─── Parser ─────────────────────────────────────────────────────────────────

/// Parse a selector string into a Selector AST.
///
/// # Errors
///
/// Returns an error if the selector string is empty or has an invalid chain.
pub fn parse(selector: &str) -> Result<Selector, String> {
  let selector = selector.trim();
  if selector.is_empty() {
    return Err("Selector cannot be empty".into());
  }

  // Split by >> respecting quoted strings
  let raw_parts = split_by_chain(selector);
  let mut parts = Vec::new();

  for raw in raw_parts {
    let raw = raw.trim();
    if raw.is_empty() {
      return Err("Empty selector part in chain".into());
    }
    parts.push(parse_part(raw));
  }

  Ok(Selector { parts })
}

fn split_by_chain(s: &str) -> Vec<String> {
  // Fast path: no chain operator, avoid scanning
  if !s.contains(">>") {
    let t = s.trim();
    return if t.is_empty() { Vec::new() } else { vec![t.to_string()] };
  }

  let mut parts = Vec::new();
  let bytes = s.as_bytes();
  let mut start = 0;
  let mut i = 0;
  let mut in_quote: u8 = 0; // 0 = none, b'"' or b'\''

  while i < bytes.len() {
    let c = bytes[i];

    if c == b'\\' && i + 1 < bytes.len() {
      i += 2;
      continue;
    }

    if in_quote != 0 {
      if c == in_quote {
        in_quote = 0;
      }
      i += 1;
      continue;
    }

    if c == b'"' || c == b'\'' {
      in_quote = c;
      i += 1;
      continue;
    }

    if c == b'>' && i + 1 < bytes.len() && bytes[i + 1] == b'>' {
      let part = s[start..i].trim();
      if !part.is_empty() {
        parts.push(part.to_string());
      }
      i += 2;
      while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
      }
      start = i;
      continue;
    }

    i += 1;
  }

  let part = s[start..].trim();
  if !part.is_empty() {
    parts.push(part.to_string());
  }

  parts
}

fn parse_part(s: &str) -> SelectorPart {
  // Try each engine prefix
  let engines = [
    ("role=", Engine::Role),
    ("text=", Engine::Text),
    ("testid=", Engine::TestId),
    ("label=", Engine::Label),
    ("placeholder=", Engine::Placeholder),
    ("alt=", Engine::Alt),
    ("title=", Engine::Title),
    ("xpath=", Engine::XPath),
    ("id=", Engine::Id),
    ("css=", Engine::Css),
    ("nth=", Engine::Nth),
    ("visible=", Engine::Visible),
    ("has=", Engine::Has),
    ("has-text=", Engine::HasText),
    ("has-not=", Engine::HasNot),
    ("has-not-text=", Engine::HasNotText),
    ("internal:and=", Engine::InternalAnd),
    ("internal:or=", Engine::InternalOr),
  ];

  for (prefix, engine) in &engines {
    if let Some(body) = s.strip_prefix(prefix) {
      return SelectorPart {
        engine: engine.clone(),
        body: body.to_string(),
      };
    }
  }

  // Default: treat as CSS selector
  SelectorPart {
    engine: Engine::Css,
    body: s.to_string(),
  }
}

// ─── JS Query Builder ───────────────────────────────────────────────────────

/// JS to inject the unified runtime once. Idempotent -- safe to call multiple times.
#[must_use]
pub fn build_inject_js() -> String {
  // Bundled Playwright-compatible selector engine + actionability checks + ferridriver helpers.
  // Built from crates/ferridriver/src/injected/ via `bun build.ts`.
  ENGINE_JS.to_string()
}

/// Build a lightweight query call (runtime must already be injected).
fn build_query_js(selector: &Selector, fd: &str) -> String {
  let parts_json = build_parts_json(selector);
  format!("{fd}.sel({parts_json})")
}

/// Builds a JSON array of selector parts for the injected engine.
/// Writes directly into a single buffer to avoid intermediate allocations.
#[must_use]
pub fn build_parts_json(selector: &Selector) -> String {
  // Pre-allocate: ~40 bytes per part is a reasonable estimate
  let mut buf = String::with_capacity(selector.parts.len() * 40 + 2);
  buf.push('[');
  for (i, p) in selector.parts.iter().enumerate() {
    if i > 0 {
      buf.push(',');
    }
    let engine = engine_str(&p.engine);
    buf.push_str(r#"{"engine":""#);
    buf.push_str(engine);
    buf.push_str(r#"","body":"#);
    // Inline JSON string escaping into the buffer
    json_escape_string_into(&mut buf, &p.body);
    buf.push('}');
  }
  buf.push(']');
  buf
}

/// Map Engine variant to its protocol string.
fn engine_str(engine: &Engine) -> &'static str {
  match engine {
    Engine::Css => "css",
    Engine::Text => "text",
    Engine::Role => "role",
    Engine::TestId => "testid",
    Engine::Label => "label",
    Engine::Placeholder => "placeholder",
    Engine::Alt => "alt",
    Engine::Title => "title",
    Engine::XPath => "xpath",
    Engine::Id => "id",
    Engine::Nth => "nth",
    Engine::Visible => "visible",
    Engine::Has => "has",
    Engine::HasText => "has-text",
    Engine::HasNot => "has-not",
    Engine::HasNotText => "has-not-text",
    Engine::InternalAnd => "internal:and",
    Engine::InternalOr => "internal:or",
  }
}

/// Write a JSON-escaped string (with surrounding quotes) directly into `buf`.
/// Avoids the intermediate String allocation that `serde_json::to_string` would produce.
fn json_escape_string_into(buf: &mut String, s: &str) {
  use std::fmt::Write as _;

  buf.push('"');
  for ch in s.chars() {
    match ch {
      '"' => buf.push_str(r#"\""#),
      '\\' => buf.push_str(r"\\"),
      '\n' => buf.push_str(r"\n"),
      '\r' => buf.push_str(r"\r"),
      '\t' => buf.push_str(r"\t"),
      c if c.is_control() => {
        let _ = write!(buf, "\\u{:04x}", c as u32);
      },
      c => buf.push(c),
    }
  }
  buf.push('"');
}

/// The injected JS engine — bundled from `src/injected/` TypeScript sources.
/// Rebuild with: `cd crates/ferridriver/src/injected && bun build.ts`
const ENGINE_JS: &str = include_str!("injected/dist/engine.min.js");

// ─── Query functions ────────────────────────────────────────────────────────

/// Query all elements matching a rich selector. Returns lightweight info.
/// Injects the engine JS on first use, then subsequent calls are lightweight.
///
/// # Errors
///
/// Returns an error if selector parsing or JS evaluation fails.
pub async fn query_all(page: &AnyPage, selector: &str) -> Result<Vec<MatchedElement>, String> {
  let parsed = parse(selector)?;
  page.ensure_engine_injected().await?;
  let fd = "window.__fd";
  let js = build_query_js(&parsed, fd);
  let result_str = page
    .evaluate(&js)
    .await?
    .and_then(|v| v.as_str().map(std::string::ToString::to_string))
    .unwrap_or_else(|| "[]".into());

  // Check for error
  if let Ok(val) = serde_json::from_str::<serde_json::Value>(&result_str) {
    if let Some(err) = val.get("error").and_then(|e| e.as_str()) {
      return Err(err.to_string());
    }
  }

  let elements: Vec<MatchedElement> =
    serde_json::from_str(&result_str).map_err(|e| format!("Parse selector results: {e}"))?;
  Ok(elements)
}

/// Query a single element. If strict=true, errors when 0 or >1 matches.
///
/// # Errors
///
/// Returns an error if selector parsing fails, no element is found, or (in strict mode)
/// multiple elements match.
pub async fn query_one(page: &AnyPage, selector: &str, strict: bool) -> Result<AnyElement, String> {
  let parsed = parse(selector)?;
  let parts_json = build_parts_json(&parsed);

  if strict {
    let matches = query_all(page, selector).await?;
    if matches.is_empty() {
      return Err(format!("No element found for selector: {selector}"));
    }
    if matches.len() > 1 {
      cleanup_tags(page).await;
      return Err(format!(
        "Selector \"{selector}\" resolved to {} elements. Use a more specific selector.",
        matches.len()
      ));
    }
    let el = page
      .find_element("[data-fd-sel='0']")
      .await
      .map_err(|_| format!("Could not resolve matched element for: {selector}"))?;
    cleanup_tags(page).await;
    return Ok(el);
  }

  page.ensure_engine_injected().await?;
  let fd = "window.__fd";
  let js = format!("{fd}.selOne({parts_json})");
  page
    .evaluate_to_element(&js)
    .await
    .map_err(|_| format!("No element found for selector: {selector}"))
}

/// Query a single element using pre-built JS (avoids re-parsing the selector).
/// The `sel_js` should be the output of `build_selone_js()`.
///
/// # Errors
///
/// Returns an error if no element is found or JS evaluation fails.
pub async fn query_one_prebuilt(page: &AnyPage, sel_js: &str, selector_display: &str) -> Result<AnyElement, String> {
  page
    .evaluate_to_element(sel_js)
    .await
    .map_err(|_| format!("No element found for selector: {selector_display}"))
}

/// Build the JS expression for `selOne` from a selector string.
/// Call once, then pass to `query_one_prebuilt` in a retry loop.
///
/// # Errors
///
/// Returns an error if the selector string cannot be parsed.
pub fn build_selone_js(selector: &str, fd: &str) -> Result<String, String> {
  let parsed = parse(selector)?;
  let parts_json = build_parts_json(&parsed);
  Ok(format!("{fd}.selOne({parts_json})"))
}

/// Clean up any leftover selector tags (call after operations).
pub async fn cleanup_tags(page: &AnyPage) {
  let _ = page
    .evaluate(
      "(function() { \
        document.querySelectorAll('[data-fd-sel]').forEach(function(e) { \
            e.removeAttribute('data-fd-sel'); \
        }); \
    })()",
    )
    .await;
}
