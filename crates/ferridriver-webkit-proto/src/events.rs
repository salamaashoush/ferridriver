//! Typed `WebKit` Inspector event envelope parsed from `(method, params)`.

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
      "Console.messageAdded" => serde_json::from_value(params.clone())
        .ok()
        .map(Self::ConsoleMessageAdded),
      "Dialog.javascriptDialogOpening" => serde_json::from_value(params.clone())
        .ok()
        .map(Self::JavascriptDialogOpening),
      "Dialog.javascriptDialogClosed" => Some(Self::JavascriptDialogClosed),
      "Page.fileChooserOpened" => serde_json::from_value(params.clone()).ok().map(Self::FileChooserOpened),
      "Target.targetCreated" => serde_json::from_value(params.clone()).ok().map(Self::TargetCreated),
      "Target.didCommitProvisionalTarget" => serde_json::from_value(params.clone())
        .ok()
        .map(Self::DidCommitProvisionalTarget),
      "Playwright.downloadCreated" => serde_json::from_value(params.clone()).ok().map(Self::DownloadCreated),
      "Playwright.downloadFilenameSuggested" => serde_json::from_value(params.clone())
        .ok()
        .map(Self::DownloadFilenameSuggested),
      "Playwright.downloadFinished" => serde_json::from_value(params.clone()).ok().map(Self::DownloadFinished),
      "Playwright.pageProxyCreated" => serde_json::from_value(params.clone()).ok().map(Self::PageProxyCreated),
      "Playwright.pageProxyDestroyed" => serde_json::from_value(params.clone())
        .ok()
        .map(Self::PageProxyDestroyed),
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
}
