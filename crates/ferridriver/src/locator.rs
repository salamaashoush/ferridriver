//! Lazy element locator.
//!
//! A Locator stores a selector string and a reference to its Page.
//! It does NOT query the DOM when created. Resolution happens lazily
//! when an action method (click, fill, etc.) is called.
//!
//! Locators can be chained to narrow scope:
//! ```ignore
//! page.locator("css=.form").get_by_role("textbox", &Default::default()).fill("hello").await?;
//! ```

use std::fmt::Write as _;
use std::sync::Arc;

use crate::actions;
use crate::backend::AnyElement;
use crate::error::Result;
use crate::options::{BoundingBox, FilterOptions, RoleOptions, StringOrRegex, TextOptions, WaitOptions};
use crate::selectors;

/// Zero-cost retry macro that resolves an element with backoff, then runs an
/// action body inline. Provides `$el: AnyElement` and `$page: &AnyPage` to the
/// body without any `AnyPage` cloning — the page reference is borrowed from `self`
/// for the entire retry loop.
///
/// The body must be an `async move { ... }` block returning
/// [`crate::error::Result<R>`]. The macro forwards every error through
/// the [`crate::error::FerriError`] taxonomy so call sites declare
/// `-> crate::error::Result<R>`.
///
/// `$timeout_ms` is an `Option<u64>` — the per-call override from the action's
/// option bag. `None` falls back to `page.default_timeout()` (set via
/// `page.setDefaultTimeout`). A resolved value of `0` means "no timeout" and
/// loops forever. `$op` is a `&str` used in the timeout-error message
/// (`TimeoutError { while $op }`).
///
/// Polling schedule: `[0, 0, 20, 50, 100, 100, 500]`, clamped at the last
/// value on overflow.
macro_rules! retry_resolve {
  ($self:expr, $timeout_ms:expr, $op:expr, |$el:ident, $page:ident| $body:expr) => {{
    // Trace span for the whole retried action — every exit path below
    // funnels through `break 'retry` so the span always closes with the
    // final outcome (timeout / strict violation / success / hard error).
    let __trace_page = $self.frame.page_arc();
    let __trace_span = {
      let __trace_composite = __trace_page.context().map(|c| c.composite());
      $crate::trace::begin_action(
        __trace_composite.as_deref(),
        "Locator",
        $op,
        ::std::option::Option::Some(format!("page@{}", __trace_page.backend_page_id())),
        ::serde_json::json!({ "selector": $self.selector }),
      )
    };
    let __trace_span = __trace_page.snapshot_before(__trace_span).await;
    if let ::std::option::Option::Some(__s) = __trace_span.as_ref() {
      __s.log(format!("waiting for locator('{}')", $self.selector));
    }
    // Input-time capture (target mark + input@ snapshot + point) fires
    // once, on the first successful resolution.
    let mut __trace_input_pending = __trace_span.is_some();
    // Last call-log line, to collapse identical retry messages.
    let mut __trace_last_log = ::std::string::String::new();
    let __result = 'retry: {
      // Resolve `frameLocator` enter-frame hops to the real child frame
      // + trailing selector (no-op for plain selectors).
      let (__rframe, __rsel) = match $self.resolved().await {
        ::std::result::Result::Ok(v) => v,
        ::std::result::Result::Err(e) => break 'retry ::std::result::Result::Err($crate::error::FerriError::from(e)),
      };
      let $page: &$crate::backend::AnyPage = __rframe.page_arc().inner();
      if let ::std::result::Result::Err(e) = $page.ensure_engine_injected().await {
        break 'retry ::std::result::Result::Err($crate::error::FerriError::from(e));
      }
      let __fd = "window.__fd";
      let __sel_js = match $crate::selectors::build_selone_js(&__rsel, &__fd, $self.strict) {
        ::std::result::Result::Ok(v) => v,
        ::std::result::Result::Err(e) => break 'retry ::std::result::Result::Err($crate::error::FerriError::from(e)),
      };
      // Pass `None` for main-frame locators so the backend skips a
      // `frame_contexts` lookup; child frames thread their cached id.
      let __frame_id: ::std::option::Option<&str> = if __rframe.is_main_frame() {
        ::std::option::Option::None
      } else {
        ::std::option::Option::Some(__rframe.id())
      };

      let __op_name: &str = $op;
      let __resolved_timeout: u64 = $timeout_ms.unwrap_or_else(|| $self.frame.page_arc().default_timeout());
      let __deadline: ::std::option::Option<::std::time::Instant> = if __resolved_timeout == 0 {
        ::std::option::Option::None
      } else {
        ::std::option::Option::Some(
          ::std::time::Instant::now() + ::std::time::Duration::from_millis(__resolved_timeout),
        )
      };

      let mut __idx: usize = 0;
      loop {
        // Deadline check up-front so we never race into one more attempt after
        // time has already run out.
        if let ::std::option::Option::Some(__d) = __deadline {
          if ::std::time::Instant::now() >= __d {
            break 'retry ::std::result::Result::Err($crate::error::FerriError::timeout(
              __op_name.to_string(),
              __resolved_timeout,
            ));
          }
        }

        // Action pre-checks: run registered locator handlers if any of their
        // overlays are currently visible (Playwright `performActionPreChecks`).
        $crate::locator_handler::perform_checkpoint(__rframe.page_arc()).await;

        let __delay_ms = Locator::RETRY_BACKOFFS_MS[__idx.min(Locator::RETRY_BACKOFFS_MS.len() - 1)];
        __idx = __idx.saturating_add(1);
        if __delay_ms > 0 {
          // Clamp the sleep to whatever's left on the deadline so the timeout
          // error fires on time rather than after an overshoot sleep.
          let __sleep_ms = match __deadline {
            ::std::option::Option::Some(__d) => {
              let __left = u64::try_from(__d.saturating_duration_since(::std::time::Instant::now()).as_millis())
                .unwrap_or(__delay_ms);
              __delay_ms.min(__left)
            },
            ::std::option::Option::None => __delay_ms,
          };
          if __sleep_ms > 0 {
            ::tokio::time::sleep(::std::time::Duration::from_millis(__sleep_ms)).await;
          }
        }

        // Strict mode (the default) is folded into the same engine-side
        // `selOne(parts, strict)` call below — the JS throws
        // `strict mode violation: <count>` when the selector matches more
        // than one element, the host catches the exception and converts
        // to a typed `FerriError::StrictModeViolation`. Saves the
        // separate `query_all` + `cleanup_tags` round-trips the previous
        // implementation paid on every retry attempt (~2 RTTs).
        match $crate::selectors::query_one_prebuilt($page, &__sel_js, &$self.selector, __frame_id).await {
          ::std::result::Result::Ok($el) => {
            if __trace_input_pending {
              __trace_input_pending = false;
              if let ::std::option::Option::Some(__s) = __trace_span.as_ref() {
                __trace_page.trace_capture_input(__s, &$el, $op).await;
              }
            }
            match ($body).await {
              ::std::result::Result::Ok(val) => break 'retry ::std::result::Result::Ok(val),
              ::std::result::Result::Err(e) => {
                let __msg = e.to_string();
                if $crate::locator::is_retryable_action_error(&__msg) {
                  // Retriable: actionability signals
                  // (`error:notvisible` / `error:notenabled` / ...) plus
                  // stale-handle backend errors (the node detached
                  // mid-action). Keep re-resolving until the deadline.
                  if let ::std::option::Option::Some(__s) = __trace_span.as_ref() {
                    if __msg != __trace_last_log {
                      __s.log(__msg.clone());
                      __trace_last_log = __msg;
                    }
                  }
                } else {
                  break 'retry ::std::result::Result::Err($crate::error::FerriError::from(e));
                }
              },
            }
          },
          ::std::result::Result::Err(__err) => {
            // Strict-mode violation: the engine threw
            // `strict mode violation: <count>` from inside `selOne`.
            // Surface it as a typed error rather than a retry signal.
            if let ::std::option::Option::Some(__count) = $crate::selectors::parse_strict_violation_count(&__err) {
              break 'retry ::std::result::Result::Err($crate::error::FerriError::strict(
                $self.selector.clone(),
                __count,
              ));
            }
            // Otherwise: element not found this iteration; retry until deadline.
          },
        }
      }
    };
    if let ::std::option::Option::Some(__s) = __trace_span {
      __trace_page
        .snapshot_after_and_finish(__s, __result.as_ref().err())
        .await;
    }
    __result
  }};
}

/// Should a failed action attempt be retried (re-resolve + try again
/// until the action timeout), rather than surfaced as a hard error?
///
/// Two families:
/// 1. Actionability signals from `checkElementStates`
///    (`error:notvisible` / `error:notenabled` / `error:noteditable`,
///    "not connected", "detached") — the element exists but is not yet
///    actionable.
/// 2. Stale-handle / navigated-away backend errors — the resolved node
///    vanished mid-action (React re-mount, SPA route swap, in-flight
///    navigation). Playwright's actionability loop re-resolves and
///    retries these; ferridriver previously surfaced the raw protocol
///    error (e.g. CDP "Object id doesn't reference a Node"), which
///    escaped callers' try/catch and made every real SPA interaction
///    fragile.
pub(crate) fn is_retryable_action_error(msg: &str) -> bool {
  msg.contains("not connected")
    || msg.contains("not found")
    || msg.contains("detached")
    || msg.contains("error:not")
    // CDP stale-node / stale-context messages.
    || msg.contains("reference a Node")
    || msg.contains("Could not find node")
    || msg.contains("No node with given")
    || msg.contains("Node with given id")
    || msg.contains("belong to the document")
    || msg.contains("Execution context was destroyed")
    || msg.contains("Cannot find context with specified id")
    // WebKit / BiDi stale-node phrasings.
    || msg.contains("Node was not found")
    || msg.contains("no such node")
    || msg.contains("stale element")
}

/// A lazy element locator bound to a [`crate::Frame`]. Every Locator
/// carries a Frame reference, and all DOM resolution and action dispatch
/// happens in that frame's execution context. Chaining (`.locator()`,
/// `.filter()`, `.first()`, etc.) returns a new Locator on the same
/// Frame; the Frame itself is cheap to clone (two `Arc`s).
#[derive(Clone)]
pub struct Locator {
  /// Owning frame. Provides the page back-reference (`frame.page_arc()`)
  /// and the execution-context id (`frame.id()`) used by every action.
  pub(crate) frame: crate::frame::Frame,
  pub(crate) selector: String,
  /// Strict mode: error with [`crate::error::FerriError::StrictModeViolation`]
  /// if the selector resolves to multiple elements. Every Locator action
  /// runs in strict mode by default; `first()` / `last()` / `nth()` /
  /// `strict(false)` opt out.
  pub(crate) strict: bool,
}

impl Locator {
  /// Construct a Locator with strict mode enabled (the default).
  #[must_use]
  pub(crate) fn new(frame: crate::frame::Frame, selector: String) -> Self {
    Self {
      frame,
      selector,
      strict: true,
    }
  }

  /// Resolve any `internal:control=enter-frame` hops embedded in
  /// `self.selector` into the actual child [`Frame`], returning the
  /// deepest frame plus the trailing selector to run inside it. A
  /// no-op clone for selectors without a frame hop.
  ///
  /// `FrameLocator` builds a selector chain like
  /// `#if >> internal:control=enter-frame >> #btn`. The injected
  /// engine's `enter-frame` control returns `[]` by design — the
  /// boundary is resolved HERE (server side), mirroring Playwright's
  /// frame chunking: query the `<iframe>` in the current frame, hop to
  /// its content frame, continue with the next chunk. Re-resolved on
  /// every action attempt so a re-attached iframe is picked up.
  pub(crate) async fn resolved(&self) -> Result<(crate::frame::Frame, String)> {
    const MARK: &str = ">> internal:control=enter-frame >>";
    if !self.selector.contains("internal:control=enter-frame") {
      return Ok((self.frame.clone(), self.selector.clone()));
    }
    let mut parts = self.selector.split(MARK).map(str::trim);
    let mut cur = self.frame.clone();
    let mut pending = parts.next().unwrap_or("").to_string();
    for next in parts {
      let page_arc = std::sync::Arc::clone(cur.page_arc());
      let fid: Option<String> = if cur.is_main_frame() {
        None
      } else {
        Some(cur.id().to_string())
      };
      let el = crate::selectors::query_one(page_arc.inner(), &pending, false, fid.as_deref()).await?;
      let handle = crate::element_handle::ElementHandle::from_any_element(std::sync::Arc::clone(&page_arc), el).await?;
      cur = handle
        .content_frame()
        .await?
        .ok_or_else(|| crate::error::FerriError::protocol("frameLocator", "<iframe> has no content frame"))?;
      pending = next.to_string();
    }
    Ok((cur, pending))
  }

  /// Returns a copy of this locator with strict-mode toggled.
  ///
  /// In strict mode (default), any action on a locator that matches more than
  /// one element raises [`crate::error::FerriError::StrictModeViolation`].
  /// Pass `false` to explicitly allow multi-match (the behaviour of
  /// `locator.first()` / `.last()` / `.nth()`).
  #[must_use]
  pub fn strict(&self, strict: bool) -> Locator {
    Locator {
      frame: self.frame.clone(),
      selector: self.selector.clone(),
      strict,
    }
  }
  // ── Sub-locators (chain with >>) ──────────────────────────────────────────

