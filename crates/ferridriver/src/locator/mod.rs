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

use crate::backend::AnyElement;
use crate::error::Result;
use crate::options::{FilterOptions, RoleOptions, StringOrRegex, TextOptions};
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
  // Actions whose arguments a reader (or a generated test) needs pass them as
  // the fourth argument; an action that takes none uses the short form. The
  // trace and the echoed source both render exactly these, so an argument
  // omitted here is an argument missing from the code the run hands back.
  ($self:expr, $timeout_ms:expr, $op:expr, |$el:ident, $page:ident| $body:expr) => {
    retry_resolve!($self, $timeout_ms, $op, ::serde_json::json!({}), |$el, $page| $body)
  };
  ($self:expr, $timeout_ms:expr, $op:expr, $extra:expr, |$el:ident, $page:ident| $body:expr) => {{
    // Trace span for the whole retried action — every exit path below
    // funnels through `break 'retry` so the span always closes with the
    // final outcome (timeout / strict violation / success / hard error).
    // When no trace recorder, action observer, debugger gate, or pending-call
    // tracker is active, skip building the JSON params map and `"page@..."`
    // string completely.
    let __trace_page = $self.frame.page_arc();
    let __trace_span = if $crate::trace::call_origins_wanted() {
      ::std::boxed::Box::pin($crate::locator::begin_locator_trace(
        &__trace_page,
        &$self.selector,
        $op,
        $extra,
      ))
      .await
    } else {
      ::std::option::Option::None
    };
    let __op_name: &str = $op;
    // Playwright's `Call log:`, lazily materialized only on tracing, retry,
    // abort, or timeout so the common first-try success path pays zero
    // allocations and skips `as_locator` selector re-parsing.
    let mut __call_log = $crate::locator::LazyCallLog::new(&$self.selector, __op_name);
    if let ::std::option::Option::Some(__s) = __trace_span.as_ref() {
      __s.log(__call_log.waiting_line().to_owned());
    }
    // Input-time capture (target mark + input@ snapshot + point) fires
    // once, on the first successful resolution.
    let mut __trace_input_pending = __trace_span.is_some();
    let __resolved_timeout: u64 = $timeout_ms.unwrap_or_else(|| $self.frame.page_arc().default_timeout());
    let __deadline: ::std::option::Option<::std::time::Instant> = if __resolved_timeout == 0 {
      ::std::option::Option::None
    } else {
      ::std::option::Option::Some(::std::time::Instant::now() + ::std::time::Duration::from_millis(__resolved_timeout))
    };
    let __expired = || __deadline.is_some_and(|__d| ::std::time::Instant::now() >= __d);
    // Every protocol command of every attempt waits at most until the
    // action's deadline, so a stalled browser fails the action on time.
    // The attempt itself is not cut off: it reports why it failed, which
    // the call log carries (Playwright's "element is outside of the
    // viewport").
    let __budget = $crate::operation_budget::OperationBudget::operation(__resolved_timeout);
    let __result = match __budget {
      ::std::result::Result::Err(__e) => ::std::result::Result::Err(__e),
      ::std::result::Result::Ok(__budget) => {
        __budget
          .scope(async {
            'retry: {
              let __signal = $crate::abort::current();
              if let ::std::option::Option::Some(__signal) = __signal.as_ref()
                && __signal.was_refused()
              {
                break 'retry ::std::result::Result::Err(__signal.error());
              }
              let mut __idx: usize = 0;
              // Tries of the action itself (an element was found and acted on),
              // which pick each pointer try's scroll alignment.
              let mut __tries: usize = 0;
              loop {
                if let ::std::option::Option::Some(__signal) = __signal.as_ref()
                  && __signal.is_aborted()
                {
                  let __error = __signal.error();
                  __call_log.push_aborted(&__error);
                  break 'retry ::std::result::Result::Err(__error.with_call_log(__call_log.into_vec()));
                }
                // Deadline check up-front so we never race into one more attempt after
                // time has already run out.
                if __expired() {
                  break 'retry ::std::result::Result::Err(
                    $crate::error::FerriError::timeout(__op_name.to_string(), __resolved_timeout)
                      .with_call_log(__call_log.into_vec()),
                  );
                }

                // Action pre-checks: run registered locator handlers if any of their
                // overlays are currently visible (Playwright `performActionPreChecks`).
                $crate::locator_handler::perform_checkpoint($self.frame.page_arc()).await;

                let __delay_ms = Locator::RETRY_BACKOFFS_MS[__idx.min(Locator::RETRY_BACKOFFS_MS.len() - 1)];
                __idx = __idx.saturating_add(1);
                if __delay_ms > 0 {
                  // Clamp the sleep to whatever's left on the deadline so the timeout
                  // error fires on time rather than after an overshoot sleep.
                  let __sleep_ms = match __deadline {
                    ::std::option::Option::Some(__d) => {
                      let __left =
                        u64::try_from(__d.saturating_duration_since(::std::time::Instant::now()).as_millis())
                          .unwrap_or(__delay_ms);
                      __delay_ms.min(__left)
                    },
                    ::std::option::Option::None => __delay_ms,
                  };
                  if __sleep_ms > 0 {
                    $self
                      .frame
                      .pause_until_change(::std::time::Duration::from_millis(__sleep_ms))
                      .await;
                  }
                }

                // Resolve `frameLocator` enter-frame hops to the real child frame
                // + trailing selector (a no-op clone for plain selectors). This
                // is INSIDE the loop because an `<iframe>` swaps documents: a
                // chain resolved once, before the frame finished loading, would
                // keep querying a dead frame for the rest of the deadline (and
                // an unknown frame id falls back to the main document, so the
                // selector silently matches the WRONG page).
                // A failure here means the iframe itself is not there yet — same
                // retry semantics as a missing element.
                let (__rframe, __rsel) = match $self.resolved().await {
                  ::std::result::Result::Ok(resolved) => resolved,
                  ::std::result::Result::Err(e @ $crate::error::FerriError::InvalidSelector { .. }) => {
                    break 'retry ::std::result::Result::Err(e);
                  },
                  ::std::result::Result::Err(_) => continue,
                };
                let $page: &$crate::backend::AnyPage = __rframe.page_arc().inner();
                if let ::std::result::Result::Err(e) = $page.ensure_engine_injected().await {
                  if __expired() {
                    continue;
                  }
                  break 'retry ::std::result::Result::Err($crate::error::FerriError::from(e));
                }
                let __sel_js = match $crate::selectors::build_selone_js(&__rsel, "window.__fd", $self.strict) {
                  ::std::result::Result::Ok(v) => v,
                  ::std::result::Result::Err(e) => {
                    break 'retry ::std::result::Result::Err($crate::error::FerriError::from(e));
                  },
                };
                // Pass `None` for main-frame locators so the backend skips a
                // `frame_contexts` lookup; child frames thread their cached id.
                let __frame_id: ::std::option::Option<&str> = if __rframe.is_main_frame() {
                  ::std::option::Option::None
                } else {
                  ::std::option::Option::Some(__rframe.id())
                };

                // Strict mode (the default) is folded into the same engine-side
                // `selOne(parts, strict)` call below — the JS throws
                // `strict mode violation: <count>` when the selector matches more
                // than one element, the host catches the exception and converts
                // to a typed `FerriError::StrictModeViolation`. Saves the
                // separate `query_all` + `cleanup_tags` round-trips the previous
                // implementation paid on every retry attempt (~2 RTTs).
                match $crate::selectors::query_one_prebuilt($page, &__sel_js, &$self.selector, __frame_id).await {
                  ::std::result::Result::Ok($el) => {
                    __call_log.mark_attempted();
                    if __trace_input_pending {
                      __trace_input_pending = false;
                      if let ::std::option::Option::Some(__s) = __trace_span.as_ref() {
                        ::std::boxed::Box::pin(__trace_page.trace_capture_input(__s, &$el, $op)).await;
                      }
                    }
                    // Boxed: the guarded attempt would otherwise sit inline in the
                    // state machine of every action this macro expands into.
                    let __try = __tries;
                    __tries = __tries.saturating_add(1);
                    match ::std::boxed::Box::pin($crate::actions::with_pointer_context(
                      __try,
                      (!__rframe.is_main_frame()).then(|| __rframe.clone()),
                      $crate::abort::guard($body),
                    ))
                    .await
                    {
                      ::std::result::Result::Ok(val) => break 'retry ::std::result::Result::Ok(val),
                      ::std::result::Result::Err(e @ $crate::error::FerriError::Aborted { .. }) => {
                        __call_log.push_aborted(&e);
                        break 'retry ::std::result::Result::Err(e.with_call_log(__call_log.into_vec()));
                      },
                      ::std::result::Result::Err(e) => {
                        let __msg = e.to_string();
                        if $crate::locator::retries(&e) {
                          // Retriable: actionability signals
                          // (`error:notvisible` / `error:notenabled` / ...) plus
                          // stale-handle backend errors (the node detached
                          // mid-action). Keep re-resolving until the deadline.
                          let __line = $crate::locator::action_log_line(&__msg);
                          if __call_log.record_retry(&__line)
                            && let ::std::option::Option::Some(__s) = __trace_span.as_ref()
                          {
                            __s.log(__line);
                          }
                        } else if !__expired() {
                          break 'retry ::std::result::Result::Err($crate::error::FerriError::from(e));
                        }
                        // Otherwise a command the deadline cut off: the loop's own
                        // check reports the action timing out, with its call log.
                      },
                    }
                  },
                  ::std::result::Result::Err(__err) => {
                    // Strict-mode violation: the engine threw
                    // `strict mode violation: <count>` from inside `selOne`.
                    // Surface it as a typed error rather than a retry signal.
                    if let ::std::option::Option::Some(__count) =
                      $crate::selectors::parse_strict_violation_count(&__err)
                    {
                      break 'retry ::std::result::Result::Err($crate::error::FerriError::strict(
                        $self.selector.clone(),
                        __count,
                      ));
                    }
                    // Otherwise: element not found this iteration; retry until deadline.
                  },
                }
              }
            }
          })
          .await
      },
    };
    if let ::std::option::Option::Some(__s) = __trace_span {
      ::std::boxed::Box::pin(__trace_page.snapshot_after_and_finish(__s, __result.as_ref().err())).await;
    }
    __trace_page.slow_mo_pause("Locator", $op).await;
    __result
  }};
}

