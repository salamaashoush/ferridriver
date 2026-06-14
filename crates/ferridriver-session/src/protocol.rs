//! Wire protocol for the session socket.
//!
//! Every exchange is one [`Command`] frame answered by exactly one
//! [`Response`] frame. Frames are JSON values terminated by a single NUL
//! (`\x00`) byte — the same framing ferridriver already speaks to `WebKit`'s
//! inspector pipe, chosen here for the same reasons: compact, dependency-free,
//! and trivially debuggable with `socat`/`nc`.
//!
//! The protocol is deliberately verb-agnostic: the [`Command::verb`] string
//! and free-form [`Command::args`] object are interpreted by the host's
//! [`crate::Dispatcher`], so adding a new CLI verb never touches this module.

use serde::{Deserialize, Serialize};

/// A single request from a client to a session server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Command {
  /// Correlates the response to this request. The client assigns it; the
  /// server echoes it back. Sequential per connection.
  pub id: u64,
  /// The action to perform (`snapshot`, `goto`, `click`, `eval`, ...).
  /// Interpreted by the host [`crate::Dispatcher`].
  pub verb: String,
  /// Browser context within the bound browser to act on. `None` targets the
  /// session's default context. Mirrors the `:context` half of an MCP
  /// session key.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub context: Option<String>,
  /// Verb-specific arguments. Shape is the verb's contract, validated by the
  /// dispatcher, not here.
  #[serde(default)]
  pub args: serde_json::Value,
}

impl Command {
  /// Build a command with no context and the given args.
  pub fn new(id: u64, verb: impl Into<String>, args: serde_json::Value) -> Self {
    Self {
      id,
      verb: verb.into(),
      context: None,
      args,
    }
  }

  /// Set the target context (builder style).
  #[must_use]
  pub fn with_context(mut self, context: Option<String>) -> Self {
    self.context = context;
    self
  }
}

/// The server's answer to a [`Command`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
  /// Echoes [`Command::id`] so the client can match it.
  pub id: u64,
  /// `true` when the verb succeeded; `false` carries [`Response::error`].
  pub ok: bool,
  /// Human / agent readable result text (a snapshot, a status line, an
  /// evaluation result). Always present on success; empty allowed.
  #[serde(default)]
  pub text: String,
  /// Base64-encoded binary payload for verbs that produce bytes
  /// (`screenshot`, `pdf`). `None` for text-only verbs.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub data: Option<String>,
  /// Failure detail when `ok` is `false`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub error: Option<String>,
}

impl Response {
  /// A successful text response.
  pub fn ok(id: u64, text: impl Into<String>) -> Self {
    Self {
      id,
      ok: true,
      text: text.into(),
      data: None,
      error: None,
    }
  }

  /// A successful response carrying base64 binary data plus a status line.
  pub fn ok_data(id: u64, text: impl Into<String>, data: impl Into<String>) -> Self {
    Self {
      id,
      ok: true,
      text: text.into(),
      data: Some(data.into()),
      error: None,
    }
  }

  /// A failure response.
  pub fn err(id: u64, error: impl Into<String>) -> Self {
    Self {
      id,
      ok: false,
      text: String::new(),
      data: None,
      error: Some(error.into()),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn command_omits_empty_context_and_roundtrips() {
    let cmd = Command::new(7, "snapshot", serde_json::json!({}));
    let wire = serde_json::to_string(&cmd).unwrap();
    assert!(!wire.contains("context"), "absent context must not serialize: {wire}");
    let back: Command = serde_json::from_str(&wire).unwrap();
    assert_eq!(back.id, 7);
    assert_eq!(back.verb, "snapshot");
    assert!(back.context.is_none());
  }

  #[test]
  fn command_with_context_roundtrips() {
    let cmd = Command::new(1, "click", serde_json::json!({ "selector": "#go" })).with_context(Some("admin".into()));
    let back: Command = serde_json::from_str(&serde_json::to_string(&cmd).unwrap()).unwrap();
    assert_eq!(back.context.as_deref(), Some("admin"));
    assert_eq!(back.args["selector"], "#go");
  }

  #[test]
  fn response_variants_roundtrip() {
    let ok = Response::ok(3, "done");
    let back: Response = serde_json::from_str(&serde_json::to_string(&ok).unwrap()).unwrap();
    assert!(back.ok && back.error.is_none() && back.data.is_none());

    let data = Response::ok_data(4, "captured", "QUJD");
    let back: Response = serde_json::from_str(&serde_json::to_string(&data).unwrap()).unwrap();
    assert_eq!(back.data.as_deref(), Some("QUJD"));

    let err = Response::err(5, "boom");
    let back: Response = serde_json::from_str(&serde_json::to_string(&err).unwrap()).unwrap();
    assert!(!back.ok);
    assert_eq!(back.error.as_deref(), Some("boom"));
  }
}
