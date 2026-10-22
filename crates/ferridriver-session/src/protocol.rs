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
//! [`crate::Dispatcher`]. A bound browser's main verb is `run`, whose args
//! are a [`ScriptRequest`] — anything a script can express goes through
//! it. The other verbs carry what a script cannot do well: a stream of
//! screencast frames ([`SCREENCAST_VERB`]), input relayed one event at a
//! time with no compile step ([`INPUT_VERB`]), and a recorder whose actions
//! arrive as they happen ([`RECORDER_START_VERB`] and its siblings).
//!
//! A command answers in order on its connection, and a long-running one
//! (`screencast`, `recorder.start`) holds its connection until it ends, so
//! a client watching and driving at once uses one connection per stream.
//! Hanging up ends a stream.

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

/// Run a script against the bound browser.
pub const RUN_VERB: &str = "run";
pub const CLOSE_VERB: &str = "session.close";
/// The first command on a connection to a token-protected endpoint; args
/// `{"token": "..."}`. Any other first command is refused.
pub const AUTH_VERB: &str = "session.auth";
/// Dispatch one [`InputEvent`] to the context's page.
pub const INPUT_VERB: &str = "input";
/// Stream [`EventPayload::Frame`]s until the client hangs up; args are a
/// [`ScreencastRequest`].
pub const SCREENCAST_VERB: &str = "screencast";
/// Record the context's page, streaming [`EventPayload::Recorded`] and
/// [`EventPayload::RecorderMode`] until [`RECORDER_STOP_VERB`] or a hang-up;
/// args are a [`RecorderStartRequest`], the response a [`RecordingResult`].
pub const RECORDER_START_VERB: &str = "recorder.start";
/// End the context's recording; answers with its [`RecordingResult`].
pub const RECORDER_STOP_VERB: &str = "recorder.stop";
/// Change what the next click on the recorded page means; args are a
/// [`RecorderModeRequest`].
pub const RECORDER_MODE_VERB: &str = "recorder.mode";
/// Record an assertion about the page itself; args are a
/// [`RecorderAssertRequest`], the response the recorded action.
pub const RECORDER_ASSERT_VERB: &str = "recorder.assert";

/// One input event for [`INPUT_VERB`]. Coordinates are CSS pixels in the
/// page's viewport, the space a screencast frame depicts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputEvent {
  MouseMove {
    x: f64,
    y: f64,
  },
  /// `button` is `left` (default), `right` or `middle`. `click_count` is
  /// the press's place in a run of clicks: 2 for the second press of a
  /// double click, which is what makes the page see `dblclick`.
  MouseDown {
    x: f64,
    y: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    button: Option<String>,
    #[serde(default = "first_click")]
    click_count: u32,
  },
  MouseUp {
    x: f64,
    y: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    button: Option<String>,
    #[serde(default = "first_click")]
    click_count: u32,
  },
  /// Scrolls whatever is under `(x, y)`.
  MouseWheel {
    x: f64,
    y: f64,
    #[serde(default)]
    delta_x: f64,
    #[serde(default)]
    delta_y: f64,
  },
  /// A key by its DOM `key` name (`a`, `Enter`, `Shift`), held until
  /// [`InputEvent::KeyUp`].
  KeyDown {
    key: String,
  },
  KeyUp {
    key: String,
  },
  /// Text entered as an IME would, with no key events.
  InsertText {
    text: String,
  },
}

fn first_click() -> u32 {
  1
}

/// [`SCREENCAST_VERB`]'s arguments. Frames are JPEG, scaled down to fit
/// `max_width` x `max_height`; unset bounds default to the viewport.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScreencastRequest {
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub quality: Option<u8>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub max_width: Option<u32>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub max_height: Option<u32>,
}

/// [`RECORDER_START_VERB`]'s arguments.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecorderStartRequest {
  /// `ts` (default), `rust` or `gherkin`: the language of each event's
  /// `code` and of the final source.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub language: Option<String>,
  /// `test` (default), an `@ferridriver/test` spec, or `script`, a file
  /// `ferridriver run` executes. Only TypeScript has the choice.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub target: Option<String>,
  /// The test's title in a TypeScript spec. Defaults to `test`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub title: Option<String>,
  /// Navigate here before recording starts, so the recording opens on it.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub url: Option<String>,
  /// The mode to start in. Defaults to `recording`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub mode: Option<String>,
}

