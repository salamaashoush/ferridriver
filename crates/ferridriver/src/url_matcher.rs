//! Playwright-compatible URL matching.
//!
//! Unifies the three ways a caller can express "which URLs does this apply to":
//!
//! * **Glob** — a string with `*` / `**` / `{a,b}` / `?` — the default for
//!   `page.route("**/api/*", ...)`. Converted to a regex at construction using
//!   the exact algorithm from
//!   `/tmp/playwright/packages/isomorphic/urlMatch.ts::globToRegexPattern`.
//! * **Regex** — a pre-compiled [`regex::Regex`]. Accepts the JS `RegExp`
//!   source/flags pair from the NAPI layer via [`UrlMatcher::regex_from_source`].
//! * **Predicate** — an arbitrary `Fn(&str) -> bool` closure. Not serializable;
//!   lives on whichever side constructed it.
//!
//! All call sites that take a URL filter — `page.route`, `context.route`,
//! `page.wait_for_url`, `page.wait_for_request`, `page.wait_for_response`,
//! and (once HAR recording lands) `context.route_from_har` — accept a
//! [`UrlMatcher`] so `string | RegExp | (url) => boolean` round-trips from
//! Playwright TS callers through NAPI into the Rust core unchanged.
//!
//! # Parity notes
//!
//! * Empty string and [`UrlMatcher::Any`] both match everything (Playwright:
//!   `match === undefined || match === ''`).
//! * `glob_to_regex_pattern` is a **byte-for-byte** port of Playwright's
//!   algorithm. Any divergence would break Playwright test fixtures that
//!   expect specific glob semantics (e.g. `**/x` vs `**x`, `{a,b}` groups).
//! * JS regex flags are translated to `regex` crate inline flags: `i`, `m`,
//!   `s` map to `(?i)`, `(?m)`, `(?s)`. `g` is a no-op in Rust (regex crate
//!   has no global-match state). `y` (sticky) and `u` (unicode — already on
//!   by default) are dropped with no error to avoid rejecting valid JS regex.

use std::fmt;
use std::sync::Arc;

use crate::error::{FerriError, Result};

/// Arbitrary URL predicate. Takes the full URL string and returns whether the
/// matcher accepts it. Must be `Send + Sync` because matchers cross task
/// boundaries in the backend route dispatcher.
pub type UrlPredicate = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Playwright-compatible URL matcher.
///
/// Construct with [`UrlMatcher::glob`], [`UrlMatcher::regex`],
/// [`UrlMatcher::regex_from_source`], [`UrlMatcher::predicate`], or
/// [`UrlMatcher::any`]. Use [`UrlMatcher::matches`] to test a URL.
#[derive(Clone)]
pub enum UrlMatcher {
  /// Matches every URL. Produced by the empty glob `""` (Playwright parity)
  /// and returned by [`UrlMatcher::any`].
  Any,
  /// Glob pattern: original source plus its compiled regex.
  Glob {
    /// Original glob source, preserved for equality checks (used by
    /// `unroute` to find the matching registration) and for diagnostic
    /// display.
    pattern: String,
    /// Pre-compiled regex — avoids paying `regex::Regex::new` on every match.
    regex: regex::Regex,
  },
  /// Raw regex. Constructed either from a [`regex::Regex`] directly or from
  /// a JS `RegExp` source+flags pair via [`UrlMatcher::regex_from_source`].
  Regex(regex::Regex),
  /// Caller-supplied predicate. Not serializable; only usable on the side
  /// of the NAPI boundary that constructed it.
  Predicate(UrlPredicate),
}

impl UrlMatcher {
  /// Matcher that accepts every URL.
  #[must_use]
  pub fn any() -> Self {
    Self::Any
  }

  /// Build a matcher from a Playwright-style glob string.
  ///
  /// An empty pattern collapses to [`UrlMatcher::Any`] so `page.route("", h)`
  /// behaves the same as the Playwright JS API.
  ///
  /// # Errors
  ///
  /// Returns [`FerriError::InvalidArgument`] if the glob, once converted to
  /// its regex form, cannot be compiled by the `regex` crate. In practice
  /// the Playwright conversion is total for every input string so this is
  /// a defensive branch.
  pub fn glob(pattern: impl Into<String>) -> Result<Self> {
    let pattern = pattern.into();
    if pattern.is_empty() {
      return Ok(Self::Any);
    }
    let regex_source = glob_to_regex_pattern(&pattern);
    let regex = regex::Regex::new(&regex_source).map_err(|e| {
      FerriError::invalid_argument(
        "url",
        format!("glob {pattern:?} (compiled as {regex_source:?}) is not a valid regex: {e}"),
      )
    })?;
    Ok(Self::Glob { pattern, regex })
  }

