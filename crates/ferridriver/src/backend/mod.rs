#![allow(clippy::missing_errors_doc)]
//! Backend abstraction layer for browser automation.
//!
//! Provides a unified API across multiple browser backends:
//! - `CdpPipe`: Chrome `DevTools` Protocol over pipes (--remote-debugging-pipe, fd 3/4)
//! - `CdpRaw`: Chrome `DevTools` Protocol over WebSocket (our own, fully parallel)
//! - `WebKit`: Native `WKWebView` on macOS (subprocess model)
//!
//! Uses enum dispatch (not trait objects) for zero-cost abstraction and Clone support.

pub mod cdp;
pub(crate) mod json_scan;
#[cfg(target_os = "macos")]
pub mod webkit;

pub mod bidi;

/// Empty JSON object `{}` — avoids `serde_json::json!({})` heap allocation per call.
#[inline]
pub(crate) fn empty_params() -> serde_json::Value {
  serde_json::Value::Object(serde_json::Map::new())
}

use crate::events::EventEmitter;
use crate::state::{ConsoleMsg, NetRequest};
use std::sync::Arc;
use tokio::sync::RwLock;

// ─── Backend-agnostic types ─────────────────────────────────────────────────

/// Frame metadata (backend-agnostic).
#[derive(Debug, Clone, serde::Serialize)]
pub struct FrameInfo {
  pub frame_id: String,
  pub parent_frame_id: Option<String>,
  pub name: String,
  pub url: String,
}

/// Accessibility tree node (backend-agnostic).
#[derive(Debug, Clone)]
pub struct AxNodeData {
  pub node_id: String,
  pub parent_id: Option<String>,
  pub backend_dom_node_id: Option<i64>,
  pub ignored: bool,
  pub role: Option<String>,
  pub name: Option<String>,
  pub description: Option<String>,
  pub properties: Vec<AxProperty>,
}

#[derive(Debug, Clone)]
pub struct AxProperty {
  pub name: String,
  pub value: Option<serde_json::Value>,
}

/// Cookie `SameSite` attribute (matches Playwright's `Strict | Lax | None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SameSite {
  Strict,
  Lax,
  None,
}

impl SameSite {
  /// Convert to a CDP/WebKit string.
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Strict => "Strict",
      Self::Lax => "Lax",
      Self::None => "None",
    }
  }
}

impl std::str::FromStr for SameSite {
  type Err = ();

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "Strict" => Ok(Self::Strict),
      "Lax" => Ok(Self::Lax),
      "None" => Ok(Self::None),
      _ => Err(()),
    }
  }
}

/// Cookie data (backend-agnostic, matches Playwright's `NetworkCookie`).
///
/// Wire format is camelCase (`httpOnly`, `sameSite`) to match Playwright /
/// CDP / Web Cookies RFC; Rust field names stay `snake_case` per convention.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CookieData {
  pub name: String,
  pub value: String,
  pub domain: String,
  pub path: String,
  pub secure: bool,
  pub http_only: bool,
  pub expires: Option<f64>,
  /// `SameSite` attribute (`Strict`, `Lax`, or `None`).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub same_site: Option<SameSite>,
}

/// Options for setting a cookie (matches Playwright's `SetNetworkCookieParam`).
/// Use `url` to derive domain/path automatically, or set `domain`/`path` directly.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCookieParams {
  pub name: String,
  pub value: String,
  /// URL to derive domain/path from. Mutually exclusive with domain/path.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub url: Option<String>,
  #[serde(default)]
  pub domain: String,
  #[serde(default)]
  pub path: String,
  #[serde(default)]
  pub secure: bool,
  #[serde(default)]
  pub http_only: bool,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub expires: Option<f64>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub same_site: Option<SameSite>,
}

impl From<SetCookieParams> for CookieData {
  fn from(p: SetCookieParams) -> Self {
    Self {
      name: p.name,
      value: p.value,
      domain: p.domain,
      path: if p.path.is_empty() { "/".to_string() } else { p.path },
      secure: p.secure,
      http_only: p.http_only,
      expires: p.expires,
      same_site: p.same_site,
    }
  }
}

/// Options for clearing cookies (matches Playwright's `ClearNetworkCookieOptions`).
/// All fields are optional filters -- only cookies matching ALL specified filters are cleared.
/// If no filters are specified, all cookies are cleared.
#[derive(Debug, Clone, Default)]
pub struct ClearCookieOptions {
  /// Filter by cookie name (exact match).
  pub name: Option<String>,
  /// Filter by domain (exact match).
  pub domain: Option<String>,
  /// Filter by path (exact match).
  pub path: Option<String>,
}

