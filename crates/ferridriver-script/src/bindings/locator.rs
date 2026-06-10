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

/// Parse `Locator.highlight`'s optional `{ style }` bag. `style` is
/// `string | Record<string, string | number>`. A string becomes
/// [`ferridriver::options::HighlightStyle::Css`]; an object becomes
/// `Object` with each value rendered to CSS text (numbers stringified,
/// strings verbatim; other value types skipped). Parsed synchronously so
/// no `!Send` JS value is held across the async `highlight` await.
fn parse_highlight_style(
  options: Opt<rquickjs::Value<'_>>,
) -> rquickjs::Result<Option<ferridriver::options::HighlightStyle>> {
  let Some(val) = options.0 else {
    return Ok(None);
  };
  let Some(obj) = val.as_object() else {
    return Ok(None);
  };
  let style: rquickjs::Value<'_> = obj.get("style")?;
  if style.is_undefined() || style.is_null() {
    return Ok(None);
  }
  if let Some(s) = style.as_string() {
    return Ok(Some(ferridriver::options::HighlightStyle::Css(s.to_string()?)));
  }
  if let Some(map) = style.as_object() {
    let mut entries = Vec::new();
    for key_res in map.keys::<String>() {
      let key = key_res?;
      let v: rquickjs::Value<'_> = map.get(&key)?;
      let rendered = if let Some(s) = v.as_string() {
        s.to_string()?
      } else if let Some(n) = v.as_number() {
        // Match `cssObjectToString`'s template-literal: integers print
        // without a trailing `.0`.
        if n.fract() == 0.0 && n.is_finite() {
          format!("{}", n as i64)
        } else {
          n.to_string()
        }
      } else {
        continue;
      };
      entries.push((key, rendered));
    }
    return Ok(Some(ferridriver::options::HighlightStyle::Object(entries)));
  }
  Ok(None)
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

  /// Read-only access to the wrapped core `Locator` for cross-binding
  /// consumers (e.g. the `expect()` binding lifting a `LocatorJs` into
  /// an assertion target).
  #[must_use]
  pub fn inner_ref(&self) -> &Locator {
    &self.inner
  }
}

#[rquickjs::methods]
impl LocatorJs {
  /// The resolved selector string for this locator. Mirrors the NAPI
  /// `Locator.selector` getter; used by `{ selector }`-style locator
  /// round-tripping and by `normalize()` callers reading the canonical form.
  #[qjs(get, rename = "selector")]
  pub fn selector(&self) -> String {
    self.inner.selector().to_string()
  }

  /// Whether this locator is in strict mode (mirrors NAPI `isStrict`).
  #[qjs(get, rename = "isStrict")]
  pub fn is_strict(&self) -> bool {
    self.inner.is_strict()
  }

  /// Returns a copy of this locator with strict-mode toggled.
  #[qjs(rename = "setStrict")]
  pub fn set_strict(&self, strict: bool) -> LocatorJs {
    LocatorJs::new(self.inner.strict(strict))
  }

  /// Playwright: `locator.selectText(options?)`. Selects the element's text.
  #[qjs(rename = "selectText")]
  pub async fn select_text(&self) -> rquickjs::Result<()> {
    self.inner.select_text().await.into_js()
  }

  /// Playwright: `locator.click({ button: 'right' })` shorthand.
  #[qjs(rename = "rightClick")]
  pub async fn right_click(&self) -> rquickjs::Result<()> {
    self.inner.right_click().await.into_js()
  }