  /// Wrap a pre-compiled regex in a matcher.
  #[must_use]
  pub fn regex(re: regex::Regex) -> Self {
    Self::Regex(re)
  }

  /// Build a matcher from a JS `RegExp` source/flags pair as it arrives
  /// across NAPI.
  ///
  /// JS flags are translated to `regex` crate inline flags at the front of
  /// the pattern. `i`, `m`, `s` are honored; `g` is silently dropped (no
  /// equivalent in the `regex` crate); unknown flags yield
  /// [`FerriError::InvalidArgument`] so a typo on the TS side surfaces
  /// instead of being silently ignored.
  ///
  /// # Errors
  ///
  /// Returns [`FerriError::InvalidArgument`] for unknown flags or when the
  /// resulting regex fails to compile.
  pub fn regex_from_source(source: &str, flags: &str) -> Result<Self> {
    let mut inline_flags = String::new();
    for c in flags.chars() {
      match c {
        'i' | 'm' | 's' => inline_flags.push(c),
        // `g` (global) has no meaning for Rust's `regex` crate — it does
        // not track match position across calls. `u` (unicode) is on by
        // default in the `regex` crate. Both are silently dropped.
        'g' | 'u' => {},
        // `y` (sticky) has no equivalent; `d` (hasIndices) does not affect
        // match semantics. Reject rather than silently misbehave.
        other => {
          return Err(FerriError::invalid_argument(
            "url",
            format!("unsupported JS regex flag {other:?} (supported: i, m, s, g, u)"),
          ));
        },
      }
    }
    let pattern = if inline_flags.is_empty() {
      source.to_string()
    } else {
      format!("(?{inline_flags}){source}")
    };
    let regex = regex::Regex::new(&pattern)
      .map_err(|e| FerriError::invalid_argument("url", format!("regex {source:?} failed to compile: {e}")))?;
    Ok(Self::Regex(regex))
  }

  /// Build a matcher from a predicate closure.
  pub fn predicate<F>(f: F) -> Self
  where
    F: Fn(&str) -> bool + Send + Sync + 'static,
  {
    Self::Predicate(Arc::new(f))
  }

  /// Test whether this matcher accepts `url`.
  ///
  /// Regexes use `is_match` (unanchored) — Playwright's glob-to-regex adds
  /// explicit `^` / `$` anchors when the source is a glob, so an
  /// unanchored `is_match` still yields full-URL matching for globs while
  /// letting raw regex callers opt into substring matching.
  #[must_use]
  pub fn matches(&self, url: &str) -> bool {
    match self {
      Self::Any => true,
      Self::Glob { regex, .. } | Self::Regex(regex) => regex.is_match(url),
      Self::Predicate(f) => f(url),
    }
  }

  /// Return a human-readable identifier for this matcher.
  ///
  /// Used by `unroute` to find the registration that corresponds to the
  /// caller's pattern and by debug logs. Predicates get a synthetic label
  /// since function pointers don't have meaningful equality.
  #[must_use]
  pub fn identifier(&self) -> String {
    match self {
      Self::Any => String::new(),
      Self::Glob { pattern, .. } => pattern.clone(),
      Self::Regex(r) => r.as_str().to_string(),
      Self::Predicate(_) => "<predicate>".to_string(),
    }
  }

  /// A regex source suitable for a JS-side pre-filter (e.g. `WebKit`'s
  /// injected route interceptor), used when the matcher is routed through
  /// an out-of-process component that cannot evaluate arbitrary Rust
  /// predicates.
  ///
  /// `Any` and `Predicate` return `.*` so the JS side forwards every URL
  /// to Rust, where [`Self::matches`] performs the authoritative decision.
  /// `Glob` and `Regex` return their compiled pattern so the JS side can
  /// early-reject non-matching URLs without an IPC round-trip.
  #[must_use]
  pub fn regex_source_for_prefilter(&self) -> String {
    match self {
      Self::Any | Self::Predicate(_) => ".*".to_string(),
      Self::Glob { regex, .. } | Self::Regex(regex) => regex.as_str().to_string(),
    }
  }

  /// Matcher equality used by `unroute` to find prior registrations.
  ///
  /// Mirrors Playwright's `urlMatchesEqual`: two globs are equal iff their
  /// source patterns are identical; two regexes are equal iff their regex
  /// sources are identical; predicates compare by `Arc` pointer identity
  /// (two predicates from the same closure `.clone()` share the Arc and
  /// compare equal; two predicates from separate `UrlMatcher::predicate`
  /// calls do not — matching Playwright, where functions only compare to
  /// themselves).
  #[must_use]
  pub fn equivalent(&self, other: &Self) -> bool {
    match (self, other) {
      (Self::Any, Self::Any) => true,
      (Self::Glob { pattern: a, .. }, Self::Glob { pattern: b, .. }) => a == b,
      (Self::Regex(a), Self::Regex(b)) => a.as_str() == b.as_str(),
      (Self::Predicate(a), Self::Predicate(b)) => Arc::ptr_eq(a, b),
      _ => false,
    }
  }
}

