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

use crate::bindings::convert::{
  FerriResultExt, init_script_from_js, json_value_to_quickjs, quickjs_arg_to_serialized, serde_from_js,
  serialized_value_to_quickjs,
};
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

#[derive(serde::Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
struct JsGotoOptions {
  wait_until: Option<String>,
  timeout: Option<u64>,
  referer: Option<String>,
}

fn parse_goto_options<'js>(
  ctx: &rquickjs::Ctx<'js>,
  value: Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::GotoOptions>> {
  match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => {
      let js: JsGotoOptions = serde_from_js(ctx, v)?;
      Ok(Some(ferridriver::options::GotoOptions {
        wait_until: js.wait_until,
        timeout: js.timeout,
        referer: js.referer,
      }))
    },
    _ => Ok(None),
  }
}

#[derive(serde::Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
struct JsPageCloseOptions {
  run_before_unload: Option<bool>,
  reason: Option<String>,
}

/// Shape of `page.dragAndDrop` / `locator.dragTo` options. Mirrors
/// Playwright's `FrameDragAndDropOptions & TimeoutOptions` per
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:2486`.
#[derive(serde::Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct JsDragAndDropOptions {
  force: Option<bool>,
  no_wait_after: Option<bool>,
  source_position: Option<JsPoint>,
  target_position: Option<JsPoint>,
  steps: Option<u32>,
  strict: Option<bool>,
  timeout: Option<u64>,
  trial: Option<bool>,
}

#[derive(serde::Deserialize, Debug, Default, Clone, Copy)]
pub(crate) struct JsPoint {
  x: f64,
  y: f64,
}

impl From<JsPoint> for ferridriver::options::Point {
  fn from(p: JsPoint) -> Self {
    Self { x: p.x, y: p.y }
  }
}

/// Parse the Playwright-shaped `emulateMedia` options bag from a
/// `rquickjs::Value`. Unlike `serde_from_js`, this walks the JS object
/// manually so we can distinguish three states for every field:
///
/// * absent → [`MediaOverride::Unchanged`]
/// * explicit `null` → [`MediaOverride::Disabled`]
/// * string value → [`MediaOverride::Set`]
///
/// serde-based deserialization conflates `undefined` and `null` into a
/// single `Option::None`, which breaks the Playwright null-disables-the-
/// override contract. See `/tmp/playwright/packages/playwright-core/types/types.d.ts:2580`
/// for the `T | null | undefined` shape we're mirroring.
fn parse_emulate_media_field<'js>(
  obj: &rquickjs::Object<'js>,
  key: &str,
) -> rquickjs::Result<ferridriver::options::MediaOverride> {
  use ferridriver::options::MediaOverride;
  if !obj.contains_key(key)? {
    return Ok(MediaOverride::Unchanged);
  }
  let val: rquickjs::Value<'js> = obj.get(key)?;
  if val.is_undefined() {
    Ok(MediaOverride::Unchanged)
  } else if val.is_null() {
    Ok(MediaOverride::Disabled)
  } else if let Some(s) = val.as_string() {
    Ok(MediaOverride::Set(s.to_string()?))
  } else {
    Err(rquickjs::Error::new_from_js_message(
      "emulateMedia options",
      "field",
      format!("{key}: expected null, undefined, or string"),
    ))
  }
}

pub(crate) fn parse_emulate_media_options<'js>(
  _ctx: &rquickjs::Ctx<'js>,
  value: Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<ferridriver::options::EmulateMediaOptions> {
  let Some(v) = value.0.filter(|v| !v.is_undefined() && !v.is_null()) else {
    return Ok(ferridriver::options::EmulateMediaOptions::default());
  };
  let Some(obj) = v.as_object() else {
    return Ok(ferridriver::options::EmulateMediaOptions::default());
  };
  Ok(ferridriver::options::EmulateMediaOptions {
    media: parse_emulate_media_field(obj, "media")?,
    color_scheme: parse_emulate_media_field(obj, "colorScheme")?,
    reduced_motion: parse_emulate_media_field(obj, "reducedMotion")?,
    forced_colors: parse_emulate_media_field(obj, "forcedColors")?,
    contrast: parse_emulate_media_field(obj, "contrast")?,
  })
}

pub(crate) fn parse_drag_options<'js>(
  ctx: &rquickjs::Ctx<'js>,
  value: Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::DragAndDropOptions>> {
  match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => {
      let js: JsDragAndDropOptions = serde_from_js(ctx, v)?;
      Ok(Some(ferridriver::options::DragAndDropOptions {
        force: js.force,
        no_wait_after: js.no_wait_after,
        source_position: js.source_position.map(Into::into),
        target_position: js.target_position.map(Into::into),
        steps: js.steps,
        strict: js.strict,
        timeout: js.timeout,
        trial: js.trial,
      }))
    },
    _ => Ok(None),
  }
}

fn parse_page_close_options<'js>(
  ctx: &rquickjs::Ctx<'js>,
  value: Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<Option<ferridriver::options::PageCloseOptions>> {
  match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => {
      let js: JsPageCloseOptions = serde_from_js(ctx, v)?;
      Ok(Some(ferridriver::options::PageCloseOptions {
        run_before_unload: js.run_before_unload,
        reason: js.reason,
      }))
    },
    _ => Ok(None),
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

  /// Navigate to `url`. Accepts `{ waitUntil?, timeout?, referer? }` to
  /// mirror Playwright's `page.goto(url, options?)`.
  #[qjs(rename = "goto")]
  pub async fn goto<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    url: String,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = parse_goto_options(&ctx, options)?;
    self.inner.goto(&url, opts).await.into_js()
  }

  /// Reload the current page. Accepts the same option bag as `goto`.
  #[qjs(rename = "reload")]
  pub async fn reload<'js>(&self, ctx: rquickjs::Ctx<'js>, options: Opt<rquickjs::Value<'js>>) -> rquickjs::Result<()> {
    let opts = parse_goto_options(&ctx, options)?;
    self.inner.reload(opts).await.into_js()
  }

  /// Navigate back in history. Accepts the same option bag as `goto`.
  #[qjs(rename = "goBack")]
  pub async fn go_back<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = parse_goto_options(&ctx, options)?;
    self.inner.go_back(opts).await.into_js()
  }

  /// Navigate forward in history. Accepts the same option bag as `goto`.
  #[qjs(rename = "goForward")]
  pub async fn go_forward<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = parse_goto_options(&ctx, options)?;
    self.inner.go_forward(opts).await.into_js()
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

  /// Register a JS snippet to run on every new document before any page
  /// script executes. Mirrors Playwright's
  /// `page.addInitScript(script, arg)` — see
  /// `/tmp/playwright/packages/playwright-core/src/client/page.ts:520`.
  /// Accepts `Function | string | { path?, content? }` + optional `arg`
  /// exactly like the NAPI binding; all lowering runs in Rust core via
  /// [`ferridriver::options::evaluation_script`].
  #[qjs(rename = "addInitScript")]
  pub async fn add_init_script<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    script: rquickjs::Value<'js>,
    arg: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<String> {
    let (init, arg_json) = init_script_from_js(&ctx, script, arg.0)?;
    self.inner.add_init_script(init, arg_json).await.into_js()
  }

  /// Remove a previously-registered init script by identifier.
  #[qjs(rename = "removeInitScript")]
  pub async fn remove_init_script(&self, identifier: String) -> rquickjs::Result<()> {
    self.inner.remove_init_script(&identifier).await.into_js()
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

  /// Playwright: `page.querySelector(selector): Promise<ElementHandle | null>`.
  /// Mints a lifecycle [`crate::bindings::element_handle::ElementHandleJs`]
  /// pinned to the first element matching `selector`, or `null` when no
  /// element matches. Callers `dispose()` the handle when done to
  /// release the backend remote.
  #[qjs(rename = "querySelector")]
  pub async fn query_selector(
    &self,
    selector: String,
  ) -> rquickjs::Result<Option<crate::bindings::element_handle::ElementHandleJs>> {
    let inner = self.inner.query_selector(&selector).await.into_js()?;
    Ok(inner.map(crate::bindings::element_handle::ElementHandleJs::new))
  }

  /// Playwright `$` shortcut for [`Self::query_selector`].
  #[qjs(rename = "$")]
  pub async fn dollar(
    &self,
    selector: String,
  ) -> rquickjs::Result<Option<crate::bindings::element_handle::ElementHandleJs>> {
    self.query_selector(selector).await
  }

  /// Playwright: `page.querySelectorAll(selector): Promise<ElementHandle[]>`.
  #[qjs(rename = "querySelectorAll")]
  pub async fn query_selector_all(
    &self,
    selector: String,
  ) -> rquickjs::Result<Vec<crate::bindings::element_handle::ElementHandleJs>> {
    let inner_handles = self.inner.query_selector_all(&selector).await.into_js()?;
    Ok(
      inner_handles
        .into_iter()
        .map(crate::bindings::element_handle::ElementHandleJs::new)
        .collect(),
    )
  }

  /// Playwright `$$` shortcut for [`Self::query_selector_all`].
  #[qjs(rename = "$$")]
  pub async fn dollar_dollar(
    &self,
    selector: String,
  ) -> rquickjs::Result<Vec<crate::bindings::element_handle::ElementHandleJs>> {
    self.query_selector_all(selector).await
  }

  /// Playwright: `page.evaluate(fn, arg?)` — function-call variant.
  /// Serialises `arg` through the isomorphic wire protocol and returns
  /// the function's result as a JSON-like value. Rich types that have
  /// no native JSON form surface as `null` — callers needing lossless
  /// round-trip use [`Self::evaluateWithArgWire`].
  #[qjs(rename = "evaluateWithArg")]
  pub async fn evaluate_with_arg<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    fn_source: String,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
    let result = self
      .inner
      .evaluate_with_arg(&fn_source, serialized, Some(true))
      .await
      .into_js()?;
    serialized_value_to_quickjs(&ctx, &result)
  }

  /// Raw isomorphic wire shape variant of [`Self::evaluateWithArg`].
  #[qjs(rename = "evaluateWithArgWire")]
  pub async fn evaluate_with_arg_wire<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    fn_source: String,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
    let result = self
      .inner
      .evaluate_with_arg(&fn_source, serialized, Some(true))
      .await
      .into_js()?;
    let wire = serde_json::to_value(&result)
      .map_err(|e| rquickjs::Error::new_from_js_message("evaluateWithArgWire", "", &e.to_string()))?;
    json_value_to_quickjs(&ctx, &wire)
  }

  /// Playwright: `page.evaluateHandle(fn, arg?)`.
  #[qjs(rename = "evaluateHandleWithArg")]
  pub async fn evaluate_handle_with_arg<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    fn_source: String,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<crate::bindings::js_handle::JSHandleJs> {
    let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
    let handle = self
      .inner
      .evaluate_handle_with_arg(&fn_source, serialized, Some(true))
      .await
      .into_js()?;
    Ok(crate::bindings::js_handle::JSHandleJs::new(handle))
  }

  /// Playwright: `page.locator(selector, options?: LocatorOptions): Locator`.
  /// Thin delegator to Rust core's `Page::locator`.
  #[qjs(rename = "locator")]
  pub fn locator<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<LocatorJs> {
    let parsed = crate::bindings::locator::parse_locator_options_public(&ctx, options, true)?;
    let opts = ferridriver::options::FilterOptions {
      has_text: parsed.has_text,
      has_not_text: parsed.has_not_text,
      has: parsed.has,
      has_not: parsed.has_not,
      visible: parsed.visible,
    };
    let filter = if crate::bindings::locator::is_empty_filter(&opts) {
      None
    } else {
      Some(opts)
    };
    Ok(LocatorJs::new(self.inner.locator(&selector, filter)))
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

  /// Click the first element matching `selector`. Accepts Playwright's
  /// full `PageClickOptions` bag.
  #[qjs(rename = "click")]
  pub async fn click<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_click_options(&ctx, options)?;
    self.inner.click(&selector, opts).await.into_js()
  }

  /// Double-click the first element matching `selector`. Accepts
  /// Playwright's full `PageDblClickOptions` bag.
  #[qjs(rename = "dblclick")]
  pub async fn dblclick<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_dblclick_options(&ctx, options)?;
    self.inner.dblclick(&selector, opts).await.into_js()
  }

  /// Fill `value` into the input matching `selector`. Accepts
  /// Playwright's full `PageFillOptions` bag.
  #[qjs(rename = "fill")]
  pub async fn fill<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    value: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_fill_options(&ctx, options)?;
    self.inner.fill(&selector, &value, opts).await.into_js()
  }

  /// Type `text` into the input matching `selector`. Accepts
  /// Playwright's full `PageTypeOptions` bag.
  ///
  /// Exposed as `type` in JS (matches Playwright) — Rust renames to avoid
  /// the `type` keyword.
  #[qjs(rename = "type")]
  pub async fn type_<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    text: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_type_options(&ctx, options)?;
    self.inner.r#type(&selector, &text, opts).await.into_js()
  }

  /// Press `key` on the element matching `selector`. Accepts Playwright's
  /// full `PagePressOptions` bag.
  #[qjs(rename = "press")]
  pub async fn press<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    key: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_press_options(&ctx, options)?;
    self.inner.press(&selector, &key, opts).await.into_js()
  }

  /// Hover the first element matching `selector`. Accepts Playwright's
  /// full `PageHoverOptions` bag.
  #[qjs(rename = "hover")]
  pub async fn hover<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_hover_options(&ctx, options)?;
    self.inner.hover(&selector, opts).await.into_js()
  }

  /// Dispatch a DOM event on the first element matching `selector`.
  /// Mirrors Playwright's `page.dispatchEvent(selector, type, eventInit?, options?)`.
  #[qjs(rename = "dispatchEvent")]
  pub async fn dispatch_event<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    event_type: String,
    event_init: rquickjs::function::Opt<rquickjs::Value<'js>>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let init_json = match event_init.0 {
      Some(v) if !v.is_undefined() && !v.is_null() => {
        Some(crate::bindings::convert::serde_from_js::<serde_json::Value>(&ctx, v)?)
      },
      _ => None,
    };
    let opts = crate::bindings::convert::parse_dispatch_event_options(&ctx, options)?;
    self
      .inner
      .dispatch_event(&selector, &event_type, init_json, opts)
      .await
      .into_js()
  }

  /// Tap (touch) the first element matching `selector`. Accepts
  /// Playwright's full `PageTapOptions` bag.
  #[qjs(rename = "tap")]
  pub async fn tap<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_tap_options(&ctx, options)?;
    self.inner.tap(&selector, opts).await.into_js()
  }

  /// Check a checkbox matching `selector`. Accepts Playwright's full
  /// `PageCheckOptions` bag.
  #[qjs(rename = "check")]
  pub async fn check<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_check_options(&ctx, options)?;
    self.inner.check(&selector, opts).await.into_js()
  }

  /// Uncheck a checkbox matching `selector`. Accepts Playwright's full
  /// `PageUncheckOptions` bag.
  #[qjs(rename = "uncheck")]
  pub async fn uncheck<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_check_options(&ctx, options)?;
    self.inner.uncheck(&selector, opts).await.into_js()
  }

  /// Set the checked state of a checkbox/radio matching `selector`.
  /// Accepts Playwright's full `PageSetCheckedOptions` bag.
  #[qjs(rename = "setChecked")]
  pub async fn set_checked<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    checked: bool,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_check_options(&ctx, options)?;
    self.inner.set_checked(&selector, checked, opts).await.into_js()
  }

  /// Select options on the `<select>` matching `selector`. Returns the
  /// values of the selected options. Accepts Playwright's full
  /// `string | string[] | { value?, label?, index? } | Array<...>` union.
  #[qjs(rename = "selectOption")]
  pub async fn select_option<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    values: rquickjs::Value<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Vec<String>> {
    let values = crate::bindings::convert::parse_select_option_values(&ctx, values)?;
    let opts = crate::bindings::convert::parse_select_option_options(&ctx, options)?;
    self.inner.select_option(&selector, values, opts).await.into_js()
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

  /// Drag from the source selector to the target selector. Accepts
  /// Playwright's `FrameDragAndDropOptions & TimeoutOptions` bag:
  /// `{ force?, noWaitAfter?, sourcePosition?, targetPosition?, steps?, strict?, timeout?, trial? }`.
  #[qjs(rename = "dragAndDrop")]
  pub async fn drag_and_drop<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    source: String,
    target: String,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = parse_drag_options(&ctx, options)?;
    self.inner.drag_and_drop(&source, &target, opts).await.into_js()
  }

  // ── File input ────────────────────────────────────────────────────────────

  /// Attach files to a `<input type="file">` selector. Accepts
  /// Playwright's full `string | string[] | FilePayload | FilePayload[]`
  /// union plus the `PageSetInputFilesOptions` bag.
  #[qjs(rename = "setInputFiles")]
  pub async fn set_input_files<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    files: rquickjs::Value<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let files = crate::bindings::convert::parse_input_files(&ctx, files)?;
    let opts = crate::bindings::convert::parse_set_input_files_options(&ctx, options)?;
    self.inner.set_input_files(&selector, files, opts).await.into_js()
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

  /// Emulate media features. Accepts Playwright's
  /// `{ media?, colorScheme?, reducedMotion?, forcedColors?, contrast? }`
  /// option bag — each call is a partial update layered on top of the
  /// page's persistent emulated-media state.
  #[qjs(rename = "emulateMedia")]
  pub async fn emulate_media<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = parse_emulate_media_options(&ctx, options)?;
    self.inner.emulate_media(&opts).await.into_js()
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

  /// Close the page. Accepts `{ runBeforeUnload?, reason? }` to mirror
  /// Playwright's `page.close(options?)`.
  #[qjs(rename = "close")]
  pub async fn close<'js>(&self, ctx: rquickjs::Ctx<'js>, options: Opt<rquickjs::Value<'js>>) -> rquickjs::Result<()> {
    let opts = parse_page_close_options(&ctx, options)?;
    self.inner.close(opts).await.into_js()
  }

  /// Set the default timeout for all non-navigation operations
  /// (milliseconds). Mirrors Playwright's `page.setDefaultTimeout(timeout)`.
  #[qjs(rename = "setDefaultTimeout")]
  pub fn set_default_timeout(&self, ms: u64) {
    self.inner.set_default_timeout(ms);
  }

  /// Set the default timeout for navigation-family operations
  /// (`goto`, `reload`, `goBack`, `goForward`, `waitForUrl`). Mirrors
  /// Playwright's `page.setDefaultNavigationTimeout(timeout)`.
  #[qjs(rename = "setDefaultNavigationTimeout")]
  pub fn set_default_navigation_timeout(&self, ms: u64) {
    self.inner.set_default_navigation_timeout(ms);
  }

  /// Whether the page has been closed.
  #[qjs(rename = "isClosed")]
  pub fn is_closed(&self) -> bool {
    self.inner.is_closed()
  }

  // ── Frames (sync, Playwright parity — task 3.8) ─────────────────────
  //
  // Mirrors `/tmp/playwright/packages/playwright-core/src/client/page.ts:258-275`
  // — `mainFrame`, `frames`, `frame(selector)` are all sync and read
  // from the page-owned [`ferridriver::frame_cache::FrameCache`].

  /// Main frame of this page. Playwright: `page.mainFrame(): Frame`.
  /// Always returns a Frame — the cache is seeded inside `Page::new` /
  /// `Page::with_context` before the Page is handed out.
  #[qjs(rename = "mainFrame")]
  pub fn main_frame(&self) -> crate::bindings::frame::FrameJs {
    crate::bindings::frame::FrameJs::new(self.inner.main_frame())
  }

  /// All non-detached frames on the page. Playwright:
  /// `page.frames(): Frame[]`.
  #[qjs(rename = "frames")]
  pub fn frames(&self) -> Vec<crate::bindings::frame::FrameJs> {
    self
      .inner
      .frames()
      .into_iter()
      .map(crate::bindings::frame::FrameJs::new)
      .collect()
  }

  /// Locate a frame by name or URL. Accepts Playwright's union:
  /// `frame(string | { name?: string; url?: string })`.
  ///
  /// Distinct null/undefined handling (like emulateMedia in task 3.24)
  /// is not required here — both absent and explicit-null mean "no
  /// filter on this field", which matches Playwright's optional-field
  /// semantics.
  #[qjs(rename = "frame")]
  pub fn frame<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector: rquickjs::Value<'js>,
  ) -> rquickjs::Result<Option<crate::bindings::frame::FrameJs>> {
    let core_sel = if let Some(s) = selector.as_string() {
      ferridriver::options::FrameSelector::by_name(s.to_string()?)
    } else if let Some(obj) = selector.as_object() {
      let read = |key: &str| -> rquickjs::Result<Option<String>> {
        let v: rquickjs::Value<'_> = obj
          .get(key)
          .unwrap_or_else(|_| rquickjs::Value::new_undefined(ctx.clone()));
        if v.is_undefined() || v.is_null() {
          Ok(None)
        } else if let Some(s) = v.as_string() {
          Ok(Some(s.to_string()?))
        } else {
          Ok(None)
        }
      };
      ferridriver::options::FrameSelector {
        name: read("name")?,
        url: read("url")?,
      }
    } else {
      return Ok(None);
    };

    if core_sel.is_empty() {
      return Ok(None);
    }
    Ok(self.inner.frame(core_sel).map(crate::bindings::frame::FrameJs::new))
  }
}

/// Shape of `page.screenshot` options accepted from JS. Full Playwright
/// `PageScreenshotOptions` surface per
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:23280`.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JsScreenshotOptions {
  animations: Option<String>,
  caret: Option<String>,
  clip: Option<JsClipRect>,
  full_page: Option<bool>,
  #[serde(rename = "type")]
  format: Option<String>,
  /// `mask` accepts selector strings. Full `Locator` instances aren't
  /// deserialisable from JS via serde, so Playwright-style
  /// `mask: [page.locator('.foo')]` should be rewritten at the call
  /// site as `mask: ['.foo']` — documented on the QuickJS binding.
  mask: Option<Vec<String>>,
  mask_color: Option<String>,
  omit_background: Option<bool>,
  path: Option<String>,
  quality: Option<i64>,
  scale: Option<String>,
  style: Option<String>,
  timeout: Option<u64>,
}

#[derive(Debug, Default, Deserialize, Clone, Copy)]
struct JsClipRect {
  x: f64,
  y: f64,
  width: f64,
  height: f64,
}

impl From<JsClipRect> for ferridriver::options::ClipRect {
  fn from(c: JsClipRect) -> Self {
    Self {
      x: c.x,
      y: c.y,
      width: c.width,
      height: c.height,
    }
  }
}

fn parse_screenshot_options<'js>(
  ctx: &rquickjs::Ctx<'js>,
  value: Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<ferridriver::options::ScreenshotOptions> {
  match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => {
      let js: JsScreenshotOptions = serde_from_js(ctx, v)?;
      Ok(ferridriver::options::ScreenshotOptions {
        animations: js.animations,
        caret: js.caret,
        clip: js.clip.map(Into::into),
        full_page: js.full_page,
        format: js.format,
        mask: js.mask.unwrap_or_default(),
        mask_color: js.mask_color,
        omit_background: js.omit_background,
        path: js.path.map(std::path::PathBuf::from),
        quality: js.quality,
        scale: js.scale,
        style: js.style,
        timeout: js.timeout,
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
