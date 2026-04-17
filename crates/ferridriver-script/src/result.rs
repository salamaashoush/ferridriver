//! Structured result returned by `ScriptEngine::run`.

use crate::error::ScriptError;

/// Severity of a captured console entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsoleLevel {
  Log,
  Info,
  Warn,
  Error,
  Debug,
  /// Emitted by the engine itself (e.g., truncation notices).
  System,
}

/// One captured `console.*` call from inside the script.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConsoleEntry {
  pub level: ConsoleLevel,
  pub message: String,
  /// Milliseconds since the script started running.
  pub ts_ms: u64,
}

/// Payload returned by a successful script.
///
/// `value` is the JSON-serialized return of the script's top-level expression
/// (or `null` if nothing was returned).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScriptSuccess {
  pub value: serde_json::Value,
}

/// Full result of running a script.
///
/// Regardless of success or failure, `console` and `duration_ms` are populated.
/// On failure, `outcome` carries a structured `ScriptError`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScriptResult {
  #[serde(flatten)]
  pub outcome: Outcome,
  pub duration_ms: u64,
  pub console: Vec<ConsoleEntry>,
}

/// Tagged-union representation of success vs failure for JSON output.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
  Ok {
    #[serde(flatten)]
    success: ScriptSuccess,
  },
  Error {
    error: ScriptError,
  },
}

impl ScriptResult {
  #[must_use]
  pub fn ok(value: serde_json::Value, duration_ms: u64, console: Vec<ConsoleEntry>) -> Self {
    Self {
      outcome: Outcome::Ok {
        success: ScriptSuccess { value },
      },
      duration_ms,
      console,
    }
  }

  #[must_use]
  pub fn err(error: ScriptError, duration_ms: u64, console: Vec<ConsoleEntry>) -> Self {
    Self {
      outcome: Outcome::Error { error },
      duration_ms,
      console,
    }
  }

  #[must_use]
  pub fn is_ok(&self) -> bool {
    matches!(self.outcome, Outcome::Ok { .. })
  }

  #[must_use]
  pub fn is_err(&self) -> bool {
    matches!(self.outcome, Outcome::Error { .. })
  }
}