/// Backend-level screenshot options — the flat, Playwright-independent
/// wire struct each backend consumes. `crate::options::ScreenshotOptions`
/// (the user-facing Playwright-shaped bag) is lowered into this by
/// [`crate::page::Page::screenshot`], which also handles the Rust-side
/// concerns (`path` write-to-disk, `timeout` race) that don't belong in
/// the per-backend dispatch path.
#[derive(Debug, Clone)]
pub struct ScreenshotOpts {
  pub format: ImageFormat,
  pub quality: Option<i64>,
  pub full_page: bool,
  /// Pixel rectangle relative to the viewport (or full-page bounds
  /// when `full_page` is true). `None` captures the whole viewport /
  /// full page.
  pub clip: Option<crate::options::ClipRect>,
  /// When `true`, emit a PNG with transparent pixels where the page
  /// doesn't have its own background. Ignored for JPEG (no alpha).
  pub omit_background: bool,
  /// `"css"` → one image pixel per CSS pixel (smaller, stable across
  /// DPR); `"device"` → one image pixel per device pixel (default,
  /// Retina captures are 2× bigger). `None` = Playwright default.
  pub scale: Option<ScreenshotScale>,
  /// `"disabled"` pauses CSS animations and Web Animations during
  /// capture; finite animations fast-forward to completion, infinite
  /// ones revert to their initial state. `"allow"` leaves them
  /// running. `None` = Playwright default (`"allow"`).
  pub animations: Option<ScreenshotAnimations>,
  /// `"hide"` (Playwright default) hides the text caret; `"initial"`
  /// leaves it visible. `None` = Playwright default.
  pub caret: Option<ScreenshotCaret>,
  /// Selectors whose matches are overlaid with [`Self::mask_color`]
  /// before capture. Backends resolve each against the target and
  /// paint a fixed-position div at each element's bounding rect.
  pub mask: Vec<String>,
  /// CSS color for the mask overlay. Backends default to `#FF00FF`
  /// (Playwright's pink) when this is `None`.
  pub mask_color: Option<String>,
  /// Raw CSS injected before capture and removed afterwards. Pierces
  /// shadow DOM, applies to subframes.
  pub style: Option<String>,
}

impl Default for ScreenshotOpts {
  fn default() -> Self {
    Self {
      format: ImageFormat::Png,
      quality: None,
      full_page: false,
      clip: None,
      omit_background: false,
      scale: None,
      animations: None,
      caret: None,
      mask: Vec::new(),
      mask_color: None,
      style: None,
    }
  }
}

/// `scale` option for screenshots — mirrors Playwright's `"css" | "device"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotScale {
  /// One image pixel per CSS pixel — small, DPR-independent output.
  Css,
  /// One image pixel per device pixel — sharp, larger on Retina.
  Device,
}

/// `animations` option for screenshots — mirrors Playwright's
/// `"disabled" | "allow"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotAnimations {
  /// Pause animations during capture.
  Disabled,
  /// Leave animations running.
  Allow,
}

/// `caret` option for screenshots — mirrors Playwright's `"hide" | "initial"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotCaret {
  /// Hide the text input caret (Playwright default).
  Hide,
  /// Leave the caret visible in its natural state.
  Initial,
}

/// Backend-agnostic JS helpers for the DOM-side part of `screenshot()`
/// — caret hiding, user-style injection, animation pause via CSS, and
/// mask overlay painting. Each backend wraps its own
/// protocol-specific execution (CDP `Runtime.evaluate`, `BiDi`
/// `script.callFunction`, `WebKit` `evaluate`) around these helpers so
/// all three produce the same observable DOM state before capturing.
pub mod screenshot_js {
  use super::{ScreenshotAnimations, ScreenshotCaret, ScreenshotOpts};

  /// Build the combined CSS rules that `screenshot()` injects before
  /// capture: caret-hide (unless the caller explicitly opted into
  /// `Initial`), optional user `style`, and optional animation pause.
  /// Returns an empty string when no rules apply — the caller should
  /// skip the install/teardown JS entirely in that case.
  #[must_use]
  pub fn build_css(opts: &ScreenshotOpts) -> String {
    let mut css = String::new();
    if !matches!(opts.caret, Some(ScreenshotCaret::Initial)) {
      css.push_str("* { caret-color: transparent !important; }");
    }
    if matches!(opts.animations, Some(ScreenshotAnimations::Disabled)) {
      css.push_str(" *, *::before, *::after { animation-play-state: paused !important; transition: none !important; }");
    }
    if let Some(ref user) = opts.style {
      css.push_str(user);
    }
    css
  }

