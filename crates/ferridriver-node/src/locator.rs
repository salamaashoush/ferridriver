//! Locator class -- NAPI binding for `ferridriver::Locator`.

use crate::abort::Bag;
use crate::error::IntoNapi;
use crate::types::{FilterOptions, WaitOptions};
use napi::Result;
use napi_derive::napi;

/// A lazy element locator. Does not query the DOM until an action is called.
#[napi]
pub struct Locator {
  inner: ferridriver::Locator,
}

impl Locator {
  pub(crate) fn wrap(inner: ferridriver::Locator) -> Self {
    Self { inner }
  }

  pub(crate) fn core(&self) -> &ferridriver::Locator {
    &self.inner
  }
}

crate::macros::napi_locator_queries!(Locator, required_frame_locator);
crate::macros::napi_element_actions!(Locator);

#[napi]
#[expect(
  clippy::needless_pass_by_value,
  reason = "napi-derive takes JS strings, functions and option bags only by value"
)]
impl Locator {
  /// The selector string for this locator.
  #[napi(getter)]
  pub fn selector(&self) -> String {
    self.inner.selector().to_string()
  }

  /// Whether this locator runs under strict mode (Playwright default). Use
  /// [`Locator::setStrict`] to opt out on a per-locator basis, or `first()` /
  /// `last()` / `nth(i)` which drop strictness implicitly.
  #[napi(getter)]
  pub fn is_strict(&self) -> bool {
    self.inner.is_strict()
  }

  /// Returns a copy of this locator with strict-mode toggled. Mirrors
  /// Playwright's strict-selectors context option on a per-locator basis.
  #[napi]
  pub fn set_strict(&self, strict: bool) -> Locator {
    Self::wrap(self.inner.strict(strict))
  }

  // ── Handle materialisation (Playwright `locator.elementHandle`) ───────

  /// Playwright: `locator.elementHandle(): Promise<ElementHandle>`.
  /// Resolves the locator and returns a pinned `ElementHandle`.
  #[napi]
  pub async fn element_handle(&self) -> Result<crate::element_handle::ElementHandle> {
    let inner = self.inner.element_handle().await.map_err(crate::error::to_napi)?;
    Ok(crate::element_handle::ElementHandle::wrap(inner))
  }

  /// Playwright: `locator.elementHandles(): Promise<ElementHandle[]>`.
  #[napi]
  pub async fn element_handles(&self) -> Result<Vec<crate::element_handle::ElementHandle>> {
    let inner = self.inner.element_handles().await.map_err(crate::error::to_napi)?;
    Ok(
      inner
        .into_iter()
        .map(crate::element_handle::ElementHandle::wrap)
        .collect(),
    )
  }

  // ── Sub-locators ────────────────────────────────────────────────────────

  /// Playwright: `locator.locator(selectorOrLocator, options?: Omit<LocatorOptions, 'visible'>): Locator`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:164`).
  /// Thin delegator to Rust core's `Locator::locator` — core does all
  /// encoding and option application; this binding only lowers JS types.
  #[napi(ts_args_type = "selectorOrLocator: string | Locator, options?: FilterOptions")]
  pub fn locator(
    &self,
    selector_or_locator: napi::Either<String, crate::types::LocatorRef>,
    options: Option<crate::types::FilterOptions>,
  ) -> Locator {
    let like = match selector_or_locator {
      napi::Either::A(selector) => ferridriver::options::LocatorLike::Selector(selector),
      napi::Either::B(inner) => ferridriver::options::LocatorLike::Selector(inner.selector),
    };
    let opts = options.map(ferridriver::options::FilterOptions::from);
    Self::wrap(match opts {
      Some(f) => self.inner.locator_with(like, &f),
      None => self.inner.locator(like),
    })
  }

  /// Playwright: `locator.contentFrame(): FrameLocator`. Treats the
  /// matched element as an `<iframe>` and returns a `FrameLocator`
  /// scoped to its contents.
  #[napi]
  pub fn content_frame(&self) -> crate::frame_locator::FrameLocator {
    crate::frame_locator::FrameLocator::wrap(self.inner.content_frame())
  }

  /// Playwright: `locator.page(): Page`.
  #[napi(ts_return_type = "Page")]
  pub fn page(&self) -> crate::identity::PageObject {
    crate::identity::PageObject(std::sync::Arc::clone(self.inner.page()))
  }

  /// Playwright: `locator.describe(description: string): Locator`.
  #[napi]
  pub fn describe(&self, description: String) -> Locator {
    Self::wrap(self.inner.describe(&description))
  }

  /// Playwright: `locator.description(): string | null`. The description set
  /// by `describe`, or `null` if none.
  #[napi(ts_return_type = "string | null")]
  pub fn description(&self) -> Option<String> {
    self.inner.description()
  }

