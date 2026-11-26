//! CDP `Page` domain.

use serde::{Deserialize, Serialize};

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Enable {}

impl Command for Enable {
  const METHOD: &'static str = "Page.enable";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Disable {}

impl Command for Disable {
  const METHOD: &'static str = "Page.disable";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Navigate<'a> {
  pub url: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub referrer: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub frame_id: Option<&'a str>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigateResponse {
  #[serde(default)]
  pub frame_id: String,
  #[serde(default)]
  pub loader_id: Option<String>,
  #[serde(default)]
  pub error_text: Option<String>,
}

impl Command for Navigate<'_> {
  const METHOD: &'static str = "Page.navigate";
  type Response = NavigateResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reload {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub ignore_cache: Option<bool>,
}

impl Command for Reload {
  const METHOD: &'static str = "Page.reload";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct GetNavigationHistory {}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigationEntry {
  pub id: i64,
  #[serde(default)]
  pub url: String,
  #[serde(default)]
  pub user_typed_url: String,
  #[serde(default)]
  pub title: String,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetNavigationHistoryResponse {
  #[serde(default)]
  pub current_index: usize,
  #[serde(default)]
  pub entries: Vec<NavigationEntry>,
}

impl Command for GetNavigationHistory {
  const METHOD: &'static str = "Page.getNavigationHistory";
  type Response = GetNavigationHistoryResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigateToHistoryEntry {
  pub entry_id: i64,
}

impl Command for NavigateToHistoryEntry {
  const METHOD: &'static str = "Page.navigateToHistoryEntry";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct GetFrameTree {}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
  #[serde(default)]
  pub id: String,
  #[serde(default)]
  pub parent_id: Option<String>,
  #[serde(default)]
  pub loader_id: String,
  #[serde(default)]
  pub name: Option<String>,
  #[serde(default)]
  pub url: String,
  #[serde(default)]
  pub url_fragment: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameTree {
  pub frame: Frame,
  #[serde(default)]
  pub child_frames: Vec<FrameTree>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetFrameTreeResponse {
  pub frame_tree: FrameTree,
}

impl Command for GetFrameTree {
  const METHOD: &'static str = "Page.getFrameTree";
  type Response = GetFrameTreeResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetDocumentContent<'a> {
  pub frame_id: &'a str,
  pub html: &'a str,
}

impl Command for SetDocumentContent<'_> {
  const METHOD: &'static str = "Page.setDocumentContent";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddScriptToEvaluateOnNewDocument<'a> {
  pub source: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub world_name: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub include_command_line_api: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub run_immediately: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddScriptToEvaluateOnNewDocumentResponse {
  pub identifier: String,
}

impl Command for AddScriptToEvaluateOnNewDocument<'_> {
  const METHOD: &'static str = "Page.addScriptToEvaluateOnNewDocument";
  type Response = AddScriptToEvaluateOnNewDocumentResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveScriptToEvaluateOnNewDocument<'a> {
  pub identifier: &'a str,
}

impl Command for RemoveScriptToEvaluateOnNewDocument<'_> {
  const METHOD: &'static str = "Page.removeScriptToEvaluateOnNewDocument";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateIsolatedWorld<'a> {
  pub frame_id: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub world_name: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub grant_univeral_access: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateIsolatedWorldResponse {
  pub execution_context_id: i64,
}

impl Command for CreateIsolatedWorld<'_> {
  const METHOD: &'static str = "Page.createIsolatedWorld";
  type Response = CreateIsolatedWorldResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Viewport {
  pub x: f64,
  pub y: f64,
  pub width: f64,
  pub height: f64,
  pub scale: f64,
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureScreenshot<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub format: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub quality: Option<u8>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub clip: Option<Viewport>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub from_surface: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub capture_beyond_viewport: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub optimize_for_speed: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureScreenshotResponse {
  pub data: String,
}

impl Command for CaptureScreenshot<'_> {
  const METHOD: &'static str = "Page.captureScreenshot";
  type Response = CaptureScreenshotResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetLifecycleEventsEnabled {
  pub enabled: bool,
}

impl Command for SetLifecycleEventsEnabled {
  const METHOD: &'static str = "Page.setLifecycleEventsEnabled";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetInterceptFileChooserDialog {
  pub enabled: bool,
}

impl Command for SetInterceptFileChooserDialog {
  const METHOD: &'static str = "Page.setInterceptFileChooserDialog";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HandleJavaScriptDialog<'a> {
  pub accept: bool,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub prompt_text: Option<&'a str>,
}

impl Command for HandleJavaScriptDialog<'_> {
  const METHOD: &'static str = "Page.handleJavaScriptDialog";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetBypassCsp {
  pub enabled: bool,
}

impl Command for SetBypassCsp {
  const METHOD: &'static str = "Page.setBypassCSP";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct BringToFront {}

impl Command for BringToFront {
  const METHOD: &'static str = "Page.bringToFront";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Close {}

impl Command for Close {
  const METHOD: &'static str = "Page.close";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct StopLoading {}

impl Command for StopLoading {
  const METHOD: &'static str = "Page.stopLoading";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartScreencast<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub format: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub quality: Option<u8>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub max_width: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub max_height: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub every_nth_frame: Option<u32>,
}

impl Command for StartScreencast<'_> {
  const METHOD: &'static str = "Page.startScreencast";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct StopScreencast {}

impl Command for StopScreencast {
  const METHOD: &'static str = "Page.stopScreencast";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreencastFrameAck {
  pub session_id: i64,
}

impl Command for ScreencastFrameAck {
  const METHOD: &'static str = "Page.screencastFrameAck";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[expect(
  clippy::struct_excessive_bools,
  reason = "CDP Page.printToPDF wire schema defines six boolean flags"
)]
pub struct PrintToPdf<'a> {
  pub landscape: bool,
  pub display_header_footer: bool,
  pub header_template: &'a str,
  pub footer_template: &'a str,
  pub print_background: bool,
  pub scale: f64,
  pub paper_width: f64,
  pub paper_height: f64,
  pub margin_top: f64,
  pub margin_bottom: f64,
  pub margin_left: f64,
  pub margin_right: f64,
  pub page_ranges: &'a str,
  #[serde(rename = "preferCSSPageSize")]
  pub prefer_css_page_size: bool,
  #[serde(rename = "generateTaggedPDF")]
  pub generate_tagged_pdf: bool,
  pub generate_document_outline: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrintToPdfResponse {
  pub data: String,
}

impl Command for PrintToPdf<'_> {
  const METHOD: &'static str = "Page.printToPDF";
  type Response = PrintToPdfResponse;
}

/// `Page.frameNavigated` event payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameNavigatedEvent {
  pub frame: Frame,
}

/// `Page.navigatedWithinDocument` event payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigatedWithinDocumentEvent {
  pub frame_id: String,
  pub url: String,
}

/// `Page.frameAttached` event payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameAttachedEvent {
  pub frame_id: String,
  pub parent_frame_id: String,
}

/// `Page.frameDetached` event payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameDetachedEvent {
  pub frame_id: String,
  #[serde(default)]
  pub reason: Option<String>,
}

/// `Page.lifecycleEvent` event payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleEvent {
  #[serde(default)]
  pub frame_id: String,
  #[serde(default)]
  pub loader_id: String,
  #[serde(default)]
  pub name: String,
  #[serde(default)]
  pub timestamp: f64,
}

/// `Page.javascriptDialogOpening` event payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavascriptDialogOpeningEvent {
  #[serde(default)]
  pub url: String,
  #[serde(default)]
  pub message: String,
  #[serde(default, rename = "type")]
  pub dialog_type: String,
  #[serde(default)]
  pub default_prompt: String,
}

/// `Page.fileChooserOpened` event payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChooserOpenedEvent {
  #[serde(default)]
  pub frame_id: String,
  #[serde(default)]
  pub mode: String,
  #[serde(default)]
  pub backend_node_id: Option<i64>,
}

/// `Page.javascriptDialogClosed` event payload.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavascriptDialogClosedEvent {
  #[serde(default)]
  pub result: bool,
  #[serde(default)]
  pub user_input: String,
}

/// `Page.loadEventFired` event payload.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadEventFiredEvent {
  #[serde(default)]
  pub timestamp: f64,
}

/// `Page.domContentEventFired` event payload.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DomContentEventFiredEvent {
  #[serde(default)]
  pub timestamp: f64,
}
