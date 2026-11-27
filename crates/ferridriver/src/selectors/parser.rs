//! Selector AST, Playwright-compatible parser, and zero-allocation JSON part serializer.

use crate::error::{FerriError, Result};

use super::is_registered_selector_engine;

// ─── Types ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Selector {
  pub parts: Vec<SelectorPart>,
}

#[derive(Debug, Clone)]
pub struct SelectorPart {
  pub engine: Engine,
  pub body: String,
  /// Playwright `*` capture modifier: in a chain, the query returns this
  /// part's element instead of the last part's.
  pub capture: bool,
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
  /// Playwright-native `internal:has-text` engine emitted by
  /// `locator.filter({ hasText })`. Accepts the full text-matcher body
  /// (`"quoted"i` / `"quoted"s` / `/regex/flags` via the injected
  /// `createTextMatcher`) — unlike the user-facing `has-text` CSS
  /// engine, which is a lax string-only substring match. The two must
  /// not be collapsed: routing this body to `has-text` silently drops
  /// regex and case-sensitivity semantics.
  InternalHasText,
  /// Playwright-native `internal:has-not-text` engine
  /// (`filter({ hasNotText })`). Same body format as
  /// [`Engine::InternalHasText`].
  InternalHasNotText,
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
  /// Playwright-native `internal:describe` engine emitted by
  /// `locator.describe(description)`. Pure metadata: the injected
  /// engine passes the current element through untouched, so matching
  /// is unaffected. Body is the JSON-quoted description.
  InternalDescribe,
  /// Any other engine Playwright's injected registry resolves natively
  /// (`:light` variants, `data-testid` aliases, `aria-ref`,
  /// `internal:control`, `internal:chain`). The name is forwarded
  /// verbatim; matching happens in the injected engine.
  Other(String),
}

// ─── Detector ───────────────────────────────────────────────────────────────

/// Check if a selector string uses the rich engine format (not plain CSS).
#[must_use]
pub fn is_rich_selector(s: &str) -> bool {
  let trimmed = s.trim();
  // Chaining operator (Playwright splits on `>>` regardless of spacing)
  if trimmed.contains(">>") {
    return true;
  }
  // Explicit engine prefix (`name=`, optionally `*`-captured)
  if let Some((raw_name, _)) = trimmed.split_once('=') {
    let raw_name = raw_name.trim();
    if is_engine_name(raw_name) {
      let name = raw_name.strip_prefix('*').unwrap_or(raw_name);
      if engine_by_name(name).is_some() {
        return true;
      }
    }
  }
  // Playwright auto-detected engines (quoted text, bare xpath) need the
  // selector engine too — a backend CSS query would silently match nothing.
  let bytes = trimmed.as_bytes();
  if trimmed.len() > 1
    && ((bytes[0] == b'"' && bytes[trimmed.len() - 1] == b'"')
      || (bytes[0] == b'\'' && bytes[trimmed.len() - 1] == b'\''))
  {
    return true;
  }
  if trimmed.trim_start_matches('(').starts_with("//") || trimmed.starts_with("..") {
    return true;
  }
  false
}

// ─── Parser ─────────────────────────────────────────────────────────────────

/// Parse a selector string into a Selector AST. Mirrors Playwright's
/// `parseSelectorString`
/// (`/tmp/playwright/packages/isomorphic/selectorParser.ts:154`):
/// same tokenizer, engine-name detection, `*` capture modifier, and
/// text/xpath/css auto-detection.
///
/// # Errors
///
/// Returns an error if the selector string is empty, has an invalid
/// chain, names an unknown engine, or uses more than one `*` capture.
pub fn parse(selector: &str) -> Result<Selector> {
  let original = selector;
  let selector = selector.trim();
  if selector.is_empty() {
    return Err(FerriError::invalid_selector(selector, "selector cannot be empty"));
  }

  // Split by >> respecting quoted strings
  let raw_parts = split_by_chain(selector);
  let mut parts = Vec::new();
  let mut has_capture = false;

  for raw in raw_parts {
    let raw = raw.trim();
    if raw.is_empty() {
      return Err(FerriError::invalid_selector(selector, "empty selector part in chain"));
    }
    let part = parse_part(original, raw)?;
    if part.capture {
      if has_capture {
        return Err(FerriError::invalid_selector(
          original,
          "Only one of the selectors can capture using * modifier",
        ));
      }
      has_capture = true;
    }
    parts.push(part);
  }

  Ok(Selector { parts })
}