  /// Playwright: `locator.boundingBox()`. Returns `{x, y, width, height}` or `null`.
  #[qjs(rename = "boundingBox")]
  pub async fn bounding_box<'js>(&self, ctx: rquickjs::Ctx<'js>) -> rquickjs::Result<rquickjs::Value<'js>> {
    match self.inner.bounding_box().await.into_js()? {
      None => Ok(rquickjs::Value::new_null(ctx)),
      Some(b) => {
        let obj = rquickjs::Object::new(ctx.clone())?;
        obj.set("x", b.x)?;
        obj.set("y", b.y)?;
        obj.set("width", b.width)?;
        obj.set("height", b.height)?;
        Ok(obj.into_value())
      },
    }
  }

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

  /// Playwright: `locator.and(locator: Locator): Locator`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts` —
  /// matches elements satisfying BOTH this and `other` on the same
  /// element). Thin delegator to core's `Locator::and`.
  #[qjs(rename = "and")]
  pub fn and<'js>(&self, ctx: rquickjs::Ctx<'js>, other: rquickjs::Value<'js>) -> rquickjs::Result<LocatorJs> {
    let _ = ctx;
    let class = rquickjs::Class::<LocatorJs>::from_value(&other)
      .map_err(|_| rquickjs::Error::new_from_js_message("Locator", "and", "expected a Locator instance"))?;
    Ok(LocatorJs::new(self.inner.and(&class.borrow().inner)))
  }

  /// Playwright: `locator.or(locator: Locator): Locator` — matches
  /// elements from EITHER selector. Thin delegator to `Locator::or`.
  #[qjs(rename = "or")]
  pub fn or<'js>(&self, ctx: rquickjs::Ctx<'js>, other: rquickjs::Value<'js>) -> rquickjs::Result<LocatorJs> {
    let _ = ctx;
    let class = rquickjs::Class::<LocatorJs>::from_value(&other)
      .map_err(|_| rquickjs::Error::new_from_js_message("Locator", "or", "expected a Locator instance"))?;
    Ok(LocatorJs::new(self.inner.or(&class.borrow().inner)))
  }

  /// Playwright: `locator.elementHandle(): Promise<ElementHandle>`.
  /// Resolves and returns a pinned ElementHandle.
  #[qjs(rename = "elementHandle")]
  pub async fn element_handle(&self) -> rquickjs::Result<crate::bindings::element_handle::ElementHandleJs> {
    let inner = self.inner.element_handle().await.into_js()?;
    Ok(crate::bindings::element_handle::ElementHandleJs::new(inner))
  }

  /// Playwright: `locator.elementHandles(): Promise<ElementHandle[]>`.
  #[qjs(rename = "elementHandles")]
  pub async fn element_handles(&self) -> rquickjs::Result<Vec<crate::bindings::element_handle::ElementHandleJs>> {
    let inner = self.inner.element_handles().await.into_js()?;
    Ok(
      inner
        .into_iter()
        .map(crate::bindings::element_handle::ElementHandleJs::new)
        .collect(),
    )
  }

  #[qjs(rename = "getByRole")]
  pub fn get_by_role(
    &self,
    role: String,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let opts = crate::bindings::page::parse_role_options(options)?;
    Ok(LocatorJs::new(self.inner.get_by_role(&role, &opts)))
  }

  #[qjs(rename = "getByText")]
  pub fn get_by_text(
    &self,
    text: rquickjs::Value<'_>,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = crate::bindings::page::string_or_regex_from_js(text)?;
    let opts = crate::bindings::page::parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_text(&t, &opts)))
  }

  #[qjs(rename = "getByLabel")]
  pub fn get_by_label(
    &self,
    text: rquickjs::Value<'_>,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = crate::bindings::page::string_or_regex_from_js(text)?;
    let opts = crate::bindings::page::parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_label(&t, &opts)))
  }

  #[qjs(rename = "getByPlaceholder")]
  pub fn get_by_placeholder(
    &self,
    text: rquickjs::Value<'_>,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = crate::bindings::page::string_or_regex_from_js(text)?;
    let opts = crate::bindings::page::parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_placeholder(&t, &opts)))
  }

  #[qjs(rename = "getByAltText")]
  pub fn get_by_alt_text(
    &self,
    text: rquickjs::Value<'_>,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = crate::bindings::page::string_or_regex_from_js(text)?;
    let opts = crate::bindings::page::parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_alt_text(&t, &opts)))
  }

  #[qjs(rename = "getByTitle")]
  pub fn get_by_title(
    &self,
    text: rquickjs::Value<'_>,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = crate::bindings::page::string_or_regex_from_js(text)?;
    let opts = crate::bindings::page::parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_title(&t, &opts)))
  }

  #[qjs(rename = "getByTestId")]
  pub fn get_by_test_id(&self, test_id: rquickjs::Value<'_>) -> rquickjs::Result<LocatorJs> {
    let t = crate::bindings::page::string_or_regex_from_js(test_id)?;
    Ok(LocatorJs::new(self.inner.get_by_test_id(&t)))
  }

  /// Playwright: `locator.contentFrame(): FrameLocator`.
  #[qjs(rename = "contentFrame")]
  pub fn content_frame(&self) -> crate::bindings::frame_locator::FrameLocatorJs {
    crate::bindings::frame_locator::FrameLocatorJs::new(self.inner.content_frame())
  }

  /// Playwright: `locator.frameLocator(selector): FrameLocator`.
  #[qjs(rename = "frameLocator")]
  pub fn frame_locator(&self, selector: String) -> crate::bindings::frame_locator::FrameLocatorJs {
    crate::bindings::frame_locator::FrameLocatorJs::new(self.inner.frame_locator(&selector))
  }

  /// Playwright: `locator.page(): Page`. Carries the session's
  /// `AsyncContext` (via userdata) so `page.route` /
  /// `page.exposeFunction` work on the returned handle.
  #[qjs(rename = "page")]
  pub fn page(&self, ctx: rquickjs::Ctx<'_>) -> crate::bindings::page::PageJs {
    crate::bindings::page::pagejs_for_ctx(&ctx, self.inner.page().clone())
  }

  /// Playwright: `locator.describe(description: string): Locator`.
  #[qjs(rename = "describe")]
  pub fn describe(&self, description: String) -> LocatorJs {
    LocatorJs::new(self.inner.describe(&description))
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

  /// Playwright: `highlight(options?: { style?: string | Record<string,
  /// string | number> }): Promise<Disposable>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:158`).
  /// Shows the element-highlight overlay; returns a `Disposable` whose
  /// `dispose()` hides it. The optional `style` is parsed synchronously
  /// (the JS scope is `!Send`) into [`ferridriver::options::HighlightStyle`]
  /// before the async body forwards to core.
  #[qjs(rename = "highlight")]
  pub async fn highlight(
    &self,
    options: Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<crate::bindings::disposable::DisposableJs> {
    let style = parse_highlight_style(options)?;
    let disposable = self.inner.highlight(style).await.into_js()?;
    Ok(crate::bindings::disposable::DisposableJs::new(disposable))
  }

  /// Playwright: `hideHighlight(): Promise<void>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:164`).
  #[qjs(rename = "hideHighlight")]
  pub async fn hide_highlight(&self) -> rquickjs::Result<()> {
    self.inner.hide_highlight().await.into_js()
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

  /// Drop a file/data payload onto this element. Mirrors Playwright's
  /// `locator.drop(payload, options?)` per `client/locator.ts:129`.
  /// `payload` is the native `{ files?, data? }` shape; `options` is the
  /// trimmed `{ modifiers?, position?, timeout? }` bag.
  #[qjs(rename = "drop")]
  pub async fn drop<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    payload: rquickjs::Value<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let payload = crate::bindings::convert::parse_drop_payload(&ctx, payload)?;
    let opts = crate::bindings::convert::parse_drop_options(&ctx, options)?;
    self.inner.drop(payload, opts).await.into_js()
  }

  #[qjs(rename = "scrollIntoViewIfNeeded")]
  pub async fn scroll_into_view_if_needed(&self) -> rquickjs::Result<()> {
    self.inner.scroll_into_view_if_needed().await.into_js()
  }

  /// Playwright: `locator.waitFor(options?: { state?: 'attached' |
  /// 'detached' | 'visible' | 'hidden', timeout?: number })`. Thin
  /// delegator to core `Locator::wait_for(WaitOptions)`.
  #[qjs(rename = "waitFor")]
  pub async fn wait_for<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    #[derive(serde::Deserialize, Default)]
    #[serde(rename_all = "camelCase", default)]
    struct JsWaitOpts {
      state: Option<String>,
      timeout: Option<u64>,
    }
    let parsed: JsWaitOpts = match options.0 {
      Some(v) if !v.is_undefined() && !v.is_null() => crate::bindings::convert::serde_from_js(&ctx, v)?,
      _ => JsWaitOpts::default(),
    };
    self
      .inner
      .wait_for(ferridriver::options::WaitOptions {
        state: parsed.state,
        timeout: parsed.timeout,
      })
      .await
      .into_js()
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

  /// Playwright: `locator.normalize(): Promise<Locator>`. Resolves the
  /// selector to its canonical recorder/codegen form and returns a new
  /// locator built from it.
  #[qjs(rename = "normalize")]
  pub async fn normalize(&self) -> rquickjs::Result<LocatorJs> {
    self.inner.normalize().await.into_js().map(LocatorJs::new)
  }

  /// Playwright: `locator.screenshot(options?): Promise<Buffer>`.
  /// Thin delegator to core `Locator::screenshot` (PNG bytes).
  #[qjs(rename = "screenshot")]
  pub async fn screenshot(&self) -> rquickjs::Result<Vec<u8>> {
    self.inner.screenshot().await.into_js()
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

  /// Playwright: `locator.ariaSnapshot(options?: TimeoutOptions &
  /// { mode?: 'ai' | 'default', depth?: number }): Promise<string>`.
  #[qjs(rename = "ariaSnapshot")]
  pub async fn aria_snapshot<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<String> {
    let core_opts = match options.0 {
      Some(v) if !v.is_undefined() && !v.is_null() => {
        #[derive(serde::Deserialize, Default)]
        #[serde(rename_all = "camelCase", default)]
        struct JsAria {
          mode: Option<String>,
          depth: Option<i32>,
          timeout: Option<u64>,
        }
        let p: JsAria = crate::bindings::convert::serde_from_js(&ctx, v)?;
        ferridriver::options::AriaSnapshotOptions {
          mode: Some(ferridriver::options::AriaSnapshotMode::from_opt_str(p.mode.as_deref())),
          depth: p.depth,
          timeout: p.timeout,
        }
      },
      _ => ferridriver::options::AriaSnapshotOptions::default(),
    };
    self.inner.aria_snapshot(core_opts).await.into_js()
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

  /// Playwright: `locator.evaluate(pageFunction, arg?, options?): Promise<R>`.
  #[qjs(rename = "evaluate")]
  pub async fn evaluate<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    page_function: rquickjs::Value<'js>,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    let (source, is_fn) = crate::bindings::convert::extract_page_function(&ctx, page_function)?;
    let serialized = crate::bindings::convert::quickjs_arg_to_serialized(&ctx, arg.0)?;
    let result = self.inner.evaluate(&source, serialized, is_fn, None).await.into_js()?;
    crate::bindings::convert::serialized_value_to_quickjs(&ctx, &result)
  }

  /// Playwright: `locator.evaluateHandle(pageFunction, arg?, options?): Promise<JSHandle>`.
  #[qjs(rename = "evaluateHandle")]
  pub async fn evaluate_handle<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    page_function: rquickjs::Value<'js>,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<crate::bindings::js_handle::JSHandleJs> {
    let (source, is_fn) = crate::bindings::convert::extract_page_function(&ctx, page_function)?;
    let serialized = crate::bindings::convert::quickjs_arg_to_serialized(&ctx, arg.0)?;
    let handle = self
      .inner
      .evaluate_handle(&source, serialized, is_fn, None)
      .await
      .into_js()?;
    Ok(crate::bindings::js_handle::JSHandleJs::new(handle))
  }

  /// Playwright: `locator.evaluateAll(pageFunction, arg?): Promise<R>`.
  #[qjs(rename = "evaluateAll")]
  pub async fn evaluate_all<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    page_function: rquickjs::Value<'js>,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    let (source, is_fn) = crate::bindings::convert::extract_page_function(&ctx, page_function)?;
    let serialized = crate::bindings::convert::quickjs_arg_to_serialized(&ctx, arg.0)?;
    let result = self.inner.evaluate_all(&source, serialized, is_fn).await.into_js()?;
    crate::bindings::convert::serialized_value_to_quickjs(&ctx, &result)
  }
}