/// Outcome of [`Locator::match_aria_snapshot`] — whether the template
/// matched, plus the rendered live tree for the failure message.
#[derive(Debug, Clone)]
pub struct AriaSnapshotMatch {
  pub matches: bool,
  pub received: String,
  /// The live tree rendered with its text as patterns: what a newly
  /// generated baseline file holds.
  pub regex: String,
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
  ///
  /// A selector that starts with `frameLocator()`'s any-frame token is
  /// tried from every frame in this one's subtree (Playwright's
  /// `resolveFramesForSelector`): the one frame where it matches wins, and
  /// matches in more than one frame are an error retrying cannot fix.
  pub(crate) async fn resolved(&self) -> Result<(crate::frame::Frame, String)> {
    match any_frame_rest(&self.selector)? {
      Some(rest) => Box::pin(resolve_in_any_frame(&self.frame, rest)).await,
      None => Box::pin(resolve_frame_hops(self.frame.clone(), &self.selector)).await,
    }
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
    self.locator(selector_or_locator).filter_with(&opts)
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
    self.chain(&build_testid_selector(&self.frame.test_id_attribute(), &test_id.into()))
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
    crate::locator_generator::locator_custom_description(&self.selector)
  }

  /// Playwright: `locator.visible(): Locator` — only the visible matches.
  #[must_use]
  pub fn visible(&self) -> Locator {
    self.filter_with(&crate::options::FilterOptions {
      visible: Some(true),
      ..Default::default()
    })
  }