/// Playwright quirk (`selectorParser.ts:202`): inside a `text=` body that
/// already has content, a quote character is literal text, not a quote
/// opener — `text=some "quoted" >> css=div` must still split at `>>`.
fn should_ignore_text_selector_quote(prefix: &str) -> bool {
  let t = prefix.trim_start();
  let Some(rest) = t.strip_prefix("text") else {
    return false;
  };
  let rest = rest.trim_start();
  let Some(body) = rest.strip_prefix('=') else {
    return false;
  };
  !body.is_empty()
}

fn split_by_chain(s: &str) -> Vec<&str> {
  // Fast path: no chain operator, avoid scanning
  if !s.contains(">>") {
    let t = s.trim();
    return if t.is_empty() { Vec::new() } else { vec![t] };
  }

  // Cuts land only on ASCII bytes, so `get` never misses a boundary.
  let slice = |from: usize, to: usize| s.get(from..to).unwrap_or_default();
  let mut parts = Vec::new();
  let bytes = s.as_bytes();
  let mut start = 0;
  let mut i = 0;
  let mut in_quote: u8 = 0; // 0 = none, b'"', b'\'', or b'`'

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

    if (c == b'"' || c == b'\'' || c == b'`') && !should_ignore_text_selector_quote(slice(start, i)) {
      in_quote = c;
      i += 1;
      continue;
    }

    if c == b'>' && i + 1 < bytes.len() && bytes[i + 1] == b'>' {
      let part = slice(start, i).trim();
      if !part.is_empty() {
        parts.push(part);
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

  let part = slice(start, s.len()).trim();
  if !part.is_empty() {
    parts.push(part);
  }

  parts
}

/// Map an explicit engine name (the text before `=`) to its engine.
///
/// `internal:has`, `internal:has-text`, `internal:has-not`,
/// `internal:has-not-text` are aliases Playwright emits from
/// `client/locator.ts` (`filter({ hasText, has, ... })`). They are the
/// same semantics as the non-prefixed engines on the server side —
/// see `/tmp/playwright/packages/playwright-core/src/server/selectors.ts:42-43`.
///
/// Names without a dedicated variant that Playwright's injected registry
/// knows (`injectedScript.ts:214-244`: the `:light` variants,
/// `data-testid`/`data-test-id`/`data-test`, `aria-ref`,
/// `internal:control`, `internal:chain`) pass through as
/// [`Engine::Other`] — the injected engine resolves them natively.
fn engine_by_name(name: &str) -> Option<Engine> {
  let engine = match name {
    "role" => Engine::Role,
    "text" => Engine::Text,
    "testid" => Engine::TestId,
    "label" => Engine::Label,
    "placeholder" => Engine::Placeholder,
    "alt" => Engine::Alt,
    "title" => Engine::Title,
    "xpath" => Engine::XPath,
    "id" => Engine::Id,
    "css" => Engine::Css,
    "nth" => Engine::Nth,
    "visible" => Engine::Visible,
    "has" | "internal:has" => Engine::Has,
    "has-text" => Engine::HasText,
    "internal:has-text" => Engine::InternalHasText,
    "has-not" | "internal:has-not" => Engine::HasNot,
    "has-not-text" => Engine::HasNotText,
    "internal:has-not-text" => Engine::InternalHasNotText,
    "internal:and" => Engine::InternalAnd,
    "internal:or" => Engine::InternalOr,
    "internal:text" => Engine::InternalText,
    "internal:label" => Engine::InternalLabel,
    "internal:attr" => Engine::InternalAttr,
    "internal:testid" => Engine::InternalTestId,
    "internal:role" => Engine::InternalRole,
    "internal:describe" => Engine::InternalDescribe,
    "css:light" | "xpath:light" | "text:light" | "id:light" | "data-testid" | "data-testid:light" | "data-test-id"
    | "data-test-id:light" | "data-test" | "data-test:light" | "aria-ref" | "internal:control" | "internal:chain" => {
      Engine::Other(name.to_string())
    },
    // A name `selectors.register` claimed. The parser only has to route
    // it: the engine itself is JS living in the page, because matching
    // needs the DOM — Rust owns parsing and authoring, the injected
    // engine owns matching.
    other if is_registered_selector_engine(other) => Engine::Other(other.to_string()),
    _ => return None,
  };
  Some(engine)
}

/// True when the name before `=` is shaped like an engine name —
/// Playwright's `/^[a-zA-Z_0-9-+:*]+$/` (`selectorParser.ts:164`).
fn is_engine_name(name: &str) -> bool {
  !name.is_empty()
    && name
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+' | b':' | b'*'))
}

fn parse_part(selector: &str, s: &str) -> Result<SelectorPart> {
  // Mirrors the `append()` closure of Playwright's `parseSelectorString`
  // (`/tmp/playwright/packages/isomorphic/selectorParser.ts:160-193`):
  // an engine-name prefix before `=` wins; then a fully quoted part is
  // the text engine (quotes kept in the body); then `//` (optionally
  // behind opening parentheses) or a leading `..` is xpath; everything
  // else is CSS. A leading `*` on the name marks the capture part.
  if let Some((raw_name, body)) = s.split_once('=') {
    let raw_name = raw_name.trim();
    if is_engine_name(raw_name) {
      let (capture, name) = match raw_name.strip_prefix('*') {
        Some(rest) => (true, rest),
        None => (false, raw_name),
      };
      let Some(engine) = engine_by_name(name) else {
        return Err(FerriError::invalid_selector(
          selector,
          format!("unknown selector engine \"{name}\""),
        ));
      };
      return Ok(SelectorPart {
        engine,
        body: body.to_string(),
        capture,
      });
    }
  }

  let bytes = s.as_bytes();
  if s.len() > 1
    && ((bytes[0] == b'"' && bytes[s.len() - 1] == b'"') || (bytes[0] == b'\'' && bytes[s.len() - 1] == b'\''))
  {
    return Ok(SelectorPart {
      engine: Engine::Text,
      body: s.to_string(),
      capture: false,
    });
  }
  if s.trim_start_matches('(').starts_with("//") || s.starts_with("..") {
    return Ok(SelectorPart {
      engine: Engine::XPath,
      body: s.to_string(),
      capture: false,
    });
  }

  Ok(SelectorPart {
    engine: Engine::Css,
    body: s.to_string(),
    capture: false,
  })
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
    if p.capture {
      buf.push_str(r#","capture":true"#);
    }
    buf.push('}');
  }
  buf.push(']');
  buf
}

/// Map Engine variant to its protocol string.
fn engine_str(engine: &Engine) -> &str {
  match engine {
    Engine::Other(name) => name,
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
    Engine::InternalHasText => "internal:has-text",
    Engine::InternalHasNotText => "internal:has-not-text",
    Engine::HasNot => "has-not",
    Engine::HasNotText => "has-not-text",
    Engine::InternalAnd => "internal:and",
    Engine::InternalOr => "internal:or",
    Engine::InternalText => "internal:text",
    Engine::InternalLabel => "internal:label",
    Engine::InternalAttr => "internal:attr",
    Engine::InternalTestId => "internal:testid",
    Engine::InternalRole => "internal:role",
    Engine::InternalDescribe => "internal:describe",
  }
}

/// Write a JSON-escaped string (with surrounding quotes) directly into `buf`.
/// Avoids the intermediate String allocation that `serde_json::to_string` would produce.
fn json_escape_string_into(buf: &mut String, s: &str) {
  use std::fmt::Write as _;

  buf.push('"');
  let mut run_start = 0;
  for (idx, ch) in s.char_indices() {
    let escaped = match ch {
      '"' => Some(r#"\""#),
      '\\' => Some(r"\\"),
      '\n' => Some(r"\n"),
      '\r' => Some(r"\r"),
      '\t' => Some(r"\t"),
      c if c.is_control() => {
        if let Some(clean) = s.get(run_start..idx) {
          buf.push_str(clean);
        }
        let _ = write!(buf, "\\u{:04x}", c as u32);
        run_start = idx + ch.len_utf8();
        continue;
      },
      _ => None,
    };
    if let Some(esc) = escaped {
      if let Some(clean) = s.get(run_start..idx) {
        buf.push_str(clean);
      }
      buf.push_str(esc);
      run_start = idx + ch.len_utf8();
    }
  }
  if let Some(tail) = s.get(run_start..) {
    buf.push_str(tail);
  }
  buf.push('"');
}