  /// Narrow this locator's scope.
  ///
  /// `locator(selectorOrLocator: string | Locator,
  ///          options?: Omit<LocatorOptions, 'visible'>): Locator`.
  ///
  /// Infallible by design — chainable Locator API. A cross-page inner
  /// locator encodes a sentinel clause that the selector engine rejects
  /// at resolve time; JSON encoding never fails for a valid UTF-8
  /// selector. `visible` is stripped from the option bag (only
  /// `filter()` and the constructor accept it).
  #[must_use]
  pub fn locator(&self, selector_or_locator: impl Into<crate::options::LocatorLike>) -> Locator {
    let inner = selector_or_locator.into();
    match &inner {
      crate::options::LocatorLike::Selector(s) => self.chain(s),
      crate::options::LocatorLike::Locator(l) => {
        if Arc::ptr_eq(self.frame.page_arc(), l.frame.page_arc()) {
          self.chain(&format!("internal:chain={}", json_quote(&l.selector)))
        } else {
          // Encoded sentinel — the selector engine rejects it, so the
          // caller sees an explicit InvalidSelector at the first action
          // rather than a silently-wrong filter. Deferred to resolve
          // time to keep the Locator chain API infallible.
          self.chain("internal:cross-frame-error=true")
        }
      },
    }
  }

  /// [`Self::locator`] with Playwright's `Omit<LocatorOptions, 'visible'>`
  /// filter bag (`visible` is stripped; only `filter()` and the
  /// constructor accept it).
  #[must_use]
  pub fn locator_with(
    &self,
    selector_or_locator: impl Into<crate::options::LocatorLike>,
    options: &crate::options::FilterOptions,
  ) -> Locator {
    let mut opts = options.clone();
    opts.visible = None; // Omit<LocatorOptions, 'visible'>
    self.locator(selector_or_locator).filter(&opts)
  }

  /// Locate elements by ARIA role, refined via builder setters.
  /// `.name(...)` accepts `string | RegExp` — a regex matches the
  /// accessible name with its full JS regex semantics (flags preserved),
  /// while a literal string matches case-insensitively unless
  /// `.exact(true)`.
  #[must_use]
  pub fn get_by_role(
    &self,
    role: impl Into<crate::options::Role>,
  ) -> crate::locator_builder::LocatorBuilder<RoleOptions> {
    let base = self.clone();
    let role = role.into();
    crate::locator_builder::LocatorBuilder::new(move |opts| base.chain(&build_role_selector(role.as_str(), opts)))
  }

  /// Locate elements by visible text content. `text` accepts
  /// `string | RegExp` per Playwright's `getByText`.
  #[must_use]
  pub fn get_by_text(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.text_like_builder("internal:text", text.into())
  }

  /// Locate form elements by their associated label text. Accepts
  /// `string | RegExp` — the `getByLabel` form.
  #[must_use]
  pub fn get_by_label(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.text_like_builder("internal:label", text.into())
  }

  /// Locate input elements by their placeholder text. Accepts
  /// `string | RegExp` — the `getByPlaceholder` form.
  #[must_use]
  pub fn get_by_placeholder(
    &self,
    text: impl Into<StringOrRegex>,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.attr_builder("placeholder", text.into())
  }

  /// Locate elements by their `alt` attribute text. Accepts
  /// `string | RegExp` — the `getByAltText` form.
  #[must_use]
  pub fn get_by_alt_text(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.attr_builder("alt", text.into())
  }

  /// Locate elements by their `title` attribute text. Accepts
  /// `string | RegExp` — the `getByTitle` form.
  #[must_use]
  pub fn get_by_title(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.attr_builder("title", text.into())
  }

  /// Locate elements by their `data-testid` (or the configured
  /// test-id attribute). Accepts `string | RegExp` — the `getByTestId`
  /// form. Matches are always exact.
  #[must_use]
  pub fn get_by_test_id(&self, test_id: impl Into<StringOrRegex>) -> Locator {
    self.chain(&build_testid_selector("data-testid", &test_id.into()))
  }

  fn text_like_builder(
    &self,
    kind: &'static str,
    text: StringOrRegex,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    let base = self.clone();
    crate::locator_builder::LocatorBuilder::new(move |opts| base.chain(&build_text_like_selector(kind, &text, opts)))
  }

  fn attr_builder(
    &self,
    attr: &'static str,
    text: StringOrRegex,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    let base = self.clone();
    crate::locator_builder::LocatorBuilder::new(move |opts| base.chain(&build_attr_selector(attr, &text, opts)))
  }

  /// Attach a human-readable description to this locator for trace /
  /// error reporting. Matching is unaffected — the injected
  /// `internal:describe` engine passes elements through untouched.
  /// Playwright: `locator.describe(description: string): Locator`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts`).
  #[must_use]
  pub fn describe(&self, description: &str) -> Locator {
    self.chain(&format!("internal:describe={}", json_quote(description)))
  }

  /// The custom description previously set with [`Self::describe`], or `None`
  /// if this locator has none. Mirrors Playwright's `locator.description():
  /// string | null` — only the LAST selector part counts (a `describe` that
  /// isn't the final chained step is shadowed by later steps), matching
  /// `locatorCustomDescription` in `packages/isomorphic/locatorGenerators.ts`.
  #[must_use]
  pub fn description(&self) -> Option<String> {
    let last = self.selector.rsplit(" >> ").next().unwrap_or(&self.selector).trim();
    let body = last.strip_prefix("internal:describe=")?;
    serde_json::from_str::<String>(body).ok()
  }

  /// First element. Opts out of strict mode because the selector explicitly
  /// narrows to a single match.
  #[must_use]
  pub fn first(&self) -> Locator {
    self.chain("nth=0").strict(false)
  }

  /// Last element. Opts out of strict mode (explicit single match).
  #[must_use]
  pub fn last(&self) -> Locator {
    self.chain("nth=-1").strict(false)
  }

  /// nth element. Opts out of strict mode (explicit single match).
  #[must_use]
  pub fn nth(&self, index: i32) -> Locator {
    self.chain(&format!("nth={index}")).strict(false)
  }

  /// Filter this locator by text content, inner-locator presence/absence,
  /// or visibility. Option-to-selector encoding:
  ///
  /// * `has_text` → ` >> internal:has-text=<escaped>` (plain-text clause).
  /// * `has_not_text` → ` >> internal:has-not-text=<escaped>`.
  /// * `has` (inner [`Locator`]) → ` >> internal:has=<JSON inner selector>`.
  /// * `has_not` (inner [`Locator`]) → ` >> internal:has-not=<JSON inner selector>`.
  /// * `visible: Some(b)` → ` >> visible=true|false`.
  ///
  /// Inner locators must belong to the same page as `self`; otherwise
  /// this returns a locator whose selector contains an explicit error
  /// marker — when resolved, the selector engine rejects it and the
  /// caller sees an [`crate::error::FerriError::InvalidSelector`]. The
  /// method itself stays infallible.
  #[must_use]
  pub fn filter(&self, opts: &FilterOptions) -> Locator {
    use std::fmt::Write as _;

    // Build the combined filter suffix in one buffer, then chain once.
    let mut suffix = String::new();
    let push_sep = |buf: &mut String| {
      if !buf.is_empty() {
        buf.push_str(" >> ");
      }
    };

    if let Some(text) = &opts.has_text {
      let _ = write!(suffix, "internal:has-text={}", json_quote(text));
    }
    if let Some(text) = &opts.has_not_text {
      push_sep(&mut suffix);
      let _ = write!(suffix, "internal:has-not-text={}", json_quote(text));
    }
    if let Some(inner) = &opts.has {
      push_sep(&mut suffix);
      if inner
        .as_locator()
        .is_some_and(|l| !Arc::ptr_eq(self.frame.page_arc(), l.frame.page_arc()))
      {
        // Same-page invariant violation — inject a sentinel the selector
        // engine will reject so the caller sees an explicit error rather
        // than a silently-mismatched filter. Only enforceable when the
        // caller supplied a full `Locator`; raw selector strings skip
        // this check by design.
        let _ = write!(suffix, "internal:has-cross-frame-error=true");
      } else {
        let _ = write!(suffix, "internal:has={}", json_quote(inner.as_selector()));
      }
    }
    if let Some(inner) = &opts.has_not {
      push_sep(&mut suffix);
      if inner
        .as_locator()
        .is_some_and(|l| !Arc::ptr_eq(self.frame.page_arc(), l.frame.page_arc()))
      {
        let _ = write!(suffix, "internal:has-not-cross-frame-error=true");
      } else {
        let _ = write!(suffix, "internal:has-not={}", json_quote(inner.as_selector()));
      }
    }
    if let Some(v) = opts.visible {
      push_sep(&mut suffix);
      let _ = write!(suffix, "visible={}", if v { "true" } else { "false" });
    }
    if suffix.is_empty() {
      self.clone()
    } else {
      self.chain(&suffix)
    }
  }

  // ── Actions ───────────────────────────────────────────────────────────────
  //
  // All action methods use the `retry_resolve!` macro which:
  //   1. Pre-builds selector JS once (no re-parsing per retry)
  //   2. Borrows `&AnyPage` from self — zero AnyPage clones
  //   3. Borrows `&str` parameters directly — zero String clones
  //   4. Expands inline — no closure/future type-erasure overhead