  /// Build a JS expression that installs a `<style id="__fd_screenshot_style__">`
  /// element carrying the supplied CSS. Paired with [`uninstall_style_js`]
  /// for teardown.
  #[must_use]
  pub fn install_style_js(css: &str) -> String {
    let esc = css.replace('\\', "\\\\").replace('`', "\\`");
    format!(
      r"(function(){{
        const s = document.createElement('style');
        s.id = '__fd_screenshot_style__';
        s.textContent = `{esc}`;
        (document.head || document.documentElement).appendChild(s);
      }})()"
    )
  }

  /// Removes the style element installed by [`install_style_js`].
  #[must_use]
  pub fn uninstall_style_js() -> &'static str {
    "document.getElementById('__fd_screenshot_style__')?.remove()"
  }

  /// Build a JS expression that paints a fixed `<div>` over every
  /// match of each selector in [`ScreenshotOpts::mask`]. Returns
  /// `None` when there are no selectors to mask — caller should skip
  /// the install/teardown JS entirely.
  ///
  /// The overlay divs are tagged with a random class name stored on
  /// `window.__fd_mask_tag` so [`uninstall_mask_js`] can remove them
  /// without relying on the selectors resolving to the same matches
  /// a second time.
  #[must_use]
  pub fn install_mask_js(opts: &ScreenshotOpts) -> Option<String> {
    if opts.mask.is_empty() {
      return None;
    }
    let selectors_json = serde_json::to_string(&opts.mask).unwrap_or_else(|_| "[]".into());
    let color = opts.mask_color.clone().unwrap_or_else(|| "#FF00FF".into());
    let color_json = serde_json::to_string(&color).unwrap_or_else(|_| "\"#FF00FF\"".into());
    Some(format!(
      r"(function(){{
        const selectors = {selectors_json};
        const color = {color_json};
        const tag = '__fd_mask_' + Math.random().toString(36).slice(2);
        window.__fd_mask_tag = tag;
        for (const sel of selectors) {{
          try {{
            const els = document.querySelectorAll(sel);
            for (const el of els) {{
              const r = el.getBoundingClientRect();
              const o = document.createElement('div');
              o.className = tag;
              o.style.cssText = `all: initial; position: fixed; left: ${{r.left}}px; top: ${{r.top}}px; width: ${{r.width}}px; height: ${{r.height}}px; background: ${{color}}; z-index: 2147483647; pointer-events: none;`;
              document.body.appendChild(o);
            }}
          }} catch (e) {{}}
        }}
      }})()"
    ))
  }

  /// Removes the mask overlay divs installed by [`install_mask_js`].
  #[must_use]
  pub fn uninstall_mask_js() -> &'static str {
    "(function(){const t=window.__fd_mask_tag;if(!t)return;document.querySelectorAll('.'+t).forEach(n=>n.remove());delete window.__fd_mask_tag;})()"
  }
}

/// Image format for screenshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
  Png,
  Jpeg,
  Webp,
}

/// Performance metric (backend-agnostic).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MetricData {
  pub name: String,
  pub value: f64,
}

/// Navigation lifecycle target — which CDP event to wait for after Page.navigate.
/// Matches Playwright's `waitUntil` semantics exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavLifecycle {
  /// `Page.frameNavigated` — response committed, new document started.
  Commit,
  /// `Page.lifecycleEvent` name="`DOMContentLoaded`" — HTML parsed, DOM ready.
  DomContentLoaded,
  /// `Page.lifecycleEvent` name="load" — all resources loaded.
  Load,
}

impl NavLifecycle {
  /// Parse from a `waitUntil` string (Playwright / MCP convention).
  /// Unknown values default to `Load`.
  #[must_use]
  pub fn parse_lifecycle(s: &str) -> Self {
    match s {
      "commit" => Self::Commit,
      "domcontentloaded" => Self::DomContentLoaded,
      _ => Self::Load,
    }
  }
}

/// Which backend to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
  /// Chrome `DevTools` Protocol over pipes (--remote-debugging-pipe)
  CdpPipe,
  /// Chrome `DevTools` Protocol over WebSocket (our own, fully parallel)
  CdpRaw,
  /// Native WebKit/WKWebView (macOS only)
  #[cfg(target_os = "macos")]
  WebKit,
  /// `WebDriver` `BiDi` protocol (cross-browser: Chrome, Firefox, future Safari)
  Bidi,
}

// ─── AnyBrowser ─────────────────────────────────────────────────────────────

