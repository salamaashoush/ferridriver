//! Minimal serde-typed surface of Playwright's `WebKit` Inspector protocol.
//!
//! Method names follow the wire format (`Namespace.method`, e.g.
//! `Playwright.createContext`, `Page.navigate`). Constants are kept
//! as `&'static str` so callers can pass them to
//! [`crate::backend::webkit::Session::send`] without further
//! allocation.

use serde::{Deserialize, Serialize};

/// Top-level message envelope shared by requests, responses and
/// events. Per-target traffic is routed through `pageProxyId`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub id: Option<i64>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub method: Option<String>,
  #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
  pub params: serde_json::Value,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub result: Option<serde_json::Value>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub error: Option<ErrorPayload>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub page_proxy_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ErrorPayload {
  pub message: String,
  #[serde(default)]
  pub code: Option<i64>,
  #[serde(default)]
  pub data: Option<serde_json::Value>,
}

// ── Browser-session methods ────────────────────────────────────────────

pub const PLAYWRIGHT_ENABLE: &str = "Playwright.enable";
pub const PLAYWRIGHT_CLOSE: &str = "Playwright.close";
pub const PLAYWRIGHT_GET_INFO: &str = "Playwright.getInfo";
pub const PLAYWRIGHT_CREATE_CONTEXT: &str = "Playwright.createContext";
pub const PLAYWRIGHT_DELETE_CONTEXT: &str = "Playwright.deleteContext";
pub const PLAYWRIGHT_CREATE_PAGE: &str = "Playwright.createPage";
pub const PLAYWRIGHT_NAVIGATE: &str = "Playwright.navigate";
pub const PLAYWRIGHT_TAKE_SCREENSHOT: &str = "Playwright.takePageScreenshot";

// ── Browser-session events ─────────────────────────────────────────────

pub const EVT_PAGE_PROXY_CREATED: &str = "Playwright.pageProxyCreated";
pub const EVT_PAGE_PROXY_DESTROYED: &str = "Playwright.pageProxyDestroyed";

// ── Per-page-session methods (subset) ──────────────────────────────────

pub const PAGE_RELOAD: &str = "Page.reload";
pub const PAGE_GO_BACK: &str = "Page.goBack";
pub const PAGE_GO_FORWARD: &str = "Page.goForward";
pub const PAGE_NAVIGATE_WITHIN: &str = "Page.navigatedWithinDocument";
pub const RUNTIME_EVALUATE: &str = "Runtime.evaluate";
pub const RUNTIME_CALL_FUNCTION_ON: &str = "Runtime.callFunctionOn";
pub const RUNTIME_RELEASE_OBJECT: &str = "Runtime.releaseObject";
pub const DOM_QUERY_SELECTOR: &str = "DOM.querySelector";
pub const INPUT_DISPATCH_MOUSE: &str = "Input.dispatchMouseEvent";
pub const INPUT_DISPATCH_KEY: &str = "Input.dispatchKeyEvent";
pub const CONSOLE_ENABLE: &str = "Console.enable";

// ── Common parameter / return shapes ───────────────────────────────────

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateContextParams {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub proxy_server: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub proxy_bypass_list: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateContextResult {
  pub browser_context_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePageParams {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub browser_context_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePageResult {
  pub page_proxy_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigateParams {
  pub url: String,
  pub page_proxy_id: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub frame_id: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub referrer: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NavigateResult {
  #[serde(default)]
  pub loader_id: Option<String>,
  #[serde(default)]
  pub error_text: Option<String>,
}
