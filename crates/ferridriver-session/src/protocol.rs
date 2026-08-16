//! Wire protocol for the session socket.
//!
//! A client sends one [`Command`] frame. The server answers with zero or more
//! [`ServerFrame::Event`] frames while the command runs, then exactly one
//! [`ServerFrame::Response`]. Frames are JSON values terminated by a single NUL
//! (`\x00`) byte — the same framing ferridriver already speaks to `WebKit`'s
//! inspector pipe, chosen here for the same reasons: compact, dependency-free,
//! and trivially debuggable with `socat`/`nc`.
//!
//! The protocol is deliberately verb-agnostic: the [`Command::verb`] string
//! and free-form [`Command::args`] object are interpreted by the host's
//! [`crate::Dispatcher`], so adding a new verb never touches this module.
//! In practice a bound browser understands one verb, `run`, whose args are a
//! [`ScriptRequest`] — the session surface IS the scripting surface.

use serde::{Deserialize, Serialize};

/// A single request from a client to a session server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Command {
  /// Correlates the response to this request. The client assigns it; the
  /// server echoes it back. Sequential per connection.
  pub id: u64,
  /// The action to perform — [`RUN_VERB`] for a bound browser. Interpreted by
  /// the host [`crate::Dispatcher`].
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

/// The only verb a bound browser understands: run a script against it.
pub const RUN_VERB: &str = "run";

/// How the host should treat [`ScriptRequest::code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptKind {
  /// A plain script body, where a top-level `return` yields the result.
  #[default]
  Source,
  /// A bundled ES module whose `default` export is the result. The client
  /// bundles (so relative imports resolve against ITS working directory) and
  /// the host compiles, which keeps `QuickJS` bytecode from ever crossing the
  /// wire between differently-built binaries.
  Module,
}

/// The `run` verb's arguments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptRequest {
  #[serde(default)]
  pub kind: ScriptKind,
  /// Script body, or bundled ES module source for [`ScriptKind::Module`].
  pub code: String,
  /// Source map JSON for a bundled module, so host-side stack frames point
  /// back at the client's original `.ts`/`.js` files.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub source_map: Option<String>,
  /// Module label used in stack frames. Defaults host-side when absent.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub module_name: Option<String>,
  /// Positional arguments exposed to the script as the `args` global.
  #[serde(default)]
  pub args: Vec<serde_json::Value>,
  /// Wall-clock budget for this run.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub timeout_ms: Option<u64>,
  /// Stream every browser action ([`EventPayload::Action`]) as it happens.
  /// Off by default: an untraced run should not pay for the observer.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub trace: bool,
  /// Stream the source of each action the script performs
  /// ([`EventPayload::Code`]), in this language (`ts`, `rust`, `gherkin`).
  /// `None` — the default — emits none.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub code_language: Option<String>,
  /// Report the page the context is left on ([`EventPayload::Page`]) once the
  /// script finishes. Off by default: reading the title is a round-trip a run
  /// that does not want the report should not pay for.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub page_state: bool,
}

impl ScriptRequest {
  /// A plain-source request with no args and no timeout.
  pub fn source(code: impl Into<String>) -> Self {
    Self {
      kind: ScriptKind::Source,
      code: code.into(),
      source_map: None,
      module_name: None,
      args: Vec::new(),
      timeout_ms: None,
      trace: false,
      code_language: None,
      page_state: false,
    }
  }
}

/// A frame sent by the server. Console output streams as [`ServerFrame::Event`]
/// while a command runs; exactly one [`ServerFrame::Response`] ends it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
  Event(Event),
  Response(Response),
}

/// An out-of-band notification emitted while a command is still running.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
  /// The [`Command::id`] this event belongs to.
  pub id: u64,
  #[serde(flatten)]
  pub payload: EventPayload,
}

/// What an [`Event`] carries.
///
/// `console` levels are the same lowercase names `ScriptResult` serializes
/// (`log`, `info`, `warn`, `error`, `debug`, `trace`, `system`), so a client
/// that already knows the scripting crate can decode them straight into its
/// own console type. This crate stays below the scripting layer and treats the
/// level as an opaque string.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventPayload {
  Console {
    level: String,
    message: String,
    ts_ms: u64,
  },
  /// One browser action (`page.*`, `locator.*`, `expect.*`) starting,
  /// logging a line, or finishing. Sent only when the request asked for it
  /// ([`ScriptRequest::trace`]).
  Action {
    phase: ActionPhase,
    /// `call@N`, so the frames of one action can be paired up.
    call_id: String,
    /// Display title (`page.goto`).
    title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// The call-log line, for [`ActionPhase::Log`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    /// `file:line` the call was written at, when the host captured one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    location: Option<String>,
  },
  /// One line of source reproducing an action the script just performed, in
  /// the language the request asked for. Sent only when the request set
  /// [`ScriptRequest::code_language`].
  Code {
    line: String,
  },
  /// The page the context is left on once the run finished. Sent once, after
  /// the last action, and only when the request set
  /// [`ScriptRequest::page_state`].
  Page {
    url: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    console_errors: usize,
    #[serde(default)]
    console_warnings: usize,
    #[serde(default)]
    page_errors: usize,
  },
}

