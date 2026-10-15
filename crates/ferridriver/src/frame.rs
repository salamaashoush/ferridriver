//! Frame API -- mirrors Playwright's Frame interface.
//!
//! A Frame represents an execution context within a Page.
//! The main frame is the top-level page frame. Child frames
//! correspond to `<iframe>` elements.
//!
//! Frame has the same evaluation and locator methods as Page,
//! but scoped to its specific frame context.

use std::sync::Arc;

use crate::error::Result;
use crate::locator::Locator;
use crate::options::{RoleOptions, StringOrRegex, TextOptions, WaitOptions};
use crate::page::Page;

/// A frame within a page. Mirrors Playwright's
/// [Frame interface](https://playwright.dev/docs/api/class-frame).
///
/// Frame instances are thin handles — the authoritative name/url/parent
/// state lives in `crate::frame_cache::FrameCache` on the owning Page.
/// Cloning a Frame is cheap (`Arc<Page>` + `Arc<str>`) and multiple
/// clones see the same live state.
#[derive(Clone)]
pub struct Frame {
  /// The page this frame belongs to (Arc for cheap cloning in locator chains).
  page: Arc<Page>,
  /// Frame ID (from CDP or backend). `Arc<str>` so locator chains are cheap.
  pub(crate) id: Arc<str>,
}

impl Frame {
  #[must_use]
  pub fn webmcp(&self) -> crate::web_mcp::WebMcp {
    crate::web_mcp::WebMcp::new(self.clone())
  }

  /// Create a frame handle pointing at an id present in the page's
  /// frame cache. The cache is the source of truth for name/url/parent.
  pub(crate) fn new(page: Arc<Page>, id: Arc<str>) -> Self {
    Self { page, id }
  }

  /// Backend-issued frame identifier. Sync — the underlying `Arc<str>`
  /// is set at construction. Used by the network module to match
  /// `Request.frame()` lookups against the page's frame cache.
  #[must_use]
  pub fn frame_id(&self) -> &str {
    &self.id
  }

  /// Frame name (from the `name` attribute of the iframe element).
  /// Playwright: [`frame.name()`](https://playwright.dev/docs/api/class-frame#frame-name)
  /// -- `name(): string` sync, reads cached state.
  #[must_use]
  pub fn name(&self) -> String {
    self
      .page
      .with_frame_cache(|c| c.record(&self.id).map(|r| r.info.name.clone()).unwrap_or_default())
  }

  /// Frame URL.
  /// Playwright: [`frame.url()`](https://playwright.dev/docs/api/class-frame#frame-url)
  /// -- `url(): string` sync.
  #[must_use]
  pub fn url(&self) -> String {
    let (url, is_main) = self.page.with_frame_cache(|c| {
      (
        c.record(&self.id).map(|r| r.info.url.clone()).unwrap_or_default(),
        c.main_frame_id().as_deref() == Some(&*self.id),
      )
    });
    // A page is on its initial empty document until a navigation
    // commits, and no backend reports that one: Playwright's
    // `page.url()` reads `about:blank` there.
    if url.is_empty() && is_main {
      return String::from("about:blank");
    }
    url
  }

  /// Whether this is the main (top-level) frame. Mirrors Playwright's
  /// equivalent of `frame.parentFrame() === null`.
  #[must_use]
  pub fn is_main_frame(&self) -> bool {
    self
      .page
      .with_frame_cache(|c| c.main_frame_id().as_deref() == Some(&*self.id))
  }

  /// Parent frame. Returns `None` for the main frame. Sync — reads from
  /// the page's frame cache (Playwright:
  /// [`frame.parentFrame()`](https://playwright.dev/docs/api/class-frame#frame-parent-frame)).
  #[must_use]
  pub fn parent_frame(&self) -> Option<Frame> {
    let pid = self.page.with_frame_cache(|c| c.parent_id(&self.id))?;
    Some(Frame::new(Arc::clone(&self.page), pid))
  }

  /// Child frames. Sync — reads from the page's frame cache.
  /// Playwright: [`frame.childFrames()`](https://playwright.dev/docs/api/class-frame#frame-child-frames).
  #[must_use]
  pub fn child_frames(&self) -> Vec<Frame> {
    let ids = self.page.with_frame_cache(|c| c.child_ids(&self.id));
    ids
      .into_iter()
      .map(|id| Frame::new(Arc::clone(&self.page), id))
      .collect()
  }

  // ── Evaluation (frame-scoped) ────────────────────────────────────────

