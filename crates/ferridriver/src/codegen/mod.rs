//! Interactive code recorder (codegen).
//!
//! Opens a headed browser, records user interactions, and generates test code
//! in real-time. Supports Rust, TypeScript, and Gherkin output formats.
//!
//! Usage: `ferridriver codegen <url> [--language rust|typescript|gherkin]`

pub mod emitter;
pub mod recorder;

/// A recorded user action.
///
/// Deserialized directly from the browser-side JSON via `serde(tag = "type")`.
/// `selector` is the raw Playwright selector string (e.g., `internal:role=button[name="Submit"]`).
/// `locator` is the human-readable form from `asLocator()` (e.g., `getByRole('button', { name: 'Submit' })`).
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Action {
  Navigate {
    url: String,
  },
  Click {
    selector: String,
    locator: String,
  },
  Dblclick {
    selector: String,
    locator: String,
  },
  Fill {
    selector: String,
    locator: String,
    value: String,
  },
  Press {
    selector: String,
    locator: String,
    key: String,
  },
  Select {
    selector: String,
    locator: String,
    value: String,
  },
  Check {
    selector: String,
    locator: String,
  },
  Uncheck {
    selector: String,
    locator: String,
  },
}

/// Output language for code generation.
#[derive(Debug, Clone, Copy, Default)]
pub enum OutputLanguage {
  #[default]
  Rust,
  TypeScript,
  Gherkin,
}

impl OutputLanguage {
  /// Parse from CLI string.
  #[must_use]
  pub fn from_str(s: &str) -> Self {
    match s {
      "typescript" | "ts" => Self::TypeScript,
      "gherkin" | "feature" | "bdd" => Self::Gherkin,
      _ => Self::Rust,
    }
  }
}
