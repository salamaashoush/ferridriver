//! `LocatorJs`: JS wrapper around `ferridriver::locator::Locator`.

use ferridriver::locator::Locator;
use ferridriver::options::{FilterOptions, LocatorLike};
use rquickjs::JsLifetime;
use rquickjs::class::Trace;
use rquickjs::function::Opt;

use crate::bindings::convert::FerriResultExt;

/// Shape of filter options read out of a JS object via prototype-aware
/// property lookup. `has`/`hasNot` may be either a selector string or a
/// `LocatorJs` class instance — we accept both because Playwright's own
/// JS API does (`has: Locator` officially, but users commonly pass
/// plain `{ selector: '...' }` shapes in tests).
pub(crate) struct ParsedLocatorOptions {
  pub has_text: Option<String>,
  pub has_not_text: Option<String>,
  pub has: Option<LocatorLike>,
  pub has_not: Option<LocatorLike>,
  pub visible: Option<bool>,
}

/// Pull a string value from a JS object property, ignoring missing/null.
fn get_string<'js>(obj: &rquickjs::Object<'js>, key: &str) -> rquickjs::Result<Option<String>> {
  let v: rquickjs::Value<'js> = obj.get(key)?;
  if v.is_undefined() || v.is_null() {
    return Ok(None);
  }
  match v.as_string() {
    Some(s) => Ok(Some(s.to_string()?)),
    None => Err(rquickjs::Error::new_from_js_message(
      "filter options",
      "field",
      format!("{key}: expected string"),
    )),
  }
}

/// Pull a `LocatorLike` from a JS object property. Accepts either a
/// `LocatorJs` class instance (we read its `inner.selector()` directly
/// and wrap as [`LocatorLike::Locator`] for same-page checks) or any
/// object exposing a string `.selector` property.
fn get_locator_like<'js>(
  ctx: &rquickjs::Ctx<'js>,
  obj: &rquickjs::Object<'js>,
  key: &str,
) -> rquickjs::Result<Option<LocatorLike>> {
  let v: rquickjs::Value<'js> = obj.get(key)?;
  if v.is_undefined() || v.is_null() {
    return Ok(None);
  }
  // Preferred path: a real `LocatorJs` class instance — gives us the
  // full `ferridriver::Locator` so `FilterOptions::has` can enforce
  // same-page equality in the Rust core.
  if let Ok(class) = rquickjs::Class::<LocatorJs>::from_value(&v) {
    let inner = class.borrow();
    return Ok(Some(LocatorLike::Locator(inner.inner.clone())));
  }
  // Fallback: a plain `{ selector: '...' }` object — works but skips
  // the same-page check (no `Page` reference available).
  let _ = ctx;
  if let Some(obj) = v.as_object() {
    if let Some(sel) = get_string(obj, "selector")? {
      return Ok(Some(LocatorLike::Selector(sel)));
    }
  }
  Err(rquickjs::Error::new_from_js_message(
    "filter options",
    "field",
    format!("{key}: expected Locator instance or {{ selector: string }}"),
  ))
}

fn get_bool<'js>(obj: &rquickjs::Object<'js>, key: &str) -> rquickjs::Result<Option<bool>> {
  let v: rquickjs::Value<'js> = obj.get(key)?;
  if v.is_undefined() || v.is_null() {
    return Ok(None);
  }
  v.as_bool()
    .map(Some)
    .ok_or_else(|| rquickjs::Error::new_from_js_message("filter options", "field", format!("{key}: expected boolean")))
}

pub(crate) fn parse_locator_options_public<'js>(
  ctx: &rquickjs::Ctx<'js>,
  value: Opt<rquickjs::Value<'js>>,
  allow_visible: bool,
) -> rquickjs::Result<ParsedLocatorOptions> {
  let Some(val) = value.0 else {
    return Ok(ParsedLocatorOptions {
      has_text: None,
      has_not_text: None,
      has: None,
      has_not: None,
      visible: None,
    });
  };
  if val.is_undefined() || val.is_null() {
    return Ok(ParsedLocatorOptions {
      has_text: None,
      has_not_text: None,
      has: None,
      has_not: None,
      visible: None,
    });
  }
  let obj = val
    .as_object()
    .ok_or_else(|| rquickjs::Error::new_from_js_message("locator options", "", "expected an options object"))?;
  Ok(ParsedLocatorOptions {
    has_text: get_string(obj, "hasText")?,
    has_not_text: get_string(obj, "hasNotText")?,
    has: get_locator_like(ctx, obj, "has")?,
    has_not: get_locator_like(ctx, obj, "hasNot")?,
    visible: if allow_visible { get_bool(obj, "visible")? } else { None },
  })
}

