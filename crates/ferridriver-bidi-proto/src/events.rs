//! Typed `WebDriver` `BiDi` event envelope parsed from `(method, params)`.

use serde_json::Value;

use crate::{browsing_context, input, log, script};

/// Strongly typed `WebDriver` `BiDi` event decoded from a wire notification.
#[derive(Debug, Clone)]
pub enum BidiEvent {
  ContextCreated(browsing_context::ContextInfo),
  ContextDestroyed(browsing_context::ContextInfo),
  NavigationStarted(browsing_context::NavigationInfoEvent),
  NavigationCommitted(browsing_context::NavigationInfoEvent),
  DomContentLoaded(browsing_context::NavigationInfoEvent),
  Load(browsing_context::NavigationInfoEvent),
  HistoryUpdated(browsing_context::NavigationInfoEvent),
  FragmentNavigated(browsing_context::NavigationInfoEvent),
  UserPromptOpened(browsing_context::UserPromptOpenedEvent),
  UserPromptClosed(browsing_context::UserPromptClosedEvent),
  DownloadWillBegin(browsing_context::DownloadWillBeginEvent),
  DownloadEnd(browsing_context::DownloadEndEvent),
  LogEntryAdded(log::EntryAddedEvent),
  ScriptMessage(script::MessageEvent),
  FileDialogOpened(input::FileDialogOpenedEvent),
}

impl BidiEvent {
  /// Parse a raw `BiDi` notification's `(method, params)` into a typed [`BidiEvent`].
  #[must_use]
  pub fn parse(method: &str, params: &Value) -> Option<Self> {
    match method {
      "browsingContext.contextCreated" => serde_json::from_value(params.clone()).ok().map(Self::ContextCreated),
      "browsingContext.contextDestroyed" => serde_json::from_value(params.clone()).ok().map(Self::ContextDestroyed),
      "browsingContext.navigationStarted" => serde_json::from_value(params.clone()).ok().map(Self::NavigationStarted),
      "browsingContext.navigationCommitted" => serde_json::from_value(params.clone())
        .ok()
        .map(Self::NavigationCommitted),
      "browsingContext.domContentLoaded" => serde_json::from_value(params.clone()).ok().map(Self::DomContentLoaded),
      "browsingContext.load" => serde_json::from_value(params.clone()).ok().map(Self::Load),
      "browsingContext.historyUpdated" => serde_json::from_value(params.clone()).ok().map(Self::HistoryUpdated),
      "browsingContext.fragmentNavigated" => serde_json::from_value(params.clone()).ok().map(Self::FragmentNavigated),
      "browsingContext.userPromptOpened" => serde_json::from_value(params.clone()).ok().map(Self::UserPromptOpened),
      "browsingContext.userPromptClosed" => serde_json::from_value(params.clone()).ok().map(Self::UserPromptClosed),
      "browsingContext.downloadWillBegin" => serde_json::from_value(params.clone()).ok().map(Self::DownloadWillBegin),
      "browsingContext.downloadEnd" => serde_json::from_value(params.clone()).ok().map(Self::DownloadEnd),
      "log.entryAdded" => serde_json::from_value(params.clone()).ok().map(Self::LogEntryAdded),
      "script.message" => serde_json::from_value(params.clone()).ok().map(Self::ScriptMessage),
      "input.fileDialogOpened" => serde_json::from_value(params.clone()).ok().map(Self::FileDialogOpened),
      _ => None,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::BidiEvent;

  #[test]
  fn parses_prompt_file_dialog_and_download_events() {
    let prompt = BidiEvent::parse(
      "browsingContext.userPromptOpened",
      &serde_json::json!({
        "context": "ctx-1",
        "type": "confirm",
        "message": "Proceed?",
        "defaultValue": ""
      }),
    );
    assert!(matches!(prompt, Some(BidiEvent::UserPromptOpened(e)) if e.prompt_type == "confirm"));

    let file = BidiEvent::parse(
      "input.fileDialogOpened",
      &serde_json::json!({
        "context": "ctx-1",
        "element": { "sharedId": "el-42" },
        "multiple": true
      }),
    );
    assert!(
      matches!(file, Some(BidiEvent::FileDialogOpened(e)) if e.multiple && e.element.as_ref().and_then(|el| el.shared_id.as_deref()) == Some("el-42"))
    );
  }

  #[test]
  fn parses_navigation_log_script_and_rejects_unknown_or_malformed() {
    let nav = BidiEvent::parse(
      "browsingContext.load",
      &serde_json::json!({
        "context": "ctx-1",
        "url": "https://example.test/page"
      }),
    );
    assert!(matches!(nav, Some(BidiEvent::Load(e)) if e.context == "ctx-1" && e.url == "https://example.test/page"));

    let log = BidiEvent::parse(
      "log.entryAdded",
      &serde_json::json!({
        "type": "console",
        "level": "info",
        "method": "log",
        "text": "hello",
        "timestamp": 42,
        "source": { "context": "ctx-1" },
        "args": [{ "type": "string", "value": "hello" }]
      }),
    );
    assert!(matches!(log, Some(BidiEvent::LogEntryAdded(e)) if e.text.as_deref() == Some("hello")));

    let msg = BidiEvent::parse(
      "script.message",
      &serde_json::json!({
        "channel": "__fd_bind",
        "data": { "type": "string", "value": "{}" },
        "source": { "context": "ctx-1", "realm": "r-1" }
      }),
    );
    assert!(matches!(msg, Some(BidiEvent::ScriptMessage(e)) if e.channel == "__fd_bind"));

    assert!(BidiEvent::parse("unknown.event", &serde_json::json!({})).is_none());
    assert!(
      BidiEvent::parse(
        "browsingContext.userPromptOpened",
        &serde_json::json!({ "context": 123 })
      )
      .is_none()
    );
  }
}