  /// Playwright: `locator.toString(): string`.
  #[napi(js_name = "toString")]
  pub fn to_js_string(&self) -> String {
    self.inner.to_string()
  }

  /// Playwright: `locator.visible(): Locator`.
  #[napi]
  pub fn visible(&self) -> Locator {
    Self::wrap(self.inner.visible())
  }

  /// Playwright: `locator.within(locator: Locator): Locator`.
  #[napi]
  pub fn within(&self, locator: &Locator) -> Locator {
    Self::wrap(self.inner.within(&locator.inner))
  }

  #[napi]
  pub fn first(&self) -> Locator {
    Self::wrap(self.inner.first())
  }

  #[napi]
  pub fn last(&self) -> Locator {
    Self::wrap(self.inner.last())
  }

  #[napi]
  pub fn nth(&self, index: i32) -> Locator {
    Self::wrap(self.inner.nth(index))
  }

  #[napi]
  pub fn filter(&self, options: FilterOptions) -> Locator {
    Self::wrap(
      self
        .inner
        .filter_with(&ferridriver::options::FilterOptions::from(options)),
    )
  }

  /// Intersection: matches the same element that both locators match.
  /// Mirrors Playwright's `locator.and(other)`.
  #[napi]
  pub fn and(&self, other: &Locator) -> Locator {
    Self::wrap(self.inner.and(&other.inner))
  }

  /// Union: matches elements resolved by either locator. Mirrors
  /// Playwright's `locator.or(other)`.
  #[napi]
  pub fn or(&self, other: &Locator) -> Locator {
    Self::wrap(self.inner.or(&other.inner))
  }

  // ── Actions ─────────────────────────────────────────────────────────────

  #[napi]
  pub async fn clear(&self) -> Result<()> {
    self.inner.clear().await.map_err(crate::error::to_napi)
  }

  /// Playwright: `highlight(options?: { style?: string | Record<string,
  /// string | number> }): Promise<Disposable>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:158`).
  #[napi(ts_args_type = "options?: { style?: string | Record<string, string | number> }")]
  pub async fn highlight(&self, options: Option<serde_json::Value>) -> Result<crate::disposable::Disposable> {
    let style = options
      .and_then(|o| o.get("style").cloned())
      .and_then(crate::types::highlight_style_to_rust);
    let disposable = self.inner.highlight(style).await.map_err(crate::error::to_napi)?;
    Ok(crate::disposable::Disposable::wrap(disposable))
  }