/// Whether `opts` has no fields set — used by bindings to skip the
/// redundant `Some(default)` case before forwarding to Rust core's
/// `locator(sel, Option<FilterOptions>)`.
pub(crate) fn is_empty_filter(opts: &FilterOptions) -> bool {
  opts.has_text.is_none()
    && opts.has_not_text.is_none()
    && opts.has.is_none()
    && opts.has_not.is_none()
    && opts.visible.is_none()
}

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Locator")]
pub struct LocatorJs {
  #[qjs(skip_trace)]
  inner: Locator,
}

impl LocatorJs {
  #[must_use]
  pub fn new(inner: Locator) -> Self {
    Self { inner }
  }
}

#[rquickjs::methods]
impl LocatorJs {
  // ── Chain/refine (return new Locator) ─────────────────────────────────────

  /// Narrow this locator's scope.
  ///
  /// Full Playwright signature:
  /// `locator(selectorOrLocator: string | Locator, options?: { has?, hasNot?, hasText?, hasNotText? }): Locator`.
  /// The `visible` flag is the one `LocatorOptions` field NOT accepted
  /// here — Playwright restricts it to `filter()` and the `Locator`
  /// constructor (see
  /// `/tmp/playwright/packages/playwright-core/src/client/locator.ts:164`).
  #[qjs(rename = "locator")]
  pub fn locator<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    selector_or_locator: rquickjs::Value<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<LocatorJs> {
    // Lower the JS argument to a `LocatorLike`: real `LocatorJs` class →
    // `LocatorLike::Locator` (enables same-page check); string or plain
    // `{ selector }` object → `LocatorLike::Selector`.
    let like: ferridriver::options::LocatorLike = if let Some(s) = selector_or_locator.as_string() {
      ferridriver::options::LocatorLike::Selector(s.to_string()?)
    } else if let Ok(class) = rquickjs::Class::<LocatorJs>::from_value(&selector_or_locator) {
      ferridriver::options::LocatorLike::Locator(class.borrow().inner.clone())
    } else if let Some(obj) = selector_or_locator.as_object() {
      match get_string(obj, "selector")? {
        Some(sel) => ferridriver::options::LocatorLike::Selector(sel),
        None => {
          return Err(rquickjs::Error::new_from_js_message(
            "Locator",
            "locator",
            "expected a selector string or Locator instance",
          ));
        },
      }
    } else {
      return Err(rquickjs::Error::new_from_js_message(
        "Locator",
        "locator",
        "expected a selector string or Locator instance",
      ));
    };

    // Rust core `Locator::locator(selOrLoc, options?)` handles the
    // `internal:chain` encoding, cross-frame sentinel, and option
    // application in one infallible call — script binding is a thin
    // delegator.
    let opts = parse_locator_options_public(&ctx, options, false)?;
    let filter_opts = ferridriver::options::FilterOptions {
      has_text: opts.has_text,
      has_not_text: opts.has_not_text,
      has: opts.has,
      has_not: opts.has_not,
      visible: opts.visible,
    };
    let filter = if is_empty_filter(&filter_opts) {
      None
    } else {
      Some(filter_opts)
    };
    Ok(LocatorJs::new(self.inner.locator(like, filter)))
  }

  /// Playwright: `locator.filter(options?: LocatorOptions): Locator`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:204`).
  /// Thin delegator to Rust core's `Locator::filter`.
  #[qjs(rename = "filter")]
  pub fn filter<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<LocatorJs> {
    let parsed = parse_locator_options_public(&ctx, options, true)?;
    let opts = FilterOptions {
      has_text: parsed.has_text,
      has_not_text: parsed.has_not_text,
      has: parsed.has,
      has_not: parsed.has_not,
      visible: parsed.visible,
    };
    Ok(LocatorJs::new(self.inner.filter(&opts)))
  }

  #[qjs(rename = "first")]
  pub fn first(&self) -> LocatorJs {
    LocatorJs::new(self.inner.first())
  }