  /// Click the element matched by this locator with the full
  /// [`crate::options::ClickOptions`] surface.
  ///
  /// All options (`button`, `click_count`, `delay`, `force`, `modifiers`,
  /// `position`, `steps`, `trial`, `timeout`) are honored across all
  /// four backends; `no_wait_after` is accepted for signature parity
  /// but has no effect (ferridriver does not implicitly wait for
  /// navigation after click).
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found, is not actionable
  /// (unless `force=true`), or the click dispatch fails.
  pub fn click(&self) -> crate::action::Action<'static, crate::options::ClickOptions, ()> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.click_impl(Some(opts)).await }))
  }

  pub(crate) async fn click_impl(&self, opts: Option<crate::options::ClickOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    // Borrow `opts` across retry iterations — references are `Copy`, so
    // each `async move` closure captures a fresh ref instead of moving
    // the owned `ClickOptions` (which contains a non-Copy `Vec<Modifier>`).
    let opts_ref = &opts;
    retry_resolve!(self, opts_ref.timeout, "click", |el, page| async move {
      // Playwright `waitForSignalsCreatedBy`: snapshot nav state, click, then
      // if the click started a navigation wait (bounded, best-effort) for it
      // to commit so a following read/action sees the new document. Zero cost
      // when the click navigates nowhere.
      let snap = page.nav_snapshot();
      actions::click_with_opts(&el, page, opts_ref).await?;
      if !opts_ref.is_trial() {
        page.settle_navigation(snap, 2_000).await;
      }
      Ok::<(), crate::error::FerriError>(())
    })
  }

  /// Double-click the element matched by this locator.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or the double-click fails.
  pub fn dblclick(&self) -> crate::action::Action<'static, crate::options::DblClickOptions, ()> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.dblclick_impl(Some(opts)).await }))
  }

  pub(crate) async fn dblclick_impl(&self, opts: Option<crate::options::DblClickOptions>) -> Result<()> {
    // `dblclick` is a click pair with `clickCount` = 1 then 2. The
    // shared `click_with_opts` honors that when `click_count` is set
    // to `2`.
    let click_opts = opts.unwrap_or_default().into_click_options();
    let click_opts_ref = &click_opts;
    retry_resolve!(self, click_opts_ref.timeout, "dblclick", |el, page| async move {
      let snap = page.nav_snapshot();
      actions::click_with_opts(&el, page, click_opts_ref).await?;
      if !click_opts_ref.is_trial() {
        page.settle_navigation(snap, 2_000).await;
      }
      Ok::<(), crate::error::FerriError>(())
    })
  }

  /// Right-click (context menu click) on the element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found, its bounding box
  /// cannot be computed, or the right-click dispatch fails.
  pub async fn right_click(&self) -> Result<()> {
    retry_resolve!(
      self,
      ::std::option::Option::<u64>::None,
      "right_click",
      |el, page| async move {
        let center = el.call_js_fn_value(
        "function() { this.scrollIntoViewIfNeeded ? this.scrollIntoViewIfNeeded() : this.scrollIntoView({block: 'center', inline: 'center'}); var r = this.getBoundingClientRect(); return {x: r.x + r.width/2, y: r.y + r.height/2}; }"
      ).await?;
        if let Some(c) = center {
          let x = c.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
          let y = c.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
          page.click_at_opts(x, y, "right", 1).await?;
        }
        Ok::<(), crate::error::FerriError>(())
      }
    )
  }

  /// Fill an input or textarea element with the given value.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or is not a fillable element.
  pub fn fill(&self, value: &str) -> crate::action::Action<'static, crate::options::FillOptions, ()> {
    let locator = self.clone();
    let value = value.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.fill_impl(&value, Some(opts)).await }))
  }

  pub(crate) async fn fill_impl(&self, value: &str, opts: Option<crate::options::FillOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    let force = opts.is_force();
    let opts_ref = &opts;
    retry_resolve!(self, opts_ref.timeout, "fill", |el, page| async move {
      // `actions::fill(..., force)` runs `checkElementStates(['visible',
      // 'enabled','editable'])` internally when `force` is false and
      // returns the `error:not<state>` marker the retry loop knows to
      // keep polling on. `force=true` jumps straight to the DOM write.
      actions::fill(&el, page, value, force).await
    })
  }

  /// Clear the value of an input or textarea element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found.
  pub async fn clear(&self) -> Result<()> {
    retry_resolve!(
      self,
      ::std::option::Option::<u64>::None,
      "clear",
      |el, _page| async move {
        el.call_js_fn(
          "function() { \
        if (window.__fd) window.__fd.clearAndDispatch(this); \
        else { this.value = ''; } \
      }",
        )
        .await?;
        Ok::<(), crate::error::FerriError>(())
      }
    )
  }

  /// Show the element-highlight overlay for this locator's selector.
  ///
  /// Playwright:
  /// `highlight(options: { style?: string | Record<string, string | number> }): Promise<Disposable>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:158`).
  /// The optional `style` is collapsed to a CSS declaration string (see
  /// [`crate::options::HighlightStyle::to_css_string`]) and applied to the
  /// highlight box. The overlay re-resolves the selector on each animation
  /// frame, so no element wait happens here — matching Playwright, which
  /// just forwards to `frame._highlight`. Returns a
  /// [`crate::disposable::Disposable`] whose `dispose()` hides the overlay
  /// (Playwright returns a `DisposableStub` wrapping `hideHighlight`).
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing, frame resolution, or the
  /// injected `addHighlight` call fails.
  pub async fn highlight(
    &self,
    style: Option<crate::options::HighlightStyle>,
  ) -> Result<crate::disposable::Disposable> {
    let (frame, selector) = self.resolved().await?;
    let css = style.as_ref().map(crate::options::HighlightStyle::to_css_string);
    frame.highlight(&selector, css.as_deref()).await?;
    let this = self.clone();
    Ok(crate::disposable::Disposable::new(move || async move {
      match this.hide_highlight().await {
        Ok(()) => Ok(()),
        Err(e) if e.is_target_closed_error() => Ok(()),
        Err(e) => Err(e),
      }
    }))
  }

  /// Hide the element-highlight overlay shown by [`Locator::highlight`].
  ///
  /// Playwright: `hideHighlight(): Promise<void>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:164`).
  /// Tears down the whole overlay for this locator's frame.
  ///
  /// # Errors
  ///
  /// Returns an error if frame resolution or the injected `hideHighlight`
  /// call fails.
  pub async fn hide_highlight(&self) -> Result<()> {
    let (frame, _selector) = self.resolved().await?;
    frame.hide_highlight().await
  }

  /// Type text into the element character by character using keyboard events.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or key dispatch fails.
  pub fn r#type(&self, text: &str) -> crate::action::Action<'static, crate::options::TypeOptions, ()> {
    let locator = self.clone();
    let text = text.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.type_impl(&text, Some(opts)).await }))
  }

  pub(crate) async fn type_impl(&self, text: &str, opts: Option<crate::options::TypeOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    let delay_ms = opts.resolved_delay_ms();
    let timeout_ms = opts.timeout;
    retry_resolve!(self, timeout_ms, "type", |el, page| async move {
      actions::wait_for_actionable(&el, page).await.ok();
      if delay_ms > 0 {
        // With a per-char delay, fall back to the character-by-character
        // keyboard dispatch (same code path `pressSequentially` uses).
        for ch in text.chars() {
          page.press_key(&ch.to_string()).await?;
          tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
        Ok(())
      } else {
        el.type_str(text).await
      }
    })
  }

  /// Press a key or key combination (e.g. "Enter", "Control+a").
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or the key press fails.
  pub fn press(&self, key: &str) -> crate::action::Action<'static, crate::options::PressOptions, ()> {
    let locator = self.clone();
    let key = key.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.press_impl(&key, Some(opts)).await }))
  }

  pub(crate) async fn press_impl(&self, key: &str, opts: Option<crate::options::PressOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    let delay_ms = opts.resolved_delay_ms();
    let timeout_ms = opts.timeout;
    retry_resolve!(self, timeout_ms, "press", |el, page| async move {
      actions::wait_for_actionable(&el, page).await.ok();
      // Focus the element before dispatching keys so the event lands at
      // the intended target (`_press` → `_focus` → `keyboard.press`).
      // Without this the key dispatches to whatever's currently focused,
      // usually the body, and the element under the locator never sees
      // it.
      el.call_js_fn("function() { this.focus(); }").await?;
      if delay_ms > 0 {
        // With a delay, press is equivalent to keyDown + sleep(delay)
        // + keyUp so the page observes the held-key interval.
        page.key_down(key).await?;
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        page.key_up(key).await
      } else {
        page.press_key(key).await
      }
    })
  }

  /// Hover over the element matched by this locator.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or the hover action fails.
  pub fn hover(&self) -> crate::action::Action<'static, crate::options::HoverOptions, ()> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.hover_impl(Some(opts)).await }))
  }

  pub(crate) async fn hover_impl(&self, opts: Option<crate::options::HoverOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    let opts_ref = &opts;
    retry_resolve!(self, opts_ref.timeout, "hover", |el, page| async move {
      actions::hover_with_opts(&el, page, opts_ref).await
    })
  }

  /// Focus the element matched by this locator.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found.
  pub async fn focus(&self) -> Result<()> {
    retry_resolve!(
      self,
      ::std::option::Option::<u64>::None,
      "focus",
      |el, _page| async move {
        el.call_js_fn("function() { this.focus(); }").await?;
        Ok::<(), crate::error::FerriError>(())
      }
    )
  }

  /// Check a checkbox or radio button if it is not already checked.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or is not actionable.
  pub fn check(&self) -> crate::action::Action<'static, crate::options::CheckOptions, ()> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.check_impl(Some(opts)).await }))
  }

  pub(crate) async fn check_impl(&self, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    self.set_checked_impl(true, opts).await
  }

  /// Uncheck a checkbox if it is currently checked.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or is not actionable.
  pub fn uncheck(&self) -> crate::action::Action<'static, crate::options::CheckOptions, ()> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.uncheck_impl(Some(opts)).await }))
  }

  pub(crate) async fn uncheck_impl(&self, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    self.set_checked_impl(false, opts).await
  }

  /// Set the checked state of a checkbox or radio button to match
  /// `checked`. Reads the element's current `checked` property; if it
  /// already matches the target state, the call is a no-op (but
  /// actionability checks still run). Otherwise dispatches a real click
  /// via [`actions::click_with_opts`] so the page sees `input` /
  /// `change` events with the correct timing.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found, is not
  /// actionable, or the click dispatch fails.
  pub fn set_checked(&self, checked: bool) -> crate::action::Action<'static, crate::options::CheckOptions, ()> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.set_checked_impl(checked, Some(opts)).await }))
  }

  pub(crate) async fn set_checked_impl(&self, checked: bool, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    let trial = opts.is_trial();
    // Lower to ClickOptions for the shared click dispatch path so
    // `force` / `trial` / `position` / `timeout` all flow through.
    let click_opts = opts.into_click_options();
    let click_opts_ref = &click_opts;
    retry_resolve!(self, click_opts_ref.timeout, "check", |el, page| async move {
      // setChecked flow:
      //   1. Read current checked state (via `fd.getChecked`, which
      //      understands `input[type=checkbox|radio]` AND ARIA
      //      `aria-checked` roles — `this.checked` alone misses the
      //      latter).
      //   2. If current already matches target → done, no click.
      //   3. Uncheck of a checked radio → hard error (radios only
      //      toggle off by selecting another in their group).
      //   4. Dispatch the click with the caller's options.
      //   5. If `trial` → done (skip verification).
      //   6. Re-read state; if it still doesn't match the target →
      //      `"Clicking the checkbox did not change its state"`.
      let fd = page.injected_script().await?;
      let state_js = format!(
        "function() {{ \
           var r = {fd}.getChecked(this); \
           var isRadio = this.nodeName === 'INPUT' && this.type === 'radio'; \
           return JSON.stringify({{ state: r, isRadio: isRadio }}); \
         }}"
      );
      let read_state = async || -> crate::error::Result<(Option<bool>, bool)> {
        let raw = el
          .call_js_fn_value(&state_js)
          .await?
          .and_then(|v| v.as_str().map(std::string::ToString::to_string))
          .unwrap_or_default();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::json!({}));
        let is_radio = parsed
          .get("isRadio")
          .and_then(serde_json::Value::as_bool)
          .unwrap_or(false);
        let state_val = match parsed.get("state") {
          Some(v) if v.is_boolean() => Some(v.as_bool().unwrap_or(false)),
          _ => None,
        };
        Ok((state_val, is_radio))
      };

      let (current, is_radio) = read_state().await?;
      let Some(current) = current else {
        return Err(crate::error::FerriError::invalid_argument(
          "element",
          "not a checkbox, radio button, or ARIA-checkable element",
        ));
      };
      if current == checked {
        return Ok::<(), crate::error::FerriError>(());
      }
      if !checked && is_radio {
        return Err(crate::error::FerriError::invalid_argument(
          "element",
          "Cannot uncheck radio button. Radio buttons can only be unchecked by selecting another radio button in the same group.",
        ));
      }
      actions::click_with_opts(&el, page, click_opts_ref).await?;
      if trial {
        return Ok::<(), crate::error::FerriError>(());
      }
      let (new_state, _) = read_state().await?;
      if new_state != Some(checked) {
        return Err(crate::error::FerriError::backend(
          "clicking the checkbox did not change its state",
        ));
      }
      Ok::<(), crate::error::FerriError>(())
    })
  }

  /// Tap the element (touch event). Dispatches touchstart + touchend on platforms
  /// that support Touch/TouchEvent APIs, falls back to pointerdown + pointerup + click
  /// on desktop browsers (e.g. Playwright `WebKit`) where Touch constructors are unavailable.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or the tap event dispatch fails.
  pub fn tap(&self) -> crate::action::Action<'static, crate::options::TapOptions, ()> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.tap_impl(Some(opts)).await }))
  }

  pub(crate) async fn tap_impl(&self, opts: Option<crate::options::TapOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    let opts_ref = &opts;
    retry_resolve!(self, opts_ref.timeout, "tap", |el, page| async move {
      actions::tap_with_opts(&el, page, opts_ref).await
    })
  }

  /// Select all text in an input or textarea element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or the selection fails.
  pub async fn select_text(&self) -> Result<()> {
    let el = self.resolve().await?;
    el.call_js_fn(
      "function() { \
      this.focus(); \
      if (this.select) { this.select(); } \
      else if (this.setSelectionRange) { this.setSelectionRange(0, this.value ? this.value.length : 0); } \
    }",
    )
    .await
  }

  /// Select an `<option>` by value within a `<select>` element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or is not a `<select>`.
  pub fn select_option(
    &self,
    values: impl Into<crate::options::SelectOptionValues>,
  ) -> crate::action::Action<'static, crate::options::SelectOptionOptions, Vec<String>> {
    let values = values.into().0;
    let locator = self.clone();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { locator.select_option_impl(values, Some(opts)).await })
    })
  }

  pub(crate) async fn select_option_impl(
    &self,
    values: Vec<crate::options::SelectOptionValue>,
    opts: Option<crate::options::SelectOptionOptions>,
  ) -> Result<Vec<String>> {
    let opts = opts.unwrap_or_default();
    let timeout_ms = opts.timeout;
    let force = opts.force.unwrap_or(false);
    let values_ref = &values;
    // Mirrors Playwright's `server/dom.ts::_selectOption`: when not
    // `force`, gate the dispatch on `checkElementStates(['visible',
    // 'enabled'])` so a hidden or disabled `<select>` returns the
    // `error:not<state>` retriable marker until the deadline fires.
    // `force: true` skips the pre-check and goes straight to the
    // injected `selectOptions` call.
    retry_resolve!(self, timeout_ms, "selectOption", |el, page| async move {
      if !force {
        let fd = page.injected_script().await?;
        let state_raw = el
          .call_js_fn_value(&format!(
            "function() {{ return {fd}.checkElementStates(this, ['visible', 'enabled']); }}"
          ))
          .await?
          .and_then(|v| v.as_str().map(std::string::ToString::to_string))
          .unwrap_or_else(|| "error:notconnected".to_string());
        if state_raw != "done" {
          return Err(crate::error::FerriError::backend(state_raw));
        }
      }
      actions::select_options(&el, page, values_ref).await
    })
  }

  /// Set file paths on a file input element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not a file input or the upload fails.
  pub fn set_input_files(
    &self,
    files: impl Into<crate::options::InputFiles>,
  ) -> crate::action::Action<'static, crate::options::SetInputFilesOptions, ()> {
    let files = files.into();
    let locator = self.clone();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { locator.set_input_files_impl(files, Some(opts)).await })
    })
  }

  pub(crate) async fn set_input_files_impl(
    &self,
    files: crate::options::InputFiles,
    opts: Option<crate::options::SetInputFilesOptions>,
  ) -> Result<()> {
    // Lower `Payloads` to temp-file paths so the wire-level CDP
    // `DOM.setFileInputFiles` command can carry them unchanged — the
    // alternative would be a separate per-backend `setFileInputBytes`
    // op, which only Playwright's internal CDP protocol supports.
    // Temp files live for the action only; we delete them after the
    // backend call returns regardless of success/failure.
    let timeout_ms = opts
      .and_then(|o| o.timeout)
      .unwrap_or_else(|| self.frame.page_arc().default_timeout());
    let paths: Vec<String> = match files {
      crate::options::InputFiles::Paths(paths) => paths.into_iter().map(|p| p.display().to_string()).collect(),
      crate::options::InputFiles::Payloads(payloads) => {
        // Each payload gets its own subdirectory so the filename on
        // disk matches `payload.name` verbatim — otherwise the page
        // would see a ferridriver-internal `{i}-` prefix and
        // duplicate names would collide in the shared temp root.
        // Matches Playwright's `setInputFilePaths` server path which
        // materialises each payload to a temporary directory unique
        // to the upload.
        //
        // We deliberately DO NOT delete these temp files after
        // `upload_file` returns. CDP's `DOM.setFileInputFiles` only
        // records the paths on the `<input>`; the browser does not
        // actually read file content until the page JS calls
        // `input.files[i].size` / `reader.readAsText(...)` — which
        // happens AFTER this function returns. Deleting on the
        // success path leaves the page with zero-byte files (the
        // handle survives but the backing file is gone). The
        // process-scoped root is cleaned up by the OS on reboot and
        // the per-upload subdirs share that root, so we don't leak
        // indefinitely across a test run.
        let tmp_root = std::env::temp_dir().join(format!("ferridriver-files-{}", std::process::id()));
        tokio::fs::create_dir_all(&tmp_root)
          .await
          .map_err(|e| crate::error::FerriError::Backend(format!("failed to create upload temp dir: {e}")))?;
        let upload_id = std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .map_or(0, |d| d.as_nanos());
        let mut paths: Vec<String> = Vec::new();
        for (i, p) in payloads.iter().enumerate() {
          let sub = tmp_root.join(format!("{upload_id}-{i}"));
          tokio::fs::create_dir_all(&sub)
            .await
            .map_err(|e| crate::error::FerriError::Backend(format!("failed to create payload subdir: {e}")))?;
          let safe_name = p.name.replace(['/', '\\', '\0'], "_");
          let path = sub.join(&safe_name);
          tokio::fs::write(&path, &p.buffer)
            .await
            .map_err(|e| crate::error::FerriError::Backend(format!("failed to write upload payload: {e}")))?;
          paths.push(path.display().to_string());
        }
        paths
      },
    };

    // Retry on stale-handle errors until the timeout. The upload target
    // is often a hidden, framework-managed `<input type=file>` that
    // re-mounts during a builder/route transition; a single-shot upload
    // races the re-render and surfaces a raw "Object id doesn't
    // reference a Node" that escaped callers' try/catch. This mirrors
    // the locator action retry funnel for the file-input path, which
    // resolves the selector fresh (`upload_file` re-queries) on each
    // attempt.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let mut attempt: usize = 0;
    loop {
      match actions::upload_file(self.frame.page_arc().inner(), &self.selector, &paths).await {
        Ok(()) => return Ok(()),
        Err(e) => {
          let now = tokio::time::Instant::now();
          let timed_out = timeout_ms != 0 && now >= deadline;
          if timed_out || !is_retryable_action_error(&e.to_string()) {
            return Err(e);
          }
          let delay = Self::RETRY_BACKOFFS_MS[attempt.min(Self::RETRY_BACKOFFS_MS.len() - 1)];
          attempt = attempt.saturating_add(1);
          let sleep_ms = if timeout_ms == 0 {
            delay
          } else {
            delay.min(u64::try_from(deadline.saturating_duration_since(now).as_millis()).unwrap_or(delay))
          };
          if sleep_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(sleep_ms)).await;
          }
        },
      }
    }
  }

  /// Scroll the element into the visible area of the viewport.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or scroll fails.
  pub async fn scroll_into_view_if_needed(&self) -> Result<()> {
    retry_resolve!(self, None, "scrollIntoViewIfNeeded", |el, _page| async move {
      el.scroll_into_view().await
    })
  }

  /// Dispatch a DOM event of the given type on the element. Mirrors
  /// Playwright's `frames.ts::dispatchEvent` (see
  /// `/tmp/playwright/packages/playwright-core/src/server/frames.ts:847`):
  /// resolve the element under the retry loop (Playwright does NOT run
  /// actionability for dispatchEvent — it's a programmatic dispatch),
  /// then invoke `injectedScript.dispatchEvent` with the matching
  /// constructor. `opts.timeout` flows through to the retry deadline.
  ///
  /// # Errors
  ///
  /// Returns `FerriError::Timeout` if the element does not appear
  /// before the deadline.
  pub fn dispatch_event(
    &self,
    event_type: &str,
    event_init: Option<serde_json::Value>,
  ) -> crate::action::Action<'static, crate::options::DispatchEventOptions, ()> {
    let locator = self.clone();
    let event_type = event_type.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { locator.dispatch_event_impl(&event_type, event_init, Some(opts)).await })
    })
  }

  pub(crate) async fn dispatch_event_impl(
    &self,
    event_type: &str,
    event_init: Option<serde_json::Value>,
    opts: Option<crate::options::DispatchEventOptions>,
  ) -> Result<()> {
    let timeout_ms = opts.and_then(|o| o.timeout);
    let init_json = event_init.as_ref().map_or_else(
      || "{}".to_string(),
      |v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()),
    );
    // Escape `</script>` which would break our JS formatting if
    // event_init contained a close-script sequence.
    let init_js = init_json.replace("</", "<\\/");
    let js = format!(
      "function() {{ \
        var type = '{event_type}'; \
        var init = Object.assign({{bubbles: true, cancelable: true, composed: true}}, {init_js}); \
        var ev; \
        if (['click','dblclick','mousedown','mouseup','mouseenter','mouseleave','mousemove','mouseover','mouseout','contextmenu','auxclick'].includes(type)) {{ \
          ev = new MouseEvent(type, init); \
        }} else if (['keydown','keyup','keypress'].includes(type)) {{ \
          ev = new KeyboardEvent(type, init); \
        }} else if (['touchstart','touchend','touchmove','touchcancel'].includes(type) && typeof TouchEvent !== 'undefined') {{ \
          ev = new TouchEvent(type, init); \
        }} else if (['pointerdown','pointerup','pointermove','pointerover','pointerout','pointerenter','pointerleave','pointercancel','gotpointercapture','lostpointercapture'].includes(type)) {{ \
          ev = new PointerEvent(type, init); \
        }} else if (['dragstart','drag','dragenter','dragleave','dragover','drop','dragend'].includes(type)) {{ \
          ev = new DragEvent(type, init); \
        }} else if (['focus','blur','focusin','focusout'].includes(type)) {{ \
          ev = new FocusEvent(type, init); \
        }} else if (['input','beforeinput'].includes(type)) {{ \
          ev = new InputEvent(type, init); \
        }} else if (type === 'wheel') {{ \
          ev = new WheelEvent(type, init); \
        }} else if (['deviceorientation','deviceorientationabsolute'].includes(type)) {{ \
          ev = new DeviceOrientationEvent(type, init); \
        }} else {{ \
          ev = new Event(type, init); \
        }} \
        this.dispatchEvent(ev); \
      }}"
    );
    let js_ref = js.as_str();
    retry_resolve!(self, timeout_ms, "dispatchEvent", |el, _page| async move {
      el.call_js_fn(js_ref).await
    })
  }

  // ── Content & state ───────────────────────────────────────────────────────

  /// Return the `textContent` of the element, or `None` if not found.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn text_content(&self) -> Result<Option<String>> {
    self.eval_prop("textContent").await
  }

  /// Return the `innerText` of the element.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn inner_text(&self) -> Result<String> {
    self
      .eval_prop("innerText")
      .await
      .map(std::option::Option::unwrap_or_default)
  }

  /// Return the `innerHTML` of the element.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn inner_html(&self) -> Result<String> {
    self
      .eval_prop("innerHTML")
      .await
      .map(std::option::Option::unwrap_or_default)
  }

  /// Playwright: `locator.ariaSnapshot(options?: TimeoutOptions &
  /// { mode?: 'ai' | 'default', depth?: number }): Promise<string>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:327`).
  ///
  /// Resolves this locator to a single element under the auto-wait /
  /// retry pipeline (strict mode honored — strictness + actionability
  /// stay in Rust core), then renders the accessibility subtree rooted
  /// at that element via the vendored Playwright `InjectedScript`
  /// (`window.__fd.incrementalAriaSnapshot`). The output is
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
  pub fn aria_snapshot(&self) -> crate::action::Action<'static, crate::options::AriaSnapshotOptions, String> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.aria_snapshot_impl(opts).await }))
  }

  pub(crate) async fn aria_snapshot_impl(&self, options: crate::options::AriaSnapshotOptions) -> Result<String> {
    // The frame the element resolves in (frameLocator enter-frame hops
    // resolved here) — root for the recursive child-iframe descent.
    let (root_frame, _sel) = self.resolved().await?;
    let mode = options.mode.unwrap_or_default().as_str();
    let depth = options.depth;
    let opts_json = aria_opts_json(mode, depth, "", options.boxes);
    let root_js =
      format!("function() {{ return JSON.stringify(window.__fd.incrementalAriaSnapshot(this, {opts_json})); }}");
    retry_resolve!(self, options.timeout, "ariaSnapshot", |el, _page| async {
      let raw_s = el
        .call_js_fn_value(&root_js)
        .await?
        .and_then(|v| v.as_str().map(std::string::ToString::to_string))
        .unwrap_or_default();
      let raw = parse_aria_raw(&raw_s)?;
      let seq = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
      let lines = aria_stitch_frame(root_frame.clone(), raw, mode.to_string(), depth, options.boxes, seq).await?;
      Ok::<String, crate::error::FerriError>(lines.join("\n"))
    })
  }

  /// Get the value of an attribute on the element.
  ///
  /// Returns the raw attribute string exactly as
  /// `Element.getAttribute(name)` reports it (HTML attributes are always
  /// `string | null` per DOM spec — there is no native numeric/boolean
  /// attribute type). Playwright parity: Playwright's `getAttribute`
  /// returns `Promise<string | null>`; the previous implementation
  /// leaked the JSON-stringified form of non-string JS values (e.g.
  /// `"42"` vs `42`) — that path was unreachable with a well-behaved
  /// browser, but we now explicitly rule it out.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn get_attribute(&self, name: &str) -> Result<Option<String>> {
    let escaped = name.replace('\\', "\\\\").replace('\'', "\\'");
    let val = self
      .eval_on_element(&format!("return el.getAttribute('{escaped}');"))
      .await?;
    Ok(val.and_then(|v| match v {
      serde_json::Value::String(s) => Some(s),
      // Per the DOM spec `Element.getAttribute` only ever returns
      // `string | null`. Anything else coming back from the eval
      // indicates a browser bug or an unexpected injected script —
      // surface as `None` rather than silently JSON-stringifying.
      _ => None,
    }))
  }

  /// Return the `value` property of an input or textarea element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or JS evaluation fails.
  pub async fn input_value(&self) -> Result<String> {
    self
      .eval_prop("value")
      .await
      .map(std::option::Option::unwrap_or_default)
  }

  /// Check whether the element is visible (not `display:none`, `visibility:hidden`,
  /// or `opacity:0`). Returns `false` if the element does not exist.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn is_visible(&self) -> Result<bool> {
    // Single evaluate: find element + check visibility. Returns false if not found.
    let val = self
      .eval_on_element(
        "var s = getComputedStyle(el); \
       return s.display !== 'none' && s.visibility !== 'hidden' && s.opacity !== '0';",
      )
      .await?;
    // eval_on_element returns null if element not found -> false (Playwright behavior)
    Ok(val.and_then(|v| v.as_bool()).unwrap_or(false))
  }

  /// Check whether the element is hidden (inverse of `is_visible`).
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn is_hidden(&self) -> Result<bool> {
    self.is_visible().await.map(|v| !v)
  }

  /// Check whether the element is enabled (i.e. not `disabled`).
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or JS evaluation fails.
  pub async fn is_enabled(&self) -> Result<bool> {
    self.eval_bool("function() { return !this.disabled; }").await
  }

  /// Check whether the element is disabled.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or JS evaluation fails.
  pub async fn is_disabled(&self) -> Result<bool> {
    self.eval_bool("function() { return !!this.disabled; }").await
  }

  /// Check whether a checkbox or radio button is checked.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or JS evaluation fails.
  pub async fn is_checked(&self) -> Result<bool> {
    self.eval_bool("function() { return !!this.checked; }").await
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
    let parsed = selectors::parse(&rsel)?;
    let parts_json = selectors::build_parts_json(&parsed);
    let inner = rf.page_arc().inner();
    let fd = inner.injected_script().await?;
    let js = format!("{fd}.selCount({parts_json})");
    let val = if rf.is_main_frame() {
      inner.evaluate(&js).await
    } else {
      inner.evaluate_in_frame(&js, rf.id()).await
    }?
    .and_then(|v| v.as_u64())
    .unwrap_or(0);
    Ok(usize::try_from(val).unwrap_or(usize::MAX))
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

  /// Return the bounding box of the element, or `None` if the element is not found.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn bounding_box(&self) -> Result<Option<BoundingBox>> {
    let val = retry_resolve!(self, None, "boundingBox", |el, _page| async move {
      el.call_js_fn_value(
        "function() { var r = this.getBoundingClientRect(); return {x:r.x,y:r.y,width:r.width,height:r.height}; }",
      )
      .await
    })?;
    match val {
      Some(v) => Ok(Some(BoundingBox {
        x: v["x"].as_f64().unwrap_or(0.0),
        y: v["y"].as_f64().unwrap_or(0.0),
        width: v["width"].as_f64().unwrap_or(0.0),
        height: v["height"].as_f64().unwrap_or(0.0),
      })),
      None => Ok(None),
    }
  }

  // ── Waiting ───────────────────────────────────────────────────────────────

  /// Wait for the element to reach the specified state.
  ///
  /// Playwright states (`packages/playwright-core/src/client/locator.ts`):
  ///
  /// * `"attached"` — element is present in the DOM. Computed style is
  ///   not consulted. Matches `element.isConnected`.
  /// * `"visible"` — element is attached **and** has non-empty bounding
  ///   box, is not `display:none` / `visibility:hidden` / `opacity:0`.
  /// * `"hidden"` — element is either detached or not visible. A
  ///   detached element satisfies `"hidden"` (Playwright parity).
  /// * `"detached"` — element is not present in the DOM.
  ///
  /// Previously `"attached"` and `"visible"` were conflated — both
  /// returned as soon as a DOM query succeeded. That broke Playwright
  /// tests that rely on `attached` resolving for zero-size or
  /// currently-invisible elements.
  ///
  /// # Errors
  ///
  /// Returns an error if the timeout expires before the element reaches
  /// the desired state, or if an unknown state is specified.
  pub fn wait_for(&self) -> crate::action::Action<'static, crate::options::WaitOptions, ()> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.wait_for_impl(opts).await }))
  }

  pub(crate) async fn wait_for_impl(&self, opts: WaitOptions) -> Result<()> {
    let timeout = opts.timeout.unwrap_or(30000);
    let state = opts.state.unwrap_or_default();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout);

    loop {
      if tokio::time::Instant::now() >= deadline {
        return Err(crate::error::FerriError::timeout(
          format!("waiting for '{}' to be {state}", self.selector),
          timeout,
        ));
      }
      // Resolve frameLocator enter-frame hops each poll so a child frame
      // that is not yet attached (or was re-attached) is picked up on a
      // later attempt (no-op for plain selectors). A resolution failure
      // means the parent `<iframe>` / child frame is not present: for
      // `attached`/`visible` that is a retry, for `detached`/`hidden`
      // the element is by definition gone.
      let resolved = self.resolved().await;
      let attached = match &resolved {
        Ok((rframe, rsel)) => {
          let inner = rframe.page_arc().inner();
          let frame_id: Option<&str> = if rframe.is_main_frame() {
            None
          } else {
            Some(rframe.id())
          };
          let found = selectors::query_one(inner, rsel, false, frame_id).await.is_ok();
          selectors::cleanup_tags(inner).await;
          found
        },
        Err(_) => false,
      };
      match state {
        crate::options::WaitState::Attached => {
          // Only require DOM presence — do not consult computed style.
          if attached {
            return Ok(());
          }
        },
        crate::options::WaitState::Visible => {
          // DOM presence AND computed-style visible. Fail silently
          // (fall through to next poll) if `is_visible()` errors
          // because the element is detached mid-poll.
          if let Ok(true) = self.is_visible().await {
            return Ok(());
          }
        },
        crate::options::WaitState::Detached => {
          if !attached {
            return Ok(());
          }
        },
        crate::options::WaitState::Hidden => {
          // Playwright: `hidden` is satisfied by detachment OR by the
          // element being present but not visible.
          if !attached {
            return Ok(());
          }
          if let Ok(false) = self.is_visible().await {
            return Ok(());
          }
        },
      }
      tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
  }

  // ── Screenshot ────────────────────────────────────────────────────────────

  /// Take a screenshot of the element (PNG by default; `.format(...)`,
  /// `.path(...)`, `.timeout(...)` chain as setters).
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or screenshot capture fails.
  pub fn screenshot(&self) -> crate::action::Action<'static, crate::options::ElementScreenshotOptions, Vec<u8>> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.screenshot_impl(opts).await }))
  }

  pub(crate) async fn screenshot_impl(&self, opts: crate::options::ElementScreenshotOptions) -> Result<Vec<u8>> {
    let format = match opts.format.unwrap_or_default() {
      crate::options::ScreenshotFormat::Png => crate::backend::ImageFormat::Png,
      crate::options::ScreenshotFormat::Jpeg => crate::backend::ImageFormat::Jpeg,
      crate::options::ScreenshotFormat::Webp => crate::backend::ImageFormat::Webp,
    };
    let bytes = retry_resolve!(self, opts.timeout, "screenshot", |el, _page| async move {
      el.screenshot(format).await
    })?;
    if let Some(ref path) = opts.path {
      if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
          let _ = tokio::fs::create_dir_all(parent).await;
        }
      }
      tokio::fs::write(path, &bytes)
        .await
        .map_err(|e| crate::error::FerriError::Backend(format!("screenshot write {}: {e}", path.display())))?;
    }
    Ok(bytes)
  }

  // ── Editable check ───────────────────────────────────────────────────────

  /// Check whether the element is editable (not disabled and not read-only).
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or JS evaluation fails.
  pub async fn is_editable(&self) -> Result<bool> {
    self
      .eval_bool("function() { return !this.disabled && !this.readOnly; }")
      .await
  }

  // ── Blur ────────────────────────────────────────────────────────────────

  /// Remove focus from the element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found.
  pub async fn blur(&self) -> Result<()> {
    let el = self.resolve().await?;
    let _ = el.call_js_fn("function() { this.blur(); }").await;
    Ok(())
  }

  // ── Press sequentially ──────────────────────────────────────────────────

  /// Type text character by character with a delay between each.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or any key press fails.
  pub fn press_sequentially(&self, text: &str) -> crate::action::Action<'static, crate::options::TypeOptions, ()> {
    let locator = self.clone();
    let text = text.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { locator.press_sequentially_impl(&text, Some(opts)).await })
    })
  }

  pub(crate) async fn press_sequentially_impl(
    &self,
    text: &str,
    opts: Option<crate::options::TypeOptions>,
  ) -> Result<()> {
    // Playwright's `pressSequentially` shares the `TypeOptions` shape
    // with deprecated `type` (same three fields), so route both here.
    self.type_impl(text, opts).await
  }

  // ── Drag to another locator ─────────────────────────────────────────────

  /// Drag this element to `target`. Mirrors Playwright's
  /// `Locator.dragTo(target, options)` signature per
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:13293`.
  ///
  /// When `DragAndDropOptions::source_position` is set, the press point is
  /// the source element's padding-box origin offset by that point; otherwise
  /// the source element's center is used. Same for `target_position` on the
  /// release point. `DragAndDropOptions::steps` controls how many
  /// interpolated `mousemove` events are emitted between press and release
  /// (Playwright default: `1`). `DragAndDropOptions::trial` skips the
  /// actual mouse action, returning after both elements resolve.
  /// `DragAndDropOptions::strict` is ignored here (per Playwright) because
  /// this locator already carries its own strict flag.
  ///
  /// # Errors
  ///
  /// Returns an error if either element cannot be found, bounding box
  /// coordinates cannot be read, or the drag operation fails.
  pub fn drag_to(&self, target: &Locator) -> crate::action::Action<'static, crate::options::DragAndDropOptions, ()> {
    let locator = self.clone();
    let target = target.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.drag_to_impl(&target, Some(opts)).await }))
  }

  pub(crate) async fn drag_to_impl(
    &self,
    target: &Locator,
    options: Option<crate::options::DragAndDropOptions>,
  ) -> Result<()> {
    // `RECT_JS` scrolls the element into view and returns its full
    // padding-box rect so `sourcePosition` / `targetPosition` offset from
    // the origin as Playwright does.
    const RECT_JS: &str = "function() { try { this.scrollIntoViewIfNeeded(); } catch (e) { this.scrollIntoView(); } var r = this.getBoundingClientRect(); return {x: r.x, y: r.y, width: r.width, height: r.height}; }";
    let opts = options.unwrap_or_default();

    // Resolve source then target geometry under the action retry funnel
    // (Playwright resolves each with its own `_retryPointerAction`, source
    // first), so a node that detaches mid-drag is re-resolved rather than
    // surfacing a hard error.
    let src = retry_resolve!(self, opts.timeout, "dragTo", |el, _page| async move {
      el.call_js_fn_value(RECT_JS).await
    })?
    .ok_or_else(|| crate::error::FerriError::Backend("no source bounding box".into()))?;
    let tgt = retry_resolve!(target, opts.timeout, "dragTo", |el, _page| async move {
      el.call_js_fn_value(RECT_JS).await
    })?
    .ok_or_else(|| crate::error::FerriError::Backend("no target bounding box".into()))?;

    let from = rect_point(&src, opts.source_position);
    let to = rect_point(&tgt, opts.target_position);

    // Playwright's `trial: true` performs actionability checks (resolve) and
    // skips the actual action. We've already resolved both elements above,
    // so simply return without dispatching mouse events.
    if opts.trial.unwrap_or(false) {
      return Ok(());
    }

    let steps = opts.steps.unwrap_or(1);
    self.frame.page_arc().inner().click_and_drag(from, to, steps).await
  }

  // ── Drop a payload onto this element ────────────────────────────────────

  /// Drop a file/data payload onto this element. Mirrors Playwright's
  /// `Locator.drop(payload, options)` (`client/locator.ts:129`), which
  /// forwards to `frame._drop` -> server `dom.ts::_drop`.
  ///
  /// The drop is performed by constructing a `DataTransfer` carrying the
  /// payload's `File` objects (built from each `FilePayload`'s bytes) and
  /// `data` entries (`DataTransfer.setData(mimeType, value)`), then
  /// dispatching the `dragenter` / `dragover` / `drop` `DragEvent`
  /// sequence on the resolved element at the drop point. Matching
  /// Playwright, if the `dragover` handler does not call `preventDefault()`
  /// the target is treated as rejecting the drop: a `dragleave` is
  /// dispatched and a `FerriError::Backend` is returned.
  ///
  /// `DropOptions::position` offsets the drop point from the element's
  /// padding-box top-left; when absent the element center is used.
  /// `DropOptions::modifiers` are reflected on the dispatched `DragEvent`s'
  /// modifier flags. `DropOptions::timeout` is accepted for signature
  /// parity; the underlying single-shot resolve already honours the
  /// context's default action timeout.
  ///
  /// File paths in `DropPayload::files` are read into memory here (matching
  /// the co-located server path in Playwright) so the page can construct
  /// real `File` objects without filesystem access of its own.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be resolved, a referenced file
  /// path cannot be read, or the target rejects the drop.
  pub fn drop(
    &self,
    payload: crate::options::DropPayload,
  ) -> crate::action::Action<'static, crate::options::DropOptions, ()> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.drop_impl(payload, Some(opts)).await }))
  }

  pub(crate) async fn drop_impl(
    &self,
    payload: crate::options::DropPayload,
    options: Option<crate::options::DropOptions>,
  ) -> Result<()> {
    let opts = options.unwrap_or_default();

    // Lower the payload's files into `{name, mimeType, buffer(base64)}`
    // records the page can rebuild into `File` objects. `Paths` are read
    // from disk into buffers (Playwright's co-located server reads
    // localPaths the same way); `Payloads` carry their bytes already.
    let file_records = lower_drop_files(payload.files)?;

    let data_records: Vec<serde_json::Value> = payload
      .data
      .into_iter()
      .map(|(mime_type, value)| serde_json::json!({ "mimeType": mime_type, "value": value }))
      .collect();

    let modifiers = serde_json::json!({
      "alt": opts.modifiers.contains(&crate::options::Modifier::Alt),
      "ctrl": opts.modifiers.iter().any(|m| {
        matches!(m, crate::options::Modifier::Control)
          || (matches!(m, crate::options::Modifier::ControlOrMeta) && !cfg!(target_os = "macos"))
      }),
      "meta": opts.modifiers.iter().any(|m| {
        matches!(m, crate::options::Modifier::Meta)
          || (matches!(m, crate::options::Modifier::ControlOrMeta) && cfg!(target_os = "macos"))
      }),
      "shift": opts.modifiers.contains(&crate::options::Modifier::Shift),
    });

    let position = match opts.position {
      Some(p) => serde_json::json!({ "x": p.x, "y": p.y }),
      None => serde_json::Value::Null,
    };

    let arg = serde_json::json!({
      "payloads": file_records,
      "data": data_records,
      "modifiers": modifiers,
      "position": position,
    });
    let arg_json = serde_json::to_string(&arg)
      .map_err(|e| crate::error::FerriError::Backend(format!("failed to serialise drop payload: {e}")))?;

    let el = self.resolve().await?;
    let function = format!("function() {{ const arg = {arg_json}; {DROP_BODY} }}");
    let result = el.call_js_fn_value(&function).await?;

    match result.as_ref().and_then(serde_json::Value::as_str) {
      Some("accepted") => Ok(()),
      Some("not-accepted") => Err(crate::error::FerriError::Backend(
        "Drop target did not accept the drop -- its dragover handler did not call preventDefault()".into(),
      )),
      Some("error:notconnected") => Err(crate::error::FerriError::Backend(
        "Drop target element is not connected to the document".into(),
      )),
      _ => Err(crate::error::FerriError::Backend(
        "drop did not return a recognised status".into(),
      )),
    }
  }

  // ── Combinators ─────────────────────────────────────────────────────────

  /// Union: matches elements from either this or the other locator.
  /// Creates a new locator that matches elements matched by **either**
  /// selector. Mirrors Playwright's `Locator.or(locator)` exactly: emits
  /// `>> internal:or=<json>` where the injected selector engine handles the
  /// union.
  ///
  /// Unlike CSS `:is()`, this works for every selector engine including
  /// `text=`, `role=`, `label=`, `testid=`, and chained rich selectors.
  #[must_use]
  pub fn or(&self, other: &Locator) -> Locator {
    self.chain(&format!(
      "internal:or={}",
      serde_json::to_string(&other.selector).unwrap_or_else(|_| format!("{:?}", other.selector))
    ))
  }

  /// Creates a new locator that matches elements matched by **both** this
  /// locator and `other` on the same element. Mirrors Playwright's
  /// `Locator.and(locator)` — emits `>> internal:and=<json>`.
  ///
  /// This is a fundamentally different operation from `locator.locator(...)`
  /// which narrows scope to descendants; `and` requires the same element to
  /// satisfy both selectors.
  #[must_use]
  pub fn and(&self, other: &Locator) -> Locator {
    self.chain(&format!(
      "internal:and={}",
      serde_json::to_string(&other.selector).unwrap_or_else(|_| format!("{:?}", other.selector))
    ))
  }

  // ── All matches ─────────────────────────────────────────────────────────

  /// Return all matching locators as individual Locator instances.
  ///
  /// # Errors
  ///
  /// Returns an error if the count query fails due to selector parsing
  /// or JS evaluation errors.
  pub async fn all(&self) -> Result<Vec<Locator>> {
    let count = self.count().await?;
    let mut locators = Vec::with_capacity(count);
    let base = &self.selector;
    for i in 0..count {
      let idx = i32::try_from(i).unwrap_or(i32::MAX);
      let selector = if base.is_empty() {
        format!("nth={idx}")
      } else {
        format!("{base} >> nth={idx}")
      };
      locators.push(Locator {
        frame: self.frame.clone(),
        selector,
        strict: true,
      });
    }
    Ok(locators)
  }

  /// Get text content of all matching elements.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn all_text_contents(&self) -> Result<Vec<String>> {
    let parsed = selectors::parse(&self.selector)?;
    let parts_json = selectors::build_parts_json(&parsed);
    self.frame.page_arc().inner().ensure_engine_injected().await?;
    let fd = "window.__fd";
    let js = format!(
      "(function() {{ var r = {fd}._exec({parts_json}, document); \
       return r.map(function(e) {{ return (e.textContent || '').trim(); }}); }})()"
    );
    let val = self.frame.page_arc().inner().evaluate(&js).await?;
    match val {
      Some(serde_json::Value::Array(arr)) => Ok(
        arr
          .into_iter()
          .filter_map(|v| v.as_str().map(std::string::ToString::to_string))
          .collect(),
      ),
      _ => Ok(Vec::new()),
    }
  }

  /// Get inner text of all matching elements.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn all_inner_texts(&self) -> Result<Vec<String>> {
    // Same as all_text_contents for our implementation
    self.all_text_contents().await
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
    retry_resolve!(self, timeout_ms, "evaluate", |el, _page| async {
      let page_arc = Arc::clone(self.frame.page_arc());
      let handle = crate::element_handle::ElementHandle::from_any_element(page_arc, el).await?;
      let result = handle
        .as_js_handle()
        .evaluate(&fn_source, arg.clone(), is_function)
        .await;
      let _ = handle.dispose().await;
      result
    })
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
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout);
    loop {
      let now = tokio::time::Instant::now();
      if now >= deadline {
        return Err(crate::error::FerriError::timeout(
          format!("waiting for function on locator {}", self.selector),
          timeout,
        ));
      }
      let remaining = u64::try_from((deadline - now).as_millis()).unwrap_or(u64::MAX);
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
      tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
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
    retry_resolve!(self, timeout_ms, "evaluateHandle", |el, _page| async {
      let page_arc = Arc::clone(self.frame.page_arc());
      let handle = crate::element_handle::ElementHandle::from_any_element(page_arc, el).await?;
      let result = handle
        .as_js_handle()
        .evaluate_handle(&fn_source, arg.clone(), is_function)
        .await;
      let _ = handle.dispose().await;
      result
    })
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
    let parsed = selectors::parse(&self.selector)?;
    let parts_json = selectors::build_parts_json(&parsed);
    self.frame.page_arc().inner().ensure_engine_injected().await?;
    let probe = format!("() => window.__fd.selAll({parts_json})");
    let array_handle = self
      .frame
      .evaluate_handle(&probe, crate::protocol::SerializedArgument::default(), Some(true))
      .await?;
    let result = array_handle.evaluate(fn_source, arg, is_function).await;
    let _ = array_handle.dispose().await;
    result
  }

  // ── Page / Frame access ────────────────────────────────────────────────────

  /// Get the page this locator belongs to.
  #[must_use]
  pub fn page(&self) -> &Arc<crate::page::Page> {
    self.frame.page_arc()
  }

  /// The frame this locator resolves in. Mirrors Playwright's
  /// `locator._frame` — actions and queries always run in this frame's
  /// execution context.
  #[must_use]
  pub fn frame(&self) -> &crate::frame::Frame {
    &self.frame
  }

  /// Treat this locator as an `<iframe>` and return a `FrameLocator` for its content.
  ///
  /// Equivalent to Playwright's `locator.contentFrame()`. The returned
  /// `FrameLocator` creates locators scoped to the iframe's content document.
  #[must_use]
  pub fn content_frame(&self) -> FrameLocator {
    FrameLocator::for_iframe_in(self.frame.clone(), self.selector.clone())
  }

  /// Create a `FrameLocator` targeting an `<iframe>` matched by `selector` within
  /// this locator's scope.
  ///
  /// Equivalent to Playwright's `locator.frameLocator(selector)`.
  #[must_use]
  pub fn frame_locator(&self, selector: &str) -> FrameLocator {
    let frame_selector = if self.selector.is_empty() {
      selector.to_string()
    } else {
      format!("{} >> {selector}", self.selector)
    };
    FrameLocator::for_iframe_in(self.frame.clone(), frame_selector)
  }

  // ── Selector access ───────────────────────────────────────────────────────

  #[must_use]
  pub fn selector(&self) -> &str {
    &self.selector
  }

  // ── Handle materialisation (Playwright `locator.elementHandle`) ────

  /// Playwright: `locator.elementHandle(opts?): Promise<ElementHandle>`.
  /// Resolves this locator's selector once and returns a pinned
  /// [`crate::element_handle::ElementHandle`]. Throws when the
  /// selector matches no element — Playwright's behaviour (Playwright
  /// returns `null` from `$` but `elementHandle()` errors on miss).
  ///
  /// Phase-F MVP: no auto-wait — calls into the selector engine
  /// directly. Auto-wait + actionability are a phase-future addition
  /// once the locator's `retry_resolve!` macro is generalised to
  /// return an `ElementHandle` instead of running an action body.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError`] on selector parse failure,
  /// missing match, or strict-mode violation.
  pub async fn element_handle(&self) -> crate::error::Result<crate::element_handle::ElementHandle> {
    let page = self.frame.page_arc();
    page.inner().ensure_engine_injected().await?;
    let frame_id: Option<&str> = if self.frame.is_main_frame() {
      None
    } else {
      Some(self.frame.id())
    };
    let element = crate::selectors::query_one(page.inner(), &self.selector, self.strict, frame_id).await?;
    crate::element_handle::ElementHandle::from_any_element(Arc::clone(page), element).await
  }

  /// Playwright: `locator.elementHandles(): Promise<ElementHandle[]>`.
  /// Returns one handle per match in document order.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError`] on selector parse / protocol
  /// failure.
  pub async fn element_handles(&self) -> crate::error::Result<Vec<crate::element_handle::ElementHandle>> {
    let page = self.frame.page_arc();
    page.inner().ensure_engine_injected().await?;
    let frame_id: Option<&str> = if self.frame.is_main_frame() {
      None
    } else {
      Some(self.frame.id())
    };
    let matches = crate::selectors::query_all(page.inner(), &self.selector, frame_id).await?;
    let count = matches.len();
    let mut handles = Vec::with_capacity(count);
    for i in 0..count {
      let tagged = format!("window.__fd.selOne([{{\"engine\":\"css\",\"body\":\"[data-fd-sel='{i}']\"}}])");
      match page.inner().evaluate_to_element(&tagged, frame_id).await {
        Ok(element) => {
          handles.push(crate::element_handle::ElementHandle::from_any_element(Arc::clone(page), element).await?);
        },
        Err(err) => {
          crate::selectors::cleanup_tags(page.inner()).await;
          return Err(err);
        },
      }
    }
    crate::selectors::cleanup_tags(page.inner()).await;
    Ok(handles)
  }

  /// Whether this locator runs action methods under strict mode (multi-match
  /// is an error). Mirrors Playwright's default.
  #[must_use]
  pub fn is_strict(&self) -> bool {
    self.strict
  }

  // ── Core retry system ─────────────────────────────────────────────────────
  //
  // Matches Playwright's retryWithProgressAndTimeouts + _retryWithProgressIfNotConnected
  // + _callOnElementOnceMatches. ALL element operations go through one of these two
  // methods. Retry backoff: [0, 20, 50, 100, 100, 500]ms (same as Playwright).

  /// Backoff schedule matching Playwright's retryWithProgressAndTimeouts.
  const RETRY_BACKOFFS_MS: &'static [u64] = &[0, 0, 20, 50, 100, 100, 500];

  /// Resolve element + run JS callback in ONE CDP call, with retry.
  /// Used by: innerText, textContent, innerHTML, getAttribute, inputValue, isVisible, etc.
  /// Matches Playwright's `_callOnElementOnceMatches`.
  async fn retry_eval_on_element(&self, js_body: &str) -> Result<Option<serde_json::Value>> {
    for (i, &delay_ms) in Self::RETRY_BACKOFFS_MS.iter().enumerate() {
      if delay_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
      }
      // Re-resolve frameLocator enter-frame hops EVERY attempt so a
      // child frame that wasn't loaded / had no execution context yet
      // (or got re-attached) is picked up on a later retry. No-op for
      // plain selectors.
      let attempt: Result<Option<serde_json::Value>> = async {
        let (rf, rsel) = self.resolved().await?;
        let parsed = selectors::parse(&rsel)?;
        let parts_json = selectors::build_parts_json(&parsed);
        let inner = rf.page_arc().inner();
        inner.ensure_engine_injected().await?;
        let fd = "window.__fd";
        let js = format!("(function() {{ var el = {fd}.selOne({parts_json}); if (!el) return null; {js_body} }})()");
        if rf.is_main_frame() {
          inner.evaluate(&js).await
        } else {
          inner.evaluate_in_frame(&js, rf.id()).await
        }
      }
      .await;
      match attempt {
        // Element not found, frame not ready, or eval failed -- retry
        // if attempts remain.
        Ok(Some(serde_json::Value::Null) | None) | Err(_) if i < Self::RETRY_BACKOFFS_MS.len() - 1 => {},
        Ok(val) => return Ok(val),
        Err(e) => return Err(e),
      }
    }
    Ok(None)
  }

  // ── Internal helpers ────────────────────────────────────────────────────────

  /// Resolve the locator to a concrete element.
  ///
  /// # Errors
  ///
  /// Returns an error if the selector engine cannot be injected or the element is not found.
  pub async fn resolve(&self) -> Result<AnyElement> {
    // Resolve frameLocator enter-frame hops to the real child frame +
    // trailing selector (no-op for plain selectors). Without this, an
    // enter-frame selector queries the raw chain in the parent frame,
    // where the engine's `enter-frame` control returns `[]` by design.
    let (rframe, rsel) = self.resolved().await?;
    rframe.page_arc().inner().ensure_engine_injected().await?;
    let fd = "window.__fd";
    let sel_js = selectors::build_selone_js(&rsel, fd, self.strict)?;
    let frame_id: Option<&str> = if rframe.is_main_frame() {
      None
    } else {
      Some(rframe.id())
    };
    selectors::query_one_prebuilt(rframe.page_arc().inner(), &sel_js, &rsel, frame_id).await
  }

  fn chain(&self, sub: &str) -> Locator {
    let selector = if self.selector.is_empty() {
      sub.to_string()
    } else {
      format!("{} >> {sub}", self.selector)
    };
    Locator {
      frame: self.frame.clone(),
      selector,
      strict: self.strict,
    }
  }

  async fn eval_prop(&self, prop: &str) -> Result<Option<String>> {
    let val = self
      .retry_eval_on_element(&format!("var v = el.{prop}; return v == null ? null : String(v);"))
      .await?;
    Ok(val.and_then(|v| match v {
      serde_json::Value::String(s) => Some(s),
      serde_json::Value::Null => None,
      other => Some(other.to_string()),
    }))
  }

  async fn eval_bool(&self, func: &str) -> Result<bool> {
    let val = self
      .retry_eval_on_element(&format!("return !!({func}).call(el);"))
      .await?;
    Ok(val.and_then(|v| v.as_bool()).unwrap_or(false))
  }

  /// Legacy: non-retrying eval for callers that handle retry themselves.
  async fn eval_on_element(&self, js_body: &str) -> Result<Option<serde_json::Value>> {
    // Resolve enter-frame hops so an iframe-scoped locator evaluates in
    // its child frame (no-op for plain selectors); the raw chain would
    // return `[]` from the engine's `enter-frame` control.
    let (rframe, rsel) = self.resolved().await?;
    let parsed = selectors::parse(&rsel)?;
    let parts_json = selectors::build_parts_json(&parsed);
    let inner = rframe.page_arc().inner();
    inner.ensure_engine_injected().await?;
    let fd = "window.__fd";
    let js = format!("(function() {{ var el = {fd}.selOne({parts_json}); if (!el) return null; {js_body} }})()");
    if rframe.is_main_frame() {
      inner.evaluate(&js).await
    } else {
      inner.evaluate_in_frame(&js, rframe.id()).await
    }
  }
}