  /// Playwright: `hideHighlight(): Promise<void>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:164`).
  #[napi]
  pub async fn hide_highlight(&self) -> Result<()> {
    self.inner.hide_highlight().await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn type_text(&self, text: String, options: Option<crate::types::TypeOptions>) -> Result<()> {
    self
      .inner
      .r#type(&text)
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi(js_name = "type")]
  pub async fn type_alias(&self, text: String, options: Option<crate::types::TypeOptions>) -> Result<()> {
    self
      .inner
      .r#type(&text)
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn blur(&self) -> Result<()> {
    self.inner.blur().await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn scroll_into_view(&self) -> Result<()> {
    self.scroll_into_view_if_needed().await
  }

  /// Type `text` character-by-character. Accepts Playwright's full
  /// `LocatorPressSequentiallyOptions` (same shape as `TypeOptions`).
  #[napi]
  pub async fn press_sequentially(&self, text: String, options: Option<crate::types::TypeOptions>) -> Result<()> {
    self
      .inner
      .press_sequentially(&text)
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)
  }

  // ── Content & state ─────────────────────────────────────────────────────

  /// Playwright: `locator.ariaSnapshot(options?): Promise<string>`.
  /// Renders the accessibility subtree rooted at the matched element
  /// (siblings outside the locator excluded) via the vendored
  /// Playwright `InjectedScript` — byte-for-byte Playwright YAML.
  #[napi(
    js_name = "ariaSnapshot",
    ts_args_type = "options?: { mode?: 'ai' | 'default', depth?: number, boxes?: boolean, timeout?: number, signal?: AbortSignal }",
    ts_return_type = "Promise<string>"
  )]
  pub async fn aria_snapshot(&self, options: Option<crate::types::AriaSnapshotOptions>) -> Result<String> {
    self
      .inner
      .aria_snapshot()
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)
  }

  /// Playwright: `locator.ariaSnapshotJSON(options?): Promise<AriaSnapshotJSON>`.
  #[napi(
    js_name = "ariaSnapshotJSON",
    ts_args_type = "options?: { mode?: 'ai' | 'default', depth?: number, boxes?: boolean, timeout?: number, signal?: AbortSignal }",
    ts_return_type = "Promise<import('./types').AriaSnapshotJSON>"
  )]
  pub async fn aria_snapshot_json(
    &self,
    options: Option<crate::types::AriaSnapshotOptions>,
  ) -> Result<Vec<serde_json::Value>> {
    self
      .inner
      .aria_snapshot_json()
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn text_content(&self, options: Option<crate::types::TimeoutOptions>) -> Result<Option<String>> {
    self
      .inner
      .text_content()
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn inner_text(&self, options: Option<crate::types::TimeoutOptions>) -> Result<String> {
    self
      .inner
      .inner_text()
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi(js_name = "innerHTML")]
  pub async fn inner_html(&self, options: Option<crate::types::TimeoutOptions>) -> Result<String> {
    self
      .inner
      .inner_html()
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn get_attribute(
    &self,
    name: String,
    options: Option<crate::types::TimeoutOptions>,
  ) -> Result<Option<String>> {
    self
      .inner
      .get_attribute(&name)
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn input_value(&self, options: Option<crate::types::TimeoutOptions>) -> Result<String> {
    self
      .inner
      .input_value()
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn is_visible(&self, options: Option<crate::types::TimeoutOptions>) -> Result<bool> {
    self
      .inner
      .is_visible()
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn is_hidden(&self, options: Option<crate::types::TimeoutOptions>) -> Result<bool> {
    self
      .inner
      .is_hidden()
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn is_enabled(&self, options: Option<crate::types::TimeoutOptions>) -> Result<bool> {
    self
      .inner
      .is_enabled()
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn is_disabled(&self, options: Option<crate::types::TimeoutOptions>) -> Result<bool> {
    self
      .inner
      .is_disabled()
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn is_checked(&self, options: Option<crate::types::TimeoutOptions>) -> Result<bool> {
    self
      .inner
      .is_checked()
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn is_editable(&self, options: Option<crate::types::TimeoutOptions>) -> Result<bool> {
    self
      .inner
      .is_editable()
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn count(&self) -> Result<i32> {
    let n = self.inner.count().await.map_err(crate::error::to_napi)?;
    i32::try_from(n).map_err(|_| napi::Error::from_reason(format!("element count {n} exceeds i32::MAX")))
  }

  /// Playwright: `locator.normalize(): Promise<Locator>`. Resolves the
  /// selector to the canonical form the recorder/codegen would emit for
  /// the matched element and returns a new locator built from it.
  #[napi]
  pub async fn normalize(&self) -> Result<Locator> {
    let inner = self.inner.normalize().await.map_err(crate::error::to_napi)?;
    Ok(Self::wrap(inner))
  }

  /// The locator expression to paste into a test, such as
  /// `getByRole('button', { name: 'Sign in' })`, rather than the
  /// selector `normalize()` returns.
  #[napi(ts_args_type = "language?: 'typescript' | 'rust' | 'gherkin'")]
  pub async fn generate_locator(&self, language: Option<String>) -> Result<String> {
    let language = language.map_or(ferridriver_codegen::OutputLanguage::TypeScript, |l| {
      ferridriver_codegen::OutputLanguage::parse_cli(&l)
    });
    self
      .inner
      .generate_locator(language)
      .await
      .map_err(crate::error::to_napi)
  }

  /// Drag this element to `target`. Mirrors Playwright's
  /// `locator.dragTo(target, options?)` per
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:13293`.
  #[napi(js_name = "dragTo")]
  pub async fn drag_to(&self, target: &Locator, options: Option<crate::types::DragAndDropOptions>) -> Result<()> {
    self.inner.drag_to(&target.inner).bag(options)?.await.into_napi()
  }

  /// Drop a file/data payload onto this element. Mirrors Playwright's
  /// `locator.drop(payload, options?)` per `client/locator.ts:129`.
  #[napi(
    ts_args_type = "payload: { files?: string | string[] | FilePayload | FilePayload[], data?: Record<string, string> }, options?: DropOptions"
  )]
  pub async fn drop(
    &self,
    payload: crate::types::NapiDropPayload,
    options: Option<crate::types::DropOptions>,
  ) -> Result<()> {
    self
      .inner
      .drop(payload.0)
      .bag(options)?
      .await
      .map_err(crate::error::to_napi)
  }

  // ── Waiting ─────────────────────────────────────────────────────────────

  #[napi]
  pub async fn wait_for(&self, options: Option<WaitOptions>) -> Result<()> {
    self.inner.wait_for().bag(options)?.await.map_err(crate::error::to_napi)
  }

  // ── All matches ─────────────────────────────────────────────────────────

  #[napi]
  pub async fn all_text_contents(&self) -> Result<Vec<String>> {
    self.inner.all_text_contents().await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn all_inner_texts(&self) -> Result<Vec<String>> {
    self.inner.all_inner_texts().await.map_err(crate::error::to_napi)
  }

  // ── Missing methods ─────────────────────────────────────────────────────

  #[napi]
  pub async fn right_click(&self) -> Result<()> {
    self.inner.right_click().await.map_err(crate::error::to_napi)
  }

  #[napi]
  pub async fn is_attached(&self) -> Result<bool> {
    self.inner.is_attached().await.map_err(crate::error::to_napi)
  }

  /// Playwright: `locator.evaluate(pageFunction, arg?, options?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:129`).
  #[napi(
    ts_args_type = "pageFunction: string | Function, arg?: unknown, options?: { timeout?: number, signal?: AbortSignal, exposeFunctions?: boolean }",
    ts_return_type = "Promise<unknown>"
  )]
  pub async fn evaluate(
    &self,
    page_function: crate::types::NapiPageFunction,
    arg: Option<crate::types::NapiEvaluateArg>,
    options: Option<crate::types::EvaluateOptions>,
  ) -> Result<crate::serialize_out::Evaluated> {
    let expose_functions = options.as_ref().and_then(|o| o.expose_functions).unwrap_or(false);
    let serialized = crate::page::build_evaluate_argument(arg, expose_functions)?;
    let result = self
      .inner
      .evaluate(&page_function.source, serialized, page_function.is_function)
      .bag(options)?
      .await
      .into_napi()?;
    Ok(crate::serialize_out::Evaluated(result))
  }

  /// Playwright: `locator.evaluateHandle(pageFunction, arg?, options?): Promise<JSHandle>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:138`).
  #[napi(
    ts_args_type = "pageFunction: string | Function, arg?: unknown, options?: { timeout?: number, signal?: AbortSignal, exposeFunctions?: boolean }"
  )]
  pub async fn evaluate_handle(
    &self,
    page_function: crate::types::NapiPageFunction,
    arg: Option<crate::types::NapiEvaluateArg>,
    options: Option<crate::types::EvaluateOptions>,
  ) -> Result<crate::js_handle::JSHandle> {
    let expose_functions = options.as_ref().and_then(|o| o.expose_functions).unwrap_or(false);
    let serialized = crate::page::build_evaluate_argument(arg, expose_functions)?;
    let handle = self
      .inner
      .evaluate_handle(&page_function.source, serialized, page_function.is_function)
      .bag(options)?
      .await
      .into_napi()?;
    Ok(crate::js_handle::JSHandle::wrap(handle))
  }

  /// Playwright: `locator.evaluateAll(pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:133`).
  #[napi(
    ts_args_type = "pageFunction: string | Function, arg?: unknown",
    ts_return_type = "Promise<unknown>"
  )]
  pub async fn evaluate_all(
    &self,
    page_function: crate::types::NapiPageFunction,
    arg: Option<crate::types::NapiEvaluateArg>,
  ) -> Result<crate::serialize_out::Evaluated> {
    let serialized = crate::page::build_serialized_argument(arg)?;
    let result = self
      .inner
      .evaluate_all(&page_function.source, serialized, page_function.is_function)
      .await
      .into_napi()?;
    Ok(crate::serialize_out::Evaluated(result))
  }

  /// Playwright: `locator.waitForFunction(pageFunction, arg?, options?): Promise<void>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:396`).
  #[napi(
    ts_args_type = "pageFunction: string | Function, arg?: unknown, options?: { timeout?: number, signal?: AbortSignal }"
  )]
  pub async fn wait_for_function(
    &self,
    page_function: crate::types::NapiPageFunction,
    arg: Option<crate::types::NapiEvaluateArg>,
    mut options: Option<crate::types::EvaluateOptions>,
  ) -> Result<()> {
    let serialized = crate::page::build_serialized_argument(arg)?;
    let signal = crate::abort::take(&mut options);
    let opts: Option<ferridriver::options::EvaluateOptions> = options.map(Into::into);
    let timeout = opts.and_then(|o| o.timeout);
    crate::abort::run(
      signal,
      self
        .inner
        .wait_for_function(&page_function.source, serialized, page_function.is_function, timeout),
    )
    .await
  }

  #[napi]
  pub fn or_locator(&self, other: &Locator) -> Locator {
    Locator {
      inner: self.inner.or(&other.inner),
    }
  }

  #[napi]
  pub fn and_locator(&self, other: &Locator) -> Locator {
    Locator {
      inner: self.inner.and(&other.inner),
    }
  }

  #[napi]
  pub async fn all(&self) -> Result<Vec<Locator>> {
    let locators = self.inner.all().await.map_err(crate::error::to_napi)?;
    Ok(locators.into_iter().map(|l| Locator { inner: l }).collect())
  }
}