  /// Playwright: `locator.within(locator: Locator): Locator` — this
  /// locator, searched for inside `scope`.
  #[must_use]
  pub fn within(&self, scope: &Locator) -> Locator {
    scope.locator(self.clone())
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

  /// Playwright: `filter(options?: { has?, hasNot?, hasText?, hasNotText?,
  /// visible? }): Locator`. The options chain as setters on the returned
  /// builder, which derefs to the filtered [`Locator`]:
  ///
  /// ```ignore
  /// page.get_by_role("listitem").filter().has_text("Ship it").click().await?;
  /// rows.filter().has(page.get_by_role("button").name("Delete")).first();
  /// ```
  ///
  /// See [`Self::filter_with`] for the selector encoding.
  #[must_use]
  pub fn filter(&self) -> crate::locator_builder::LocatorBuilder<FilterOptions> {
    let base = self.clone();
    crate::locator_builder::LocatorBuilder::new(move |opts: &FilterOptions| base.filter_with(opts))
  }

  /// [`Self::filter`] with a whole option bag, for callers that already
  /// hold one (the bindings). Option-to-selector encoding:
  ///
  /// * `has_text` → ` >> internal:has-text=<escaped>` — string is
  ///   `"quoted"i` (case-insensitive substring), regex is `/source/flags`
  ///   (`escapeForTextSelector` with `exact: false`, matching Playwright's
  ///   `Locator` constructor).
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
  pub fn filter_with(&self, opts: &FilterOptions) -> Locator {
    // Build the combined filter suffix in one buffer, then chain once.
    let mut suffix = String::new();
    let push_sep = |buf: &mut String| {
      if !buf.is_empty() {
        buf.push_str(" >> ");
      }
    };

    if let Some(text) = &opts.has_text {
      let _ = write!(suffix, "internal:has-text={}", escape_for_text_selector(text, false));
    }
    if let Some(text) = &opts.has_not_text {
      push_sep(&mut suffix);
      let _ = write!(
        suffix,
        "internal:has-not-text={}",
        escape_for_text_selector(text, false)
      );
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

  /// Playwright: `locator.allTextContents()`, each matching element's
  /// `textContent` as the page has it, in its own frame.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn all_text_contents(&self) -> Result<Vec<String>> {
    self.all_strings("ee => ee.map(e => e.textContent || '')").await
  }

  /// Playwright: `locator.allInnerTexts()`, each matching element's
  /// `innerText`: the rendered text, so table cells and blocks are
  /// separated as a reader sees them, where `textContent` runs them
  /// together.
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn all_inner_texts(&self) -> Result<Vec<String>> {
    self.all_strings("ee => ee.map(e => e.innerText)").await
  }

  async fn all_strings(&self, fn_source: &str) -> Result<Vec<String>> {
    let value = self
      .evaluate_all(fn_source, crate::protocol::SerializedArgument::default(), Some(true))
      .await?;
    Ok(match value.to_json_like() {
      Some(serde_json::Value::Array(items)) => items
        .into_iter()
        .map(|item| item.as_str().unwrap_or_default().to_owned())
        .collect(),
      _ => Vec::new(),
    })
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
  pub(crate) const RETRY_BACKOFFS_MS: &'static [u64] = &[0, 0, 20, 50, 100, 100, 500];

  /// How long one page-side wait watches before the host asks again.
  /// The page reports a change as it happens, so this bounds only what
  /// the host cannot see from inside one document: a `frameLocator` hop
  /// that now resolves to a different frame, and how long a wait the
  /// caller abandoned keeps observing.
  const WAIT_SLICE_MS: u64 = 100;

  /// Resolve element + run JS callback in ONE CDP call, with a short
  /// retry and no timeout: `toMatchAriaSnapshot` polls it, and its
  /// poller owns the deadline. The public reads wait through [`Self::read`].
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
        let js = selectors::build_read_js(&parts_json, self.strict, js_body, fd);
        let raw = if rf.is_main_frame() {
          inner.evaluate(&js).await
        } else {
          inner.evaluate_in_frame(&js, rf.id()).await
        }?;
        selectors::decode_read_result(raw, &rsel)
      }
      .await;
      match attempt {
        // A strict-mode breach is settled: more elements will not
        // become fewer, so retrying only burns the timeout before
        // reporting the same thing.
        Err(e) if e.is_strict_mode_violation() => return Err(e),
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

  async fn read_state(&self, op: &'static str, state: &str, timeout: Option<u64>) -> Result<bool> {
    let value = self
      .read(
        op,
        &format!("return window.__fd.elementState(el, '{state}') === true;"),
        timeout,
      )
      .await?;
    Ok(value.as_bool().unwrap_or(false))
  }

  async fn read_string(&self, op: &'static str, js_body: &str, timeout: Option<u64>) -> Result<String> {
    let value = self.read(op, js_body, timeout).await?;
    Ok(match value {
      serde_json::Value::String(s) => s,
      serde_json::Value::Null => String::new(),
      other => other.to_string(),
    })
  }

  /// Playwright's `_waitForFunctionOnSelector`: run `js_body` on the
  /// first match, waiting for one to appear until `timeout` (the page's
  /// default when `None`, without limit when `0`).
  async fn read(&self, op: &'static str, js_body: &str, timeout: Option<u64>) -> Result<serde_json::Value> {
    let page = self.frame.page_arc();
    let params = if crate::trace::call_origins_wanted() {
      serde_json::json!({ "selector": self.selector })
    } else {
      serde_json::Value::Null
    };
    page
      .traced_as("Locator", op, params, self.read_untraced(op, js_body, timeout))
      .await
  }

  async fn read_untraced(&self, op: &'static str, js_body: &str, timeout: Option<u64>) -> Result<serde_json::Value> {
    let timeout = timeout.unwrap_or_else(|| self.frame.page_arc().default_timeout());
    let deadline = (timeout != 0).then(|| tokio::time::Instant::now() + std::time::Duration::from_millis(timeout));
    // `retryWithProgressAndBackoff` drops the long waits from the
    // schedule when the timeout is short, so it still gets a few tries.
    let backoff_for = |attempt: usize| -> u64 {
      let mut last = 0u64;
      let mut idx = 0usize;
      for &ms in Self::RETRY_BACKOFFS_MS {
        if timeout == 0 || ms <= timeout / 5 {
          last = ms;
          if idx == attempt {
            return ms;
          }
          idx += 1;
        }
      }
      last
    };
    let call_log = || {
      vec![format!(
        "waiting for {}",
        crate::locator_generator::as_locator(&self.selector)
      )]
    };
    let mut attempt = 0usize;
    // Commands only: the loop owns the deadline, so its timeout keeps
    // the `Call log:` the operation recorded.
    crate::operation_budget::OperationBudget::operation(timeout)?
      .scope(async move {
        loop {
          let delay = backoff_for(attempt);
          attempt += 1;
          if delay > 0 {
            let left = deadline.map_or(delay, |deadline| {
              u64::try_from(
                deadline
                  .saturating_duration_since(tokio::time::Instant::now())
                  .as_millis(),
              )
              .unwrap_or(delay)
            });
            self
              .frame
              .pause_until_change(std::time::Duration::from_millis(delay.min(left)))
              .await;
          }
          if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            return Err(crate::error::FerriError::timeout(op, timeout).with_call_log(call_log()));
          }
          let evaluated: Result<(Option<serde_json::Value>, String)> = async {
            let (frame, selector) = self.resolved().await?;
            let parts_json = selectors::build_parts_json(&selectors::parse(&selector)?);
            let inner = frame.page_arc().inner();
            inner.ensure_engine_injected().await?;
            let js = selectors::build_read_js(&parts_json, self.strict, js_body, "window.__fd");
            let raw = if frame.is_main_frame() {
              inner.evaluate(&js).await
            } else {
              inner.evaluate_in_frame(&js, frame.id()).await
            }?;
            Ok((raw, selector))
          }
          .await;
          match evaluated {
            // What the page answered is settled: a value, a breach of strict
            // mode, or an element the read rejects. Only no match waits.
            Ok((raw, selector)) => {
              if let Some(value) = selectors::decode_read_result(raw, &selector)? {
                return Ok(value);
              }
            },
            Err(
              e @ (crate::error::FerriError::InvalidSelector { .. }
              | crate::error::FerriError::TargetClosed { .. }
              | crate::error::FerriError::Aborted { .. }),
            ) => return Err(e),
            // A frame between documents, or one not attached yet.
            Err(_) => {},
          }
        }
      })
      .await
  }

  /// Whether a trace recorder is currently attached to this page's context.
  fn is_tracing_active(&self) -> bool {
    crate::trace::has_action_observer_or_gate()
      || self
        .frame
        .page_arc()
        .context()
        .and_then(|c| crate::trace::recorder_for(&c.composite()))
        .is_some()
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
    let js = selectors::build_read_js(&parts_json, self.strict, js_body, fd);
    let raw = if rframe.is_main_frame() {
      inner.evaluate(&js).await
    } else {
      inner.evaluate_in_frame(&js, rframe.id()).await
    }?;
    selectors::decode_read_result(raw, &rsel)
  }
}

/// Playwright's `locator.toString()`: the code that would build this
/// locator, or the description a trailing `describe()` gave it.
impl std::fmt::Display for Locator {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(&crate::locator_generator::as_locator_description(&self.selector))
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

mod content;
mod fast;
mod frame_locator;
mod frame_resolve;
mod interaction;
mod retry;

pub use frame_locator::{FrameLocator, guess_mime_type};
pub(crate) use frame_locator::{
  build_attr_selector, build_role_selector, build_testid_selector, build_text_like_selector,
};
use frame_locator::{escape_for_text_selector, json_quote};
pub use frame_resolve::{ANY_FRAME, any_frame_resolves_to};
use frame_resolve::{any_frame_rest, count_in, resolve_frame_hops, resolve_in_any_frame};
pub(crate) use retry::{LazyCallLog, action_log_line, begin_locator_trace, is_retryable_action_error, retries};
