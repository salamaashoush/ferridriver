//! Typed CDP event envelope parsed from `(method, params)`.

use serde_json::Value;

use crate::{browser, page, runtime, target};

/// Strongly typed CDP event decoded from a wire notification.
#[derive(Debug, Clone)]
pub enum CdpEvent {
  ConsoleApiCalled(runtime::ConsoleApiCalledEvent),
  ExceptionThrown(runtime::ExceptionThrownEvent),
  ExecutionContextCreated(runtime::ExecutionContextCreatedEvent),
  ExecutionContextDestroyed(runtime::ExecutionContextDestroyedEvent),
  ExecutionContextsCleared,
  BindingCalled(runtime::BindingCalledEvent),
  FrameNavigated(page::FrameNavigatedEvent),
  NavigatedWithinDocument(page::NavigatedWithinDocumentEvent),
  FrameAttached(page::FrameAttachedEvent),
  FrameDetached(page::FrameDetachedEvent),
  LifecycleEvent(page::LifecycleEvent),
  LoadEventFired(page::LoadEventFiredEvent),
  DomContentEventFired(page::DomContentEventFiredEvent),
  JavascriptDialogOpening(page::JavascriptDialogOpeningEvent),
  JavascriptDialogClosed(page::JavascriptDialogClosedEvent),
  FileChooserOpened(page::FileChooserOpenedEvent),
  DownloadWillBegin(browser::DownloadWillBeginEvent),
  DownloadProgress(browser::DownloadProgressEvent),
  AttachedToTarget(target::AttachedToTargetEvent),
  DetachedFromTarget(target::DetachedFromTargetEvent),
  TargetCreated(target::TargetCreatedEvent),
  TargetDestroyed(target::TargetDestroyedEvent),
  TargetCrashed,
}

impl CdpEvent {
  /// Parse a raw CDP notification's `(method, params)` pair into a typed [`CdpEvent`].
  /// Returns `None` for unrecognized methods or malformed payloads.
  #[must_use]
  pub fn parse(method: &str, params: &Value) -> Option<Self> {
    match method {
      "Runtime.consoleAPICalled" => serde_json::from_value(params.clone()).ok().map(Self::ConsoleApiCalled),
      "Runtime.exceptionThrown" => serde_json::from_value(params.clone()).ok().map(Self::ExceptionThrown),
      "Runtime.executionContextCreated" => serde_json::from_value(params.clone())
        .ok()
        .map(Self::ExecutionContextCreated),
      "Runtime.executionContextDestroyed" => serde_json::from_value(params.clone())
        .ok()
        .map(Self::ExecutionContextDestroyed),
      "Runtime.executionContextsCleared" => Some(Self::ExecutionContextsCleared),
      "Runtime.bindingCalled" => serde_json::from_value(params.clone()).ok().map(Self::BindingCalled),
      "Page.frameNavigated" => serde_json::from_value(params.clone()).ok().map(Self::FrameNavigated),
      "Page.navigatedWithinDocument" => serde_json::from_value(params.clone())
        .ok()
        .map(Self::NavigatedWithinDocument),
      "Page.frameAttached" => serde_json::from_value(params.clone()).ok().map(Self::FrameAttached),
      "Page.frameDetached" => serde_json::from_value(params.clone()).ok().map(Self::FrameDetached),
      "Page.lifecycleEvent" => serde_json::from_value(params.clone()).ok().map(Self::LifecycleEvent),
      "Page.loadEventFired" => serde_json::from_value(params.clone()).ok().map(Self::LoadEventFired),
      "Page.domContentEventFired" => serde_json::from_value(params.clone())
        .ok()
        .map(Self::DomContentEventFired),
      "Page.javascriptDialogOpening" => serde_json::from_value(params.clone())
        .ok()
        .map(Self::JavascriptDialogOpening),
      "Page.javascriptDialogClosed" => serde_json::from_value(params.clone())
        .ok()
        .map(Self::JavascriptDialogClosed),
      "Page.fileChooserOpened" => serde_json::from_value(params.clone()).ok().map(Self::FileChooserOpened),
      "Browser.downloadWillBegin" => serde_json::from_value(params.clone()).ok().map(Self::DownloadWillBegin),
      "Browser.downloadProgress" => serde_json::from_value(params.clone()).ok().map(Self::DownloadProgress),
      "Target.attachedToTarget" => serde_json::from_value(params.clone()).ok().map(Self::AttachedToTarget),
      "Target.detachedFromTarget" => serde_json::from_value(params.clone())
        .ok()
        .map(Self::DetachedFromTarget),
      "Target.targetCreated" => serde_json::from_value(params.clone()).ok().map(Self::TargetCreated),
      "Target.targetDestroyed" => serde_json::from_value(params.clone()).ok().map(Self::TargetDestroyed),
      "Inspector.targetCrashed" => Some(Self::TargetCrashed),
      _ => None,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::CdpEvent;

  #[test]
  fn parses_console_dialog_and_download_events() {
    let console = CdpEvent::parse(
      "Runtime.consoleAPICalled",
      &serde_json::json!({
        "type": "warning",
        "args": [{ "type": "string", "value": "hi" }],
        "executionContextId": 3,
        "timestamp": 1234.5
      }),
    );
    assert!(matches!(console, Some(CdpEvent::ConsoleApiCalled(e)) if e.kind == "warning" && e.args.len() == 1));

    let dialog = CdpEvent::parse(
      "Page.javascriptDialogOpening",
      &serde_json::json!({
        "type": "prompt",
        "message": "Name?",
        "defaultPrompt": "Alice"
      }),
    );
    assert!(
      matches!(dialog, Some(CdpEvent::JavascriptDialogOpening(e)) if e.dialog_type == "prompt" && e.default_prompt == "Alice")
    );

    let dl = CdpEvent::parse(
      "Browser.downloadProgress",
      &serde_json::json!({
        "guid": "g1",
        "state": "completed"
      }),
    );
    assert!(matches!(dl, Some(CdpEvent::DownloadProgress(e)) if e.guid == "g1" && e.state == "completed"));
  }
}
