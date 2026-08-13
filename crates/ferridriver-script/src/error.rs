//! Script execution errors with source-level diagnostics.

use std::fmt;

/// Kind of failure a script can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ScriptError {
  pub kind: ScriptErrorKind,
  /// The thrown value's JS constructor name (`TypeError`, ...) when the
  /// failure came from a JS exception. Hosts render `name: message` the way
  /// every JS runtime does; `None` for engine-level failures.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub name: Option<String>,
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
  /// Replace declared secret values everywhere this error carries text.
  ///
  /// `source_snippet` is the reason this cannot be left to the caller: it
  /// quotes the script's own source around the throwing line, so a failure
  /// inside `page.fill('#pw', 'hunter2')` prints the credential back even
  /// when neither the message nor the stack mentions it.
  pub fn redact(&mut self, secrets: &ferridriver::response::Secrets) {
    if secrets.is_empty() {
      return;
    }
    let apply = |text: &mut String| {
      if let std::borrow::Cow::Owned(redacted) = secrets.redact(text) {
        *text = redacted;
      }
    };
    apply(&mut self.message);
    for field in [&mut self.name, &mut self.stack, &mut self.source_snippet] {
      if let Some(text) = field.as_mut() {
        apply(text);
      }
    }
  }

  #[must_use]
  pub fn internal(message: impl Into<String>) -> Self {
    Self {
      kind: ScriptErrorKind::Internal,
      name: None,
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
      name: None,
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
      name: None,
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
      name: None,
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