/// Browser instance — enum dispatch across backends.
#[derive(Clone)]
pub enum AnyBrowser {
  CdpPipe(cdp::CdpBrowser<cdp::pipe::PipeTransport>),
  CdpRaw(cdp::CdpBrowser<cdp::ws::WsTransport>),
  #[cfg(target_os = "macos")]
  WebKit(webkit::WebKitBrowser),

  Bidi(bidi::BidiBrowser),
}

impl AnyBrowser {
  /// List all open pages in this browser.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to enumerate targets or pages.
  pub async fn pages(&self) -> Result<Vec<AnyPage>, String> {
    match self {
      Self::CdpPipe(b) => Box::pin(b.pages()).await,
      Self::CdpRaw(b) => Box::pin(b.pages()).await,
      #[cfg(target_os = "macos")]
      Self::WebKit(b) => Box::pin(b.pages()).await,

      Self::Bidi(b) => Box::pin(b.pages()).await,
    }
  }

  /// Create a new browser context (isolated cookies, storage, cache).
  ///
  /// # Errors
  ///
  /// Returns an error if context creation fails.
  pub async fn new_context(&self) -> Result<String, String> {
    match self {
      Self::CdpPipe(b) => b.new_context().await,
      Self::CdpRaw(b) => b.new_context().await,
      #[cfg(target_os = "macos")]
      Self::WebKit(_) => Err("WebKit does not support multiple browser contexts".into()),
      Self::Bidi(b) => b.new_context().await,
    }
  }

  /// Dispose a browser context.
  ///
  /// # Errors
  ///
  /// Returns an error if context disposal fails.
  pub async fn dispose_context(&self, browser_context_id: &str) -> Result<(), String> {
    match self {
      Self::CdpPipe(b) => b.dispose_context(browser_context_id).await,
      Self::CdpRaw(b) => b.dispose_context(browser_context_id).await,
      #[cfg(target_os = "macos")]
      Self::WebKit(_) => Ok(()),
      Self::Bidi(b) => b.dispose_context(browser_context_id).await,
    }
  }

  /// Open a new page, optionally in a specific browser context.
  ///
  /// # Errors
  ///
  /// Returns an error if the backend fails to create a new target or navigate to the URL.
  pub async fn new_page(
    &self,
    url: &str,
    browser_context_id: Option<&str>,
    viewport: Option<&crate::options::ViewportConfig>,
  ) -> Result<AnyPage, String> {
    match self {
      Self::CdpPipe(b) => Box::pin(b.new_page(url, browser_context_id, viewport)).await,
      Self::CdpRaw(b) => Box::pin(b.new_page(url, browser_context_id, viewport)).await,
      #[cfg(target_os = "macos")]
      Self::WebKit(b) => Box::pin(b.new_page(url)).await,
      Self::Bidi(b) => Box::pin(b.new_page(url, browser_context_id, viewport)).await,
    }
  }

  /// Close the browser and all its pages.
  ///
  /// # Errors
  ///
  /// Returns an error if the browser process fails to shut down cleanly.
  pub async fn close(&mut self) -> Result<(), String> {
    match self {
      Self::CdpPipe(b) => b.close().await,
      Self::CdpRaw(b) => b.close().await,
      #[cfg(target_os = "macos")]
      Self::WebKit(b) => b.close().await,

      Self::Bidi(b) => b.close().await,
    }
  }

  /// Real product version string for the running browser, captured at
  /// handshake/session-open time. Every backend surfaces a genuine value
  /// — no placeholders:
  ///
  /// * `cdp-pipe` / `cdp-raw` → CDP `Browser.getVersion().product`
  ///   (e.g. `"HeadlessChrome/120.0.6099.109"`).
  /// * `webkit` → `Op::GetWebKitVersion` IPC → `CFBundleShortVersionString`
  ///   from the running `WebKit.framework` (e.g. `"WebKit/617.1.2 (17618)"`).
  /// * `bidi` → `BiDi` `session.new` response capabilities, formatted as
  ///   `"{browserName}/{browserVersion}"` (e.g. `"firefox/135.0.1"`).
  #[must_use]
  pub fn version(&self) -> String {
    match self {
      Self::CdpPipe(b) => b.version().to_string(),
      Self::CdpRaw(b) => b.version().to_string(),
      #[cfg(target_os = "macos")]
      Self::WebKit(b) => b.version().to_string(),

      Self::Bidi(b) => b.version(),
    }
  }
}

// ─── AnyPage ────────────────────────────────────────────────────────────────