/// Which edge of an action an [`EventPayload::Action`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionPhase {
  Begin,
  Log,
  End,
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
      error: None,
    }
  }

  /// A successful response whose text is a JSON document (the serialized
  /// script result). Kept distinct from [`Response::ok`] at the call site so
  /// the intent — "this text is structured, parse it" — is visible.
  #[must_use]
  pub fn ok_json(id: u64, value: &serde_json::Value) -> Self {
    Self::ok(id, value.to_string())
  }

  /// A failure response.
  pub fn err(id: u64, error: impl Into<String>) -> Self {
    Self {
      id,
      ok: false,
      text: String::new(),
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
    let cmd = Command::new(1, RUN_VERB, serde_json::json!({ "code": "return 1" })).with_context(Some("admin".into()));
    let back: Command = serde_json::from_str(&serde_json::to_string(&cmd).unwrap()).unwrap();
    assert_eq!(back.context.as_deref(), Some("admin"));
    assert_eq!(back.args["code"], "return 1");
  }

  #[test]
  fn script_request_defaults_to_source_kind() {
    let req: ScriptRequest = serde_json::from_value(serde_json::json!({ "code": "return 1" })).unwrap();
    assert_eq!(req.kind, ScriptKind::Source);
    assert!(req.args.is_empty());
    assert!(req.timeout_ms.is_none());
  }

  #[test]
  fn script_request_module_roundtrips() {
    let req = ScriptRequest {
      kind: ScriptKind::Module,
      code: "export default 1".into(),
      source_map: Some("{}".into()),
      module_name: Some("run.js".into()),
      args: vec![serde_json::json!("a")],
      timeout_ms: Some(500),
      trace: true,
      code_language: Some("rust".into()),
      page_state: true,
    };
    let back: ScriptRequest = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
    assert_eq!(back.kind, ScriptKind::Module);
    assert_eq!(back.module_name.as_deref(), Some("run.js"));
    assert_eq!(back.timeout_ms, Some(500));
    assert!(back.page_state);
  }

  #[test]
  fn a_page_event_roundtrips_and_an_older_client_defaults_its_fields() {
    let wire = serde_json::to_string(&ServerFrame::Event(Event {
      id: 2,
      payload: EventPayload::Page {
        url: "https://example.com/".into(),
        title: "Example".into(),
        console_errors: 1,
        console_warnings: 0,
        page_errors: 2,
      },
    }))
    .unwrap();
    assert!(wire.contains("\"event\":\"page\""), "{wire}");
    match serde_json::from_str::<ServerFrame>(&wire).unwrap() {
      ServerFrame::Event(Event {
        payload: EventPayload::Page { url, page_errors, .. },
        ..
      }) => {
        assert_eq!(url, "https://example.com/");
        assert_eq!(page_errors, 2);
      },
      other => panic!("page event decoded as {other:?}"),
    }

    // A host built before the counts existed sends only the url; decoding
    // must not fail the whole run over a missing tally.
    let minimal: EventPayload = serde_json::from_value(serde_json::json!({
      "event": "page",
      "url": "https://example.com/",
    }))
    .expect("url alone decodes");
    assert!(matches!(minimal, EventPayload::Page { console_errors: 0, .. }));
  }

  #[test]
  fn server_frames_are_distinguishable_on_the_wire() {
    let event = ServerFrame::Event(Event {
      id: 9,
      payload: EventPayload::Console {
        level: "warn".into(),
        message: "careful".into(),
        ts_ms: 12,
      },
    });
    let wire = serde_json::to_string(&event).unwrap();
    assert!(wire.contains("\"type\":\"event\""), "{wire}");
    assert!(wire.contains("\"event\":\"console\""), "{wire}");
    match serde_json::from_str::<ServerFrame>(&wire).unwrap() {
      ServerFrame::Event(e) => {
        assert_eq!(e.id, 9);
        let EventPayload::Console { level, message, ts_ms } = e.payload else {
          panic!("console payload decoded as another variant");
        };
        assert_eq!((level.as_str(), message.as_str(), ts_ms), ("warn", "careful", 12));
      },
      ServerFrame::Response(_) => panic!("event frame decoded as a response"),
    }

    let response = ServerFrame::Response(Response::ok(9, "done"));
    let wire = serde_json::to_string(&response).unwrap();
    assert!(wire.contains("\"type\":\"response\""), "{wire}");
    assert!(matches!(
      serde_json::from_str::<ServerFrame>(&wire).unwrap(),
      ServerFrame::Response(r) if r.text == "done"
    ));
  }

  #[test]
  fn response_variants_roundtrip() {
    let ok = Response::ok(3, "done");
    let back: Response = serde_json::from_str(&serde_json::to_string(&ok).unwrap()).unwrap();
    assert!(back.ok && back.error.is_none());

    let json = Response::ok_json(4, &serde_json::json!({ "status": "ok" }));
    let back: Response = serde_json::from_str(&serde_json::to_string(&json).unwrap()).unwrap();
    assert_eq!(
      serde_json::from_str::<serde_json::Value>(&back.text).unwrap()["status"],
      "ok"
    );

    let err = Response::err(5, "boom");
    let back: Response = serde_json::from_str(&serde_json::to_string(&err).unwrap()).unwrap();
    assert!(!back.ok);
    assert_eq!(back.error.as_deref(), Some("boom"));
  }
}
