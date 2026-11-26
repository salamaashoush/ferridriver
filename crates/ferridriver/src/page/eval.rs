//! Page locators, handle materialisation (`$` / `$$`), and JavaScript evaluation.

use super::same_document_url;
use std::sync::Arc;

use crate::error::Result;
use crate::locator::Locator;
use crate::options::{RoleOptions, TextOptions};

use super::Page;

impl Page {
  // ── Locators (delegate to mainFrame — Playwright parity) ───────────
  //
  // `Page` is a facade over `mainFrame` for ergonomics. Mirrors
  // `/tmp/playwright/packages/playwright-core/src/client/page.ts:307+`,
  // where every locator-construction and action method does
  // `this._mainFrame.<method>(...)`. The Frame is the unit of execution
  // context; Page never constructs Locators directly.

  #[must_use]
  pub fn locator(self: &Arc<Self>, selector: &str) -> Locator {
    self.main_frame().locator(selector)
  }

  /// [`Self::locator`] with Playwright's `LocatorOptions` filter bag.
  #[must_use]
  pub fn locator_with(self: &Arc<Self>, selector: &str, options: &crate::options::FilterOptions) -> Locator {
    self.main_frame().locator_with(selector, options)
  }

  /// Internal accessor for the locator-handler registry. Consumed by the
  /// actionability checkpoint and by the public add/remove methods below.
  pub(crate) fn locator_handlers(&self) -> &crate::locator_handler::LocatorHandlerRegistry {
    &self.locator_handlers
  }

  /// Register a handler that runs when `locator` becomes visible during an
  /// actionability wait. Mirrors Playwright
  /// `page.addLocatorHandler(locator, handler, options?: { times?, noWaitAfter? })`
  /// (`/tmp/playwright/packages/playwright-core/src/client/page.ts:397`).
  ///
  /// The handler must belong to the main frame of this page. `times: Some(0)`
  /// registers nothing. When `times` runs out the handler auto-removes.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError`] if `locator` is not bound to this
  /// page's main frame.
  pub fn add_locator_handler(
    self: &Arc<Self>,
    locator: &Locator,
    handler: crate::locator_handler::LocatorHandlerFn,
    times: Option<u32>,
    no_wait_after: bool,
  ) -> Result<()> {
    if !locator.frame.is_main_frame() {
      return Err(crate::error::FerriError::protocol(
        "addLocatorHandler",
        "Locator must belong to the main frame of this page",
      ));
    }
    self
      .locator_handlers
      .register(locator.selector().to_string(), handler, times, no_wait_after);
    Ok(())
  }

  /// Remove all handlers registered for `locator`. Mirrors Playwright
  /// `page.removeLocatorHandler(locator)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/page.ts:423`).
  pub fn remove_locator_handler(self: &Arc<Self>, locator: &Locator) {
    self.locator_handlers.remove_by_selector(locator.selector());
  }

  #[must_use]
  pub fn get_by_role(
    self: &Arc<Self>,
    role: impl Into<crate::options::Role>,
  ) -> crate::locator_builder::LocatorBuilder<RoleOptions> {
    self.main_frame().get_by_role(role)
  }

  #[must_use]
  pub fn get_by_text(
    self: &Arc<Self>,
    text: impl Into<crate::options::StringOrRegex>,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.main_frame().get_by_text(text)
  }

  #[must_use]
  pub fn get_by_label(
    self: &Arc<Self>,
    text: impl Into<crate::options::StringOrRegex>,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.main_frame().get_by_label(text)
  }

  #[must_use]
  pub fn get_by_placeholder(
    self: &Arc<Self>,
    text: impl Into<crate::options::StringOrRegex>,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.main_frame().get_by_placeholder(text)
  }

  #[must_use]
  pub fn get_by_alt_text(
    self: &Arc<Self>,
    text: impl Into<crate::options::StringOrRegex>,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.main_frame().get_by_alt_text(text)
  }

