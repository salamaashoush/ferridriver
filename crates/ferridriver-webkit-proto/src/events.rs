//! Typed `WebKit` Inspector event envelope parsed from `(method, params)`.

use serde::Deserialize;
use serde_json::Value;

use crate::{console, page, playwright, target};

/// Strongly typed `WebKit` Inspector event decoded from a wire notification.
#[derive(Debug, Clone)]
pub enum WebKitEvent {
  ConsoleMessageAdded(console::MessageAddedEvent),
  JavascriptDialogOpening(page::JavascriptDialogOpeningEvent),
  JavascriptDialogClosed,
  FileChooserOpened(page::FileChooserOpenedEvent),
  TargetCreated(target::TargetCreatedEvent),
  DidCommitProvisionalTarget(target::DidCommitProvisionalTargetEvent),
  DownloadCreated(playwright::DownloadCreatedEvent),
  DownloadFilenameSuggested(playwright::DownloadFilenameSuggestedEvent),
  DownloadFinished(playwright::DownloadFinishedEvent),
  PageProxyCreated(playwright::PageProxyCreatedEvent),
  PageProxyDestroyed(playwright::PageProxyDestroyedEvent),
}

impl WebKitEvent {
  /// Parse a raw `WebKit` Inspector notification's `(method, params)` into a typed [`WebKitEvent`].
  #[must_use]
  pub fn parse(method: &str, params: &Value) -> Option<Self> {
    match method {
      "Console.messageAdded" => Deserialize::deserialize(params).ok().map(Self::ConsoleMessageAdded),
      "Dialog.javascriptDialogOpening" => Deserialize::deserialize(params).ok().map(Self::JavascriptDialogOpening),
      "Dialog.javascriptDialogClosed" => Some(Self::JavascriptDialogClosed),
      "Page.fileChooserOpened" => Deserialize::deserialize(params).ok().map(Self::FileChooserOpened),
      "Target.targetCreated" => Deserialize::deserialize(params).ok().map(Self::TargetCreated),
      "Target.didCommitProvisionalTarget" => Deserialize::deserialize(params)
        .ok()
        .map(Self::DidCommitProvisionalTarget),
      "Playwright.downloadCreated" => Deserialize::deserialize(params).ok().map(Self::DownloadCreated),
      "Playwright.downloadFilenameSuggested" => Deserialize::deserialize(params)
        .ok()
        .map(Self::DownloadFilenameSuggested),
      "Playwright.downloadFinished" => Deserialize::deserialize(params).ok().map(Self::DownloadFinished),
      "Playwright.pageProxyCreated" => Deserialize::deserialize(params).ok().map(Self::PageProxyCreated),
      "Playwright.pageProxyDestroyed" => Deserialize::deserialize(params).ok().map(Self::PageProxyDestroyed),
      _ => None,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::WebKitEvent;

  #[test]
  fn parses_console_dialog_and_download_events() {
    let msg = WebKitEvent::parse(
      "Console.messageAdded",
      &serde_json::json!({
        "message": { "source": "console-api", "level": "log", "text": "hello" }
      }),
    );
    assert!(matches!(msg, Some(WebKitEvent::ConsoleMessageAdded(e)) if e.message.text == "hello"));

    let dl = WebKitEvent::parse(
      "Playwright.downloadCreated",
      &serde_json::json!({
        "uuid": "u-1",
        "pageProxyId": "p-1",
        "frameId": "f-1",
        "url": "https://example.com/a.zip"
      }),
    );
    assert!(matches!(dl, Some(WebKitEvent::DownloadCreated(e)) if e.uuid == "u-1"));
  }

  #[test]
  fn parses_dialog_target_file_chooser_and_rejects_unknown_or_malformed() {
    let dialog = WebKitEvent::parse(
      "Dialog.javascriptDialogOpening",
      &serde_json::json!({
        "type": "alert",
        "message": "Saved!",
        "defaultPrompt": ""
      }),
    );
    assert!(matches!(dialog, Some(WebKitEvent::JavascriptDialogOpening(e)) if e.message == "Saved!"));
    assert!(matches!(
      WebKitEvent::parse("Dialog.javascriptDialogClosed", &serde_json::json!({})),
      Some(WebKitEvent::JavascriptDialogClosed)
    ));

    let swap = WebKitEvent::parse(
      "Target.didCommitProvisionalTarget",
      &serde_json::json!({
        "oldTargetId": "page-1",
        "newTargetId": "page-2"
      }),
    );
    assert!(
      matches!(swap, Some(WebKitEvent::DidCommitProvisionalTarget(e)) if e.old_target_id == "page-1" && e.new_target_id == "page-2")
    );

    let proxy = WebKitEvent::parse(
      "Playwright.pageProxyCreated",
      &serde_json::json!({
        "pageProxyInfo": {
          "pageProxyId": "pp-1",
          "browserContextId": "bc-1",
          "openerId": "pp-0"
        }
      }),
    );
    assert!(
      matches!(proxy, Some(WebKitEvent::PageProxyCreated(e)) if e.page_proxy_info.page_proxy == "pp-1" && e.page_proxy_info.opener.as_deref() == Some("pp-0"))
    );

    assert!(WebKitEvent::parse("Unknown.event", &serde_json::json!({})).is_none());
    assert!(WebKitEvent::parse("Playwright.downloadCreated", &serde_json::json!({ "uuid": 123 })).is_none());
  }
}