/// Page handle — enum dispatch across backends. Cheaply cloneable (Arc-based).
#[derive(Clone)]
pub enum AnyPage {
  CdpPipe(cdp::CdpPage<cdp::pipe::PipeTransport>),
  CdpRaw(cdp::CdpPage<cdp::ws::WsTransport>),
  #[cfg(target_os = "macos")]
  WebKit(webkit::WebKitPage),

  Bidi(bidi::BidiPage),
}

/// Macro to dispatch a method call across all `AnyPage` variants.
macro_rules! page_dispatch {
    ($self:expr, $method:ident ( $($arg:expr),* $(,)? )) => {
        match $self {
            AnyPage::CdpPipe(p) => p.$method($($arg),*).await,
            AnyPage::CdpRaw(p) => p.$method($($arg),*).await,
            #[cfg(target_os = "macos")]
            AnyPage::WebKit(p) => p.$method($($arg),*).await,

            AnyPage::Bidi(p) => p.$method($($arg),*).await,
        }
    };
}

impl AnyPage {
  // ── Events ──

  /// Get the event emitter for this page.
  #[must_use]
  pub fn events(&self) -> &EventEmitter {
    match self {
      AnyPage::CdpPipe(p) => &p.events,
      AnyPage::CdpRaw(p) => &p.events,
      #[cfg(target_os = "macos")]
      AnyPage::WebKit(p) => &p.events,

      AnyPage::Bidi(p) => &p.events,
    }
  }

  // ── Frames ──

  pub async fn get_frame_tree(&self) -> Result<Vec<FrameInfo>, String> {
    page_dispatch!(self, get_frame_tree())
  }

  pub async fn evaluate_in_frame(&self, expression: &str, frame_id: &str) -> Result<Option<serde_json::Value>, String> {
    page_dispatch!(self, evaluate_in_frame(expression, frame_id))
  }

  // ── Navigation ──

  pub async fn goto(
    &self,
    url: &str,
    lifecycle: NavLifecycle,
    timeout_ms: u64,
    referer: Option<&str>,
  ) -> Result<(), String> {
    page_dispatch!(self, goto(url, lifecycle, timeout_ms, referer))
  }

  pub async fn wait_for_navigation(&self) -> Result<(), String> {
    page_dispatch!(self, wait_for_navigation())
  }

