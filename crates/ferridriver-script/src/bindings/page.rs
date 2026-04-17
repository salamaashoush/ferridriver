//! `PageJs`: JS wrapper around `ferridriver::Page`.
//!
//! Methods mirror `ferridriver::Page`'s public surface one-for-one; each is a
//! small delegation that converts `FerriError` into `rquickjs::Error` at the
//! boundary via [`super::convert::FerriResultExt`].

use std::sync::Arc;

use ferridriver::Page;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;

use ferridriver::options::WaitOptions;
use rquickjs::function::Opt;
use serde::Deserialize;

use crate::bindings::convert::{FerriResultExt, serde_from_js};
use crate::bindings::keyboard::KeyboardJs;
use crate::bindings::locator::LocatorJs;
use crate::bindings::mouse::MouseJs;

/// Shape of `waitForSelector` options accepted from JS.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct JsWaitOptions {
  state: Option<String>,
  timeout: Option<u64>,
}

fn parse_wait_options<'js>(
  ctx: &rquickjs::Ctx<'js>,
  value: Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<WaitOptions> {
  match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => {
      let js: JsWaitOptions = serde_from_js(ctx, v)?;
      Ok(WaitOptions {
        state: js.state,
        timeout: js.timeout,
      })
    },
    _ => Ok(WaitOptions::default()),
  }
}

/// JS-visible wrapper around [`ferridriver::Page`].
///
/// Held as `Arc<Page>` so the same page can be shared with the MCP session
/// while the script runs; dropping the wrapper does not close the page.
#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Page")]
pub struct PageJs {
  // rquickjs requires fields to implement Trace/JsLifetime; Arc<Page> does
  // not, and there's nothing inside a Page that holds JS values. Mark with
  // `#[qjs(skip_trace)]` so the macro skips tracing this field.
  #[qjs(skip_trace)]
  inner: Arc<Page>,
}

impl PageJs {
  #[must_use]
  pub fn new(inner: Arc<Page>) -> Self {
    Self { inner }
  }

  #[must_use]
  pub fn page(&self) -> &Arc<Page> {
    &self.inner
  }
}

#[rquickjs::methods]
impl PageJs {
  // ── Navigation ────────────────────────────────────────────────────────────

  /// Navigate to `url`. Mirrors `page.goto(url)` in Playwright.
  #[qjs(rename = "goto")]
  pub async fn goto(&self, url: String) -> rquickjs::Result<()> {
    self.inner.goto(&url, None).await.into_js()
  }

  /// Reload the current page.
  #[qjs(rename = "reload")]
  pub async fn reload(&self) -> rquickjs::Result<()> {
    self.inner.reload(None).await.into_js()
  }

  /// Navigate back in history.
  #[qjs(rename = "goBack")]
  pub async fn go_back(&self) -> rquickjs::Result<()> {
    self.inner.go_back(None).await.into_js()
  }

  /// Navigate forward in history.
  #[qjs(rename = "goForward")]
  pub async fn go_forward(&self) -> rquickjs::Result<()> {
    self.inner.go_forward(None).await.into_js()
  }

  /// Current URL of the page.
  #[qjs(rename = "url")]
  pub async fn url(&self) -> rquickjs::Result<String> {
    self.inner.url().await.into_js()
  }

  /// Document title.
  #[qjs(rename = "title")]
  pub async fn title(&self) -> rquickjs::Result<String> {
    self.inner.title().await.into_js()
  }

  /// Full HTML content of the page.
  #[qjs(rename = "content")]
  pub async fn content(&self) -> rquickjs::Result<String> {
    self.inner.content().await.into_js()
  }

  /// Replace the page's HTML with `html`.
  #[qjs(rename = "setContent")]
  pub async fn set_content(&self, html: String) -> rquickjs::Result<()> {
    self.inner.set_content(&html).await.into_js()
  }

  /// Full page rendered as clean Markdown (headings, lists, links, tables
  /// preserved; chrome and boilerplate stripped).
  #[qjs(rename = "markdown")]
  pub async fn markdown(&self) -> rquickjs::Result<String> {
    self.inner.markdown().await.into_js()
  }

