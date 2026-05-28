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
use crate::error::{FerriError, Result};

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
  /// Playwright-native `internal:text` engine. Accepts the Playwright body
  /// format (`"quoted"i` / `"quoted"s` / `/regex/flags`). Used by
  /// `get_by_text` when the input is a regex or needs to round-trip
  /// `RegExp` semantics through the injected engine.
  InternalText,
  /// Playwright-native `internal:label` engine. Same body format as
  /// `internal:text`.
  InternalLabel,
  /// Playwright-native `internal:attr=[name=value]` engine. Used by
  /// `get_by_alt_text`, `get_by_title`, `get_by_placeholder` — the body
  /// is `[name=<escaped>]` where `<escaped>` is Playwright's attribute
  /// escape (quoted string with `i`/`s` suffix or `/regex/flags`).
  InternalAttr,
  /// Playwright-native `internal:testid` engine. Body is
  /// `[data-testid=<escaped>]` with attribute-selector escape
  /// (test-id matches are always exact per Playwright).
  InternalTestId,
  /// Playwright-native `internal:role` engine. Body is `<role>[<props>...]`
  /// with `[name=<escaped>]` using attribute-selector escape when a
  /// name filter is supplied.
  InternalRole,
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
    "internal:has=",
    "has-text=",
    "internal:has-text=",
    "has-not=",
    "internal:has-not=",
    "has-not-text=",
    "internal:has-not-text=",
    "internal:and=",
    "internal:or=",
    "internal:text=",
    "internal:label=",
    "internal:attr=",
    "internal:testid=",
    "internal:role=",
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
pub fn parse(selector: &str) -> Result<Selector> {
  let selector = selector.trim();
  if selector.is_empty() {
    return Err(FerriError::invalid_selector(selector, "selector cannot be empty"));
  }

  // Split by >> respecting quoted strings
  let raw_parts = split_by_chain(selector);
  let mut parts = Vec::new();

  for raw in raw_parts {
    let raw = raw.trim();
    if raw.is_empty() {
      return Err(FerriError::invalid_selector(selector, "empty selector part in chain"));
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
  // Try each engine prefix.
  //
  // `internal:has`, `internal:has-text`, `internal:has-not`,
  // `internal:has-not-text` are aliases Playwright emits from
  // `client/locator.ts` (`filter({ hasText, has, ... })`). They are the
  // same semantics as the non-prefixed engines on the server side —
  // see `/tmp/playwright/packages/playwright-core/src/server/selectors.ts:42-43`
  // which lists `internal:has`, `internal:has-not`, `internal:has-text`,
  // `internal:has-not-text` alongside the bare engines. Alias them here
  // so ferridriver's filter() output is accepted unchanged.
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
    ("internal:has=", Engine::Has),
    ("has-text=", Engine::HasText),
    ("internal:has-text=", Engine::HasText),
    ("has-not=", Engine::HasNot),
    ("internal:has-not=", Engine::HasNot),
    ("has-not-text=", Engine::HasNotText),
    ("internal:has-not-text=", Engine::HasNotText),
    ("internal:and=", Engine::InternalAnd),
    ("internal:or=", Engine::InternalOr),
    ("internal:text=", Engine::InternalText),
    ("internal:label=", Engine::InternalLabel),
    ("internal:attr=", Engine::InternalAttr),
    ("internal:testid=", Engine::InternalTestId),
    ("internal:role=", Engine::InternalRole),
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
    Engine::InternalText => "internal:text",
    Engine::InternalLabel => "internal:label",
    Engine::InternalAttr => "internal:attr",
    Engine::InternalTestId => "internal:testid",
    Engine::InternalRole => "internal:role",
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
pub(crate) const MCP_SUPPORT_JS: &str = include_str!("injected/dist/mcp-support.min.js");
pub(crate) const AX_SUPPORT_JS: &str = include_str!("injected/dist/ax-support.min.js");

// ─── Query functions ────────────────────────────────────────────────────────

/// Query all elements matching a rich selector inside the execution
/// context of `frame_id` (or the main frame when `None`). Returns
/// lightweight info; injects the engine JS on first use.
///
/// # Errors
///
/// Returns an error if selector parsing or JS evaluation fails.
pub async fn query_all(page: &AnyPage, selector: &str, frame_id: Option<&str>) -> Result<Vec<MatchedElement>> {
  let parsed = parse(selector)?;
  page.ensure_engine_injected().await?;
  let fd = "window.__fd";
  let js = build_query_js(&parsed, fd);
  let result_str = match frame_id {
    Some(fid) => page.evaluate_in_frame(&js, fid).await,
    None => page.evaluate(&js).await,
  }?
  .and_then(|v| v.as_str().map(std::string::ToString::to_string))
  .unwrap_or_else(|| "[]".into());

  // Check for error
  if let Ok(val) = serde_json::from_str::<serde_json::Value>(&result_str) {
    if let Some(err) = val.get("error").and_then(|e| e.as_str()) {
      return Err(FerriError::evaluation(err.to_string()));
    }
  }

  let elements: Vec<MatchedElement> =
    serde_json::from_str(&result_str).map_err(|e| FerriError::Backend(format!("Parse selector results: {e}")))?;
  Ok(elements)
}

/// Query a single element in `frame_id`'s execution context. If
/// `strict=true`, errors when 0 or >1 matches.
///
/// # Errors
///
/// Returns an error if selector parsing fails, no element is found, or (in strict mode)
/// multiple elements match.
pub async fn query_one(page: &AnyPage, selector: &str, strict: bool, frame_id: Option<&str>) -> Result<AnyElement> {
  let parsed = parse(selector)?;
  let parts_json = build_parts_json(&parsed);

  if strict {
    let matches = query_all(page, selector, frame_id).await?;
    if matches.is_empty() {
      return Err(FerriError::invalid_selector(selector, "no element found"));
    }
    if matches.len() > 1 {
      cleanup_tags(page).await;
      return Err(FerriError::strict(selector, matches.len()));
    }
    // The query_all JS tags the match with `data-fd-sel='0'`; we resolve
    // that tag in the SAME frame so we get an element bound to the right
    // execution context.
    let fd = "window.__fd";
    let tagged_js = format!("{fd}.selOne([{{\"engine\":\"css\",\"body\":\"[data-fd-sel='0']\"}}])");
    let el = page
      .evaluate_to_element(&tagged_js, frame_id)
      .await
      .map_err(|_| FerriError::invalid_selector(selector, "could not resolve matched element"))?;
    cleanup_tags(page).await;
    return Ok(el);
  }

  page.ensure_engine_injected().await?;
  let fd = "window.__fd";
  let js = format!("{fd}.selOne({parts_json})");
  page
    .evaluate_to_element(&js, frame_id)
    .await
    .map_err(|_| FerriError::invalid_selector(selector, "no element found"))
}

/// Query a single element using pre-built JS (avoids re-parsing the selector).
/// The `sel_js` should be the output of `build_selone_js()`. Element
/// resolution always runs in a frame's execution context — `frame_id`
/// is `None` for the main frame, `Some(id)` for an iframe. Mirrors
/// Playwright's frame-bound resolution model.
///
/// # Errors
///
/// Returns an error if no element is found or JS evaluation fails.
pub async fn query_one_prebuilt(
  page: &AnyPage,
  sel_js: &str,
  selector_display: &str,
  frame_id: Option<&str>,
) -> Result<AnyElement> {
  // Surface the underlying error verbatim when it carries
  // recognisable signal — most notably `strict mode violation: <N>`
  // thrown by the engine's `selOne(parts, strict=true)`. Falling back
  // to a generic "No element found" message would swallow that signal
  // and the locator retry loop would spin until timeout instead of
  // converting the strict-mode breach into a typed
  // `FerriError::StrictModeViolation`.
  page.evaluate_to_element(sel_js, frame_id).await.map_err(|err| {
    let msg = err.to_string();
    if msg.contains("strict mode violation") {
      err
    } else {
      FerriError::invalid_selector(selector_display, "no element found")
    }
  })
}

/// Build the JS expression for `selOne` from a selector string.
/// Call once, then pass to `query_one_prebuilt` in a retry loop.
///
/// `strict = true` makes the engine throw a recognisable
/// `strict mode violation: <count>` error when the selector matches
/// more than one element — mirrors Playwright's
/// `injected.querySelector(parsed, root, strict)` pattern. The host
/// catches the exception and converts it to a typed
/// `FerriError::StrictModeViolation`. Skipping the separate
/// `query_all` + `cleanup_tags` round-trips that the strict check
/// would otherwise need (~2 RTTs per locator action).
///
/// # Errors
///
/// Returns an error if the selector string cannot be parsed.
pub fn build_selone_js(selector: &str, fd: &str, strict: bool) -> Result<String> {
  let parsed = parse(selector)?;
  let parts_json = build_parts_json(&parsed);
  let strict_lit = if strict { "true" } else { "false" };
  Ok(format!("{fd}.selOne({parts_json},{strict_lit})"))
}

/// Resolve `selector` to a single element in `frame_id`'s execution
/// context and return the canonical selector Playwright's
/// recorder/codegen would emit for it (`injected.generateSelectorSimple`).
/// Mirrors `Frame.resolveSelector` in
/// `/tmp/playwright/packages/playwright-core/src/server/frames.ts:1274`.
///
/// # Errors
///
/// Returns an error if selector parsing fails, no element matches, or
/// (strict, like Playwright's `query`) more than one element matches.
pub async fn normalize_selector(page: &AnyPage, selector: &str, frame_id: Option<&str>) -> Result<String> {
  let parsed = parse(selector)?;
  let parts_json = build_parts_json(&parsed);
  page.ensure_engine_injected().await?;
  let js = format!("window.__fd.normalizeSelector({parts_json})");
  let result = match frame_id {
    Some(fid) => page.evaluate_in_frame(&js, fid).await,
    None => page.evaluate(&js).await,
  };
  let value = result.map_err(|err| {
    if let Some(count) = parse_strict_violation_count(&err) {
      FerriError::strict(selector, count)
    } else {
      err
    }
  })?;
  value
    .and_then(|v| v.as_str().map(std::string::ToString::to_string))
    .ok_or_else(|| FerriError::invalid_selector(selector, "no element found"))
}

/// Parse a `strict mode violation: <count>` exception message thrown
/// by the engine-side `selOne(parts, strict=true)` and return the
/// match count. Used by [`crate::locator::Locator`] to convert a
/// page-side strict violation into a typed
/// [`crate::error::FerriError::StrictModeViolation`] without paying
/// the separate `query_all` round-trip the previous implementation
/// did up-front. Mirrors Playwright's exception flow from
/// `injected.querySelector(parsed, root, strict)` —
/// `/tmp/playwright/packages/injected/src/injectedScript.ts:278`.
#[must_use]
pub fn parse_strict_violation_count<E: std::fmt::Display + ?Sized>(err: &E) -> Option<usize> {
  // Engine output (ferridriver's bundled selOne): `strict mode
  // violation: <N>` — `<N>` is the match count and appears immediately
  // after the colon.
  let message = err.to_string();
  let needle = "strict mode violation:";
  let idx = message.find(needle)?;
  let tail = message[idx + needle.len()..].trim();
  let count_str: String = tail.chars().take_while(char::is_ascii_digit).collect();
  count_str.parse().ok()
}

/// Show the element highlight overlay for `selector` in `frame_id`'s
/// execution context. `style` is an optional resolved CSS string applied
/// to the highlight box (the caller composes the `style` record into a
/// string, mirroring Playwright's `cssObjectToString`). Mirrors
/// Playwright's `frame._highlight(selector, style)` ->
/// `injected.addHighlight(parsed, style)` flow
/// (`/tmp/playwright/packages/playwright-core/src/server/frames.ts:1333`).
///
/// # Errors
///
/// Returns an error if selector parsing or JS evaluation fails.
pub async fn highlight(page: &AnyPage, selector: &str, style: Option<&str>, frame_id: Option<&str>) -> Result<()> {
  let parsed = parse(selector)?;
  page.ensure_engine_injected().await?;
  let parts_json = build_parts_json(&parsed);
  let style_arg = match style {
    Some(s) => serde_json::to_string(s).unwrap_or_else(|_| "undefined".to_string()),
    None => "undefined".to_string(),
  };
  let js = format!("window.__fd.addHighlight({parts_json},{style_arg})");
  run_void(page, &js, frame_id).await
}

/// Hide the element highlight overlay in `frame_id`'s execution context.
/// Mirrors Playwright's `frame._hideHighlight(selector)` ->
/// `injected.hideHighlight()`
/// (`/tmp/playwright/packages/playwright-core/src/server/frames.ts:1351`).
/// Playwright's `removeHighlight` is per-selector, but its public
/// `hideHighlight` tears the whole overlay down; `Locator.hideHighlight`
/// uses the latter, so we mirror that and drop the entire overlay.
///
/// # Errors
///
/// Returns an error if JS evaluation fails.
pub async fn hide_highlight(page: &AnyPage, frame_id: Option<&str>) -> Result<()> {
  page.ensure_engine_injected().await?;
  run_void(page, "window.__fd.hideHighlight()", frame_id).await
}

async fn run_void(page: &AnyPage, js: &str, frame_id: Option<&str>) -> Result<()> {
  match frame_id {
    Some(fid) => page.evaluate_in_frame(js, fid).await,
    None => page.evaluate(js).await,
  }?;
  Ok(())
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