  pub async fn reload(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<(), String> {
    page_dispatch!(self, reload(lifecycle, timeout_ms))
  }

  pub async fn go_back(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<(), String> {
    page_dispatch!(self, go_back(lifecycle, timeout_ms))
  }

  pub async fn go_forward(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<(), String> {
    page_dispatch!(self, go_forward(lifecycle, timeout_ms))
  }

  pub async fn url(&self) -> Result<Option<String>, String> {
    page_dispatch!(self, url())
  }

  pub async fn title(&self) -> Result<Option<String>, String> {
    page_dispatch!(self, title())
  }

  // ── JavaScript ──

  /// Returns a script that ensures the selector engine is injected.
  /// Mirrored after Playwright's `injectedScript()`.
  pub async fn injected_script(&self) -> Result<String, String> {
    page_dispatch!(self, injected_script())
  }

  pub async fn ensure_engine_injected(&self) -> Result<(), String> {
    page_dispatch!(self, ensure_engine_injected())
  }

  pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>, String> {
    page_dispatch!(self, evaluate(expression))
  }

  // ── Elements ──

  pub async fn find_element(&self, selector: &str) -> Result<AnyElement, String> {
    page_dispatch!(self, find_element(selector))
  }

  pub async fn evaluate_to_element(&self, js: &str) -> Result<AnyElement, String> {
    page_dispatch!(self, evaluate_to_element(js))
  }

  // ── Content ──

  pub async fn content(&self) -> Result<String, String> {
    page_dispatch!(self, content())
  }

  pub async fn set_content(&self, html: &str) -> Result<(), String> {
    page_dispatch!(self, set_content(html))
  }

  // ── Screenshots ──

  pub async fn screenshot(&self, opts: ScreenshotOpts) -> Result<Vec<u8>, String> {
    page_dispatch!(self, screenshot(opts))
  }

  // ── Accessibility ──

  pub async fn accessibility_tree(&self) -> Result<Vec<AxNodeData>, String> {
    page_dispatch!(self, accessibility_tree())
  }

  pub async fn accessibility_tree_with_depth(&self, depth: i32) -> Result<Vec<AxNodeData>, String> {
    page_dispatch!(self, accessibility_tree_with_depth(depth))
  }

  // ── Input ──

  pub async fn click_at(&self, x: f64, y: f64) -> Result<(), String> {
    page_dispatch!(self, click_at(x, y))
  }

  pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<(), String> {
    page_dispatch!(self, click_at_opts(x, y, button, click_count))
  }

  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<(), String> {
    page_dispatch!(self, move_mouse(x, y))
  }

  pub async fn move_mouse_smooth(
    &self,
    from_x: f64,
    from_y: f64,
    to_x: f64,
    to_y: f64,
    steps: u32,
  ) -> Result<(), String> {
    page_dispatch!(self, move_mouse_smooth(from_x, from_y, to_x, to_y, steps))
  }

  pub async fn mouse_wheel(&self, delta_x: f64, delta_y: f64) -> Result<(), String> {
    page_dispatch!(self, mouse_wheel(delta_x, delta_y))
  }

  pub async fn mouse_down(&self, x: f64, y: f64, button: &str) -> Result<(), String> {
    page_dispatch!(self, mouse_down(x, y, button))
  }

  pub async fn mouse_up(&self, x: f64, y: f64, button: &str) -> Result<(), String> {
    page_dispatch!(self, mouse_up(x, y, button))
  }

  pub async fn click_and_drag(&self, from: (f64, f64), to: (f64, f64), steps: u32) -> Result<(), String> {
    page_dispatch!(self, click_and_drag(from, to, steps))
  }

  pub async fn type_str(&self, text: &str) -> Result<(), String> {
    page_dispatch!(self, type_str(text))
  }

  /// Insert text without emitting keyboard events (only `input` event).
  /// This is Playwright's `keyboard.insertText()` semantic.
  pub async fn insert_text(&self, text: &str) -> Result<(), String> {
    // type_str on all backends uses Input.insertText / equivalent
    self.type_str(text).await
  }

  pub async fn key_down(&self, key: &str) -> Result<(), String> {
    page_dispatch!(self, key_down(key))
  }

  pub async fn key_up(&self, key: &str) -> Result<(), String> {
    page_dispatch!(self, key_up(key))
  }

  pub async fn press_key(&self, key: &str) -> Result<(), String> {
    page_dispatch!(self, press_key(key))
  }

  // ── Cookies ──

  pub async fn get_cookies(&self) -> Result<Vec<CookieData>, String> {
    page_dispatch!(self, get_cookies())
  }

  pub async fn set_cookie(&self, cookie: CookieData) -> Result<(), String> {
    page_dispatch!(self, set_cookie(cookie))
  }

  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<(), String> {
    page_dispatch!(self, delete_cookie(name, domain))
  }

  pub async fn clear_cookies(&self) -> Result<(), String> {
    page_dispatch!(self, clear_cookies())
  }

  /// Clear cookies matching the given filters. If no filters, clears all.
  pub async fn clear_cookies_filtered(&self, options: &ClearCookieOptions) -> Result<(), String> {
    if options.name.is_none() && options.domain.is_none() && options.path.is_none() {
      return self.clear_cookies().await;
    }
    // Get all cookies, delete the ones that match the filters.
    let cookies = self.get_cookies().await?;
    for c in &cookies {
      let name_match = options.name.as_ref().is_none_or(|n| &c.name == n);
      let domain_match = options.domain.as_ref().is_none_or(|d| &c.domain == d);
      let path_match = options.path.as_ref().is_none_or(|p| &c.path == p);
      if name_match && domain_match && path_match {
        self.delete_cookie(&c.name, Some(&c.domain)).await?;
      }
    }
    Ok(())
  }

  // ── Emulation ──

  pub async fn emulate_viewport(&self, config: &crate::options::ViewportConfig) -> Result<(), String> {
    page_dispatch!(self, emulate_viewport(config))
  }

  pub async fn set_user_agent(&self, ua: &str) -> Result<(), String> {
    page_dispatch!(self, set_user_agent(ua))
  }

  pub async fn set_geolocation(&self, lat: f64, lng: f64, accuracy: f64) -> Result<(), String> {
    page_dispatch!(self, set_geolocation(lat, lng, accuracy))
  }

  pub async fn set_locale(&self, locale: &str) -> Result<(), String> {
    page_dispatch!(self, set_locale(locale))
  }

  pub async fn set_timezone(&self, timezone_id: &str) -> Result<(), String> {
    page_dispatch!(self, set_timezone(timezone_id))
  }

  pub async fn emulate_media(&self, opts: &crate::options::EmulateMediaOptions) -> Result<(), String> {
    page_dispatch!(self, emulate_media(opts))
  }

  pub async fn set_javascript_enabled(&self, enabled: bool) -> Result<(), String> {
    page_dispatch!(self, set_javascript_enabled(enabled))
  }

  pub async fn set_extra_http_headers(&self, headers: &rustc_hash::FxHashMap<String, String>) -> Result<(), String> {
    page_dispatch!(self, set_extra_http_headers(headers))
  }

  pub async fn grant_permissions(&self, permissions: &[String], origin: Option<&str>) -> Result<(), String> {
    page_dispatch!(self, grant_permissions(permissions, origin))
  }

  pub async fn set_bypass_csp(&self, enabled: bool) -> Result<(), String> {
    page_dispatch!(self, set_bypass_csp(enabled))
  }

  pub async fn set_ignore_certificate_errors(&self, ignore: bool) -> Result<(), String> {
    page_dispatch!(self, set_ignore_certificate_errors(ignore))
  }

  pub async fn set_download_behavior(&self, behavior: &str, download_path: &str) -> Result<(), String> {
    page_dispatch!(self, set_download_behavior(behavior, download_path))
  }

  pub async fn set_http_credentials(&self, username: &str, password: &str) -> Result<(), String> {
    page_dispatch!(self, set_http_credentials(username, password))
  }

  pub async fn set_service_workers_blocked(&self, blocked: bool) -> Result<(), String> {
    page_dispatch!(self, set_service_workers_blocked(blocked))
  }

  pub async fn reset_permissions(&self) -> Result<(), String> {
    page_dispatch!(self, reset_permissions())
  }

  pub async fn set_focus_emulation_enabled(&self, enabled: bool) -> Result<(), String> {
    page_dispatch!(self, set_focus_emulation_enabled(enabled))
  }

  // ── Network ──

  pub async fn set_network_state(&self, offline: bool, latency: f64, download: f64, upload: f64) -> Result<(), String> {
    page_dispatch!(self, set_network_state(offline, latency, download, upload))
  }

  // ── Tracing ──

  pub async fn start_tracing(&self) -> Result<(), String> {
    page_dispatch!(self, start_tracing())
  }

  pub async fn stop_tracing(&self) -> Result<(), String> {
    page_dispatch!(self, stop_tracing())
  }

  pub async fn metrics(&self) -> Result<Vec<MetricData>, String> {
    page_dispatch!(self, metrics())
  }

  // ── Ref resolution ──

  pub async fn resolve_backend_node(&self, backend_node_id: i64, ref_id: &str) -> Result<AnyElement, String> {
    page_dispatch!(self, resolve_backend_node(backend_node_id, ref_id))
  }

  // ── Event listeners ──

  pub fn attach_listeners(
    &self,
    console_log: Arc<RwLock<Vec<ConsoleMsg>>>,
    network_log: Arc<RwLock<Vec<NetRequest>>>,
    dialog_log: Arc<RwLock<Vec<crate::state::DialogEvent>>>,
  ) {
    match self {
      Self::CdpPipe(p) => p.attach_listeners(console_log, network_log, dialog_log),
      Self::CdpRaw(p) => p.attach_listeners(console_log, network_log, dialog_log),
      #[cfg(target_os = "macos")]
      Self::WebKit(p) => p.attach_listeners(console_log, network_log, dialog_log),

      Self::Bidi(p) => p.attach_listeners(console_log, network_log, dialog_log),
    }
  }

  // ── Element screenshot (by selector) ──

  pub async fn screenshot_element(&self, selector: &str, format: ImageFormat) -> Result<Vec<u8>, String> {
    page_dispatch!(self, screenshot_element(selector, format))
  }

  // ── PDF generation ──

  pub async fn pdf(&self, opts: crate::options::PdfOptions) -> Result<Vec<u8>, String> {
    page_dispatch!(self, pdf(opts))
  }

  // ── Screencast (video recording) ──

  pub async fn start_screencast(
    &self,
    quality: u8,
    max_width: u32,
    max_height: u32,
  ) -> Result<tokio::sync::mpsc::UnboundedReceiver<(Vec<u8>, f64)>, String> {
    match self {
      AnyPage::CdpPipe(p) => p.start_screencast(quality, max_width, max_height).await,
      AnyPage::CdpRaw(p) => p.start_screencast(quality, max_width, max_height).await,
      #[cfg(target_os = "macos")]
      AnyPage::WebKit(_) => Err("Video recording is not supported on WebKit backend".into()),

      AnyPage::Bidi(p) => p.start_screencast(quality, max_width, max_height).await,
    }
  }

  pub async fn stop_screencast(&self) -> Result<(), String> {
    match self {
      AnyPage::CdpPipe(p) => p.stop_screencast().await,
      AnyPage::CdpRaw(p) => p.stop_screencast().await,
      #[cfg(target_os = "macos")]
      AnyPage::WebKit(_) => Ok(()), // No-op if never started.

      AnyPage::Bidi(p) => p.stop_screencast().await,
    }
  }

  // ── File upload ──

  pub async fn set_file_input(&self, selector: &str, paths: &[String]) -> Result<(), String> {
    page_dispatch!(self, set_file_input(selector, paths))
  }

  // ── Dialog handling ──

  pub async fn set_dialog_handler(&self, handler: crate::events::DialogHandler) {
    match self {
      Self::CdpPipe(p) => *p.dialog_handler.write().await = handler,
      Self::CdpRaw(p) => *p.dialog_handler.write().await = handler,
      #[cfg(target_os = "macos")]
      Self::WebKit(_) => {
        // WebKit dialog handling is in the ObjC subprocess via WKUIDelegate.
        // Custom handlers would need a new IPC op. For now, auto-behavior only.
      },

      Self::Bidi(p) => *p.dialog_handler.write().await = handler,
    }
  }

  // ── Network Interception ──

  pub async fn route(
    &self,
    matcher: crate::url_matcher::UrlMatcher,
    handler: crate::route::RouteHandler,
  ) -> Result<(), String> {
    page_dispatch!(self, route(matcher, handler))
  }

  pub async fn unroute(&self, matcher: &crate::url_matcher::UrlMatcher) -> Result<(), String> {
    page_dispatch!(self, unroute(matcher))
  }

  // ── Lifecycle ──

  pub async fn close_page(&self, opts: crate::options::PageCloseOptions) -> Result<(), String> {
    page_dispatch!(self, close_page(opts))
  }

  #[must_use]
  pub fn is_closed(&self) -> bool {
    match self {
      Self::CdpPipe(p) => p.is_closed(),
      Self::CdpRaw(p) => p.is_closed(),
      #[cfg(target_os = "macos")]
      Self::WebKit(p) => p.is_closed(),

      Self::Bidi(p) => p.is_closed(),
    }
  }

  // ── Exposed Functions ──

  pub async fn expose_function(&self, name: &str, func: crate::events::ExposedFn) -> Result<(), String> {
    page_dispatch!(self, expose_function(name, func))
  }

  pub async fn remove_exposed_function(&self, name: &str) -> Result<(), String> {
    page_dispatch!(self, remove_exposed_function(name))
  }

  // ── Init Scripts ──

  pub async fn add_init_script(&self, source: &str) -> Result<String, String> {
    page_dispatch!(self, add_init_script(source))
  }

  pub async fn remove_init_script(&self, identifier: &str) -> Result<(), String> {
    page_dispatch!(self, remove_init_script(identifier))
  }
}

// ─── AnyElement ─────────────────────────────────────────────────────────────

/// Element handle — enum dispatch across backends.
pub enum AnyElement {
  CdpPipe(cdp::CdpElement<cdp::pipe::PipeTransport>),
  CdpRaw(cdp::CdpElement<cdp::ws::WsTransport>),
  #[cfg(target_os = "macos")]
  WebKit(webkit::WebKitElement),

  Bidi(bidi::BidiElement),
}

macro_rules! element_dispatch {
    ($self:expr, $method:ident ( $($arg:expr),* $(,)? )) => {
        match $self {
            AnyElement::CdpPipe(e) => e.$method($($arg),*).await,
            AnyElement::CdpRaw(e) => e.$method($($arg),*).await,
            #[cfg(target_os = "macos")]
            AnyElement::WebKit(e) => e.$method($($arg),*).await,

            AnyElement::Bidi(e) => e.$method($($arg),*).await,
        }
    };
}

impl AnyElement {
  pub async fn click(&self) -> Result<(), String> {
    element_dispatch!(self, click())
  }

  pub async fn dblclick(&self) -> Result<(), String> {
    element_dispatch!(self, dblclick())
  }

  pub async fn hover(&self) -> Result<(), String> {
    element_dispatch!(self, hover())
  }

  pub async fn type_str(&self, text: &str) -> Result<(), String> {
    element_dispatch!(self, type_str(text))
  }

  pub async fn call_js_fn(&self, function: &str) -> Result<(), String> {
    element_dispatch!(self, call_js_fn(function))
  }

  pub async fn call_js_fn_value(&self, function: &str) -> Result<Option<serde_json::Value>, String> {
    element_dispatch!(self, call_js_fn_value(function))
  }

  pub async fn scroll_into_view(&self) -> Result<(), String> {
    element_dispatch!(self, scroll_into_view())
  }

  pub async fn screenshot(&self, format: ImageFormat) -> Result<Vec<u8>, String> {
    element_dispatch!(self, screenshot(format))
  }
}