  #[must_use]
  pub fn get_by_title(
    self: &Arc<Self>,
    text: impl Into<crate::options::StringOrRegex>,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.main_frame().get_by_title(text)
  }

  #[must_use]
  pub fn get_by_test_id(self: &Arc<Self>, test_id: impl Into<crate::options::StringOrRegex>) -> Locator {
    self.main_frame().get_by_test_id(test_id)
  }

  /// Playwright: `page.frameLocator(selector?: string): FrameLocator`,
  /// [`crate::frame::Frame::frame_locator`] on the main frame.
  #[must_use]
  pub fn frame_locator<'a>(self: &Arc<Self>, selector: impl Into<Option<&'a str>>) -> crate::locator::FrameLocator {
    self.main_frame().frame_locator(selector)
  }

  /// Playwright: `page.getByRef(ref: string): Locator` — the element an
  /// AI-mode aria snapshot labelled `[ref=<ref>]`.
  #[must_use]
  pub fn get_by_ref(self: &Arc<Self>, aria_ref: &str) -> Locator {
    self.locator(&format!("aria-ref={aria_ref}"))
  }

  // ── Handle materialisation (Playwright `page.$` / `page.$$`) ─────

  /// Resolve the selector once and return a lifecycle
  /// [`crate::element_handle::ElementHandle`] — or `None` when the
  /// selector matches no element. Playwright: `page.$(selector,
  /// options?: { strict?: boolean })`
  /// (`packages/playwright-core/src/client/page.ts`); without `strict`
  /// the context's `strictSelectors` decides.
  ///
  /// Unlike [`Self::locator`] (lazy, re-resolves on every action), the
  /// returned handle is pinned to the element resolved at call time.
  /// Subsequent DOM mutations that remove the element won't invalidate
  /// the handle itself — actions against a detached element surface a
  /// backend-specific error — but the handle's lifecycle is decoupled
  /// from the page's frame cache. Callers release it via
  /// [`crate::element_handle::ElementHandle::dispose`] or let it
  /// drop when the page closes.
  ///
  /// # Errors
  ///
  /// Returns an error if the selector does not parse, matches more than
  /// one element under strict mode, or the query fails. A selector that
  /// does not match any element returns `Ok(None)`.
  pub async fn query_selector(
    self: &Arc<Self>,
    selector: &str,
    strict: Option<bool>,
  ) -> Result<Option<crate::element_handle::ElementHandle>> {
    self
      .traced(
        "$",
        serde_json::json!({ "selector": selector }),
        self.main_frame().query_selector_untraced(selector, strict),
      )
      .await
  }

  /// Playwright: `page.querySelectorAll(selector): Promise<ElementHandle[]>`.
  /// Returns one [`crate::element_handle::ElementHandle`] per match in
  /// document order. Each element is pinned individually — disposing
  /// one does not affect the others.
  ///
  /// Implementation uses the selector engine's `query_all` which
  /// tags every match with `data-fd-sel='<i>'`; we then evaluate a
  /// lookup by tag for each index and wrap the result. Tags are
  /// cleaned up on completion.
  ///
  /// # Errors
  ///
  /// Returns an error on selector parse failure, protocol error, or
  /// if a match cannot be resolved (e.g. the DOM changed mid-iteration).
  pub async fn query_selector_all(
    self: &Arc<Self>,
    selector: &str,
  ) -> Result<Vec<crate::element_handle::ElementHandle>> {
    let matches = crate::selectors::query_all(&self.inner, selector, None).await?;
    let count = matches.len();
    let mut handles = Vec::with_capacity(count);
    for i in 0..count {
      let tagged = format!("window.__fd.selOne([{{\"engine\":\"css\",\"body\":\"[data-fd-sel='{i}']\"}}])");
      match self.inner.evaluate_to_element(&tagged, None).await {
        Ok(element) => {
          handles.push(crate::element_handle::ElementHandle::from_any_element(Arc::clone(self), element).await?);
        },
        Err(err) => {
          crate::selectors::cleanup_tags(&self.inner).await;
          return Err(err);
        },
      }
    }
    crate::selectors::cleanup_tags(&self.inner).await;
    Ok(handles)
  }

  /// Playwright: `page.$eval(selector, pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/page.ts:342`).
  /// Delegates to the main frame, same as Playwright's `this._mainFrame.$eval(...)`.
  ///
  /// # Errors
  ///
  /// See [`crate::frame::Frame::eval_on_selector`].
  pub async fn eval_on_selector(
    self: &Arc<Self>,
    selector: &str,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<crate::protocol::SerializedValue> {
    self
      .traced(
        "$eval",
        serde_json::json!({ "selector": selector, "expression": fn_source }),
        self
          .main_frame()
          .eval_on_selector(selector, fn_source, arg, is_function),
      )
      .await
  }

  /// Playwright: `page.$$eval(selector, pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/page.ts:347`).
  /// Delegates to the main frame, same as Playwright's `this._mainFrame.$$eval(...)`.
  ///
  /// # Errors
  ///
  /// See [`crate::frame::Frame::eval_on_selector_all`].
  pub async fn eval_on_selector_all(
    self: &Arc<Self>,
    selector: &str,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<crate::protocol::SerializedValue> {
    self
      .traced(
        "$$eval",
        serde_json::json!({ "selector": selector, "expression": fn_source }),
        self
          .main_frame()
          .eval_on_selector_all(selector, fn_source, arg, is_function),
      )
      .await
  }

  /// Register the callbacks behind the functions in an `evaluate`
  /// argument. Playwright erases them when the main frame commits a new
  /// document; here the next registration removes those a navigation
  /// retired.
  pub(crate) async fn expose_arg_functions(&self, functions: &[crate::protocol::ArgFunction]) -> Result<()> {
    if functions.is_empty() {
      return Ok(());
    }
    let retired = match self.inner.observed().lock() {
      Ok(mut observed) => observed.track_evaluate_callbacks(functions.iter().map(|f| f.name.clone())),
      Err(poisoned) => poisoned
        .into_inner()
        .track_evaluate_callbacks(functions.iter().map(|f| f.name.clone())),
    };
    for name in retired {
      self.inner.remove_callback(&name).await;
    }
    for function in functions {
      self
        .inner
        .expose_callback(&function.name, Arc::clone(&function.binding))
        .await?;
    }
    Ok(())
  }

  // ── evaluate (Playwright parity) ─────────────────────────────────────

  /// Playwright: `page.evaluate(pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/page.ts:515`).
  /// Delegates to the main frame, same as Playwright's `this._mainFrame.evaluate(...)`.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::error::FerriError`] on page-side exception or
  /// protocol failure.
  pub async fn evaluate(
    self: &Arc<Self>,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<crate::protocol::SerializedValue> {
    self
      .traced(
        "evaluate",
        serde_json::json!({ "expression": fn_source }),
        self.main_frame().evaluate(fn_source, arg.clone(), is_function),
      )
      .await
  }

  /// Typed evaluate: run `fn_source` in the page and deserialize the
  /// result via serde. Ergonomic wrapper over the wire-level
  /// [`Self::evaluate`] for JSON-shaped values:
  ///
  /// ```ignore
  /// let title: String = page.eval("() => document.title").await?;
  /// let ok: bool = page.eval_with("sel => !!document.querySelector(sel)", &"#app").await?;
  /// ```
  ///
  /// # Errors
  ///
  /// See [`Self::evaluate`], plus [`crate::error::FerriError::Json`] /
  /// [`crate::error::FerriError::Evaluation`] when the result does not
  /// decode into `T` (rich JS values need [`Self::evaluate_handle`]).
  pub async fn eval<T: serde::de::DeserializeOwned>(self: &Arc<Self>, fn_source: &str) -> Result<T> {
    self.main_frame().eval(fn_source).await
  }

  /// [`Self::eval`] with a serde-serialized argument, passed to the page
  /// function as its single parameter.
  ///
  /// # Errors
  ///
  /// See [`Self::eval`].
  pub async fn eval_with<T: serde::de::DeserializeOwned>(
    self: &Arc<Self>,
    fn_source: &str,
    arg: &(impl serde::Serialize + ?Sized),
  ) -> Result<T> {
    self.main_frame().eval_with(fn_source, arg).await
  }

  /// Audit the page for accessibility problems with axe-core.
  ///
  /// Covers what Lighthouse's whole accessibility category covers,
  /// because that category is 67 wrappers around this same engine.
  ///
  /// axe-core is fetched by `ferridriver install axe` rather than
  /// bundled, so the first call on a machine without it fails with a
  /// [`crate::error::FerriError::Unsupported`] saying so.
  ///
  /// # Errors
  ///
  /// [`crate::error::FerriError::Unsupported`] when axe-core is not
  /// installed, and [`crate::error::FerriError::Backend`] when the page
  /// could not run it.
  ///
  /// Options chain as setters: `.include(["main"])`, `.tags(["wcag2a"])`.
  #[track_caller]
  pub fn check_accessibility(
    self: &Arc<Self>,
  ) -> crate::action::Action<'_, crate::accessibility::AccessibilityOptions, crate::accessibility::AccessibilityReport>
  {
    crate::action::Action::new(move |opts| Box::pin(async move { self.check_accessibility_impl(opts).await }))
  }

  async fn check_accessibility_impl(
    self: &Arc<Self>,
    options: crate::accessibility::AccessibilityOptions,
  ) -> Result<crate::accessibility::AccessibilityReport> {
    let path = crate::install::BrowserInstaller::new().axe_core_path();
    let source = crate::accessibility::load_axe_source(&path)?;
    let argument = crate::accessibility::AuditArgument {
      source: &source,
      options: options.into(),
    };
    let value = self
      .evaluate(
        &crate::accessibility::inject_and_run_source(),
        crate::protocol::serializers::argument_from_serde(&argument)?,
        Some(true),
      )
      .await?;
    // The page hands back a JSON string, not a structure: axe's own
    // result is large enough that letting each backend re-serialise it
    // through its remote-value format is both slow and lossy.
    let payload = value
      .as_str()
      .ok_or_else(|| crate::error::FerriError::backend("axe-core returned a non-string result"))?;
    crate::accessibility::parse_report(payload)
  }

  /// The tools this page offers about itself.
  ///
  /// A page answers a `devtoolstooldiscovery` event with groups of
  /// named callables, each with a JSON schema for its input, so an
  /// agent can use the site's own vocabulary instead of its DOM. Empty
  /// where the page exposes none, which is most pages.
  ///
  /// Works on every backend: it is a DOM event, not a protocol.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::Backend`] where the page
  /// answered with something other than the shape the event asks for.
  pub async fn developer_tools(self: &Arc<Self>) -> Result<Vec<crate::page_tools::PageToolGroup>> {
    let value = self
      .evaluate(
        crate::page_tools::discover_source(),
        crate::protocol::SerializedArgument::default(),
        Some(true),
      )
      .await?;
    // A JSON string rather than a structure, for the reason the
    // accessibility audit does the same: each backend re-serialises a
    // returned object through its own remote-value format.
    let payload = value
      .as_str()
      .ok_or_else(|| crate::error::FerriError::backend("tool discovery returned a non-string result"))?;
    crate::page_tools::parse_groups(payload)
  }

  /// Call one of the tools [`Page::developer_tools`] found.
  ///
  /// `params` has to satisfy that tool's own `inputSchema`; the page
  /// decides what it means. What comes back is whatever the tool
  /// returned, with the values JSON cannot carry replaced: an element
  /// by a `stashedId` naming its slot in `window.__dtmcp.stashedElements`,
  /// a cycle by `<Circular reference>`, a function by `<Function object>`.
  ///
  /// Passing an element IN is not wired: upstream substitutes elements
  /// for snapshot uids in the parameters, and this has no equivalent
  /// bridge from a `ref=eN` to a page-side element yet.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::Backend`] where the page
  /// exposes no tools, the named tool does not exist, or the tool threw.
  pub async fn execute_developer_tool(
    self: &Arc<Self>,
    name: &str,
    params: Option<serde_json::Value>,
  ) -> Result<serde_json::Value> {
    let call = serde_json::json!({ "name": name, "params": params });
    let value = self
      .evaluate(
        crate::page_tools::execute_source(),
        crate::protocol::serializers::argument_from_serde(&call)?,
        Some(true),
      )
      .await?;
    let payload = value
      .as_str()
      .ok_or_else(|| crate::error::FerriError::backend("tool execution returned a non-string result"))?;
    crate::page_tools::parse_result(payload)
  }

  /// Capture a V8 heap snapshot of this page, analysed.
  ///
  /// Garbage is collected first, so what comes back is what the page is
  /// still holding rather than what it has merely not dropped yet.
  ///
  /// Take two of these either side of the thing under suspicion and ask
  /// the second what changed: `heap.diff_since(&before)` names the
  /// classes that grew, and `retaining_paths` on one of their objects
  /// says what is holding it.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::Unsupported`] on the `WebKit`
  /// and `BiDi` backends: the format is V8's, and neither engine writes
  /// anything a reader of it could open.
  pub async fn take_heap_snapshot(self: &Arc<Self>) -> Result<crate::heap::HeapSnapshot> {
    let json = self.inner.take_heap_snapshot().await?;
    crate::heap::HeapSnapshot::parse(json)
  }

  /// Audit the page against the Lighthouse checks that read a live DOM.
  ///
  /// Ten of them: `doctype`, `meta-description`, `canonical`,
  /// `crawlable-anchors`, `link-text`, `image-aspect-ratio`,
  /// `image-size-responsive`, `paste-preventing-inputs`,
  /// `http-status-code` and `is-crawlable`. This navigates nothing, so
  /// a static page answers the same way every time.
  ///
  /// The last two also read the main document's own response, which
  /// comes from the context's network log rather than the DOM, and
  /// `is-crawlable` fetches `/robots.txt` from the page. Both are left
  /// out of the report when the current document arrived without a
  /// response of its own -- `about:blank`, `setContent`, or a network
  /// log that has since lapped past it.
  ///
  /// See [`crate::audits`] for what these are ported from, and for the
  /// one input `crawlable-anchors` cannot see.
  ///
  /// # Errors
  ///
  /// [`crate::error::FerriError::Backend`] when the page could not be
  /// read.
  ///
  /// Options chain as setters: `.only(["image-alt", "is-crawlable"])`.
  #[track_caller]
  pub fn check_page_quality(
    self: &Arc<Self>,
  ) -> crate::action::Action<'_, crate::audits::PageQualityOptions, crate::audits::PageQualityReport> {
    crate::action::Action::new(move |opts| Box::pin(async move { self.check_page_quality_impl(opts).await }))
  }

  async fn check_page_quality_impl(
    self: &Arc<Self>,
    options: crate::audits::PageQualityOptions,
  ) -> Result<crate::audits::PageQualityReport> {
    let value = self
      .evaluate(
        crate::audits::gather_source(),
        crate::protocol::SerializedArgument::default(),
        Some(true),
      )
      .await?;
    // A JSON string rather than a structure, for the reason the
    // accessibility audit does the same: each backend re-serialises a
    // returned object through its own remote-value format.
    let payload = value
      .as_str()
      .ok_or_else(|| crate::error::FerriError::backend("page audit gatherer returned a non-string result"))?;
    let mut artifacts = crate::audits::parse_artifacts(payload)?;

    if options.wants("http-status-code") || options.wants("is-crawlable") {
      artifacts.main_document = self.main_document(&artifacts.url).await;
    }
    if options.wants("is-crawlable") && artifacts.main_document.is_some() {
      artifacts.robots_txt = self.fetch_robots_txt(&artifacts.url).await;
    }
    Ok(crate::audits::run(&artifacts, &options))
  }

  /// The response that produced the document now at `url`.
  ///
  /// Lighthouse's rule, from `core/computed/main-resource.js`: the LAST
  /// navigation request whose URL matches the document's once fragments
  /// are dropped. Taking the last is what makes `location.reload()`
  /// resolve to the response on screen rather than the evicted first
  /// one, and matching on the FINAL URL is what steps over a redirect
  /// chain, whose hops each carry a different one.
  ///
  /// `is_navigation_request` stands in for Lighthouse's
  /// `resourceType === 'Document'` filter: `BiDi` infers a resource
  /// type from the initiator and would answer differently there, while
  /// the navigation flag is set from the protocol's own signal on every
  /// backend.
  async fn main_document(self: &Arc<Self>, url: &str) -> Option<crate::audits::MainDocument> {
    let log = self.context_ref.as_ref()?.network_log_handle().await?;
    let guid = self.inner.page_guid();
    let requests: Vec<crate::network::Request> = log.read().await.clone();

    for request in requests.iter().rev() {
      if !request.is_navigation_request()
        || request.page_guid() != Some(guid.as_str())
        || request.failure().is_some()
        || !same_document_url(request.url(), url)
      {
        continue;
      }
      let Some(response) = request.existing_response().await else {
        continue;
      };
      return Some(crate::audits::MainDocument {
        url: response.url().to_string(),
        status_code: response.status(),
        response_headers: response.headers_array().await,
      });
    }
    None
  }

  /// Fetch `/robots.txt` for the document's origin, from the page.
  ///
  /// Lighthouse fetches it over `Network.loadNetworkResource`, which is
  /// Chromium-only; the file is always same-origin with the document,
  /// so an in-page `fetch` reaches it on every backend with the same
  /// cookies and the same 2s budget. A body is kept only for a 2xx,
  /// which is what stops a 404 page's HTML being parsed as rules.
  ///
  /// Every failure answers `None`, meaning "no rules", because that is
  /// what a site without a `robots.txt` means and what Lighthouse's own
  /// gatherer returns when the fetch throws.
  async fn fetch_robots_txt(self: &Arc<Self>, document_url: &str) -> Option<crate::audits::RobotsTxt> {
    let robots_url = crate::audits::robots_txt_url(document_url)?;
    let source = format!(
      r"
(async () => {{
  const url = {url};
  const deadline = new Promise(resolve => setTimeout(() => resolve(null), 2000));
  const fetched = (async () => {{
    const response = await fetch(url, {{ cache: 'no-store', credentials: 'include' }});
    const status = response.status;
    const content = status >= 200 && status <= 299 ? await response.text() : null;
    return {{ status, content }};
  }})().catch(() => ({{ status: null, content: null }}));
  return JSON.stringify(await Promise.race([fetched, deadline]) ?? {{ status: null, content: null }});
}})
",
      url = serde_json::to_string(&robots_url).ok()?
    );
    let value = self
      .evaluate(&source, crate::protocol::SerializedArgument::default(), Some(true))
      .await
      .ok()?;
    serde_json::from_str(value.as_str()?).ok()
  }

  /// Playwright: `page.evaluateHandle(pageFunction, arg?): Promise<JSHandle>`.
  /// Delegates to the main frame.
  ///
  /// # Errors
  ///
  /// See [`Self::evaluate`].
  pub async fn evaluate_handle(
    self: &Arc<Self>,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<crate::js_handle::JSHandle> {
    self
      .traced(
        "evaluateHandle",
        serde_json::json!({ "expression": fn_source }),
        self.main_frame().evaluate_handle(fn_source, arg.clone(), is_function),
      )
      .await
  }
}