  #[qjs(rename = "last")]
  pub fn last(&self) -> LocatorJs {
    LocatorJs::new(self.inner.last())
  }

  #[qjs(rename = "nth")]
  pub fn nth(&self, index: i32) -> LocatorJs {
    LocatorJs::new(self.inner.nth(index))
  }

  // ── Interaction ───────────────────────────────────────────────────────────

  #[qjs(rename = "click")]
  pub async fn click<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_click_options(&ctx, options)?;
    self.inner.click(opts).await.into_js()
  }

  #[qjs(rename = "dblclick")]
  pub async fn dblclick<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_dblclick_options(&ctx, options)?;
    self.inner.dblclick(opts).await.into_js()
  }

  #[qjs(rename = "fill")]
  pub async fn fill<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    value: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_fill_options(&ctx, options)?;
    self.inner.fill(&value, opts).await.into_js()
  }

  #[qjs(rename = "clear")]
  pub async fn clear(&self) -> rquickjs::Result<()> {
    self.inner.clear().await.into_js()
  }

  #[qjs(rename = "type")]
  pub async fn type_<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    text: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_type_options(&ctx, options)?;
    self.inner.r#type(&text, opts).await.into_js()
  }

  #[qjs(rename = "pressSequentially")]
  pub async fn press_sequentially<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    text: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_type_options(&ctx, options)?;
    self.inner.press_sequentially(&text, opts).await.into_js()
  }

  #[qjs(rename = "press")]
  pub async fn press<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    key: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_press_options(&ctx, options)?;
    self.inner.press(&key, opts).await.into_js()
  }

  #[qjs(rename = "hover")]
  pub async fn hover<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_hover_options(&ctx, options)?;
    self.inner.hover(opts).await.into_js()
  }

  #[qjs(rename = "tap")]
  pub async fn tap<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_tap_options(&ctx, options)?;
    self.inner.tap(opts).await.into_js()
  }

  #[qjs(rename = "focus")]
  pub async fn focus(&self) -> rquickjs::Result<()> {
    self.inner.focus().await.into_js()
  }

  #[qjs(rename = "blur")]
  pub async fn blur(&self) -> rquickjs::Result<()> {
    self.inner.blur().await.into_js()
  }

  #[qjs(rename = "check")]
  pub async fn check<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_check_options(&ctx, options)?;
    self.inner.check(opts).await.into_js()
  }

  #[qjs(rename = "uncheck")]
  pub async fn uncheck<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_check_options(&ctx, options)?;
    self.inner.uncheck(opts).await.into_js()
  }

  #[qjs(rename = "setChecked")]
  pub async fn set_checked<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    checked: bool,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::convert::parse_check_options(&ctx, options)?;
    self.inner.set_checked(checked, opts).await.into_js()
  }

  #[qjs(rename = "selectOption")]
  pub async fn select_option<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    values: rquickjs::Value<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Vec<String>> {
    let values = crate::bindings::convert::parse_select_option_values(&ctx, values)?;
    let opts = crate::bindings::convert::parse_select_option_options(&ctx, options)?;
    self.inner.select_option(values, opts).await.into_js()
  }

  /// Attach files to a `<input type=file>` this locator matches.
  /// Accepts Playwright's full
  /// `string | string[] | FilePayload | FilePayload[]` union.
  #[qjs(rename = "setInputFiles")]
  pub async fn set_input_files<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    files: rquickjs::Value<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let files = crate::bindings::convert::parse_input_files(&ctx, files)?;
    let opts = crate::bindings::convert::parse_set_input_files_options(&ctx, options)?;
    self.inner.set_input_files(files, opts).await.into_js()
  }

  #[qjs(rename = "scrollIntoViewIfNeeded")]
  pub async fn scroll_into_view_if_needed(&self) -> rquickjs::Result<()> {
    self.inner.scroll_into_view_if_needed().await.into_js()
  }

  /// Dispatch a DOM event on the element. Mirrors Playwright's
  /// `locator.dispatchEvent(type, eventInit?, options?)`.
  #[qjs(rename = "dispatchEvent")]
  pub async fn dispatch_event<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
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
    self.inner.dispatch_event(&event_type, init_json, opts).await.into_js()
  }

  // ── Info ──────────────────────────────────────────────────────────────────

  #[qjs(rename = "count")]
  pub async fn count(&self) -> rquickjs::Result<i32> {
    self
      .inner
      .count()
      .await
      .into_js()
      .map(|c| i32::try_from(c).unwrap_or(i32::MAX))
  }

  #[qjs(rename = "textContent")]
  pub async fn text_content(&self) -> rquickjs::Result<Option<String>> {
    self.inner.text_content().await.into_js()
  }

  #[qjs(rename = "innerText")]
  pub async fn inner_text(&self) -> rquickjs::Result<String> {
    self.inner.inner_text().await.into_js()
  }

  #[qjs(rename = "innerHTML")]
  pub async fn inner_html(&self) -> rquickjs::Result<String> {
    self.inner.inner_html().await.into_js()
  }

  #[qjs(rename = "inputValue")]
  pub async fn input_value(&self) -> rquickjs::Result<String> {
    self.inner.input_value().await.into_js()
  }

  #[qjs(rename = "getAttribute")]
  pub async fn get_attribute(&self, name: String) -> rquickjs::Result<Option<String>> {
    self.inner.get_attribute(&name).await.into_js()
  }

  #[qjs(rename = "isVisible")]
  pub async fn is_visible(&self) -> rquickjs::Result<bool> {
    self.inner.is_visible().await.into_js()
  }

  #[qjs(rename = "isHidden")]
  pub async fn is_hidden(&self) -> rquickjs::Result<bool> {
    self.inner.is_hidden().await.into_js()
  }

  #[qjs(rename = "isEnabled")]
  pub async fn is_enabled(&self) -> rquickjs::Result<bool> {
    self.inner.is_enabled().await.into_js()
  }

  #[qjs(rename = "isDisabled")]
  pub async fn is_disabled(&self) -> rquickjs::Result<bool> {
    self.inner.is_disabled().await.into_js()
  }

  #[qjs(rename = "isChecked")]
  pub async fn is_checked(&self) -> rquickjs::Result<bool> {
    self.inner.is_checked().await.into_js()
  }

  #[qjs(rename = "isEditable")]
  pub async fn is_editable(&self) -> rquickjs::Result<bool> {
    self.inner.is_editable().await.into_js()
  }

  #[qjs(rename = "isAttached")]
  pub async fn is_attached(&self) -> rquickjs::Result<bool> {
    self.inner.is_attached().await.into_js()
  }

  // ── Drag ──────────────────────────────────────────────────────────────────

  /// Drag this element to `target`. Mirrors Playwright's
  /// `locator.dragTo(target, options?)` per
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:13293`.
  ///
  /// Accepts `{ force?, noWaitAfter?, sourcePosition?, targetPosition?,
  /// steps?, timeout?, trial? }`. `strict` is omitted here (present on
  /// Playwright's `page.dragAndDrop` options but not `locator.dragTo`,
  /// because the locator already carries its own strict flag).
  #[qjs(rename = "dragTo")]
  pub async fn drag_to<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    target: rquickjs::Class<'js, LocatorJs>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let target_inner = target.borrow().inner.clone();
    let opts = crate::bindings::page::parse_drag_options(&ctx, options)?;
    self.inner.drag_to(&target_inner, opts).await.into_js()
  }

  // ── All variants ──────────────────────────────────────────────────────────

  #[qjs(rename = "allTextContents")]
  pub async fn all_text_contents(&self) -> rquickjs::Result<Vec<String>> {
    self.inner.all_text_contents().await.into_js()
  }

  #[qjs(rename = "allInnerTexts")]
  pub async fn all_inner_texts(&self) -> rquickjs::Result<Vec<String>> {
    self.inner.all_inner_texts().await.into_js()
  }

  // ── Evaluation ────────────────────────────────────────────────────────────

  /// Evaluate `expression` against this locator's first element. Returns the
  /// JSON-encoded result as a string (or `null`).
  ///
  /// Parity gap: core takes a string, not a function. See
  /// `PLAYWRIGHT_COMPAT.md` "Gaps surfaced by scripting bindings" item 8.
  #[qjs(rename = "evaluate")]
  pub async fn evaluate(&self, expression: String) -> rquickjs::Result<Option<String>> {
    let value = self.inner.evaluate(&expression).await.into_js()?;
    Ok(value.map(|v| serde_json::to_string(&v).unwrap_or_default()))
  }
}