  /// Playwright: `frame.evaluate(pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:196`).
  ///
  /// Runs `fn_source` in this frame's execution context with `arg`
  /// serialised through the isomorphic wire protocol. Main-frame calls
  /// pass `frame_id = None`; child-frame calls thread `self.id()` so
  /// the utility script resolves the target frame's context.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::error::FerriError`] on page-side exception or
  /// protocol failure.
  pub async fn evaluate(
    &self,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<crate::protocol::SerializedValue> {
    self
      .page
      .traced_as(
        "Frame",
        "evaluate",
        serde_json::json!({ "expression": fn_source }),
        self.evaluate_untraced(fn_source, arg, is_function),
      )
      .await
  }

  /// [`Self::evaluate`] without a trace action of its own — what a
  /// public method that already opened one calls.
  pub(crate) async fn evaluate_untraced(
    &self,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<crate::protocol::SerializedValue> {
    self.page.expose_arg_functions(&arg.functions).await?;
    let frame_id = if self.is_main_frame() { None } else { Some(&*self.id) };
    let empty = matches!(
      arg.value,
      crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Undefined)
    ) && arg.handles.is_empty();
    let args_slice: &[crate::protocol::SerializedValue] = if empty { &[] } else { std::slice::from_ref(&arg.value) };
    let result = self
      .page
      .inner
      .call_utility_evaluate(fn_source, args_slice, &arg.handles, frame_id, is_function, true)
      .await?;
    match result {
      crate::js_handle::EvaluateResult::Value(v) => Ok(v),
      crate::js_handle::EvaluateResult::Handle(..) => Err(crate::error::FerriError::Evaluation(
        "Frame::evaluate: backend returned handle but returnByValue=true was requested".into(),
      )),
    }
  }

  /// Playwright: `frame.evaluateHandle(pageFunction, arg?): Promise<JSHandle>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:190`).
  ///
  /// Same wire path as [`Self::evaluate`] but retains the result on
  /// the page and hands back a fresh [`crate::js_handle::JSHandle`].
  ///
  /// # Errors
  ///
  /// See [`Self::evaluate`].
  pub async fn evaluate_handle(
    &self,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<crate::js_handle::JSHandle> {
    self
      .page
      .traced_as(
        "Frame",
        "evaluateHandle",
        serde_json::json!({ "expression": fn_source }),
        self.evaluate_handle_untraced(fn_source, arg, is_function),
      )
      .await
  }

  /// [`Self::evaluate_handle`] without a trace action of its own — the
  /// selector probes `$eval` / `$$eval` run are part of those calls.
  pub(crate) async fn evaluate_handle_untraced(
    &self,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<crate::js_handle::JSHandle> {
    self.page.expose_arg_functions(&arg.functions).await?;
    let frame_id = if self.is_main_frame() { None } else { Some(&*self.id) };
    let empty = matches!(
      arg.value,
      crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Undefined)
    ) && arg.handles.is_empty();
    let args_slice: &[crate::protocol::SerializedValue] = if empty { &[] } else { std::slice::from_ref(&arg.value) };
    let result = self
      .page
      .inner
      .call_utility_evaluate(fn_source, args_slice, &arg.handles, frame_id, is_function, false)
      .await?;
    match result {
      crate::js_handle::EvaluateResult::Handle(backing, is_node) => Ok(crate::js_handle::JSHandle::from_backing(
        Arc::clone(&self.page),
        backing,
        is_node,
      )),
      crate::js_handle::EvaluateResult::Value(_) => Err(crate::error::FerriError::Evaluation(
        "Frame::evaluate_handle: backend returned value but returnByValue=false was requested".into(),
      )),
    }
  }

  /// Playwright: `frame.waitForFunction(pageFunction, arg?, options?): Promise<JSHandle>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:481`).
  ///
  /// Polls `pageFunction` (with `arg`) in this frame until it returns a
  /// truthy value, then returns that value as a
  /// [`crate::js_handle::JSHandle`]. `options.polling` is `'raf'`
  /// (default) — one poll per animation frame — or a number of
  /// milliseconds between polls. Probe errors from a navigating frame
  /// (context destroyed) re-poll against the new document, mirroring
  /// Playwright's rerunnable-task re-install; other page-side
  /// exceptions propagate immediately.
  ///
  /// # Errors
  ///
  /// [`crate::error::FerriError::timeout`] when the function never
  /// turns truthy before the deadline; page-side exceptions from the
  /// function itself are forwarded.
  pub async fn wait_for_function(
    &self,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
    options: Option<crate::options::WaitForFunctionOptions>,
  ) -> Result<crate::js_handle::JSHandle> {
    let opts = options.unwrap_or_default();
    let polling = opts.polling.unwrap_or(crate::options::Polling::Raf);
    let timeout = opts.timeout.unwrap_or_else(|| self.page.default_timeout());
    let deadline = (timeout != 0).then(|| tokio::time::Instant::now() + std::time::Duration::from_millis(timeout));
    // Truthiness is decided page-side (`!!`) so objects, NaN, and
    // empty-string edge cases follow JS semantics exactly instead of a
    // lossy serialized-value reconstruction.
    let probe_is_function = is_function.unwrap_or(false);
    let probe = if probe_is_function {
      format!("arg => !!(({fn_source})(arg))")
    } else {
      format!("!!({fn_source})")
    };
    loop {
      if let Some(d) = deadline
        && tokio::time::Instant::now() >= d
      {
        return Err(crate::error::FerriError::timeout(
          format!("waiting for function: {fn_source}"),
          timeout,
        ));
      }
      match self.evaluate(&probe, arg.clone(), Some(probe_is_function)).await {
        Ok(value) => {
          if crate::protocol::result_to_serde::<bool>(&value).unwrap_or(false) {
            break;
          }
        },
        Err(e) => {
          if !crate::locator::is_retryable_action_error(&e.to_string()) {
            return Err(e);
          }
        },
      }
      match polling {
        crate::options::Polling::Interval(ms) => {
          tokio::time::sleep(std::time::Duration::from_millis(ms.max(1))).await;
        },
        crate::options::Polling::Raf => {
          // One animation frame between polls; a failing round-trip
          // (frame mid-navigation) degrades to a frame-length sleep.
          if self
            .evaluate(
              "() => new Promise(f => requestAnimationFrame(() => f(1)))",
              crate::protocol::SerializedArgument::default(),
              Some(true),
            )
            .await
            .is_err()
          {
            tokio::time::sleep(std::time::Duration::from_millis(16)).await;
          }
        },
      }
    }
    // Materialize the truthy result as a handle. The function runs once
    // more; waitForFunction predicates are truthy-stable by contract.
    self.evaluate_handle(fn_source, arg, is_function).await
  }

  /// Playwright: `frame.$eval(selector, pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:242`).
  /// Resolves the first element matching `selector` through the selector
  /// engine, then calls `fn(element, arg)`. Throws when no element
  /// matches, matching Playwright's `evalOnSelector`.
  ///
  /// # Errors
  ///
  /// [`crate::error::FerriError::Evaluation`] when the selector matches
  /// nothing or the user function throws; selector-parse / backend error.
  pub async fn eval_on_selector(
    &self,
    selector: &str,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<crate::protocol::SerializedValue> {
    let parsed = crate::selectors::parse(selector)?;
    let parts_json = crate::selectors::build_parts_json(&parsed);
    self.page.inner.ensure_engine_injected().await?;
    let sel_str = serde_json::to_string(selector)
      .map_err(|e| crate::error::FerriError::Backend(format!("$eval selector escape: {e}")))?;
    let probe = format!(
      "() => {{ const r = window.__fd.selOne({parts_json}); \
        if (!r) throw new Error('failed to find element matching selector ' + {sel_str}); \
        return r; }}"
    );
    let match_handle = self
      .evaluate_handle_untraced(&probe, crate::protocol::SerializedArgument::default(), Some(true))
      .await?;
    let result = match_handle.evaluate(fn_source, arg, is_function).await;
    let _ = match_handle.dispose().await;
    result
  }

  /// Playwright: `frame.$$eval(selector, pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:248`).
  /// Resolves every element matching `selector` as an array, then calls
  /// `fn(elements, arg)`. Unlike `$eval`, an empty match is not an error
  /// -- the function receives an empty array.
  ///
  /// # Errors
  ///
  /// Returns an error only when the user function throws, or on selector
  /// parse / protocol failure.
  pub async fn eval_on_selector_all(
    &self,
    selector: &str,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<crate::protocol::SerializedValue> {
    let parsed = crate::selectors::parse(selector)?;
    let parts_json = crate::selectors::build_parts_json(&parsed);
    self.page.inner.ensure_engine_injected().await?;
    let probe = format!("() => window.__fd.selAll({parts_json})");
    let array_handle = self
      .evaluate_handle_untraced(&probe, crate::protocol::SerializedArgument::default(), Some(true))
      .await?;
    let result = array_handle.evaluate(fn_source, arg, is_function).await;
    let _ = array_handle.dispose().await;
    result
  }

  /// Typed evaluate: run `fn_source` in this frame and deserialize the
  /// result via serde. Ergonomic wrapper over the wire-level
  /// [`Self::evaluate`] for JSON-shaped values:
  ///
  /// ```ignore
  /// let count: u32 = frame.eval("() => document.images.length").await?;
  /// ```
  ///
  /// # Errors
  ///
  /// See [`Self::evaluate`], plus [`crate::error::FerriError::Json`] /
  /// [`crate::error::FerriError::Evaluation`] when the result does not
  /// decode into `T` (rich JS values need [`Self::evaluate_handle`]).
  pub async fn eval<T: serde::de::DeserializeOwned>(&self, fn_source: &str) -> Result<T> {
    let value = self
      .evaluate(fn_source, crate::protocol::SerializedArgument::default(), None)
      .await?;
    crate::protocol::result_to_serde(&value)
  }

  /// [`Self::eval`] with a serde-serialized argument, passed to the page
  /// function as its single parameter:
  ///
  /// ```ignore
  /// let hit: bool = frame.eval_with("sel => !!document.querySelector(sel)", &"#app").await?;
  /// ```
  ///
  /// # Errors
  ///
  /// See [`Self::eval`].
  pub async fn eval_with<T: serde::de::DeserializeOwned>(
    &self,
    fn_source: &str,
    arg: &(impl serde::Serialize + ?Sized),
  ) -> Result<T> {
    let arg = crate::protocol::argument_from_serde(arg)?;
    let value = self.evaluate(fn_source, arg, None).await?;
    crate::protocol::result_to_serde(&value)
  }

  /// Backend-level expression evaluation used by the frame's internal
  /// plumbing — dispatches to the right backend method (`evaluate` vs
  /// `evaluate_in_frame`) depending on whether this is the main frame.
  /// Not part of the public Playwright API; public callers use
  /// [`Self::evaluate`] with a function literal.
  async fn backend_eval_expr(&self, expression: &str) -> Result<Option<serde_json::Value>> {
    if self.is_main_frame() {
      self.page.inner.evaluate(expression).await
    } else {
      self.page.inner.evaluate_in_frame(expression, &self.id).await
    }
  }

  // ── Locators (frame-scoped) ──────────────────────────────────────────

  /// Create a locator scoped to this frame.
  ///
  /// Playwright: `frame.locator(selector, options?: LocatorOptions): Locator`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:324`).
  /// The options-bag form is [`Self::locator_with`].
  #[must_use]
  pub fn locator(&self, selector: &str) -> Locator {
    Locator::new(self.clone(), selector.to_string())
  }

  /// The locator a SELECTOR-taking method resolves through.
  ///
  /// Not the same as [`Self::locator`]: a Locator is always strict,
  /// while `frame.click(selector)` and friends take their strictness
  /// from the context's `strictSelectors`, which defaults to false.
  /// Playwright makes the same split in one place
  /// (`server/frameSelectors.ts:53`), where an explicit per-call
  /// `strict` wins and the context option is the fallback.
  fn selector_locator(&self, selector: &str) -> Locator {
    self.locator(selector).strict(self.page.strict_selectors())
  }

  /// [`Self::locator`] with Playwright's `LocatorOptions` filter bag
  /// (including `visible`).
  #[must_use]
  pub fn locator_with(&self, selector: &str, options: &crate::options::FilterOptions) -> Locator {
    self.locator(selector).filter(options)
  }

  #[must_use]
  pub fn get_by_role(
    &self,
    role: impl Into<crate::options::Role>,
  ) -> crate::locator_builder::LocatorBuilder<RoleOptions> {
    let frame = self.clone();
    let role = role.into();
    crate::locator_builder::LocatorBuilder::new(move |opts| {
      Locator::new(frame.clone(), crate::locator::build_role_selector(role.as_str(), opts))
    })
  }

  /// `getByText` in this frame. Accepts `string | RegExp`.
  #[must_use]
  pub fn get_by_text(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.text_like_builder("internal:text", text.into())
  }

  /// `getByTestId` in this frame, against the attribute this frame's
  /// context reads ([`crate::context::ContextRef::test_id_attribute`]).
  #[must_use]
  pub fn get_by_test_id(&self, test_id: impl Into<StringOrRegex>) -> Locator {
    Locator::new(
      self.clone(),
      crate::locator::build_testid_selector(&self.test_id_attribute(), &test_id.into()),
    )
  }

  /// The `testIdAttribute` in force for this frame — its page's context,
  /// falling back to the process default for a page with no context (a
  /// standalone MCP session).
  #[must_use]
  pub fn test_id_attribute(&self) -> std::borrow::Cow<'static, str> {
    self.page.context().map_or_else(
      crate::selectors::default_test_id_attribute,
      super::context::ContextRef::test_id_attribute,
    )
  }

  /// `getByLabel` in this frame.
  #[must_use]
  pub fn get_by_label(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.text_like_builder("internal:label", text.into())
  }

  /// `getByPlaceholder` in this frame.
  #[must_use]
  pub fn get_by_placeholder(
    &self,
    text: impl Into<StringOrRegex>,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.attr_builder("placeholder", text.into())
  }

  /// Locate elements by `alt` attribute. Mirrors Playwright's
  /// `frame.getByAltText(text, options?)`.
  #[must_use]
  pub fn get_by_alt_text(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.attr_builder("alt", text.into())
  }

  /// Locate elements by `title` attribute. Mirrors Playwright's
  /// `frame.getByTitle(text, options?)`.
  #[must_use]
  pub fn get_by_title(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.attr_builder("title", text.into())
  }

  fn text_like_builder(
    &self,
    kind: &'static str,
    text: StringOrRegex,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    let frame = self.clone();
    crate::locator_builder::LocatorBuilder::new(move |opts| {
      Locator::new(
        frame.clone(),
        crate::locator::build_text_like_selector(kind, &text, opts),
      )
    })
  }

  fn attr_builder(
    &self,
    attr: &'static str,
    text: StringOrRegex,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    let frame = self.clone();
    crate::locator_builder::LocatorBuilder::new(move |opts| {
      Locator::new(frame.clone(), crate::locator::build_attr_selector(attr, &text, opts))
    })
  }

  /// Playwright: `frame.frameLocator(selector?: string): FrameLocator`.
  /// With a selector, the `<iframe>` it matches in this frame's document;
  /// without one, whichever frame in this frame's subtree (this one
  /// included) the eventual locator matches in.
  #[must_use]
  pub fn frame_locator<'a>(&self, selector: impl Into<Option<&'a str>>) -> crate::locator::FrameLocator {
    let selector = selector.into().unwrap_or(crate::locator::ANY_FRAME);
    crate::locator::FrameLocator::for_iframe_in(self.clone(), selector.to_string())
  }

  // ── Content (frame-scoped) ───────────────────────────────────────────

  /// Playwright: `frame.content(options?: { includeShadow?: boolean }):
  /// Promise<string>` — the doctype and the document element's markup.
  ///
  /// # Errors
  ///
  /// Returns an error if JS evaluation fails.
  #[track_caller]
  pub fn content(&self) -> crate::action::Action<'_, crate::options::ContentOptions, String> {
    let frame = self;
    crate::action::Action::new(move |opts: crate::options::ContentOptions| {
      Box::pin(async move {
        let params = serde_json::json!({ "includeShadow": opts.include_shadow });
        frame
          .page
          .traced_as("Frame", "content", params, frame.content_impl(opts))
          .await
      })
    })
  }

  /// The injected engine's `documentContent`, which `page.content` shares.
  pub(crate) async fn content_impl(&self, opts: crate::options::ContentOptions) -> Result<String> {
    self.page.inner.ensure_engine_injected().await?;
    let include_shadow = opts.include_shadow.unwrap_or(false);
    let r = self
      .backend_eval_expr(&format!("window.__fd._injected.documentContent({include_shadow})"))
      .await?;
    Ok(
      r.and_then(|v| v.as_str().map(std::string::ToString::to_string))
        .unwrap_or_default(),
    )
  }

  /// Get the frame's title.
  ///
  /// # Errors
  ///
  /// Returns an error if JS evaluation fails.
  pub async fn title(&self) -> Result<String> {
    self
      .page
      .traced_as("Frame", "title", serde_json::json!({}), async {
        let r = self.backend_eval_expr("document.title").await?;
        Ok(
          r.and_then(|v| v.as_str().map(std::string::ToString::to_string))
            .unwrap_or_default(),
        )
      })
      .await
  }

  // ── Navigation (frame-scoped) ────────────────────────────────────────

  /// Playwright: `frame.goto(url, options?: { referer?, timeout?,
  /// waitUntil? }): Promise<Response | null>`
  /// (`packages/playwright-core/src/client/frame.ts`).
  ///
  /// A child frame is navigated by assigning its location, then waited on
  /// like any other navigation of that frame.
  ///
  /// # Errors
  ///
  /// Returns an error if the navigation fails or does not reach
  /// `waitUntil` in time, and [`crate::error::FerriError::Unsupported`]
  /// for `referer` on a child frame.
  #[track_caller]
  pub fn goto(
    &self,
    url: &str,
  ) -> crate::action::Action<'static, crate::options::GotoOptions, Option<crate::network::Response>> {
    let frame = self.clone();
    let url = url.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        frame
          .page
          .traced_as(
            "Frame",
            "goto",
            serde_json::json!({ "url": url }),
            frame.goto_untraced(&url, opts),
          )
          .await
      })
    })
  }

  async fn goto_untraced(
    &self,
    url: &str,
    opts: crate::options::GotoOptions,
  ) -> Result<Option<crate::network::Response>> {
    if self.is_main_frame() {
      return self.page.goto_impl(url, Some(opts)).await;
    }
    if opts.referer.is_some() {
      return Err(crate::error::FerriError::unsupported(
        "A child frame is navigated by assigning its location, which cannot carry a referer",
      ));
    }
    let resolved = self.page.resolve_with_base_url(url).await;
    let target = serde_json::to_string(&resolved)?;
    // Listening starts before the assignment, which can commit a
    // same-document navigation before it returns.
    let events = self.page.events().subscribe();
    self
      .backend_eval_expr(&format!("void (window.location.href = {target})"))
      .await?;
    self
      .navigated(
        events,
        crate::options::WaitForNavigationOptions {
          timeout: opts.timeout,
          url: None,
          wait_until: opts.wait_until,
        },
      )
      .await
  }

  /// Playwright: `frame.waitForNavigation(options?: { timeout?, url?,
  /// waitUntil? }): Promise<Response | null>`. The next navigation of
  /// this frame, to a URL `url` matches when given, once it has reached
  /// `waitUntil`. The response is the new document's, and `None` for a
  /// navigation within the document.
  ///
  /// # Errors
  ///
  /// Returns an error on timeout, or if the frame is detached or the page
  /// closes first.
  #[track_caller]
  pub fn wait_for_navigation(
    &self,
  ) -> crate::action::Action<'static, crate::options::WaitForNavigationOptions, Option<crate::network::Response>> {
    let frame = self.clone();
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        // Before the trace span opens: opening one can take a round trip,
        // and the navigation this waits for is usually started alongside.
        let events = frame.page.events().subscribe();
        frame
          .page
          .traced_as(
            "Frame",
            "waitForNavigation",
            serde_json::json!({}),
            frame.navigated(events, opts),
          )
          .await
      })
    })
  }

  /// Whether `info` describes this frame. The main frame is the one with
  /// no parent whatever its id: a backend may issue a new id when a
  /// navigation moves the page to another process.
  fn is_described_by(&self, info: &crate::backend::FrameInfo, main: bool) -> bool {
    info.frame_id == *self.id || (main && info.parent_frame_id.is_none())
  }

  async fn navigated(
    &self,
    mut events: crate::events::EventSubscription<crate::events::PageEvent>,
    opts: crate::options::WaitForNavigationOptions,
  ) -> Result<Option<crate::network::Response>> {
    use crate::events::PageEvent;
    let timeout_ms = opts.timeout.unwrap_or_else(|| self.page.default_navigation_timeout());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let main = self.is_main_frame();
    let wanted = |url: &str| opts.url.as_ref().is_none_or(|matcher| matcher.matches(url));
    let mut document: Option<crate::network::Response> = None;
    let response = loop {
      let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
      let event = match tokio::time::timeout(remaining, events.recv()).await {
        Ok(Some(event)) => event,
        Ok(None) => return Err(crate::error::FerriError::target_closed(None)),
        Err(_) => return Err(crate::error::FerriError::timeout("waiting for navigation", timeout_ms)),
      };
      match event {
        PageEvent::Response(response)
          if response.request().is_navigation_request()
            && response
              .frame_id()
              .is_some_and(|id| id == &*self.id || (main && id == &*self.page.main_frame().id)) =>
        {
          document = Some(response);
        },
        PageEvent::FrameNavigated(info) if self.is_described_by(&info, main) => {
          if wanted(&info.url) {
            break match document.take() {
              Some(response) => Some(response),
              None => self.late_document_response(&mut events, main).await,
            };
          }
          document = None;
        },
        PageEvent::FrameNavigatedWithinDocument(info) if self.is_described_by(&info, main) && wanted(&info.url) => {
          break None;
        },
        PageEvent::FrameDetached { frame_id } if frame_id == *self.id => {
          return Err(crate::error::FerriError::backend("Navigating frame was detached!"));
        },
        PageEvent::Close => return Err(crate::error::FerriError::target_closed(None)),
        _ => {},
      }
    };
    let state = match opts.wait_until.unwrap_or_default() {
      crate::options::LoadState::Commit => return Ok(response),
      state => state.as_str(),
    };
    let remaining = u64::try_from(
      deadline
        .saturating_duration_since(tokio::time::Instant::now())
        .as_millis(),
    )
    .unwrap_or(u64::MAX);
    self
      .wait_for_load_state_impl(
        Some(state),
        crate::options::WaitForLoadStateOptions {
          timeout: Some(remaining),
        },
      )
      .await?;
    Ok(response)
  }

  /// The response of a document that has committed before its response
  /// event came: the two reach us on separate consumers. A document that
  /// had no response at all (`about:blank`, a `data:` URL) costs the grace.
  async fn late_document_response(
    &self,
    events: &mut crate::events::EventSubscription<crate::events::PageEvent>,
    main: bool,
  ) -> Option<crate::network::Response> {
    let deadline = tokio::time::Instant::now() + crate::network::NAV_REQUEST_GRACE;
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline, events.recv()).await {
      if let crate::events::PageEvent::Response(response) = event
        && response.request().is_navigation_request()
        && response
          .frame_id()
          .is_some_and(|id| id == &*self.id || (main && id == &*self.page.main_frame().id))
      {
        return Some(response);
      }
    }
    None
  }

  /// Playwright: `frame.waitForURL(url: string | RegExp | URLPattern |
  /// ((url: URL) => boolean), options?: { timeout?, waitUntil? })`. Once
  /// this frame's URL matches, `waitUntil` (default `load`) is awaited in
  /// what is left of the same timeout; `commit` returns on the match alone.
  ///
  /// # Errors
  ///
  /// Returns an error if the wait times out.
  #[track_caller]
  pub fn wait_for_url(
    &self,
    matcher: crate::url_matcher::UrlMatcher,
  ) -> crate::action::Action<'static, crate::options::WaitForUrlOptions, ()> {
    let frame = self.clone();
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        frame
          .page
          .traced_as(
            "Frame",
            "waitForURL",
            serde_json::json!({ "url": matcher.describe() }),
            frame.wait_for_url_impl(&matcher, opts),
          )
          .await
      })
    })
  }

  /// Implementation of [`Self::wait_for_url`]. The URL is tracked
  /// locally (no wire cost to read), so the loop parks on the page
  /// event stream and re-checks on each event, with a coarse fallback
  /// tick for anything the stream misses.
  pub(crate) async fn wait_for_url_impl(
    &self,
    matcher: &crate::url_matcher::UrlMatcher,
    opts: crate::options::WaitForUrlOptions,
  ) -> Result<()> {
    let timeout_ms = opts.timeout.unwrap_or_else(|| self.page.default_navigation_timeout());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let mut events = self.page.events().subscribe();
    loop {
      if matcher.matches(&self.url()) {
        break;
      }
      let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
      if remaining.is_zero() {
        return Err(crate::error::FerriError::timeout(
          format!("waiting for URL matching {:?}", matcher.identifier()),
          timeout_ms,
        ));
      }
      let tick = remaining.min(std::time::Duration::from_millis(250));
      tokio::select! {
        ev = events.recv() => {
          if ev.is_none() {
            // Emitter dropped (page closing) — keep ticking until the
            // deadline instead of spinning on a closed channel.
            tokio::time::sleep(tick).await;
          }
        },
        () = tokio::time::sleep(tick) => {},
      }
    }
    let state = match opts.wait_until.unwrap_or_default() {
      crate::options::LoadState::Commit => return Ok(()),
      state => state.as_str(),
    };
    let remaining = u64::try_from(
      deadline
        .saturating_duration_since(tokio::time::Instant::now())
        .as_millis(),
    )
    .unwrap_or(u64::MAX);
    self
      .wait_for_load_state_impl(
        Some(state),
        crate::options::WaitForLoadStateOptions {
          timeout: Some(remaining),
        },
      )
      .await
  }

  /// Playwright: `frame.waitForTimeout(timeout): Promise<void>`.
  pub async fn wait_for_timeout(&self, timeout_ms: u64) {
    self.page.wait_for_timeout(timeout_ms).await;
  }

  // ── Waiting ──────────────────────────────────────────────────────────

  /// Wait for a selector within this frame and return the matched handle.
  ///
  /// Playwright: `frame.waitForSelector(selector, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:217`).
  /// For the default `state: 'visible'` and for `state: 'attached'` the
  /// resolved [`crate::element_handle::ElementHandle`] is returned. For
  /// `state: 'hidden' | 'detached'` the element is gone (or invisible)
  /// so Playwright returns `null`; we mirror that with `Ok(None)`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element does not reach the requested state
  /// within the timeout.
  #[track_caller]
  pub fn wait_for_selector(
    &self,
    selector: &str,
  ) -> crate::action::Action<'static, WaitOptions, Option<crate::element_handle::ElementHandle>> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.wait_for_selector_impl(&selector, opts).await }))
  }

  pub(crate) async fn wait_for_selector_impl(
    &self,
    selector: &str,
    opts: WaitOptions,
  ) -> Result<Option<crate::element_handle::ElementHandle>> {
    let state = opts.state;
    let locator = match opts.strict {
      Some(strict) => self.locator(selector).strict(strict),
      None => self.selector_locator(selector),
    };
    locator.wait_for_impl(opts).await?;
    // Playwright returns a handle only when the element is present; the
    // `hidden` / `detached` states resolve precisely because it is not.
    let returns_handle = !matches!(
      state,
      Some(crate::options::WaitState::Hidden | crate::options::WaitState::Detached)
    );
    if returns_handle {
      Ok(Some(locator.element_handle().await?))
    } else {
      Ok(None)
    }
  }

  // ── Handles ──────────────────────────────────────────────────────────

  /// Playwright: `frame.$(selector, options?: { strict?: boolean }):
  /// Promise<ElementHandle | null>`. The first element in this frame the
  /// selector matches right now, or `None`. Without `strict`, the
  /// context's `strictSelectors` decides whether a second match is an
  /// error.
  ///
  /// # Errors
  ///
  /// Returns an error if the selector does not parse, matches more than
  /// one element under strict mode, or the query fails.
  pub async fn query_selector(
    &self,
    selector: &str,
    strict: Option<bool>,
  ) -> Result<Option<crate::element_handle::ElementHandle>> {
    self
      .page
      .traced_as(
        "Frame",
        "$",
        serde_json::json!({ "selector": selector }),
        self.query_selector_untraced(selector, strict),
      )
      .await
  }

  pub(crate) async fn query_selector_untraced(
    &self,
    selector: &str,
    strict: Option<bool>,
  ) -> Result<Option<crate::element_handle::ElementHandle>> {
    // Parsed here so that a selector that cannot be parsed is an error and
    // not, like one that matches nothing, an absent element.
    crate::selectors::parse(selector)?;
    let locator = match strict {
      Some(strict) => self.locator(selector).strict(strict),
      None => self.selector_locator(selector),
    };
    match locator.element_handle().await {
      Ok(handle) => Ok(Some(handle)),
      Err(crate::error::FerriError::InvalidSelector { reason, .. }) if reason == "no element found" => Ok(None),
      Err(error) => Err(error),
    }
  }

  /// Playwright: `frame.$$(selector): Promise<ElementHandle[]>`. Every
  /// element in this frame the selector matches, in document order.
  ///
  /// # Errors
  ///
  /// Returns an error if the selector does not parse or the query fails.
  pub async fn query_selector_all(&self, selector: &str) -> Result<Vec<crate::element_handle::ElementHandle>> {
    self
      .page
      .traced_as(
        "Frame",
        "$$",
        serde_json::json!({ "selector": selector }),
        self.locator(selector).element_handles(),
      )
      .await
  }

  /// Playwright: `frame.frameElement(): Promise<ElementHandle>`. The
  /// `<iframe>` or `<frame>` in the parent frame that holds this one.
  ///
  /// # Errors
  ///
  /// Returns an error for the main frame, which no element holds, and for
  /// a frame whose element has left its parent's document.
  pub async fn frame_element(&self) -> Result<crate::element_handle::ElementHandle> {
    let detached = || crate::error::FerriError::backend("Frame has been detached.");
    let parent = self.parent_frame().ok_or_else(detached)?;
    let mut owner = None;
    for candidate in parent.locator("iframe, frame").element_handles().await? {
      let holds_this = owner.is_none()
        && candidate
          .content_frame()
          .await?
          .is_some_and(|frame| frame.id == self.id);
      if holds_this {
        owner = Some(candidate);
      } else {
        let _ = candidate.dispose().await;
      }
    }
    owner.ok_or_else(detached)
  }

  /// Show an element highlight for `selector` in this frame, beside any
  /// already shown. Internal API mirroring Playwright's
  /// `frame._highlight(selector, style)`.
  ///
  /// # Errors
  ///
  /// Returns an error if the selector does not parse or JS evaluation fails.
  pub async fn highlight(&self, selector: &str, style: Option<&str>) -> Result<()> {
    self
      .page
      .add_highlight(Arc::clone(&self.id), selector, style.map(str::to_string))
      .await
  }

  /// Stop highlighting `selector` in this frame. Internal API mirroring
  /// Playwright's `frame._hideHighlight(selector)`.
  ///
  /// # Errors
  ///
  /// Returns an error if JS evaluation fails.
  pub async fn hide_highlight(&self, selector: &str) -> Result<()> {
    self.page.remove_highlight(&self.id, selector).await
  }

  /// Whether this frame has been detached from the page. Sync -- reads
  /// the cached `detached` flag maintained by the page's frame event
  /// listener. Playwright:
  /// [`frame.isDetached()`](https://playwright.dev/docs/api/class-frame#frame-is-detached).
  #[must_use]
  pub fn is_detached(&self) -> bool {
    self
      .page
      .with_frame_cache(|c| c.record(&self.id).is_none_or(|r| r.detached))
  }

  /// Get the page this frame belongs to.
  #[must_use]
  pub fn page(&self) -> &Page {
    &self.page
  }

  /// Reference to the owning `Arc<Page>`. Locators hold a `Frame` and
  /// reach the backend through `frame.page_arc()`.
  #[must_use]
  pub fn page_arc(&self) -> &Arc<Page> {
    &self.page
  }

  /// Backend frame id (CDP/BiDi). Stable through navigations; used to
  /// scope evaluation to this frame's execution context.
  #[must_use]
  pub fn id(&self) -> &Arc<str> {
    &self.id
  }

  /// Playwright: `frame.setContent(html, options?: { timeout?,
  /// waitUntil? }): Promise<void>`. The document is rewritten with
  /// `document.open()` and `document.write()`, so its scripts run, and the
  /// call returns once it has reached `waitUntil` (default `load`).
  ///
  /// # Errors
  ///
  /// Returns an error if the content cannot be written or the document
  /// does not reach `waitUntil` in time.
  #[track_caller]
  pub fn set_content(&self, html: &str) -> crate::action::Action<'static, crate::options::SetContentOptions, ()> {
    let frame = self.clone();
    let html = html.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        frame
          .page
          .traced_as(
            "Frame",
            "setContent",
            serde_json::json!({ "html": html }),
            frame.set_content_impl(&html, opts),
          )
          .await
      })
    })
  }

  pub(crate) async fn set_content_impl(&self, html: &str, opts: crate::options::SetContentOptions) -> Result<()> {
    let timeout_ms = opts.timeout.unwrap_or_else(|| self.page.default_navigation_timeout());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let wait_until = opts.wait_until.unwrap_or_default();
    let state = if self.is_main_frame() {
      self.page.inner.set_content(html).await?;
      match wait_until {
        crate::options::LoadState::Commit => None,
        state => Some(state.as_str()),
      }
    } else {
      // Chromium reports `complete` for a document rewritten this way while
      // its images are still loading, though its `load` event still waits
      // for them. A listener added before `document.open()` is removed by
      // it, and with nothing to load the event fires inside
      // `document.close()`, so the listener goes on between the two. The
      // event leaves a mark that is read back.
      let load = matches!(
        wait_until,
        crate::options::LoadState::Load | crate::options::LoadState::NetworkIdle
      );
      self
        .evaluate_untraced(
          "({ html, mark }) => { \
             delete window[mark]; \
             document.open(); document.write(html); \
             window.addEventListener('load', () => { window[mark] = true; }, { once: true }); \
             document.close(); \
           }",
          crate::protocol::argument_from_serde(&serde_json::json!({ "html": html, "mark": SET_CONTENT_MARK }))?,
          Some(true),
        )
        .await?;
      if load {
        loop {
          // Read in the world the mark was set in.
          let loaded = self
            .evaluate_untraced(
              "mark => { const loaded = window[mark] === true; if (loaded) delete window[mark]; return loaded; }",
              crate::protocol::argument_from_serde(SET_CONTENT_MARK)?,
              Some(true),
            )
            .await?;
          if matches!(loaded, crate::protocol::SerializedValue::Bool(true)) {
            break;
          }
          if tokio::time::Instant::now() >= deadline {
            return Err(crate::error::FerriError::timeout("setting frame content", timeout_ms));
          }
          tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
      }
      (wait_until == crate::options::LoadState::NetworkIdle).then_some("networkidle")
    };
    if let Some(state) = state {
      let remaining = u64::try_from(
        deadline
          .saturating_duration_since(tokio::time::Instant::now())
          .as_millis(),
      )
      .unwrap_or(u64::MAX);
      self
        .wait_for_load_state_impl(
          Some(state),
          crate::options::WaitForLoadStateOptions {
            timeout: Some(remaining),
          },
        )
        .await?;
    }
    // The new document's subframes are attached by now. The listener
    // can miss the ones of a page that never navigated, whose main frame
    // no event has introduced, so the tree is read back.
    if let Ok(infos) = self.page.inner.get_frame_tree().await {
      self.page.reseed_frames(infos);
    }
    Ok(())
  }

  /// Playwright: `frame.addScriptTag(options?: { content?, path?, type?,
  /// url? }): Promise<ElementHandle>`. Adds a `<script>` to this frame's
  /// `<head>` and returns it once the script has loaded or run.
  ///
  /// # Errors
  ///
  /// Returns an error if none of `url`, `path` and `content` is given,
  /// `path` cannot be read, or the script fails to load.
  pub async fn add_script_tag(
    &self,
    options: crate::options::AddScriptTagOptions,
  ) -> Result<crate::element_handle::ElementHandle> {
    self
      .page
      .traced_as(
        "Frame",
        "addScriptTag",
        serde_json::json!({ "url": options.url }),
        self.add_script_tag_untraced(options),
      )
      .await
  }

  async fn add_script_tag_untraced(
    &self,
    options: crate::options::AddScriptTagOptions,
  ) -> Result<crate::element_handle::ElementHandle> {
    const ADD_URL: &str = "async params => {        const script = document.createElement('script');        script.src = params.url;        if (params.type) script.type = params.type;        const loaded = new Promise((resolve, reject) => {          script.onload = resolve;          script.onerror = e => reject(typeof e === 'string' ? new Error(e) : new Error(`Failed to load script at ${script.src}`));        });        document.head.appendChild(script);        await loaded;        return script;      }";
    const ADD_CONTENT: &str = "params => {        const script = document.createElement('script');        script.type = params.type || 'text/javascript';        script.text = params.content;        let error = null;        script.onerror = e => error = e;        document.head.appendChild(script);        if (error) throw error;        return script;      }";
    let script_type = options.script_type.unwrap_or_default();
    let (source, params) = if let Some(url) = options.url {
      (ADD_URL, serde_json::json!({ "url": url, "type": script_type }))
    } else {
      let content = match (options.path, options.content) {
        (Some(path), _) => {
          let content = tokio::fs::read_to_string(&path).await?;
          format!(
            "{content}\n//# sourceURL={}",
            path.display().to_string().replace('\n', "")
          )
        },
        (None, Some(content)) => content,
        (None, None) => return Err(missing_tag_source()),
      };
      (
        ADD_CONTENT,
        serde_json::json!({ "content": content, "type": script_type }),
      )
    };
    self.add_tag(source, &params).await
  }

  /// Playwright: `frame.addStyleTag(options?: { content?, path?, url? }):
  /// Promise<ElementHandle>`. Adds a `<link rel="stylesheet">` for `url`,
  /// or a `<style>` otherwise, to this frame's `<head>` and returns it
  /// once the stylesheet has loaded.
  ///
  /// # Errors
  ///
  /// Returns an error if none of `url`, `path` and `content` is given,
  /// `path` cannot be read, or the stylesheet fails to load.
  pub async fn add_style_tag(
    &self,
    options: crate::options::AddStyleTagOptions,
  ) -> Result<crate::element_handle::ElementHandle> {
    self
      .page
      .traced_as(
        "Frame",
        "addStyleTag",
        serde_json::json!({ "url": options.url }),
        self.add_style_tag_untraced(options),
      )
      .await
  }

  async fn add_style_tag_untraced(
    &self,
    options: crate::options::AddStyleTagOptions,
  ) -> Result<crate::element_handle::ElementHandle> {
    const ADD_URL: &str = "async params => {        const link = document.createElement('link');        link.rel = 'stylesheet';        link.href = params.url;        const loaded = new Promise((resolve, reject) => {          link.onload = resolve;          link.onerror = () => reject(new Error(`Failed to load stylesheet at ${link.href}`));        });        document.head.appendChild(link);        await loaded;        return link;      }";
    const ADD_CONTENT: &str = "async params => {        const style = document.createElement('style');        style.type = 'text/css';        style.appendChild(document.createTextNode(params.content));        const loaded = new Promise((resolve, reject) => {          style.onload = resolve;          style.onerror = () => reject(new Error('Failed to load the stylesheet'));        });        document.head.appendChild(style);        await loaded;        return style;      }";
    let (source, params) = if let Some(url) = options.url {
      (ADD_URL, serde_json::json!({ "url": url }))
    } else {
      let content = match (options.path, options.content) {
        (Some(path), _) => {
          let content = tokio::fs::read_to_string(&path).await?;
          format!(
            "{content}/*# sourceURL={}*/",
            path.display().to_string().replace('\n', "")
          )
        },
        (None, Some(content)) => content,
        (None, None) => return Err(missing_tag_source()),
      };
      (ADD_CONTENT, serde_json::json!({ "content": content }))
    };
    self.add_tag(source, &params).await
  }

  /// Run `source`, which adds an element and returns it, and hand that
  /// element back.
  async fn add_tag(&self, source: &str, params: &serde_json::Value) -> Result<crate::element_handle::ElementHandle> {
    let handle = self
      .evaluate_handle_untraced(source, crate::protocol::argument_from_serde(params)?, Some(true))
      .await?;
    handle
      .as_element()
      .ok_or_else(|| crate::error::FerriError::evaluation("the added tag did not come back as an element"))
  }

  /// Playwright: `frame.waitForLoadState(state?: 'load' | 'domcontentloaded'
  /// | 'networkidle', options?: { timeout? }): Promise<void>`
  /// (`packages/playwright-core/src/client/frame.ts`). The state is this
  /// frame's document's own, so a child frame that is still loading holds
  /// the wait while its parent has long finished.
  ///
  /// # Errors
  ///
  /// Returns an error if the wait times out before the load state is reached.
  #[track_caller]
  pub fn wait_for_load_state(
    &self,
    state: Option<&str>,
  ) -> crate::action::Action<'static, crate::options::WaitForLoadStateOptions, ()> {
    let frame = self.clone();
    let state = state.map(str::to_string);
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        frame
          .page
          .traced_as(
            "Frame",
            "waitForLoadState",
            serde_json::json!({ "state": state.clone().unwrap_or_else(|| "load".to_string()) }),
            frame.wait_for_load_state_impl(state.as_deref(), opts),
          )
          .await
      })
    })
  }

  /// Implementation of [`Self::wait_for_load_state`]. One in-page
  /// promise per attempt instead of a protocol-eval poll loop: the page
  /// resolves the promise when the state is reached, so the wire
  /// carries a single round trip in the common case. An attempt dies
  /// with its document on cross-document navigation — the loop re-arms
  /// in the new document. Each attempt is also capped so a reply lost
  /// to a target swap (`WebKit`) re-arms instead of hanging forever.
  pub(crate) async fn wait_for_load_state_impl(
    &self,
    state: Option<&str>,
    opts: crate::options::WaitForLoadStateOptions,
  ) -> Result<()> {
    const JS_WAIT_LOAD: &str = "() => document.readyState === 'complete' \
       ? true \
       : new Promise(function(resolve) { window.addEventListener('load', function() { resolve(true); }, { once: true }); })";
    const JS_WAIT_DOMCONTENTLOADED: &str = "() => (document.readyState === 'interactive' || document.readyState === 'complete') \
       ? true \
       : new Promise(function(resolve) { document.addEventListener('DOMContentLoaded', function() { resolve(true); }, { once: true }); })";
    // Same signal the old Rust-side poll used (resource timing entries,
    // 500ms quiet window, 100ms sampling) — but sampled in-page so the
    // whole wait is one round trip.
    const JS_WAIT_NETWORKIDLE: &str = "() => new Promise(function(resolve) { \
       var idleSince = performance.now(); \
       function pending() { \
         if (window.__fd_inflight > 0) return true; \
         var rs = performance.getEntriesByType('resource'); \
         var now = performance.now(); \
         for (var i = 0; i < rs.length; i++) { \
           if (rs[i].responseEnd === 0 || now - rs[i].responseEnd < 100) return true; \
         } \
         return false; \
       } \
       (function check() { \
         var now = performance.now(); \
         if (pending()) idleSince = now; \
         if (now - idleSince >= 500) return resolve(true); \
         setTimeout(check, 100); \
       })(); \
     })";

    let state = state.unwrap_or("load");
    let timeout_ms = opts.timeout.unwrap_or_else(|| self.page.default_timeout());

    // The state check precedes the deadline, matching the old poll loop
    // and Playwright's local lifecycle bookkeeping: an already-reached
    // state passes regardless of how small the timeout is. Without this,
    // the promise attempt's own round trip would eat a tiny budget and
    // report a false timeout on slower transports.
    if state != "networkidle"
      && let Ok(Some(v)) = self.backend_eval_expr("document.readyState").await
    {
      let s = v.as_str().unwrap_or("loading");
      let reached = match state {
        "domcontentloaded" => s == "interactive" || s == "complete",
        _ => s == "complete",
      };
      if reached {
        return Ok(());
      }
    }

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);

    let (js, timeout_label) = match state {
      "domcontentloaded" => (JS_WAIT_DOMCONTENTLOADED, "waiting for domcontentloaded"),
      "networkidle" => (JS_WAIT_NETWORKIDLE, "waiting for networkidle"),
      _ => (JS_WAIT_LOAD, "waiting for load state"),
    };

    loop {
      let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
      if remaining.is_zero() {
        return Err(crate::error::FerriError::timeout(timeout_label, timeout_ms));
      }
      let attempt_cap = remaining.min(std::time::Duration::from_secs(5));
      match tokio::time::timeout(attempt_cap, self.eval::<bool>(js)).await {
        Ok(Ok(true)) => return Ok(()),
        Ok(Ok(false) | Err(_)) => {
          // Transient evaluate failure (document destroyed mid-wait);
          // brief pause, then re-arm in whatever document is live now.
          tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        },
        Err(_) => {}, // attempt cap hit — re-arm
      }
    }
  }

  // ── Action methods (Playwright parity — task 3.9) ──────────────────────
  //
  // Mirrors Playwright's frame action surface from
  // `/tmp/playwright/packages/playwright-core/src/client/frame.ts:296-447`.
  // Each method delegates to `self.selector_locator(selector).<action>()` —
  // Frame's locator already scopes by `frame_id`, so the action runs in
  // the iframe's execution context (CDP) or against the synthesized
  // iframe (WebKit). Option bags are intentionally minimal here; they
  // ride on top of the existing Locator surface and pick up extensions
  // (timeout/force/etc.) when those land on Locator itself.

  // -- Mouse / pointer ---------------------------------------------------

  /// Click the element matched by `selector`. Accepts Playwright's full
  /// `FrameClickOptions` bag (see [`crate::options::ClickOptions`]).
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the click fails.
  #[track_caller]
  pub fn click(&self, selector: &str) -> crate::action::Action<'static, crate::options::ClickOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.click_impl(&selector, Some(opts)).await }))
  }

  pub(crate) async fn click_impl(&self, selector: &str, opts: Option<crate::options::ClickOptions>) -> Result<()> {
    self.selector_locator(selector).click_impl(opts).await
  }

  /// Double-click the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the dblclick fails.
  #[track_caller]
  pub fn dblclick(&self, selector: &str) -> crate::action::Action<'static, crate::options::DblClickOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.dblclick_impl(&selector, Some(opts)).await }))
  }

  pub(crate) async fn dblclick_impl(
    &self,
    selector: &str,
    opts: Option<crate::options::DblClickOptions>,
  ) -> Result<()> {
    self.selector_locator(selector).dblclick_impl(opts).await
  }

  /// Hover the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the hover fails.
  #[track_caller]
  pub fn hover(&self, selector: &str) -> crate::action::Action<'static, crate::options::HoverOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.hover_impl(&selector, Some(opts)).await }))
  }

  pub(crate) async fn hover_impl(&self, selector: &str, opts: Option<crate::options::HoverOptions>) -> Result<()> {
    self.selector_locator(selector).hover_impl(opts).await
  }

  /// Tap (touch) the element matched by `selector`. Mirrors
  /// `frame.tap(selector, options?)` per
  /// `/tmp/playwright/packages/playwright-core/src/client/frame.ts:308`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the tap fails.
  #[track_caller]
  pub fn tap(&self, selector: &str) -> crate::action::Action<'static, crate::options::TapOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.tap_impl(&selector, Some(opts)).await }))
  }

  pub(crate) async fn tap_impl(&self, selector: &str, opts: Option<crate::options::TapOptions>) -> Result<()> {
    self.selector_locator(selector).tap_impl(opts).await
  }

  /// Focus the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or focus fails.
  pub async fn focus(&self, selector: &str) -> Result<()> {
    self.selector_locator(selector).focus().await
  }

  // -- Form input --------------------------------------------------------

  /// Fill an input matching `selector` with `value`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not fillable.
  #[track_caller]
  pub fn fill(&self, selector: &str, value: &str) -> crate::action::Action<'static, crate::options::FillOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    let value = value.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { frame.fill_impl(&selector, &value, Some(opts)).await })
    })
  }

  pub(crate) async fn fill_impl(
    &self,
    selector: &str,
    value: &str,
    opts: Option<crate::options::FillOptions>,
  ) -> Result<()> {
    self.selector_locator(selector).fill_impl(value, opts).await
  }

  /// Type characters into an element matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or typing fails.
  #[track_caller]
  pub fn r#type(&self, selector: &str, text: &str) -> crate::action::Action<'static, crate::options::TypeOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    let text = text.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.type_impl(&selector, &text, Some(opts)).await }))
  }

  pub(crate) async fn type_impl(
    &self,
    selector: &str,
    text: &str,
    opts: Option<crate::options::TypeOptions>,
  ) -> Result<()> {
    self.selector_locator(selector).type_impl(text, opts).await
  }

  /// Press a key on an element matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the key press fails.
  #[track_caller]
  pub fn press(&self, selector: &str, key: &str) -> crate::action::Action<'static, crate::options::PressOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    let key = key.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.press_impl(&selector, &key, Some(opts)).await }))
  }

  pub(crate) async fn press_impl(
    &self,
    selector: &str,
    key: &str,
    opts: Option<crate::options::PressOptions>,
  ) -> Result<()> {
    self.selector_locator(selector).press_impl(key, opts).await
  }

  /// Check a checkbox/radio matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not checkable.
  #[track_caller]
  pub fn check(&self, selector: &str) -> crate::action::Action<'static, crate::options::CheckOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.check_impl(&selector, Some(opts)).await }))
  }

  pub(crate) async fn check_impl(&self, selector: &str, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    self.selector_locator(selector).check_impl(opts).await
  }

  /// Uncheck a checkbox matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not uncheckable.
  #[track_caller]
  pub fn uncheck(&self, selector: &str) -> crate::action::Action<'static, crate::options::CheckOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.uncheck_impl(&selector, Some(opts)).await }))
  }

  pub(crate) async fn uncheck_impl(&self, selector: &str, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    self.selector_locator(selector).uncheck_impl(opts).await
  }

  /// Set the checked state of a checkbox/radio matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not checkable.
  #[track_caller]
  pub fn set_checked(
    &self,
    selector: &str,
    checked: bool,
  ) -> crate::action::Action<'static, crate::options::CheckOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { frame.set_checked_impl(&selector, checked, Some(opts)).await })
    })
  }

  pub(crate) async fn set_checked_impl(
    &self,
    selector: &str,
    checked: bool,
    opts: Option<crate::options::CheckOptions>,
  ) -> Result<()> {
    self.selector_locator(selector).set_checked_impl(checked, opts).await
  }

  /// Select a `<select>` option in the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the option cannot
  /// be selected.
  #[track_caller]
  pub fn select_option(
    &self,
    selector: &str,
    values: impl Into<crate::options::SelectOptionValues>,
  ) -> crate::action::Action<'static, crate::options::SelectOptionOptions, Vec<String>> {
    let values = values.into().0;
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { frame.select_option_impl(&selector, values, Some(opts)).await })
    })
  }

  pub(crate) async fn select_option_impl(
    &self,
    selector: &str,
    values: Vec<crate::options::SelectOptionValue>,
    opts: Option<crate::options::SelectOptionOptions>,
  ) -> Result<Vec<String>> {
    self.selector_locator(selector).select_option_impl(values, opts).await
  }

  /// Set input files on a `<input type=file>` matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or file setting fails.
  #[track_caller]
  pub fn set_input_files(
    &self,
    selector: &str,
    files: impl Into<crate::options::InputFiles>,
  ) -> crate::action::Action<'static, crate::options::SetInputFilesOptions, ()> {
    let files = files.into();
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { frame.set_input_files_impl(&selector, files, Some(opts)).await })
    })
  }

  pub(crate) async fn set_input_files_impl(
    &self,
    selector: &str,
    files: crate::options::InputFiles,
    opts: Option<crate::options::SetInputFilesOptions>,
  ) -> Result<()> {
    self.selector_locator(selector).set_input_files_impl(files, opts).await
  }

  // -- Drag and drop -----------------------------------------------------

  /// Drag from `source` to `target` selectors within this frame. Mirrors
  /// `frame.dragAndDrop(source, target, options?)` per
  /// `/tmp/playwright/packages/playwright-core/src/client/frame.ts:304`.
  ///
  /// # Errors
  ///
  /// Returns an error if either element cannot be found or the
  /// drag-and-drop operation fails.
  #[track_caller]
  pub fn drag_and_drop(
    &self,
    source: &str,
    target: &str,
  ) -> crate::action::Action<'static, crate::options::DragAndDropOptions, ()> {
    let frame = self.clone();
    let source = source.to_string();
    let target = target.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { Box::pin(frame.drag_and_drop_impl(&source, &target, Some(opts))).await })
    })
  }

  pub(crate) async fn drag_and_drop_impl(
    &self,
    source: &str,
    target: &str,
    options: Option<crate::options::DragAndDropOptions>,
  ) -> Result<()> {
    let opts = options.unwrap_or_default();
    let src = self.selector_locator(source);
    let tgt = self.selector_locator(target);
    let (src, tgt) = match opts.strict {
      Some(s) => (src.strict(s), tgt.strict(s)),
      None => (src, tgt),
    };
    src.drag_to_impl(&tgt, Some(opts)).await
  }

  // -- Synthetic events --------------------------------------------------

  /// Dispatch a DOM event on the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the dispatch fails.
  #[track_caller]
  pub fn dispatch_event(
    &self,
    selector: &str,
    event_type: &str,
    event_init: Option<serde_json::Value>,
  ) -> crate::action::Action<'static, crate::options::DispatchEventOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    let event_type = event_type.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        frame
          .dispatch_event_impl(&selector, &event_type, event_init, Some(opts))
          .await
      })
    })
  }

  pub(crate) async fn dispatch_event_impl(
    &self,
    selector: &str,
    event_type: &str,
    event_init: Option<serde_json::Value>,
    opts: Option<crate::options::DispatchEventOptions>,
  ) -> Result<()> {
    self
      .locator(selector)
      .dispatch_event_impl(event_type, event_init, opts)
      .await
  }

  // -- Content / attribute reads ----------------------------------------

  /// Get the text content of the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn text_content(&self, selector: &str) -> Result<Option<String>> {
    self.selector_locator(selector).text_content().await
  }

  /// Get `innerText` of the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn inner_text(&self, selector: &str) -> Result<String> {
    self.selector_locator(selector).inner_text().await
  }

  /// Get `innerHTML` of the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn inner_html(&self, selector: &str) -> Result<String> {
    self.selector_locator(selector).inner_html().await
  }

  /// Get an attribute on the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn get_attribute(&self, selector: &str, name: &str) -> Result<Option<String>> {
    self.selector_locator(selector).get_attribute(name).await
  }

  /// Get `value` from a form control matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn input_value(&self, selector: &str) -> Result<String> {
    self.selector_locator(selector).input_value().await
  }

  // -- State checks ------------------------------------------------------

  /// True if the element matched by `selector` is visible.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_visible(&self, selector: &str) -> Result<bool> {
    self.selector_locator(selector).is_visible().await
  }

  /// True if the element matched by `selector` is hidden.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_hidden(&self, selector: &str) -> Result<bool> {
    self.selector_locator(selector).is_hidden().await
  }

  /// True if the element matched by `selector` is enabled.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_enabled(&self, selector: &str) -> Result<bool> {
    self.selector_locator(selector).is_enabled().await
  }

  /// True if the element matched by `selector` is disabled.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_disabled(&self, selector: &str) -> Result<bool> {
    self.selector_locator(selector).is_disabled().await
  }

  /// True if the element matched by `selector` is editable.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_editable(&self, selector: &str) -> Result<bool> {
    self.selector_locator(selector).is_editable().await
  }

  /// True if a checkbox/radio matched by `selector` is checked.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_checked(&self, selector: &str) -> Result<bool> {
    self.selector_locator(selector).is_checked().await
  }
}

impl std::fmt::Debug for Frame {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let (name, url, main) = self.page.with_frame_cache(|c| {
      let rec = c.record(&self.id);
      (
        rec.map(|r| r.info.name.clone()),
        rec.map(|r| r.info.url.clone()),
        c.main_frame_id().as_deref() == Some(&*self.id),
      )
    });
    f.debug_struct("Frame")
      .field("id", &self.id)
      .field("name", &name)
      .field("url", &url)
      .field("main", &main)
      .finish_non_exhaustive()
  }
}

/// The window property a child frame's `load` sets after `setContent`.
const SET_CONTENT_MARK: &str = "__fdSetContentLoaded";

fn missing_tag_source() -> crate::error::FerriError {
  crate::error::FerriError::invalid_argument(
    "options",
    "Provide an object with a `url`, `path` or `content` property",
  )
}
