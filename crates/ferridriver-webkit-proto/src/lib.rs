//! Strongly typed Playwright `WebKit` Inspector Protocol commands, responses, and events.
//!
//! Mirrors Playwright's patched `WebKit` protocol
//! (`packages/playwright-core/src/server/webkit/protocol.d.ts`). Commands
//! implement [`Command`] with a static wire `METHOD` name and typed `Response`,
//! serializing directly onto the NUL-delimited inspector pipe without
//! intermediate `serde_json::Value` trees.

pub mod console;
pub mod dom;
pub mod emulation;
pub mod input;
pub mod network;
pub mod page;
pub mod playwright;
pub mod runtime;
pub mod screencast;
pub mod target;
pub mod worker;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// A strongly typed `WebKit` Inspector command with its wire method name and response type.
pub trait Command: Serialize {
  /// Exact `WebKit` Inspector method string (e.g. `"Playwright.navigate"`).
  const METHOD: &'static str;
  /// Typed response decoded from the `"result"` object.
  type Response: DeserializeOwned;
}

/// Empty response `{}` for `WebKit` commands that return no result fields.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub struct EmptyResponse {}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn unit_param_commands_serialize_as_empty_json_object() {
    assert_eq!(serde_json::to_string(&playwright::GetInfo {}).unwrap(), "{}");
    assert_eq!(serde_json::to_string(&page::Enable {}).unwrap(), "{}");
    assert_eq!(serde_json::to_string(&runtime::Enable {}).unwrap(), "{}");
    assert_eq!(serde_json::to_string(&network::Enable {}).unwrap(), "{}");
    assert_eq!(serde_json::to_string(&console::Enable {}).unwrap(), "{}");
    assert_eq!(serde_json::to_string(&worker::Enable {}).unwrap(), "{}");
  }

  #[test]
  fn playwright_navigate_omits_optional_referrer() {
    let cmd = playwright::Navigate {
      url: "https://example.com/",
      page_proxy_id: "page-1",
      frame_id: None,
      referrer: None,
    };
    let v = serde_json::to_value(cmd).unwrap();
    assert_eq!(
      v,
      serde_json::json!({ "url": "https://example.com/", "pageProxyId": "page-1" })
    );
  }
}
