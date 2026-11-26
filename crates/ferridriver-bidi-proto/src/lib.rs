//! Strongly typed `WebDriver` `BiDi` protocol commands, responses, and events.
//!
//! Mirrors W3C `WebDriver` `BiDi` (`packages/playwright-core/src/server/bidi/third_party/bidiProtocolCore.ts`).
//! Commands implement [`Command`] with a static wire `METHOD` and typed `Response`,
//! serializing directly onto the WebSocket frame without intermediate `serde_json::Value` trees.

pub mod browser;
pub mod browsing_context;
pub mod emulation;
pub mod input;
pub mod log;
pub mod network;
pub mod permissions;
pub mod script;
pub mod session;
pub mod storage;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// A strongly typed `WebDriver` `BiDi` command with its wire method name and response type.
pub trait Command: Serialize {
  /// Exact `BiDi` method string (e.g. `"browsingContext.navigate"`).
  const METHOD: &'static str;
  /// Typed response decoded from the JSON-RPC `"result"` object.
  type Response: DeserializeOwned;
}

/// Empty response `{}` for `BiDi` commands that return no result fields.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub struct EmptyResponse {}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn unit_param_commands_serialize_as_empty_json_object() {
    assert_eq!(serde_json::to_string(&session::Status {}).unwrap(), "{}");
    assert_eq!(serde_json::to_string(&browser::GetUserContexts {}).unwrap(), "{}");
    assert_eq!(serde_json::to_string(&browser::Close {}).unwrap(), "{}");
  }

  #[test]
  fn navigate_serializes_readiness_state_camel_case() {
    let cmd = browsing_context::Navigate {
      context: "ctx-1",
      url: "https://example.com/",
      wait: Some(browsing_context::ReadinessState::Complete),
    };
    let v = serde_json::to_value(cmd).unwrap();
    assert_eq!(
      v,
      serde_json::json!({
        "context": "ctx-1",
        "url": "https://example.com/",
        "wait": "complete"
      })
    );
  }

  #[test]
  fn script_target_serializes_context_and_optional_sandbox() {
    let target = script::Target::Context {
      context: "ctx-1",
      sandbox: Some("__ferridriver_utility"),
    };
    let v = serde_json::to_value(target).unwrap();
    assert_eq!(
      v,
      serde_json::json!({ "context": "ctx-1", "sandbox": "__ferridriver_utility" })
    );
  }
}
