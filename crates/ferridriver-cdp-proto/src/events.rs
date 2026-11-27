//! Typed CDP event envelope parsed from `(method, params)`.

use serde::Deserialize;
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
      "Runtime.consoleAPICalled" => Deserialize::deserialize(params).ok().map(Self::ConsoleApiCalled),
      "Runtime.exceptionThrown" => Deserialize::deserialize(params).ok().map(Self::ExceptionThrown),
      "Runtime.executionContextCreated" => Deserialize::deserialize(params).ok().map(Self::ExecutionContextCreated),
      "Runtime.executionContextDestroyed" => Deserialize::deserialize(params)
        .ok()
        .map(Self::ExecutionContextDestroyed),
      "Runtime.executionContextsCleared" => Some(Self::ExecutionContextsCleared),
      "Runtime.bindingCalled" => Deserialize::deserialize(params).ok().map(Self::BindingCalled),
      "Page.frameNavigated" => Deserialize::deserialize(params).ok().map(Self::FrameNavigated),
      "Page.navigatedWithinDocument" => Deserialize::deserialize(params).ok().map(Self::NavigatedWithinDocument),
      "Page.frameAttached" => Deserialize::deserialize(params).ok().map(Self::FrameAttached),
      "Page.frameDetached" => Deserialize::deserialize(params).ok().map(Self::FrameDetached),
      "Page.lifecycleEvent" => Deserialize::deserialize(params).ok().map(Self::LifecycleEvent),
      "Page.loadEventFired" => Deserialize::deserialize(params).ok().map(Self::LoadEventFired),
      "Page.domContentEventFired" => Deserialize::deserialize(params).ok().map(Self::DomContentEventFired),
      "Page.javascriptDialogOpening" => Deserialize::deserialize(params).ok().map(Self::JavascriptDialogOpening),
      "Page.javascriptDialogClosed" => Deserialize::deserialize(params).ok().map(Self::JavascriptDialogClosed),
      "Page.fileChooserOpened" => Deserialize::deserialize(params).ok().map(Self::FileChooserOpened),
      "Browser.downloadWillBegin" => Deserialize::deserialize(params).ok().map(Self::DownloadWillBegin),
      "Browser.downloadProgress" => Deserialize::deserialize(params).ok().map(Self::DownloadProgress),
      "Target.attachedToTarget" => Deserialize::deserialize(params).ok().map(Self::AttachedToTarget),
      "Target.detachedFromTarget" => Deserialize::deserialize(params).ok().map(Self::DetachedFromTarget),
      "Target.targetCreated" => Deserialize::deserialize(params).ok().map(Self::TargetCreated),
      "Target.targetDestroyed" => Deserialize::deserialize(params).ok().map(Self::TargetDestroyed),
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

  #[test]
  fn parses_frame_lifecycle_target_and_rejects_unknown_or_malformed() {
    let nav = CdpEvent::parse(
      "Page.frameNavigated",
      &serde_json::json!({
        "frame": { "id": "F1", "url": "https://example.test/", "name": "main" }
      }),
    );
    assert!(matches!(nav, Some(CdpEvent::FrameNavigated(e)) if e.frame.id == "F1"));

    let life = CdpEvent::parse(
      "Page.lifecycleEvent",
      &serde_json::json!({
        "frameId": "F1",
        "loaderId": "L1",
        "name": "load",
        "timestamp": 1.0
      }),
    );
    assert!(matches!(life, Some(CdpEvent::LifecycleEvent(e)) if e.name == "load" && e.loader_id == "L1"));

    let attached = CdpEvent::parse(
      "Target.attachedToTarget",
      &serde_json::json!({
        "sessionId": "S1",
        "targetInfo": { "targetId": "T1", "type": "page", "url": "about:blank" },
        "waitingForDebugger": true
      }),
    );
    assert!(matches!(attached, Some(CdpEvent::AttachedToTarget(e)) if e.session_id == "S1" && e.waiting_for_debugger));

    assert!(matches!(
      CdpEvent::parse("Inspector.targetCrashed", &serde_json::json!({})),
      Some(CdpEvent::TargetCrashed)
    ));
    assert!(CdpEvent::parse("Unknown.method", &serde_json::json!({})).is_none());
    assert!(CdpEvent::parse("Page.frameNavigated", &serde_json::json!({ "bad": true })).is_none());
  }
}
