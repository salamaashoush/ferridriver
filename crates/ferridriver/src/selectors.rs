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
fn build_query_js(selector: &Selector) -> String {
  let parts_json = build_parts_json(selector);
  format!("window.__fd.sel({parts_json})")
}

/// Builds a JSON array of selector parts for the injected engine.
#[must_use]
pub fn build_parts_json(selector: &Selector) -> String {
  let parts: Vec<String> = selector
    .parts
    .iter()
    .map(|p| {
      let engine = match p.engine {
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
      };
      let body_escaped = serde_json::to_string(&p.body).unwrap_or_else(|_| format!("\"{}\"", p.body));
      format!(r#"{{"engine":"{engine}","body":{body_escaped}}}"#)
    })
    .collect();
  format!("[{}]", parts.join(","))
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
  // Ensure engine is injected (idempotent)
  let js = build_query_js(&parsed);
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

  let js = format!("window.__fd.selOne({parts_json})");

  page
    .evaluate_to_element(&js)
    .await
    .map_err(|_| format!("No element found for selector: {selector}"))
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