impl std::fmt::Debug for Locator {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Locator")
      .field("selector", &self.selector)
      .field("frame", &self.frame)
      .field("strict", &self.strict)
      .finish()
  }
}

// ── FrameLocator ──────────────────────────────────────────────────────────────

/// A selector-builder that produces parent-frame [`Locator`]s targeting
/// content inside an `<iframe>`. Mirrors Playwright's `FrameLocator`
/// exactly:
///
/// `/tmp/playwright/packages/playwright-core/src/client/locator.ts::FrameLocatorImpl`
///
/// Holds the parent [`crate::Frame`] (the one whose document contains the
/// `<iframe>` element) and the iframe's CSS-selector chain. Every
/// builder method composes a Locator selector with
/// `>> internal:control=enter-frame >>` so the iframe traversal is
/// performed by the selector engine at action time — not eagerly at
/// construction. The resulting `Locator` is the same `Locator` type
/// used everywhere else (no separate iframe-aware locator).
///
/// All methods are synchronous; `internal:control=enter-frame` is the
/// engine-side directive that switches root from the iframe element to
/// its `contentDocument` when a subsequent selector part runs.
#[derive(Clone)]
pub struct FrameLocator {
  /// Parent frame whose document contains the `<iframe>` element.
  /// For top-level `page.frame_locator(sel)` this is the main frame;
  /// nested `frame_locator.frame_locator(sel)` keeps the same parent
  /// frame and just appends to the selector chain.
  frame: crate::frame::Frame,
  /// CSS-selector chain ending at the `<iframe>` element. Composed with
  /// `>> internal:control=enter-frame >>` whenever we step further in.
  frame_selector: String,
}