  /// Wait for an element matching `selector`. Optional `options` object
  /// accepts `{ state?: 'visible'|'hidden'|'attached'|'stable', timeout?: ms }`.
  /// Resolves when the condition is met; throws on timeout.
  #[qjs(rename = "waitForSelector")]
  pub async fn wait_for_selector<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = parse_wait_options(&ctx, options)?;
    self.inner.wait_for_selector(&selector, opts).await.into_js()
  }

  // ── Locators ──────────────────────────────────────────────────────────────

  /// Return a new `Locator` rooted at `selector`.
  #[qjs(rename = "locator")]
  pub fn locator(&self, selector: String) -> LocatorJs {
    LocatorJs::new(self.inner.locator(&selector))
  }

  /// Locate elements by ARIA role.
  #[qjs(rename = "getByRole")]
  pub fn get_by_role(&self, role: String) -> LocatorJs {
    LocatorJs::new(
      self
        .inner
        .get_by_role(&role, &ferridriver::options::RoleOptions::default()),
    )
  }

  /// Locate elements containing the given text.
  #[qjs(rename = "getByText")]
  pub fn get_by_text(&self, text: String) -> LocatorJs {
    LocatorJs::new(
      self
        .inner
        .get_by_text(&text, &ferridriver::options::TextOptions::default()),
    )
  }

  /// Locate form controls by associated label text.
  #[qjs(rename = "getByLabel")]
  pub fn get_by_label(&self, text: String) -> LocatorJs {
    LocatorJs::new(
      self
        .inner
        .get_by_label(&text, &ferridriver::options::TextOptions::default()),
    )
  }

  /// Locate inputs by placeholder text.
  #[qjs(rename = "getByPlaceholder")]
  pub fn get_by_placeholder(&self, text: String) -> LocatorJs {
    LocatorJs::new(
      self
        .inner
        .get_by_placeholder(&text, &ferridriver::options::TextOptions::default()),
    )
  }

  /// Locate images/media by alt text.
  #[qjs(rename = "getByAltText")]
  pub fn get_by_alt_text(&self, text: String) -> LocatorJs {
    LocatorJs::new(
      self
        .inner
        .get_by_alt_text(&text, &ferridriver::options::TextOptions::default()),
    )
  }

  /// Locate elements by `data-testid`.
  #[qjs(rename = "getByTestId")]
  pub fn get_by_test_id(&self, test_id: String) -> LocatorJs {
    LocatorJs::new(self.inner.get_by_test_id(&test_id))
  }

  // ── Interaction ───────────────────────────────────────────────────────────

  /// Click the first element matching `selector`.
  #[qjs(rename = "click")]
  pub async fn click(&self, selector: String) -> rquickjs::Result<()> {
    self.inner.click(&selector).await.into_js()
  }

  /// Double-click the first element matching `selector`.
  #[qjs(rename = "dblclick")]
  pub async fn dblclick(&self, selector: String) -> rquickjs::Result<()> {
    self.inner.dblclick(&selector).await.into_js()
  }

  /// Fill `value` into the input matching `selector`.
  #[qjs(rename = "fill")]
  pub async fn fill(&self, selector: String, value: String) -> rquickjs::Result<()> {
    self.inner.fill(&selector, &value).await.into_js()
  }

  /// Type `text` into the input matching `selector`.
  ///
  /// Exposed as `type` in JS (matches Playwright) — Rust renames to avoid
  /// the `type` keyword.
  #[qjs(rename = "type")]
  pub async fn type_(&self, selector: String, text: String) -> rquickjs::Result<()> {
    self.inner.r#type(&selector, &text).await.into_js()
  }

  /// Press `key` on the element matching `selector`.
  #[qjs(rename = "press")]
  pub async fn press(&self, selector: String, key: String) -> rquickjs::Result<()> {
    self.inner.press(&selector, &key).await.into_js()
  }

  /// Hover the first element matching `selector`.
  #[qjs(rename = "hover")]
  pub async fn hover(&self, selector: String) -> rquickjs::Result<()> {
    self.inner.hover(&selector).await.into_js()
  }

  /// Check a checkbox matching `selector`.
  #[qjs(rename = "check")]
  pub async fn check(&self, selector: String) -> rquickjs::Result<()> {
    self.inner.check(&selector).await.into_js()
  }

  /// Uncheck a checkbox matching `selector`.
  #[qjs(rename = "uncheck")]
  pub async fn uncheck(&self, selector: String) -> rquickjs::Result<()> {
    self.inner.uncheck(&selector).await.into_js()
  }

  /// Select an option by value on a `<select>` matching `selector`. Returns
  /// the values of the selected options.
  #[qjs(rename = "selectOption")]
  pub async fn select_option(&self, selector: String, value: String) -> rquickjs::Result<Vec<String>> {
    self.inner.select_option(&selector, &value).await.into_js()
  }

  // ── Info ──────────────────────────────────────────────────────────────────

  /// Text content of the first element matching `selector` (or `null`).
  #[qjs(rename = "textContent")]
  pub async fn text_content(&self, selector: String) -> rquickjs::Result<Option<String>> {
    self.inner.text_content(&selector).await.into_js()
  }

  /// `innerText` of the first element matching `selector`.
  #[qjs(rename = "innerText")]
  pub async fn inner_text(&self, selector: String) -> rquickjs::Result<String> {
    self.inner.inner_text(&selector).await.into_js()
  }

  /// `innerHTML` of the first element matching `selector`.
  #[qjs(rename = "innerHTML")]
  pub async fn inner_html(&self, selector: String) -> rquickjs::Result<String> {
    self.inner.inner_html(&selector).await.into_js()
  }

  /// Current input value of the first element matching `selector`.
  #[qjs(rename = "inputValue")]
  pub async fn input_value(&self, selector: String) -> rquickjs::Result<String> {
    self.inner.input_value(&selector).await.into_js()
  }

  /// Get attribute `name` on the first element matching `selector`
  /// (or `null` if the attribute is absent).
  #[qjs(rename = "getAttribute")]
  pub async fn get_attribute(&self, selector: String, name: String) -> rquickjs::Result<Option<String>> {
    self.inner.get_attribute(&selector, &name).await.into_js()
  }

  /// Whether the first element matching `selector` is visible.
  #[qjs(rename = "isVisible")]
  pub async fn is_visible(&self, selector: String) -> rquickjs::Result<bool> {
    self.inner.is_visible(&selector).await.into_js()
  }

  /// Whether the first element matching `selector` is hidden.
  #[qjs(rename = "isHidden")]
  pub async fn is_hidden(&self, selector: String) -> rquickjs::Result<bool> {
    self.inner.is_hidden(&selector).await.into_js()
  }

  /// Whether the first element matching `selector` is enabled.
  #[qjs(rename = "isEnabled")]
  pub async fn is_enabled(&self, selector: String) -> rquickjs::Result<bool> {
    self.inner.is_enabled(&selector).await.into_js()
  }

  /// Whether the first element matching `selector` is disabled.
  #[qjs(rename = "isDisabled")]
  pub async fn is_disabled(&self, selector: String) -> rquickjs::Result<bool> {
    self.inner.is_disabled(&selector).await.into_js()
  }

  /// Whether the first checkbox matching `selector` is checked.
  #[qjs(rename = "isChecked")]
  pub async fn is_checked(&self, selector: String) -> rquickjs::Result<bool> {
    self.inner.is_checked(&selector).await.into_js()
  }

  // ── Evaluation ────────────────────────────────────────────────────────────

  /// Evaluate a JavaScript expression in the page's JS context and return
  /// the JSON-serialized result.
  ///
  /// NOTE (parity gap): core's `evaluate` takes a string. Playwright's
  /// `evaluate(fn, arg)` function-argument form is not supported yet — see
  /// `PLAYWRIGHT_COMPAT.md` "Gaps surfaced by scripting bindings" item 1.
  #[qjs(rename = "evaluate")]
  pub async fn evaluate(&self, expression: String) -> rquickjs::Result<Option<String>> {
    let value = self.inner.evaluate(&expression).await.into_js()?;
    Ok(value.map(|v| serde_json::to_string(&v).unwrap_or_default()))
  }

  // ── Mouse / keyboard namespaces (Playwright parity) ──────────────────────

  /// `page.mouse.*` namespace: `click`, `dblclick`, `down`, `up`, `wheel`.
  /// Exposed as a JS property, matching Playwright.
  #[qjs(get, rename = "mouse")]
  pub fn mouse(&self) -> MouseJs {
    MouseJs::new(self.inner.clone())
  }

  /// `page.keyboard.*` namespace: `down`, `up`, `press` (no selector; acts on
  /// the currently focused element). Exposed as a JS property.
  #[qjs(get, rename = "keyboard")]
  pub fn keyboard(&self) -> KeyboardJs {
    KeyboardJs::new(self.inner.clone())
  }

  /// Click at viewport coordinates without a selector.
  #[qjs(rename = "clickAt")]
  pub async fn click_at(&self, x: f64, y: f64) -> rquickjs::Result<()> {
    self.inner.click_at(x, y).await.into_js()
  }

  /// Interpolated mouse move from `(fromX, fromY)` to `(toX, toY)` in `steps`
  /// intermediate points. Used for coordinate-based drag: `mouse.down()` →
  /// `moveMouseSmooth(...)` → `mouse.up()`.
  #[qjs(rename = "moveMouseSmooth")]
  pub async fn move_mouse_smooth(
    &self,
    from_x: f64,
    from_y: f64,
    to_x: f64,
    to_y: f64,
    steps: u32,
  ) -> rquickjs::Result<()> {
    self
      .inner
      .move_mouse_smooth(from_x, from_y, to_x, to_y, steps)
      .await
      .into_js()
  }

  /// Drag from the source selector to the target selector.
  #[qjs(rename = "dragAndDrop")]
  pub async fn drag_and_drop(&self, source: String, target: String) -> rquickjs::Result<()> {
    self.inner.drag_and_drop(&source, &target).await.into_js()
  }

  // ── File input ────────────────────────────────────────────────────────────

  /// Attach files to a `<input type="file">` selector. `paths` is a list of
  /// absolute file paths.
  #[qjs(rename = "setInputFiles")]
  pub async fn set_input_files(&self, selector: String, paths: Vec<String>) -> rquickjs::Result<()> {
    self.inner.set_input_files(&selector, &paths).await.into_js()
  }

  // ── Emulation (page-scoped) ──────────────────────────────────────────────

  /// Override the User-Agent string for this page.
  #[qjs(rename = "setUserAgent")]
  pub async fn set_user_agent(&self, user_agent: String) -> rquickjs::Result<()> {
    self.inner.set_user_agent(&user_agent).await.into_js()
  }

  /// Override the viewport size for this page.
  #[qjs(rename = "setViewportSize")]
  pub async fn set_viewport_size(&self, width: i64, height: i64) -> rquickjs::Result<()> {
    self.inner.set_viewport_size(width, height).await.into_js()
  }

  // ── Screenshots / PDF (return raw bytes; pair with `artifacts.writeBytes`) ─

  /// Capture the page as a PNG (raw bytes — Uint8Array in JS). Pair with
  /// `await artifacts.writeBytes('page.png', bytes)` to save to disk.
  /// Optional `options` accept `{ fullPage?: boolean, format?: 'png'|'jpeg'|'webp', quality?: number }`.
  #[qjs(rename = "screenshot")]
  pub async fn screenshot<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Vec<u8>> {
    let opts = parse_screenshot_options(&ctx, options)?;
    self.inner.screenshot(opts).await.into_js()
  }

  /// Capture a single element as PNG bytes.
  #[qjs(rename = "screenshotElement")]
  pub async fn screenshot_element(&self, selector: String) -> rquickjs::Result<Vec<u8>> {
    self.inner.screenshot_element(&selector).await.into_js()
  }

  /// Render the current page as a PDF (raw bytes). Accepts a Playwright-shape
  /// options object: `{ format?, landscape?, printBackground?, scale?, ... }`.
  /// Pair with `await artifacts.writeBytes('page.pdf', bytes)` to save.
  #[qjs(rename = "pdf")]
  pub async fn pdf<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Vec<u8>> {
    let opts = parse_pdf_options(&ctx, options)?;
    self.inner.pdf(opts).await.into_js()
  }

  // ── Lifecycle ─────────────────────────────────────────────────────────────

  /// Close the page.
  #[qjs(rename = "close")]
  pub async fn close(&self) -> rquickjs::Result<()> {
    self.inner.close().await.into_js()
  }

  /// Whether the page has been closed.
  #[qjs(rename = "isClosed")]
  pub fn is_closed(&self) -> bool {
    self.inner.is_closed()
  }
}