/// [`RECORDER_MODE_VERB`]'s arguments: one of `recording`, `none`,
/// `assertingVisibility`, `assertingText`, `assertingValue`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecorderModeRequest {
  pub mode: String,
}

/// What [`RECORDER_ASSERT_VERB`] asserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageAssertion {
  /// `toHaveURL` with the page's current URL.
  Url,
  /// `toHaveTitle` with the page's current title.
  Title,
}

/// [`RECORDER_ASSERT_VERB`]'s arguments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecorderAssertRequest {
  pub kind: PageAssertion,
}

/// What a finished recording answers with, as the response text's JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingResult {
  /// The whole file in the requested language and target.
  pub source: String,
  pub actions: Vec<ferridriver::codegen::Action>,
}

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
  /// One screencast frame, for [`SCREENCAST_VERB`]. `data` is a base64 JPEG;
  /// `viewport_width` x `viewport_height` is the CSS-pixel viewport it
  /// depicts, the space [`InputEvent`] coordinates are in, whatever size
  /// the image was scaled to. `timestamp_ms` is the backend's frame clock.
  ///
  /// No field here is a float: event payloads are flattened, and a flattened
  /// float does not decode where `serde_json`'s `arbitrary_precision` is on,
  /// which the bundler turns on for the whole CLI build.
  Frame {
    data: String,
    timestamp_ms: u64,
    viewport_width: u32,
    viewport_height: u32,
  },
  /// One recorded action, for [`RECORDER_START_VERB`], with the statement
  /// that reproduces it. `replaces_previous` marks an action that supersedes
  /// the one recorded just before it (a click that became a double click).
  Recorded {
    action: ferridriver::codegen::Action,
    code: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    replaces_previous: bool,
  },
  /// The recorder's mode changed, including its return to `recording`
  /// after an assertion was picked.
  RecorderMode {
    mode: String,
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
    assert_eq!(req.args, [] as [serde_json::Value; 0]);
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
  fn input_events_use_the_snake_case_wire_shape() {
    let down: InputEvent = serde_json::from_value(serde_json::json!({
      "type": "mouse_down", "x": 10.5, "y": 20,
    }))
    .unwrap();
    assert_eq!(
      down,
      InputEvent::MouseDown {
        x: 10.5,
        y: 20.0,
        button: None,
        click_count: 1
      }
    );
    let wheel = serde_json::to_value(InputEvent::MouseWheel {
      x: 1.0,
      y: 2.0,
      delta_x: 0.0,
      delta_y: 120.0,
    })
    .unwrap();
    assert_eq!(
      wheel,
      serde_json::json!({"type": "mouse_wheel", "x": 1.0, "y": 2.0, "delta_x": 0.0, "delta_y": 120.0})
    );
    let text: InputEvent = serde_json::from_value(serde_json::json!({"type": "insert_text", "text": "hi"})).unwrap();
    assert_eq!(text, InputEvent::InsertText { text: "hi".into() });
  }

  #[test]
  fn a_recorded_event_carries_the_action_in_its_own_wire_shape() {
    let wire = serde_json::to_value(ServerFrame::Event(Event {
      id: 3,
      payload: EventPayload::Recorded {
        action: ferridriver::codegen::Action::Click {
          selector: "#go".into(),
          locator: "locator('#go')".into(),
        },
        code: "await page.locator('#go').click();".into(),
        replaces_previous: false,
      },
    }))
    .unwrap();
    assert_eq!(
      wire,
      serde_json::json!({
        "type": "event",
        "id": 3,
        "event": "recorded",
        "action": {"type": "click", "selector": "#go", "locator": "locator('#go')"},
        "code": "await page.locator('#go').click();",
      })
    );
    let mode = serde_json::to_value(EventPayload::RecorderMode {
      mode: "assertingText".into(),
    })
    .unwrap();
    assert_eq!(
      mode,
      serde_json::json!({"event": "recorder_mode", "mode": "assertingText"})
    );
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