impl FrameLocator {
  /// Construct a `FrameLocator` for an `<iframe>` matched by
  /// `iframe_selector` inside `parent_frame`'s document. Sync.
  #[must_use]
  pub fn for_iframe_in(parent_frame: crate::frame::Frame, iframe_selector: String) -> Self {
    Self {
      frame: parent_frame,
      frame_selector: iframe_selector,
    }
  }

  fn enter(&self, selector: &str) -> String {
    format!("{} >> internal:control=enter-frame >> {selector}", self.frame_selector)
  }

  /// Locator inside this iframe. Mirrors Playwright's
  /// `frameLocator.locator(selector, options?)` — sync, returns a
  /// `Locator` bound to the parent frame with an `enter-frame`
  /// selector chain. The options-bag form is [`Self::locator_with`].
  #[must_use]
  pub fn locator(&self, selector: &str) -> Locator {
    Locator::new(self.frame.clone(), self.enter(selector))
  }

  /// [`Self::locator`] with Playwright's `LocatorOptions` filter bag.
  #[must_use]
  pub fn locator_with(&self, selector: &str, options: &crate::options::FilterOptions) -> Locator {
    self.locator(selector).filter(options)
  }

  /// `getByRole` inside this iframe.
  #[must_use]
  pub fn get_by_role(
    &self,
    role: impl Into<crate::options::Role>,
  ) -> crate::locator_builder::LocatorBuilder<RoleOptions> {
    let fl = self.clone();
    let role = role.into();
    crate::locator_builder::LocatorBuilder::new(move |opts| fl.locator(&build_role_selector(role.as_str(), opts)))
  }