/// Shape of `page.screenshot` options accepted from JS.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JsScreenshotOptions {
  full_page: Option<bool>,
  format: Option<String>,
  quality: Option<i64>,
}

fn parse_screenshot_options<'js>(
  ctx: &rquickjs::Ctx<'js>,
  value: Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<ferridriver::options::ScreenshotOptions> {
  match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => {
      let js: JsScreenshotOptions = serde_from_js(ctx, v)?;
      Ok(ferridriver::options::ScreenshotOptions {
        full_page: js.full_page,
        format: js.format,
        quality: js.quality,
      })
    },
    _ => Ok(ferridriver::options::ScreenshotOptions::default()),
  }
}

/// Subset of Playwright's `PDFOptions` exposed to scripts. Path fields and
/// advanced page-range/margin controls are not wired yet; users who need
/// those can use `page.evaluate` with `window.print` or extend here.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JsPdfOptions {
  format: Option<String>,
  landscape: Option<bool>,
  print_background: Option<bool>,
  scale: Option<f64>,
  display_header_footer: Option<bool>,
  header_template: Option<String>,
  footer_template: Option<String>,
  page_ranges: Option<String>,
  prefer_css_page_size: Option<bool>,
  outline: Option<bool>,
  tagged: Option<bool>,
}

fn parse_pdf_options<'js>(
  ctx: &rquickjs::Ctx<'js>,
  value: Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<ferridriver::options::PdfOptions> {
  match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => {
      let js: JsPdfOptions = serde_from_js(ctx, v)?;
      Ok(ferridriver::options::PdfOptions {
        format: js.format,
        path: None,
        scale: js.scale,
        display_header_footer: js.display_header_footer,
        header_template: js.header_template,
        footer_template: js.footer_template,
        print_background: js.print_background,
        landscape: js.landscape,
        page_ranges: js.page_ranges,
        width: None,
        height: None,
        margin: None,
        prefer_css_page_size: js.prefer_css_page_size,
        outline: js.outline,
        tagged: js.tagged,
      })
    },
    _ => Ok(ferridriver::options::PdfOptions::default()),
  }
}
