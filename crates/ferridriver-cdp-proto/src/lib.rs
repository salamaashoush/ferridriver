//! Strongly typed Chrome `DevTools` Protocol (CDP) commands, responses, and events.
//!
//! Each domain module mirrors Chromium's CDP specification
//! (`packages/playwright-core/src/server/chromium/protocol.d.ts`). Commands
//! implement [`Command`] with a static wire `METHOD` name and typed `Response`,
//! allowing direct serialization onto the wire without constructing an
//! intermediate `serde_json::Value` tree.

pub mod accessibility;
pub mod browser;
pub mod dom;
pub mod emulation;
pub mod fetch;
pub mod heap_profiler;
pub mod input;
pub mod network;
pub mod page;
pub mod performance;
pub mod runtime;
pub mod security;
pub mod storage;
pub mod target;
pub mod tracing;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// A strongly typed CDP command with its wire method name and response payload.
pub trait Command: Serialize {
  /// Exact CDP method string (e.g. `"Page.navigate"`).
  const METHOD: &'static str;
  /// Typed response decoded from the JSON-RPC `"result"` object.
  type Response: DeserializeOwned;
}

/// Empty response `{}` for CDP commands that return no result fields.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub struct EmptyResponse {}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn unit_param_commands_serialize_as_empty_json_object() {
    assert_eq!(serde_json::to_string(&page::Enable {}).unwrap(), "{}");
    assert_eq!(serde_json::to_string(&runtime::Enable {}).unwrap(), "{}");
    assert_eq!(serde_json::to_string(&browser::GetVersion {}).unwrap(), "{}");
    assert_eq!(serde_json::to_string(&target::GetTargets {}).unwrap(), "{}");
    assert_eq!(serde_json::to_string(&dom::Enable {}).unwrap(), "{}");
  }

  #[test]
  fn navigate_omits_absent_optional_fields() {
    let cmd = page::Navigate {
      url: "https://example.com",
      referrer: None,
      frame_id: None,
    };
    let v = serde_json::to_value(cmd).unwrap();
    assert_eq!(v, serde_json::json!({ "url": "https://example.com" }));
  }

  #[test]
  fn empty_response_accepts_empty_and_extra_fields() {
    let _: EmptyResponse = serde_json::from_str("{}").unwrap();
    let _: EmptyResponse = serde_json::from_str(r#"{"ignored":true}"#).unwrap();
  }

  #[test]
  fn typed_events_deserialize_wire_payloads() {
    let ev: target::AttachedToTargetEvent = serde_json::from_value(serde_json::json!({
      "sessionId": "S1",
      "targetInfo": {
        "targetId": "T1",
        "type": "page",
        "title": "Example",
        "url": "https://example.com/",
        "attached": true,
        "browserContextId": "BC1"
      },
      "waitingForDebugger": true
    }))
    .unwrap();
    assert_eq!(ev.session_id, "S1");
    assert_eq!(ev.target_info.target_id, "T1");
    assert_eq!(ev.target_info.target_type, "page");
    assert_eq!(ev.target_info.browser_context_id.as_deref(), Some("BC1"));
    assert!(ev.waiting_for_debugger);
  }
}
