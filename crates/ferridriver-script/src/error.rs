//! Script execution errors with source-level diagnostics.

use std::fmt;

/// Kind of failure a script can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptErrorKind {
  /// Source failed to parse.
  Syntax,
  /// Script threw an exception during execution.
  Runtime,
  /// Wall-clock timeout was exceeded.
  Timeout,
  /// `QuickJS` memory quota was exceeded.
  MemoryLimit,
  /// A sandboxed operation (e.g., `fs.readFile` with a traversal path) was rejected.
  SandboxViolation,
  /// Engine-level failure unrelated to user script (binding setup, module loader, etc.).
  Internal,
}

impl fmt::Display for ScriptErrorKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Syntax => write!(f, "syntax_error"),
      Self::Runtime => write!(f, "runtime_error"),
      Self::Timeout => write!(f, "timeout"),
      Self::MemoryLimit => write!(f, "memory_limit"),
      Self::SandboxViolation => write!(f, "sandbox_violation"),
      Self::Internal => write!(f, "internal_error"),
    }
  }
}

/// Structured error returned when a script fails.
///
/// `line`, `column`, and `source_snippet` are filled in whenever the `QuickJS`
/// runtime exposes them (syntax and runtime errors); they are `None` for
/// engine-level failures like timeouts.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScriptError {
  pub kind: ScriptErrorKind,
  pub message: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub stack: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub line: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub column: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub source_snippet: Option<String>,
}

impl ScriptError {
  #[must_use]
  pub fn internal(message: impl Into<String>) -> Self {
    Self {
      kind: ScriptErrorKind::Internal,
      message: message.into(),
      stack: None,
      line: None,
      column: None,
      source_snippet: None,
    }
  }

  #[must_use]
  pub fn timeout(elapsed_ms: u64, limit_ms: u64) -> Self {
    Self {
      kind: ScriptErrorKind::Timeout,
      message: format!("script exceeded timeout: {elapsed_ms}ms > {limit_ms}ms"),
      stack: None,
      line: None,
      column: None,
      source_snippet: None,
    }
  }

  #[must_use]
  pub fn memory_limit(limit_bytes: usize) -> Self {
    Self {
      kind: ScriptErrorKind::MemoryLimit,
      message: format!("script exceeded memory limit of {limit_bytes} bytes"),
      stack: None,
      line: None,
      column: None,
      source_snippet: None,
    }
  }

  #[must_use]
  pub fn sandbox(message: impl Into<String>) -> Self {
    Self {
      kind: ScriptErrorKind::SandboxViolation,
      message: message.into(),
      stack: None,
      line: None,
      column: None,
      source_snippet: None,
    }
  }
}

impl fmt::Display for ScriptError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "[{}] {}", self.kind, self.message)?;
    if let (Some(l), Some(c)) = (self.line, self.column) {
      write!(f, " (at {l}:{c})")?;
    }
    Ok(())
  }
}

impl std::error::Error for ScriptError {}
