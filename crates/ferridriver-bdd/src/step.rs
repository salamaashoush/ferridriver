//! Step definition types: `StepDef`, `StepParam`, `StepHandler`, `StepMatch`.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use regex::Regex;

use crate::world::BrowserWorld;

// ── Step kind ──

/// The Gherkin keyword associated with a step definition.
/// `Step` is keyword-agnostic (matches any keyword).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum StepKind {
  Given,
  When,
  Then,
  Step,
}

impl fmt::Display for StepKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Given => write!(f, "Given"),
      Self::When => write!(f, "When"),
      Self::Then => write!(f, "Then"),
      Self::Step => write!(f, "Step"),
    }
  }
}

// ── Step parameters ──

/// Typed parameter extracted from a cucumber expression match.
#[derive(Debug, Clone, PartialEq)]
pub enum StepParam {
  String(String),
  Int(i64),
  Float(f64),
  Word(String),
  Custom { type_name: String, value: String },
}

impl StepParam {
  pub fn as_string(&self) -> Option<String> {
    match self {
      Self::String(s) | Self::Word(s) => Some(s.clone()),
      Self::Int(i) => Some(i.to_string()),
      Self::Float(f) => Some(f.to_string()),
      Self::Custom { value, .. } => Some(value.clone()),
    }
  }

  pub fn as_int(&self) -> Option<i64> {
    match self {
      Self::Int(i) => Some(*i),
      Self::String(s) | Self::Word(s) => s.parse().ok(),
      Self::Float(f) => Some(*f as i64),
      Self::Custom { value, .. } => value.parse().ok(),
    }
  }

  pub fn as_float(&self) -> Option<f64> {
    match self {
      Self::Float(f) => Some(*f),
      Self::Int(i) => Some(*i as f64),
      Self::String(s) | Self::Word(s) => s.parse().ok(),
      Self::Custom { value, .. } => value.parse().ok(),
    }
  }
}

// ── Data table ──

pub use crate::data_table::DataTable;

// ── Step error ──

/// Error returned by a step handler.
#[derive(Debug, Clone)]
pub struct StepError {
  pub message: String,
  pub diff: Option<(String, String)>,
  /// When true, the step is not yet implemented (pending) rather than broken.
  pub pending: bool,
}

impl StepError {
  /// Create a pending step error (step not yet implemented).
  pub fn pending(message: impl Into<String>) -> Self {
    Self {
      message: message.into(),
      diff: None,
      pending: true,
    }
  }
}

impl fmt::Display for StepError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.message)?;
    if let Some((expected, actual)) = &self.diff {
      write!(f, "\n  expected: {expected}\n  actual:   {actual}")?;
    }
    Ok(())
  }
}

impl std::error::Error for StepError {}

impl From<String> for StepError {
  fn from(message: String) -> Self {
    Self { message, diff: None, pending: false }
  }
}

impl From<&str> for StepError {
  fn from(message: &str) -> Self {
    Self {
      message: message.to_string(),
      diff: None,
      pending: false,
    }
  }
}

// ── Step handler ──

/// Async step handler function signature.
pub type StepHandler = Arc<
  dyn for<'a> Fn(
      &'a mut BrowserWorld,
      Vec<StepParam>,
      Option<&'a DataTable>,
      Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<(), StepError>> + Send + 'a>>
    + Send
    + Sync,
>;

// ── Step location ──

/// Source location of a step definition (for diagnostics).
#[derive(Debug, Clone)]
pub struct StepLocation {
  pub file: &'static str,
  pub line: u32,
}

impl fmt::Display for StepLocation {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}:{}", self.file, self.line)
  }
}

// ── Step definition ──

/// A compiled step definition: expression + handler + metadata.
pub struct StepDef {
  /// The kind of step (Given/When/Then/Step).
  pub kind: StepKind,
  /// Original cucumber expression source string.
  pub expression: String,
  /// Compiled regex from the cucumber expression.
  pub regex: Regex,
  /// Expected parameter types extracted from the expression.
  pub param_types: Vec<crate::expression::ParamType>,
  /// Full parameter info (type + id) for named capture group resolution.
  pub param_infos: Vec<crate::expression::ParamInfo>,
  /// The async handler function.
  pub handler: StepHandler,
  /// Source location for diagnostics.
  pub location: StepLocation,
}

// ── Step match result ──

/// Result of successfully matching a step text against a `StepDef`.
pub struct StepMatch<'a> {
  pub def: &'a StepDef,
  pub params: Vec<StepParam>,
}

// ── Step match error ──

/// Error when no step definition matches, or multiple definitions match.
#[derive(Debug)]
pub enum MatchError {
  /// No step definition matched the text.
  Undefined {
    text: String,
    suggestions: Vec<String>,
  },
  /// Multiple step definitions matched the text (ambiguous).
  Ambiguous {
    text: String,
    matches: Vec<StepLocation>,
    expressions: Vec<String>,
  },
}

impl fmt::Display for MatchError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Undefined { text, suggestions } => {
        write!(f, "undefined step: \"{text}\"")?;
        if !suggestions.is_empty() {
          write!(f, "\n  did you mean:")?;
          for s in suggestions {
            write!(f, "\n    - {s}")?;
          }
        }
        Ok(())
      }
      Self::Ambiguous { text, matches, expressions } => {
        write!(f, "ambiguous step: \"{text}\" matched {} definitions:", matches.len())?;
        for (i, (loc, expr)) in matches.iter().zip(expressions.iter()).enumerate() {
          write!(f, "\n  {}. {} ({})", i + 1, expr, loc)?;
        }
        Ok(())
      }
    }
  }
}

impl std::error::Error for MatchError {}

// ── Inventory registration type ──

/// What the proc macros submit via `inventory::submit!`.
pub struct StepRegistration {
  pub kind: StepKind,
  pub expression: &'static str,
  pub handler_factory: fn() -> StepHandler,
  pub file: &'static str,
  pub line: u32,
  /// When true, `expression` is a raw regex pattern instead of a cucumber expression.
  pub is_regex: bool,
}

inventory::collect!(StepRegistration);

/// Convenience macro for submitting step registrations from proc macro expansion.
#[macro_export]
macro_rules! submit_step {
  ($name:ident, $kind:expr, $expr:expr, $handler:ident,) => {
    ferridriver_bdd::inventory::submit! {
      ferridriver_bdd::step::StepRegistration {
        kind: $kind,
        expression: $expr,
        handler_factory: $handler,
        file: file!(),
        line: line!(),
        is_regex: false,
      }
    }
  };
  ($name:ident, $kind:expr, $expr:expr, $handler:ident, regex = $is_regex:expr,) => {
    ferridriver_bdd::inventory::submit! {
      ferridriver_bdd::step::StepRegistration {
        kind: $kind,
        expression: $expr,
        handler_factory: $handler,
        file: file!(),
        line: line!(),
        is_regex: $is_regex,
      }
    }
  };
}