  /// `getByText` inside this iframe.
  #[must_use]
  pub fn get_by_text(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.text_like_builder("internal:text", text.into())
  }

  /// `getByTestId` inside this iframe.
  #[must_use]
  pub fn get_by_test_id(&self, test_id: impl Into<StringOrRegex>) -> Locator {
    self.locator(&build_testid_selector("data-testid", &test_id.into()))
  }

  /// `getByLabel` inside this iframe.
  #[must_use]
  pub fn get_by_label(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.text_like_builder("internal:label", text.into())
  }

  /// `getByPlaceholder` inside this iframe.
  #[must_use]
  pub fn get_by_placeholder(
    &self,
    text: impl Into<StringOrRegex>,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.attr_builder("placeholder", text.into())
  }

  /// `getByAltText` inside this iframe.
  #[must_use]
  pub fn get_by_alt_text(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.attr_builder("alt", text.into())
  }

  /// `getByTitle` inside this iframe.
  #[must_use]
  pub fn get_by_title(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.attr_builder("title", text.into())
  }

  fn text_like_builder(
    &self,
    kind: &'static str,
    text: StringOrRegex,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    let fl = self.clone();
    crate::locator_builder::LocatorBuilder::new(move |opts| fl.locator(&build_text_like_selector(kind, &text, opts)))
  }

