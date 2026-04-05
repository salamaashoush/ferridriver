//! BDD step definitions — trait-based registry with self-documenting steps.

use crate::page::Page;
use async_trait::async_trait;
use regex::Regex;
use rustc_hash::FxHashMap as HashMap;

#[macro_use]
mod macros;
mod registry;

pub mod assertion;
pub mod cookie;
pub mod interaction;
pub mod javascript;
pub mod navigation;
pub mod screenshot;
pub mod storage;
pub mod variable;
pub mod wait;

pub use registry::StepRegistry;

/// Every step implements this trait.
#[async_trait]
pub trait StepDef: Send + Sync {
  fn description(&self) -> &'static str;
  fn category(&self) -> StepCategory;
  fn example(&self) -> &'static str;
  fn pattern(&self) -> &Regex;

  async fn execute(
    &self,
    page: &Page,
    caps: &regex::Captures<'_>,
    data_table: Option<&[Vec<String>]>,
    vars: &mut HashMap<String, String>,
  ) -> Result<Option<serde_json::Value>, String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum StepCategory {
  Navigation,
  Interaction,
  Wait,
  Assertion,
  Variable,
  Cookie,
  Storage,
  Screenshot,
  JavaScript,
}

/// Extract a quoted or bare string from a regex capture.
#[must_use]
pub fn q(s: &str) -> String {
  let s = s.trim();
  if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
    s[1..s.len() - 1].to_string()
  } else {
    s.to_string()
  }
}

/// Escape a string for safe embedding in JS single-quoted string literals.
/// Handles all characters that could break or inject into JS strings.
#[must_use]
pub fn js_escape(s: &str) -> String {
  let mut out = String::with_capacity(s.len() + 8);
  for c in s.chars() {
    match c {
      '\\' => out.push_str("\\\\"),
      '\'' => out.push_str("\\'"),
      '"' => out.push_str("\\\""),
      '`' => out.push_str("\\`"),
      '\n' => out.push_str("\\n"),
      '\r' => out.push_str("\\r"),
      '\t' => out.push_str("\\t"),
      '\0' => out.push_str("\\0"),
      // Unicode line/paragraph separators (would terminate JS string in some engines)
      '\u{2028}' => out.push_str("\\u2028"),
      '\u{2029}' => out.push_str("\\u2029"),
      _ => out.push(c),
    }
  }
  out
}

/// Find element using the selector engine (supports role=, text=, etc.)
/// or falls back to plain CSS for simple selectors.
///
/// # Errors
///
/// Returns an error if the element cannot be found using the given selector,
/// or if the underlying browser query fails.
pub async fn find(page: &Page, selector: &str) -> Result<crate::backend::AnyElement, String> {
  let inner = page.inner();
  if crate::selectors::is_rich_selector(selector) {
    crate::selectors::query_one(inner, selector, false).await
  } else {
    inner.find_element(selector).await
  }
}
