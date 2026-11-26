//! Locator content, DOM state queries, and evaluation.

use std::sync::Arc;

use crate::error::Result;
use crate::options::BoundingBox;
use crate::selectors;

use super::frame_locator::{aria_merge_frame, aria_opts_json, parse_aria_raw, render_aria_snapshot};
use super::{AriaSnapshotMatch, Locator, count_in};

impl Locator {
  // ── Content & state ───────────────────────────────────────────────────────

  /// Playwright: `locator.textContent(options?: TimeoutOptions):
  /// Promise<string | null>`. Waits for the element.
  ///
  /// # Errors
  ///
  /// [`crate::error::FerriError::Timeout`] if nothing matches in time,
  /// and a strict-mode violation when more than one element does.
  #[track_caller]
  pub fn text_content(&self) -> crate::action::Action<'static, crate::options::TimeoutOptions, Option<String>> {
    let locator = self.clone();
    crate::action::Action::new(move |opts: crate::options::TimeoutOptions| {
      Box::pin(async move { locator.text_content_impl(opts.timeout).await })
    })
  }

  pub(crate) async fn text_content_impl(&self, timeout: Option<u64>) -> Result<Option<String>> {
    let value = self.read("textContent", "return el.textContent;", timeout).await?;
    Ok(value.as_str().map(str::to_string))
  }

  /// Playwright: `locator.innerText(options?: TimeoutOptions): Promise<string>`.
  ///
  /// # Errors
  ///
  /// As [`Self::text_content`], and an error when the element is not an
  /// HTML element.
  #[track_caller]
  pub fn inner_text(&self) -> crate::action::Action<'static, crate::options::TimeoutOptions, String> {
    let locator = self.clone();
    crate::action::Action::new(move |opts: crate::options::TimeoutOptions| {
      Box::pin(async move { locator.inner_text_impl(opts.timeout).await })
    })
  }

  pub(crate) async fn inner_text_impl(&self, timeout: Option<u64>) -> Result<String> {
    self
      .read_string(
        "innerText",
        "if (el.namespaceURI !== 'http://www.w3.org/1999/xhtml') throw new Error('Node is not an HTMLElement'); \
         return el.innerText;",
        timeout,
      )
      .await
  }

  /// Playwright: `locator.innerHTML(options?: TimeoutOptions): Promise<string>`.
  ///
  /// # Errors
  ///
  /// As [`Self::text_content`].
  #[track_caller]
  pub fn inner_html(&self) -> crate::action::Action<'static, crate::options::TimeoutOptions, String> {
    let locator = self.clone();
    crate::action::Action::new(move |opts: crate::options::TimeoutOptions| {
      Box::pin(async move { locator.inner_html_impl(opts.timeout).await })
    })
  }

  pub(crate) async fn inner_html_impl(&self, timeout: Option<u64>) -> Result<String> {
    self.read_string("innerHTML", "return el.innerHTML;", timeout).await
  }

  /// Playwright: `locator.ariaSnapshot(options?: TimeoutOptions &
  /// { mode?: 'ai' | 'default', depth?: number }): Promise<string>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:327`).
  ///
  /// Resolves this locator to a single element under the auto-wait /
  /// retry pipeline (strict mode honored — strictness + actionability
  /// stay in Rust core), then renders the accessibility subtree rooted
  /// at that element via the vendored Playwright `InjectedScript`
  /// (`window.__fd.ariaSnapshotFrame`). The output is
  /// byte-for-byte the Playwright YAML, scoped to the element — siblings
  /// outside the locator are excluded by construction.
  ///
  /// Cross-iframe: when the subtree contains `<iframe>` nodes that the
  /// renderer assigned refs to (i.e. `mode: 'ai'` — `mode: 'default'`
  /// emits no refs, exactly like Playwright, so there is nothing to
  /// stitch), the child browsing contexts are snapshotted recursively
  /// and spliced under their `- iframe [ref=...]` line, mirroring
  /// `ariaSnapshotForFrame` / `ariaSnapshotFrameRef`
  /// (`/tmp/playwright/.../server/page.ts:1103`). Each frame gets a
  /// unique `fN` ref-prefix so refs never collide across frames.
  /// Uniform across every backend (same vendored renderer + the same
  /// content-frame resolution `frameLocator` uses).
  ///
  /// # Errors
  ///
  /// [`crate::error::FerriError::Timeout`] if the element cannot be
  /// resolved within the timeout; forwards the page-side render error.
  #[track_caller]
  pub fn aria_snapshot(&self) -> crate::action::Action<'static, crate::options::AriaSnapshotOptions, String> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.aria_snapshot_impl(opts).await }))
  }

  pub(crate) async fn aria_snapshot_impl(&self, options: crate::options::AriaSnapshotOptions) -> Result<String> {
    let (root_frame, _) = self.resolved().await?;
    let json = self.aria_snapshot_json_impl(options).await?;
    render_aria_snapshot(&root_frame, &json).await
  }

  /// Playwright: `locator.ariaSnapshotJSON(options?): Promise<AriaSnapshotJSON>`
  /// — the tree [`Self::aria_snapshot`] renders, with the trees of the
  /// iframes it gave refs to merged under their `iframe` nodes.
  ///
  /// # Errors
  ///
  /// As [`Self::aria_snapshot`].
  #[track_caller]
  pub fn aria_snapshot_json(
    &self,
  ) -> crate::action::Action<'static, crate::options::AriaSnapshotOptions, Vec<serde_json::Value>> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.aria_snapshot_json_impl(opts).await }))
  }

  pub(crate) async fn aria_snapshot_json_impl(
    &self,
    options: crate::options::AriaSnapshotOptions,
  ) -> Result<Vec<serde_json::Value>> {
    // The frame the element resolves in (frameLocator enter-frame hops
    // resolved here) — root for the recursive child-iframe descent.
    let (root_frame, _sel) = self.resolved().await?;
    let root_frame_id = (!root_frame.is_main_frame()).then(|| root_frame.id().to_string());
    let mode = options.mode.unwrap_or_default().as_str();
    let depth = options.depth;
    let opts_json = aria_opts_json(mode, depth, options.boxes);
    let root_js = format!("function() {{ return JSON.stringify(window.__fd.ariaSnapshotFrame(this, {opts_json})); }}");
    retry_resolve!(self, options.timeout, "ariaSnapshot", |el, _page| async {
      selectors::ensure_aria_support(root_frame.page_arc().inner(), root_frame_id.as_deref()).await?;
      let raw_s = el
        .call_js_fn_value(&root_js)
        .await?
        .and_then(|v| v.as_str().map(std::string::ToString::to_string))
        .unwrap_or_default();
      let raw = parse_aria_raw(&raw_s)?;
      let seq = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
      aria_merge_frame(
        root_frame.clone(),
        raw,
        mode.to_string(),
        depth,
        options.boxes,
        String::new(),
        seq,
      )
      .await
    })
  }

  /// Playwright: `locator.getAttribute(name: string, options?:
  /// TimeoutOptions): Promise<string | null>`. Waits for the element;
  /// `None` is an element without the attribute.
  ///
  /// # Errors
  ///
  /// As [`Self::text_content`].
  #[track_caller]
  pub fn get_attribute(
    &self,
    name: &str,
  ) -> crate::action::Action<'static, crate::options::TimeoutOptions, Option<String>> {
    let locator = self.clone();
    let name = name.to_string();
    crate::action::Action::new(move |opts: crate::options::TimeoutOptions| {
      Box::pin(async move { locator.get_attribute_impl(&name, opts.timeout).await })
    })
  }

  pub(crate) async fn get_attribute_impl(&self, name: &str, timeout: Option<u64>) -> Result<Option<String>> {
    let name = serde_json::to_string(name)?;
    let value = self
      .read("getAttribute", &format!("return el.getAttribute({name});"), timeout)
      .await?;
    Ok(value.as_str().map(str::to_string))
  }

  /// Playwright: `locator.inputValue(options?: TimeoutOptions):
  /// Promise<string>`. A `<label>` reads the control it owns.
  ///
  /// # Errors
  ///
  /// As [`Self::text_content`], and an error when the element is not an
  /// `<input>`, `<textarea>` or `<select>`.
  #[track_caller]
  pub fn input_value(&self) -> crate::action::Action<'static, crate::options::TimeoutOptions, String> {
    let locator = self.clone();
    crate::action::Action::new(move |opts: crate::options::TimeoutOptions| {
      Box::pin(async move { locator.input_value_impl(opts.timeout).await })
    })
  }

  pub(crate) async fn input_value_impl(&self, timeout: Option<u64>) -> Result<String> {
    self
      .read_string("inputValue", "return window.__fd.inputValue(el);", timeout)
      .await
  }

  /// Playwright: `locator.isVisible(options?: TimeoutOptions): Promise<boolean>`.
  /// A non-empty box with visible style, inside visible iframes. Does not
  /// wait: no match is `false`, and Playwright ignores the `timeout`.
  ///
  /// # Errors
  ///
  /// A strict-mode violation, or a failed evaluation.
  #[track_caller]
  pub fn is_visible(&self) -> crate::action::Action<'static, crate::options::TimeoutOptions, bool> {
    let locator = self.clone();
    crate::action::Action::new(move |_| Box::pin(async move { locator.is_visible_impl().await }))
  }

  pub(crate) async fn is_visible_impl(&self) -> Result<bool> {
    let val = self
      .eval_on_element("return window.__fd.elementState(el, 'visible') === true;")
      .await?;
    Ok(val.and_then(|v| v.as_bool()).unwrap_or(false))
  }

  /// Playwright: `locator.isHidden(options?: TimeoutOptions): Promise<boolean>`,
  /// the inverse of [`Self::is_visible`].
  ///
  /// # Errors
  ///
  /// As [`Self::is_visible`].
  #[track_caller]
  pub fn is_hidden(&self) -> crate::action::Action<'static, crate::options::TimeoutOptions, bool> {
    let locator = self.clone();
    crate::action::Action::new(move |_| Box::pin(async move { locator.is_visible_impl().await.map(|v| !v) }))
  }

  /// Playwright: `locator.isEnabled(options?: TimeoutOptions): Promise<boolean>`.
  /// Waits for the element.
  ///
  /// # Errors
  ///
  /// As [`Self::text_content`].
  #[track_caller]
  pub fn is_enabled(&self) -> crate::action::Action<'static, crate::options::TimeoutOptions, bool> {
    let locator = self.clone();
    crate::action::Action::new(move |opts: crate::options::TimeoutOptions| {
      Box::pin(async move { locator.is_enabled_impl(opts.timeout).await })
    })
  }

  pub(crate) async fn is_enabled_impl(&self, timeout: Option<u64>) -> Result<bool> {
    self.read_state("isEnabled", "enabled", timeout).await
  }

  /// Playwright: `locator.isDisabled(options?: TimeoutOptions): Promise<boolean>`.
  ///
  /// # Errors
  ///
  /// As [`Self::text_content`].
  #[track_caller]
  pub fn is_disabled(&self) -> crate::action::Action<'static, crate::options::TimeoutOptions, bool> {
    let locator = self.clone();
    crate::action::Action::new(move |opts: crate::options::TimeoutOptions| {
      Box::pin(async move { locator.is_disabled_impl(opts.timeout).await })
    })
  }

  pub(crate) async fn is_disabled_impl(&self, timeout: Option<u64>) -> Result<bool> {
    self.read_state("isDisabled", "disabled", timeout).await
  }

  /// Playwright: `locator.isChecked(options?: TimeoutOptions): Promise<boolean>`.
  ///
  /// # Errors
  ///
  /// As [`Self::text_content`], and an error when the element is not a
  /// checkbox or radio button.
  #[track_caller]
  pub fn is_checked(&self) -> crate::action::Action<'static, crate::options::TimeoutOptions, bool> {
    // `this.checked` alone only sees `input[type=checkbox|radio]`.
    // Playwright reads the state through `getCheckedWithoutMixed` after
    // a `follow-label` retarget, so `aria-checked` roles (switch,
    // menuitemcheckbox, treeitem, …) and labels wrapping their control
    // report correctly (`injectedScript.ts::elementState`). A
    // non-checkable element is an error there, not `false`.
    let locator = self.clone();
    crate::action::Action::new(move |opts: crate::options::TimeoutOptions| {
      Box::pin(async move { locator.is_checked_impl(opts.timeout).await })
    })
  }

  pub(crate) async fn is_checked_impl(&self, timeout: Option<u64>) -> Result<bool> {
    let value = self
      .read(
        "isChecked",
        "var c = window.__fd.getChecked(el); \
         if (typeof c !== 'boolean') throw new Error('Not a checkbox or radio button'); \
         return c;",
        timeout,
      )
      .await?;
    Ok(value.as_bool().unwrap_or(false))
  }

  /// Playwright's `to.match.aria`: match a YAML aria template against
  /// this element's live aria tree, with Playwright's containment
  /// semantics (`injected/ariaSnapshot.ts::matchesExpectAriaTemplate` —
  /// the expected template names a SUBSET; extra siblings, attributes
  /// and depth are allowed unless the template says `/children: equal`).
  ///
  /// The YAML is parsed by the on-demand `aria-support` bundle, mirroring
  /// Playwright's split between a server-side parse and an in-page match.
  ///
  /// # Errors
  ///
  /// Selector parsing, a malformed template, or JS evaluation failure.
  pub async fn match_aria_snapshot(&self, expected_yaml: &str) -> Result<AriaSnapshotMatch> {
    let (rframe, _) = self.resolved().await?;
    let inner = rframe.page_arc().inner();
    let frame_id = (!rframe.is_main_frame()).then(|| rframe.id().to_string());
    selectors::ensure_aria_support(inner, frame_id.as_deref()).await?;
    let yaml_json = serde_json::to_string(expected_yaml).unwrap_or_else(|_| "\"\"".to_string());
    let raw = self
      .retry_eval_on_element(&format!(
        "return JSON.stringify(window.__fd.matchAriaTemplate(el, window.__fdAria.parse({yaml_json})));"
      ))
      .await?
      .and_then(|v| v.as_str().map(std::string::ToString::to_string))
      .ok_or_else(|| crate::FerriError::protocol("toMatchAriaSnapshot", "element not found"))?;
    let parsed: serde_json::Value = serde_json::from_str(&raw)
      .map_err(|e| crate::FerriError::protocol("toMatchAriaSnapshot", format!("bad matcher result: {e}")))?;
    Ok(AriaSnapshotMatch {
      matches: parsed
        .get("matches")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false),
      received: parsed
        .get("received")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string(),
      regex: parsed
        .get("regex")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string(),
    })
  }

  /// Check if the element is attached to the DOM (exists in the document).
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing fails.
  pub async fn is_attached(&self) -> Result<bool> {
    Ok(self.resolve().await.is_ok())
  }

  /// Count the number of elements matching this locator's selector.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn count(&self) -> Result<usize> {
    // Resolve frameLocator enter-frame hops, then count the trailing
    // selector inside the resolved frame (no-op for plain selectors).
    let (rf, rsel) = self.resolved().await?;
    count_in(&rf, &rsel).await
  }

  /// Resolve this locator to a canonical selector and return a new
  /// [`Locator`] built from it.
  ///
  /// Playwright: `normalize(): Promise<Locator>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:269`)
  /// which calls `frame.resolveSelector` -> `injected.generateSelectorSimple`
  /// (`/tmp/playwright/packages/playwright-core/src/server/frames.ts:1274`).
  ///
  /// The trailing selector (the part that runs inside the deepest
  /// resolved frame) is replaced with the recorder/codegen selector the
  /// injected script generates for the single matched element. When the
  /// locator targets a child frame (it carries `internal:control=enter-frame`
  /// hops), the original enter-frame prefix is preserved so the returned
  /// locator still resolves into the same frame; only the trailing
  /// segment is canonicalised. Strict by design: errors if 0 or >1
  /// elements match, mirroring Playwright's `selectors.query`.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing fails, no element matches, or
  /// more than one element matches.
  pub async fn normalize(&self) -> Result<Locator> {
    const MARK: &str = ">> internal:control=enter-frame >>";
    let (rf, rsel) = self.resolved().await?;
    let frame_id: Option<&str> = if rf.is_main_frame() { None } else { Some(rf.id()) };
    let generated = selectors::normalize_selector(rf.page_arc().inner(), &rsel, frame_id).await?;
    // Re-attach the enter-frame prefix (everything up to and including
    // the last hop) so the new locator targets the same frame; only the
    // trailing segment is replaced with the canonical generated selector.
    let new_selector = match self.selector.rsplit_once(MARK) {
      Some((prefix, _)) => format!("{prefix}{MARK} {generated}"),
      None => generated,
    };
    Ok(Locator::new(self.frame.clone(), new_selector))
  }

  /// The locator expression a developer would paste into a test.
  ///
  /// `page.getByRole('button', { name: 'Sign in' })` rather than the
  /// selector [`Locator::normalize`] returns. Strict, for the same
  /// reason: an expression that matches two elements is not one anybody
  /// can use.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing fails, no element matches, or
  /// more than one element matches.
  pub async fn generate_locator(&self, language: crate::codegen::OutputLanguage) -> Result<String> {
    let (rf, rsel) = self.resolved().await?;
    let frame_id: Option<&str> = if rf.is_main_frame() { None } else { Some(rf.id()) };
    selectors::generate_locator(rf.page_arc().inner(), &rsel, frame_id, language).await
  }

  /// Return the bounding box of the element, or `None` if the element is not found.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn bounding_box(&self) -> Result<Option<BoundingBox>> {
    let body = format!("return ({}).call(el);", crate::screenshotter::MAIN_FRAME_BOX_FN);
    let val = self.read("boundingBox", &body, None).await?;
    Ok(crate::screenshotter::parse_rect(&val).map(|r| BoundingBox {
      x: r.x,
      y: r.y,
      width: r.width,
      height: r.height,
    }))
  }

  // ── Evaluate (Playwright parity) ─────────────────────────────────────

  /// Playwright: `locator.evaluate(pageFunction, arg?, options?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:129`).
  ///
  /// Resolves this locator's element under the auto-wait / retry
  /// pipeline, then calls `fn(element, arg)` in the page context.
  /// Disposes the intermediate handle before returning.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::Timeout`] when the element
  /// cannot be resolved within the configured timeout, or forwards
  /// the page-side evaluate error.
  #[track_caller]
  pub fn evaluate(
    &self,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> crate::action::Action<'static, crate::options::EvaluateOptions, crate::protocol::SerializedValue> {
    let locator = self.clone();
    let fn_source = fn_source.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { locator.evaluate_impl(&fn_source, arg, is_function, Some(opts)).await })
    })
  }

  /// Typed evaluate: run `fn_source` against the matched element and
  /// deserialize the result via serde. Ergonomic wrapper over the
  /// wire-level [`Self::evaluate`] for JSON-shaped values:
  ///
  /// ```ignore
  /// let text: String = locator.eval("el => el.textContent").await?;
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
  /// function as its second parameter (`(el, arg) => ...`).
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

  pub(crate) async fn evaluate_impl(
    &self,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
    options: Option<crate::options::EvaluateOptions>,
  ) -> Result<crate::protocol::SerializedValue> {
    let timeout_ms = options.and_then(|o| o.timeout);
    let fn_source = fn_source.to_string();
    retry_resolve!(
      self,
      timeout_ms,
      "evaluate",
      ::serde_json::json!({ "expression": fn_source }),
      |el, _page| async {
        let page_arc = Arc::clone(self.frame.page_arc());
        let handle = crate::element_handle::ElementHandle::from_any_element(page_arc, el).await?;
        let result = handle
          .as_js_handle()
          .evaluate(&fn_source, arg.clone(), is_function)
          .await;
        let _ = handle.dispose().await;
        result
      }
    )
  }

  /// Playwright: `locator.waitForFunction(pageFunction, arg?, options?): Promise<void>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:396`).
  ///
  /// Polls `fn(element, arg)` against this locator's resolved element
  /// until it returns a truthy value or the timeout elapses. Auto-waits
  /// for the element to attach, like every other locator action.
  ///
  /// # Errors
  ///
  /// [`crate::error::FerriError::timeout`] if the function never returns
  /// truthy within the timeout.
  pub async fn wait_for_function(
    &self,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
    timeout_ms: Option<u64>,
  ) -> Result<()> {
    let timeout = timeout_ms.unwrap_or_else(|| self.frame.page_arc().default_timeout());
    // Playwright's `timeout: 0` waits for as long as it takes.
    let deadline = (timeout != 0).then(|| tokio::time::Instant::now() + std::time::Duration::from_millis(timeout));
    let timed_out = || format!("waiting for function on locator {}", self.selector);
    crate::operation_budget::scoped(
      timeout,
      timed_out,
      Box::pin(async move {
        loop {
          let remaining = match deadline {
            None => 0,
            Some(deadline) => match deadline
              .saturating_duration_since(tokio::time::Instant::now())
              .as_millis()
            {
              0 => return Err(crate::error::FerriError::timeout(timed_out(), timeout)),
              left => u64::try_from(left).unwrap_or(u64::MAX),
            },
          };
          let opts = crate::options::EvaluateOptions {
            timeout: Some(remaining),
          };
          if let Ok(value) = self
            .evaluate_impl(fn_source, arg.clone(), is_function, Some(opts))
            .await
            && let Ok(json) = crate::protocol::result_to_serde::<serde_json::Value>(&value)
          {
            let truthy = match &json {
              serde_json::Value::Bool(b) => *b,
              serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
              serde_json::Value::String(s) => !s.is_empty(),
              serde_json::Value::Null => false,
              _ => true,
            };
            if truthy {
              return Ok(());
            }
          }
          self
            .frame
            .pause_until_change(std::time::Duration::from_millis(50))
            .await;
        }
      }),
    )
    .await
  }

  /// Playwright: `locator.evaluateHandle(pageFunction, arg?, options?): Promise<JSHandle>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:138`).
  ///
  /// Resolves this locator's element under auto-wait / retry, then calls
  /// `fn(element, arg)` retaining the result on the page and returning
  /// it as a [`crate::js_handle::JSHandle`]. The intermediate
  /// `ElementHandle` is disposed — the returned handle is an independent
  /// remote reference.
  ///
  /// # Errors
  ///
  /// See [`Self::evaluate`].
  #[track_caller]
  pub fn evaluate_handle(
    &self,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> crate::action::Action<'static, crate::options::EvaluateOptions, crate::js_handle::JSHandle> {
    let locator = self.clone();
    let fn_source = fn_source.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        locator
          .evaluate_handle_impl(&fn_source, arg, is_function, Some(opts))
          .await
      })
    })
  }

  pub(crate) async fn evaluate_handle_impl(
    &self,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
    options: Option<crate::options::EvaluateOptions>,
  ) -> Result<crate::js_handle::JSHandle> {
    let timeout_ms = options.and_then(|o| o.timeout);
    let fn_source = fn_source.to_string();
    retry_resolve!(
      self,
      timeout_ms,
      "evaluateHandle",
      ::serde_json::json!({ "expression": fn_source }),
      |el, _page| async {
        let page_arc = Arc::clone(self.frame.page_arc());
        let handle = crate::element_handle::ElementHandle::from_any_element(page_arc, el).await?;
        let result = handle
          .as_js_handle()
          .evaluate_handle(&fn_source, arg.clone(), is_function)
          .await;
        let _ = handle.dispose().await;
        result
      }
    )
  }

  /// Playwright: `locator.evaluateAll(pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:133`).
  ///
  /// Resolves every matching element in this locator's frame and calls
  /// `fn(elements, arg)` with the array as the first argument. Unlike
  /// [`Self::evaluate`], no retry/auto-wait — empty matches produce an
  /// empty array (Playwright parity).
  ///
  /// # Errors
  ///
  /// Forwards page-side evaluate error.
  pub async fn evaluate_all(
    &self,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<crate::protocol::SerializedValue> {
    let (frame, selector) = self.resolved().await?;
    let parts_json = selectors::build_parts_json(&selectors::parse(&selector)?);
    frame.page_arc().inner().ensure_engine_injected().await?;
    let probe = format!("() => window.__fd.selAll({parts_json})");
    let array_handle = frame
      .evaluate_handle(&probe, crate::protocol::SerializedArgument::default(), Some(true))
      .await?;
    let result = array_handle.evaluate(fn_source, arg, is_function).await;
    let _ = array_handle.dispose().await;
    result
  }
}