  fn attr_builder(
    &self,
    attr: &'static str,
    text: StringOrRegex,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    let fl = self.clone();
    crate::locator_builder::LocatorBuilder::new(move |opts| fl.locator(&build_attr_selector(attr, &text, opts)))
  }

  /// The locator pointing at the `<iframe>` element itself, in the
  /// parent frame's context. Mirrors Playwright's `frameLocator.owner()`.
  #[must_use]
  pub fn owner(&self) -> Locator {
    Locator::new(self.frame.clone(), self.frame_selector.clone())
  }

  /// `frameLocator` for a nested `<iframe>`. Mirrors Playwright's
  /// `frameLocator.frameLocator(selector)` — appends an enter-frame
  /// step plus the next iframe selector.
  #[must_use]
  pub fn frame_locator(&self, selector: &str) -> FrameLocator {
    FrameLocator {
      frame: self.frame.clone(),
      frame_selector: self.enter(selector),
    }
  }

  /// First matching iframe (`nth=0`).
  #[must_use]
  pub fn first(&self) -> FrameLocator {
    FrameLocator {
      frame: self.frame.clone(),
      frame_selector: format!("{} >> nth=0", self.frame_selector),
    }
  }

  /// Last matching iframe (`nth=-1`).
  #[must_use]
  pub fn last(&self) -> FrameLocator {
    FrameLocator {
      frame: self.frame.clone(),
      frame_selector: format!("{} >> nth=-1", self.frame_selector),
    }
  }

  /// Nth matching iframe.
  #[must_use]
  pub fn nth(&self, index: i32) -> FrameLocator {
    FrameLocator {
      frame: self.frame.clone(),
      frame_selector: format!("{} >> nth={index}", self.frame_selector),
    }
  }
}

impl std::fmt::Debug for FrameLocator {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("FrameLocator")
      .field("frame_selector", &self.frame_selector)
      .field("frame", &self.frame)
      .finish()
  }
}

// ── Selector builders ───────────────────────────────────────────────────────

/// Compute the drag press/release point from an element's bounding rect and
/// an optional `position`. When `position` is `Some`, the point is the
/// element's padding-box top-left offset by `(position.x, position.y)` —
/// matching Playwright's `sourcePosition` / `targetPosition` semantics. When
/// `position` is `None`, the element's center is used.
/// Page-side body for [`Locator::drop`]. Runs with `this` bound to the
/// target element and a JSON-literal `arg` ({payloads, data, modifiers,
/// position}) in scope. Mirrors Playwright's `dom.ts::_drop` page-side
/// closure: builds a `DataTransfer`, adds the `File` objects + `setData`
/// entries, then dispatches `dragenter` / `dragover` / `drop`. Returns a
/// status string the Rust side maps to `Ok`/`Err`.
const DROP_BODY: &str = r"
  if (!this.isConnected || this.nodeType !== 1) return 'error:notconnected';
  this.scrollIntoViewIfNeeded ? this.scrollIntoViewIfNeeded() : this.scrollIntoView();
  const r = this.getBoundingClientRect();
  const point = arg.position
    ? { x: r.x + arg.position.x, y: r.y + arg.position.y }
    : { x: r.x + r.width / 2, y: r.y + r.height / 2 };
  const dt = new DataTransfer();
  for (const p of arg.payloads) {
    const bin = atob(p.buffer);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    dt.items.add(new File([bytes], p.name, { type: p.mimeType }));
  }
  for (const entry of arg.data) dt.setData(entry.mimeType, entry.value);
  const makeEvent = (type) => new DragEvent(type, {
    bubbles: true,
    cancelable: true,
    composed: true,
    clientX: point.x,
    clientY: point.y,
    altKey: arg.modifiers.alt,
    ctrlKey: arg.modifiers.ctrl,
    metaKey: arg.modifiers.meta,
    shiftKey: arg.modifiers.shift,
    dataTransfer: dt,
  });
  this.dispatchEvent(makeEvent('dragenter'));
  const over = makeEvent('dragover');
  this.dispatchEvent(over);
  if (!over.defaultPrevented) {
    this.dispatchEvent(makeEvent('dragleave'));
    return 'not-accepted';
  }
  this.dispatchEvent(makeEvent('drop'));
  return 'accepted';
";

/// Lower a [`crate::options::DropPayload`]'s files into the page-side
/// `{name, mimeType, buffer(base64)}` records that [`DROP_BODY`] rebuilds
/// into `File` objects. Disk paths are read into buffers (matching
/// Playwright's co-located server path); in-memory payloads carry their
/// bytes already.
fn lower_drop_files(files: Option<crate::options::InputFiles>) -> Result<Vec<serde_json::Value>> {
  use base64::Engine as _;
  match files {
    Some(crate::options::InputFiles::Paths(paths)) => {
      let mut out = Vec::with_capacity(paths.len());
      for p in paths {
        let bytes = std::fs::read(&p)
          .map_err(|e| crate::error::FerriError::Backend(format!("failed to read drop file {}: {e}", p.display())))?;
        let name = p
          .file_name()
          .map(|n| n.to_string_lossy().into_owned())
          .unwrap_or_default();
        out.push(serde_json::json!({
          "name": name,
          "mimeType": guess_mime_type(&name),
          "buffer": base64::engine::general_purpose::STANDARD.encode(&bytes),
        }));
      }
      Ok(out)
    },
    Some(crate::options::InputFiles::Payloads(payloads)) => Ok(
      payloads
        .into_iter()
        .map(|p| {
          let mime = if p.mime_type.is_empty() {
            "application/octet-stream".to_string()
          } else {
            p.mime_type
          };
          serde_json::json!({
            "name": p.name,
            "mimeType": mime,
            "buffer": base64::engine::general_purpose::STANDARD.encode(&p.buffer),
          })
        })
        .collect(),
    ),
    None => Ok(Vec::new()),
  }
}

/// Best-effort MIME type for a filename based on its extension. Used by
/// [`Locator::drop`] when reading `DropPayload` file paths from disk, where
/// the caller did not supply an explicit MIME type. Falls back to
/// `application/octet-stream` for unknown extensions.
fn guess_mime_type(name: &str) -> &'static str {
  let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
  match ext.as_str() {
    "txt" | "text" => "text/plain",
    "html" | "htm" => "text/html",
    "css" => "text/css",
    "csv" => "text/csv",
    "js" | "mjs" => "text/javascript",
    "json" => "application/json",
    "xml" => "application/xml",
    "pdf" => "application/pdf",
    "png" => "image/png",
    "jpg" | "jpeg" => "image/jpeg",
    "gif" => "image/gif",
    "svg" => "image/svg+xml",
    "webp" => "image/webp",
    "zip" => "application/zip",
    _ => "application/octet-stream",
  }
}

fn rect_point(rect: &serde_json::Value, position: Option<crate::options::Point>) -> (f64, f64) {
  let x = rect.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
  let y = rect.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
  let width = rect.get("width").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
  let height = rect.get("height").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
  match position {
    Some(p) => (x + p.x, y + p.y),
    None => (x + width / 2.0, y + height / 2.0),
  }
}

pub(crate) fn build_role_selector(role: &str, opts: &RoleOptions) -> String {
  let mut sel = format!("internal:role={role}");
  if let Some(name) = &opts.name {
    let escaped = escape_for_attribute_selector(name, opts.exact == Some(true));
    let _ = write!(sel, "[name={escaped}]");
  }
  if let Some(description) = &opts.description {
    let escaped = escape_for_attribute_selector(description, opts.exact == Some(true));
    let _ = write!(sel, "[description={escaped}]");
  }
  if let Some(checked) = opts.checked {
    let _ = write!(sel, "[checked={checked}]");
  }
  if let Some(disabled) = opts.disabled {
    let _ = write!(sel, "[disabled={disabled}]");
  }
  if let Some(expanded) = opts.expanded {
    let _ = write!(sel, "[expanded={expanded}]");
  }
  if let Some(level) = opts.level {
    let _ = write!(sel, "[level={level}]");
  }
  if let Some(pressed) = opts.pressed {
    let _ = write!(sel, "[pressed={pressed}]");
  }
  if let Some(selected) = opts.selected {
    let _ = write!(sel, "[selected={selected}]");
  }
  if let Some(include_hidden) = opts.include_hidden {
    let _ = write!(sel, "[include-hidden={include_hidden}]");
  }
  sel
}