impl fmt::Debug for UrlMatcher {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Any => write!(f, "UrlMatcher::Any"),
      Self::Glob { pattern, .. } => f.debug_struct("UrlMatcher::Glob").field("pattern", pattern).finish(),
      Self::Regex(r) => f.debug_tuple("UrlMatcher::Regex").field(&r.as_str()).finish(),
      Self::Predicate(_) => write!(f, "UrlMatcher::Predicate(<fn>)"),
    }
  }
}

/// Byte-for-byte port of Playwright's glob-to-regex conversion.
///
/// Source: `/tmp/playwright/packages/isomorphic/urlMatch.ts::globToRegexPattern`.
///
/// The algorithm:
/// 1. Prepend `^` anchor.
/// 2. Escape regex metacharacters.
/// 3. `*` → `[^/]*` (segment-only wildcard).
/// 4. `**` — context-sensitive:
///    * `/**/` collapses to `((.+/)|)` — lets the whole middle vanish.
///    * `**` at segment end becomes `(.*/)` if followed by `/` else `(.*)`.
///    * Standalone `**` not at a slash boundary still becomes `(.*)`.
/// 5. `{a,b,c}` → `(a|b|c)` alternation group.
/// 6. `\\x` — literal escape (emits `x` escaped if it's a regex metachar).
/// 7. Append `$` anchor.
#[must_use]
pub fn glob_to_regex_pattern(glob: &str) -> String {
  // Playwright's escapedChars set; every other char maps to itself.
  const ESCAPED: &[char] = &['$', '^', '+', '.', '*', '(', ')', '|', '\\', '?', '{', '}', '[', ']'];
  let chars: Vec<char> = glob.chars().collect();
  let mut out = String::with_capacity(glob.len() * 2 + 2);
  out.push('^');
  let mut in_group = false;
  let mut i = 0;
  while i < chars.len() {
    let c = chars[i];
    if c == '\\' && i + 1 < chars.len() {
      i += 1;
      let esc = chars[i];
      if ESCAPED.contains(&esc) {
        out.push('\\');
      }
      out.push(esc);
      i += 1;
      continue;
    }
    if c == '*' {
      let char_before = if i == 0 { None } else { Some(chars[i - 1]) };
      let mut star_count = 1;
      while i + 1 < chars.len() && chars[i + 1] == '*' {
        star_count += 1;
        i += 1;
      }
      if star_count > 1 {
        let char_after = chars.get(i + 1).copied();
        // Playwright: match either /..something../ or /.
        if char_after == Some('/') {
          if char_before == Some('/') {
            out.push_str("((.+/)|)");
          } else {
            out.push_str("(.*/)");
          }
          i += 1; // consume the trailing '/'
        } else {
          out.push_str("(.*)");
        }
      } else {
        out.push_str("([^/]*)");
      }
      i += 1;
      continue;
    }
    match c {
      '{' => {
        in_group = true;
        out.push('(');
      },
      '}' => {
        in_group = false;
        out.push(')');
      },
      ',' => {
        if in_group {
          out.push('|');
        } else {
          out.push('\\');
          out.push(c);
        }
      },
      _ => {
        if ESCAPED.contains(&c) {
          out.push('\\');
        }
        out.push(c);
      },
    }
    i += 1;
  }
  out.push('$');
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  // ── glob_to_regex_pattern parity vectors ────────────────────────────────
  // Each vector is checked against what Playwright's JS function produces for
  // the same input. Any drift means we are no longer compatible with
  // fixtures that rely on a specific glob semantic.

  #[test]
  fn empty_glob_yields_anchor_only() {
    assert_eq!(glob_to_regex_pattern(""), "^$");
  }

  #[test]
  fn single_star_is_segment_wildcard() {
    assert_eq!(glob_to_regex_pattern("*"), "^([^/]*)$");
  }

  #[test]
  fn double_star_standalone_is_any() {
    assert_eq!(glob_to_regex_pattern("**"), "^(.*)$");
  }

  #[test]
  fn slash_double_star_slash_collapses() {
    assert_eq!(glob_to_regex_pattern("/**/"), "^/((.+/)|)$");
  }

  #[test]
  fn leading_double_star_slash_matches_everything_including_empty() {
    assert_eq!(glob_to_regex_pattern("**/api"), "^(.*/)api$");
  }

  #[test]
  fn regex_metachars_are_escaped() {
    assert_eq!(glob_to_regex_pattern("a.b+c"), "^a\\.b\\+c$");
  }

  #[test]
  fn brace_group_becomes_alternation() {
    assert_eq!(glob_to_regex_pattern("*.{png,jpg}"), "^([^/]*)\\.(png|jpg)$");
  }

  #[test]
  fn comma_outside_group_is_literal() {
    assert_eq!(glob_to_regex_pattern("a,b"), "^a\\,b$");
  }

  #[test]
  fn backslash_escape_of_metachar_produces_escaped_literal() {
    assert_eq!(glob_to_regex_pattern(r"\*"), "^\\*$");
  }

  #[test]
  fn backslash_escape_of_plain_char_produces_plain() {
    assert_eq!(glob_to_regex_pattern(r"\a"), "^a$");
  }

  #[test]
  fn playwright_canonical_api_glob_compiles_and_matches() {
    let m = UrlMatcher::glob("**/api/*").expect("valid glob");
    assert!(m.matches("https://example.com/api/users"));
    assert!(m.matches("https://example.com/v1/api/users"));
    assert!(!m.matches("https://example.com/api/users/123")); // single * stops at /
    assert!(m.matches("/api/x"));
  }

  // ── UrlMatcher::matches semantics ───────────────────────────────────────

  #[test]
  fn any_matches_all() {
    let m = UrlMatcher::any();
    assert!(m.matches(""));
    assert!(m.matches("https://example.com"));
  }

  #[test]
  fn empty_glob_collapses_to_any() {
    let m = UrlMatcher::glob("").unwrap();
    assert!(matches!(m, UrlMatcher::Any));
  }

  #[test]
  fn regex_substring_match_when_unanchored() {
    let re = regex::Regex::new(r"/api/").unwrap();
    let m = UrlMatcher::regex(re);
    assert!(m.matches("https://example.com/api/users"));
    assert!(!m.matches("https://example.com/rest/users"));
  }

  #[test]
  fn regex_from_source_with_case_insensitive_flag() {
    let m = UrlMatcher::regex_from_source(r"/API/", "i").unwrap();
    assert!(m.matches("https://example.com/api/x"));
    assert!(m.matches("https://example.com/API/x"));
  }

  #[test]
  fn regex_from_source_global_flag_is_accepted_and_ignored() {
    let m = UrlMatcher::regex_from_source(r"/api/", "g").unwrap();
    assert!(m.matches("https://example.com/api/x"));
  }

  #[test]
  fn regex_from_source_unknown_flag_rejected() {
    let err = UrlMatcher::regex_from_source(r"/api/", "x").unwrap_err();
    assert!(matches!(err, FerriError::InvalidArgument { .. }));
  }

  #[test]
  fn predicate_matcher_invokes_closure() {
    // Use a simple substring check — we're testing closure invocation, not
    // path-extension logic.
    let m = UrlMatcher::predicate(|url| url.contains("/api/"));
    assert!(m.matches("https://example.com/api/users"));
    assert!(!m.matches("https://example.com/static/users"));
  }

  #[test]
  fn identifier_of_each_variant() {
    assert_eq!(UrlMatcher::any().identifier(), "");
    assert_eq!(UrlMatcher::glob("**/api").unwrap().identifier(), "**/api");
    assert_eq!(UrlMatcher::regex(regex::Regex::new("x").unwrap()).identifier(), "x");
    assert_eq!(UrlMatcher::predicate(|_| true).identifier(), "<predicate>");
  }

  #[test]
  fn equivalent_same_glob_source() {
    let a = UrlMatcher::glob("**/api").unwrap();
    let b = UrlMatcher::glob("**/api").unwrap();
    assert!(a.equivalent(&b));
  }

  #[test]
  fn equivalent_different_glob_source() {
    let a = UrlMatcher::glob("**/api").unwrap();
    let b = UrlMatcher::glob("**/v2").unwrap();
    assert!(!a.equivalent(&b));
  }

  #[test]
  fn equivalent_same_regex_source() {
    let a = UrlMatcher::regex(regex::Regex::new("x").unwrap());
    let b = UrlMatcher::regex(regex::Regex::new("x").unwrap());
    assert!(a.equivalent(&b));
  }

  #[test]
  fn equivalent_predicate_is_pointer_identity() {
    let a = UrlMatcher::predicate(|u| u.contains("/api/"));
    let b = a.clone();
    assert!(a.equivalent(&b));
    let c = UrlMatcher::predicate(|u| u.contains("/api/"));
    assert!(!a.equivalent(&c)); // different Arc, even though logically same.
  }

  #[test]
  fn cross_variant_not_equivalent() {
    let g = UrlMatcher::glob("x").unwrap();
    let r = UrlMatcher::regex(regex::Regex::new("x").unwrap());
    assert!(!g.equivalent(&r));
  }
}