/// Build a Playwright-native text-engine selector body. `engine_prefix`
/// is one of `internal:text` / `internal:label`. For `text: String` we
/// emit `"quoted"i` / `"quoted"s`; for `text: Regex` we emit
/// `/source/flags`. Mirrors
/// `packages/isomorphic/stringUtils.ts::escapeForTextSelector` from
/// `/tmp/playwright`.
pub(crate) fn build_text_like_selector(engine_prefix: &str, text: &StringOrRegex, opts: &TextOptions) -> String {
  let body = escape_for_text_selector(text, opts.exact == Some(true));
  format!("{engine_prefix}={body}")
}

/// Build a Playwright-native attribute-engine selector body of the
/// form `internal:attr=[<name>=<escaped>]` for `get_by_alt_text`,
/// `get_by_title`, `get_by_placeholder`. Mirrors
/// `packages/isomorphic/locatorUtils.ts::getByAttributeTextSelector`.
pub(crate) fn build_attr_selector(attr: &str, value: &StringOrRegex, opts: &TextOptions) -> String {
  let escaped = escape_for_attribute_selector(value, opts.exact == Some(true));
  format!("internal:attr=[{attr}={escaped}]")
}

/// Build a `get_by_test_id` selector. Testid matches are always exact
/// per Playwright.
pub(crate) fn build_testid_selector(attr_name: &str, testid: &StringOrRegex) -> String {
  let escaped = escape_for_attribute_selector(testid, true);
  format!("internal:testid=[{attr_name}={escaped}]")
}

/// Port of Playwright's `escapeForTextSelector` from
/// `/tmp/playwright/packages/isomorphic/stringUtils.ts`.
///
/// For a literal string returns `"quoted"i` (substring,
/// case-insensitive) or `"quoted"s` (exact, case-sensitive). For a
/// regex returns the `/source/flags` literal form with `>>` escaped so
/// the selector chain operator doesn't collide. `unicode`/`unicodeSets`
/// regex flags are preserved — the injected engine's `RegExp`
/// construction handles them natively.
fn escape_for_text_selector(value: &StringOrRegex, exact: bool) -> String {
  match value {
    StringOrRegex::String(s) => {
      let quoted = serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""));
      format!("{quoted}{}", if exact { "s" } else { "i" })
    },
    StringOrRegex::Regex { source, flags } => escape_regex_for_selector(source, flags),
  }
}

/// Port of Playwright's `escapeForAttributeSelector`.
fn escape_for_attribute_selector(value: &StringOrRegex, exact: bool) -> String {
  match value {
    StringOrRegex::String(s) => {
      let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
      format!("\"{escaped}\"{}", if exact { "s" } else { "i" })
    },
    StringOrRegex::Regex { source, flags } => escape_regex_for_selector(source, flags),
  }
}

/// Port of Playwright's `escapeRegexForSelector`. For regexes with
/// `unicode` / `unicodeSets` flags we emit the source verbatim (per the
/// upstream comment — identity character escapes aren't allowed in
/// those modes). Otherwise we escape `>>` so the selector chain
/// operator doesn't collide with the regex literal.
fn escape_regex_for_selector(source: &str, flags: &str) -> String {
  let has_unicode = flags.contains('u') || flags.contains('v');
  if has_unicode {
    format!("/{source}/{flags}")
  } else {
    // Escape `>>` as `\>\>` so the selector chain operator can't
    // prematurely split the regex literal. Matches Playwright's
    // `String(re).replace(/>>/g, '\\>\\>')` step — quote escaping is
    // only needed for attribute selectors, which our callers handle
    // separately.
    let escaped_source = source.replace(">>", "\\>\\>");
    format!("/{escaped_source}/{flags}")
  }
}

/// Produce the Playwright-compatible JSON-quoted form of an inner
/// selector string — matches the output of `JSON.stringify(str)` in JS.
/// Used by [`Locator::filter`] and [`Locator::and`] / [`Locator::or`]
/// when embedding nested selector text in `internal:*` clauses.
///
/// Falls back to a Rust `{:?}` debug form if `serde_json` cannot encode
/// (impossible for valid UTF-8 strings, but kept for defensive symmetry
/// with existing call sites at `and` / `or`).
fn json_quote(s: &str) -> String {
  serde_json::to_string(s).unwrap_or_else(|_| format!("{s:?}"))
}

// ── ariaSnapshot cross-iframe stitching ─────────────────────────────────────
//
// Mirrors Playwright's server `ariaSnapshotForFrame` /
// `ariaSnapshotFrameRef`
// (`/tmp/playwright/packages/playwright-core/src/server/page.ts:1103`).
// The injected `incrementalAriaSnapshot` renders one frame and reports
// the `<iframe>` nodes it gave refs to (`iframeRefs` / `iframeDepths`);
// the host resolves each into its child browsing context and splices
// the child render beneath the parent's `- iframe [ref=...]` line.

/// Decoded result of `window.__fd.incrementalAriaSnapshot`.
#[derive(serde::Deserialize, Default)]
struct AriaRaw {
  #[serde(default)]
  full: String,
  /// May contain `null` entries — `mode: 'default'` assigns no refs, so
  /// the renderer pushes `undefined` for un-reffed iframes (filtered out
  /// exactly like Playwright's `ref in iframeDepths` check).
  #[serde(default, rename = "iframeRefs")]
  iframe_refs: Vec<Option<String>>,
  #[serde(default, rename = "iframeDepths")]
  iframe_depths: std::collections::HashMap<String, i32>,
}

/// Build the `AriaTreeOptions` JSON for the injected call. `refPrefix`
/// is omitted when empty (top frame) — the vendored injected treats
/// missing and `''` identically (`options.refPrefix ?? ''`).
fn aria_opts_json(mode: &str, depth: Option<i32>, ref_prefix: &str, boxes: Option<bool>) -> String {
  let mut m = serde_json::Map::new();
  m.insert("mode".into(), serde_json::Value::String(mode.to_string()));
  if let Some(d) = depth {
    m.insert("depth".into(), serde_json::Value::Number(d.into()));
  }
  if !ref_prefix.is_empty() {
    m.insert("refPrefix".into(), serde_json::Value::String(ref_prefix.to_string()));
  }
  if boxes == Some(true) {
    m.insert("boxes".into(), serde_json::Value::Bool(true));
  }
  serde_json::Value::Object(m).to_string()
}

fn parse_aria_raw(s: &str) -> Result<AriaRaw> {
  if s.trim().is_empty() {
    return Ok(AriaRaw::default());
  }
  serde_json::from_str(s).map_err(|e| crate::error::FerriError::evaluation(format!("ariaSnapshot parse: {e}")))
}

/// Compiled-once matcher for the `- iframe [ref=...]` lines
/// `aria_stitch_frame` splices child snapshots under. `None` is
/// unreachable for the literal pattern but preserves the error path
/// without an unwrap.
static IFRAME_REF_RE: std::sync::OnceLock<Option<regex::Regex>> = std::sync::OnceLock::new();

/// Render `frame` (already snapshotted into `raw`), then recurse into
/// every rendered child iframe and splice the child lines under the
/// matching `- iframe [ref=...]` line. Boxed so the
/// frame -> child-frame recursion has a finite future size.
fn aria_stitch_frame(
  frame: crate::frame::Frame,
  raw: AriaRaw,
  mode: String,
  depth: Option<i32>,
  boxes: Option<bool>,
  seq: Arc<std::sync::atomic::AtomicU32>,
) -> futures::future::BoxFuture<'static, Result<Vec<String>>> {
  Box::pin(async move {
    // Playwright: `iframeRefs.filter(ref => ref in iframeDepths)` —
    // preserves order so `indexOf(ref)` aligns with `childSnapshots`.
    let rendered: Vec<String> = raw
      .iframe_refs
      .iter()
      .flatten()
      .filter(|r| raw.iframe_depths.contains_key(*r))
      .cloned()
      .collect();

    let mut child_snaps: Vec<Vec<String>> = Vec::with_capacity(rendered.len());
    for refv in &rendered {
      child_snaps.push(aria_child_snapshot(&frame, refv, &mode, depth, boxes, &raw.iframe_depths, &seq).await?);
    }

    let re = IFRAME_REF_RE
      .get_or_init(|| regex::Regex::new(r"^(\s*)- iframe (?:\[active\] )?\[ref=([^\]]*)\]").ok())
      .as_ref()
      .ok_or_else(|| crate::error::FerriError::evaluation("ariaSnapshot iframe regex failed to compile".to_string()))?;

    let mut out: Vec<String> = Vec::new();
    for line in raw.full.split('\n') {
      let Some(caps) = re.captures(line) else {
        out.push(line.to_string());
        continue;
      };
      let leading = caps.get(1).map_or("", |m| m.as_str());
      let refv = caps.get(2).map_or("", |m| m.as_str());
      let child = rendered.iter().position(|x| x == refv).map(|i| &child_snaps[i]);
      let has = child.is_some_and(|c| !c.is_empty());
      out.push(if has { format!("{line}:") } else { line.to_string() });
      if let Some(child_lines) = child {
        for l in child_lines {
          out.push(format!("{leading}  {l}"));
        }
      }
    }
    Ok(out)
  })
}

/// Resolve one child iframe (`refv`) into its content frame, snapshot
/// its `<body>`, and recurse. Mirrors `ariaSnapshotFrameRef`. A missing
/// element / unresolvable content frame yields an empty render (the
/// parent then keeps the bare `- iframe [ref=...]` line — Playwright
/// parity).
async fn aria_child_snapshot(
  frame: &crate::frame::Frame,
  refv: &str,
  mode: &str,
  depth: Option<i32>,
  boxes: Option<bool>,
  depths: &std::collections::HashMap<String, i32>,
  seq: &Arc<std::sync::atomic::AtomicU32>,
) -> Result<Vec<String>> {
  // Tag the iframe in JS, then re-resolve it through the normal
  // selector + content-frame path (the same one `frameLocator` uses).
  // Passing a utility-eval JSHandle straight into `content_frame()`
  // breaks on BiDi ("no such handle" — the cross-context handle is not
  // valid for the contentWindow call); a freshly queried element is.
  const ARIA_FRAME_ATTR: &str = "data-fd-aria-ref";
  let ref_json = serde_json::to_string(refv).unwrap_or_else(|_| format!("{refv:?}"));
  let mark_js = format!("() => window.__fd.markIframeByAriaRef({ref_json}, \"{ARIA_FRAME_ATTR}\")");
  let marked = matches!(
    frame
      .evaluate(&mark_js, crate::protocol::SerializedArgument::default(), Some(true))
      .await?,
    crate::protocol::SerializedValue::Bool(true)
  );
  if !marked {
    return Ok(Vec::new());
  }
  let page = frame.page_arc();
  let frame_id: Option<&str> = if frame.is_main_frame() { None } else { Some(frame.id()) };
  let sel = format!("[{ARIA_FRAME_ATTR}=\"{refv}\"]");
  let Ok(iframe_node) = selectors::query_one(page.inner(), &sel, false, frame_id).await else {
    return Ok(Vec::new());
  };
  let iframe_handle = crate::element_handle::ElementHandle::from_any_element(Arc::clone(page), iframe_node).await?;
  let Some(child_frame) = iframe_handle.content_frame().await? else {
    return Ok(Vec::new());
  };

  // Playwright: `childDepth = options.depth ? depth - iframeDepth - 1
  // : undefined` — `0` is falsy there too, so it maps to "unlimited".
  let iframe_depth = depths.get(refv).copied().unwrap_or(0);
  let child_depth = match depth {
    Some(d) if d != 0 => Some(d - iframe_depth - 1),
    _ => None,
  };

  // Unique per-frame ref prefix so `[ref=...]` never collides across
  // frames (Playwright uses `frame.seq`; we just need uniqueness).
  let n = seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
  let prefix = format!("f{n}");
  let copts = aria_opts_json(mode, child_depth, &prefix, boxes);
  let body_js = format!("() => JSON.stringify(window.__fd.incrementalAriaSnapshot(document.body, {copts}))");
  let raw_s = child_frame
    .evaluate(&body_js, crate::protocol::SerializedArgument::default(), Some(true))
    .await?
    .as_str()
    .map(std::string::ToString::to_string)
    .unwrap_or_default();
  if raw_s.is_empty() {
    return Ok(Vec::new());
  }
  let child_raw = parse_aria_raw(&raw_s)?;
  aria_stitch_frame(
    child_frame,
    child_raw,
    mode.to_string(),
    child_depth,
    boxes,
    Arc::clone(seq),
  )
  .await
}

#[cfg(test)]
mod retry_classify_tests {
  use super::is_retryable_action_error;

  #[test]
  fn stale_and_actionability_errors_are_retryable() {
    for msg in [
      // Actionability signals (pre-existing behaviour).
      "error:notvisible",
      "error:notenabled",
      "element is not connected",
      "element is detached",
      // Stale-handle / navigated-away signals (the fix): a resolved
      // node vanished mid-action and must be re-resolved, not surfaced
      // as a hard error.
      "protocol error (CDP): Object id doesn't reference a Node",
      "Could not find node with given id",
      "No node with given id found",
      "Node with given id does not belong to the document",
      "Execution context was destroyed.",
      "Cannot find context with specified id",
      "Node was not found",
    ] {
      assert!(is_retryable_action_error(msg), "should be retryable: {msg}");
    }
  }

  #[test]
  fn genuine_failures_are_not_retryable() {
    for msg in [
      "strict mode violation: 3 elements",
      "Timeout 30000ms exceeded",
      "Target closed",
      "boom",
    ] {
      assert!(!is_retryable_action_error(msg), "should NOT be retryable: {msg}");
    }
  }
}
