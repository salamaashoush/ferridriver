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
    let __trace_page = $self.frame.page_arc();
    let __trace_span = {
      let __trace_composite = __trace_page.context().map(|c| c.composite());
      let mut __trace_params = ::serde_json::json!({ "selector": $self.selector });
      if let (
        ::std::option::Option::Some(__dst),
        ::std::option::Option::Some(__src),
      ) = (__trace_params.as_object_mut(), ($extra).as_object())
      {
        for (__k, __v) in __src {
          __dst.insert(__k.clone(), __v.clone());
        }
      }
      $crate::trace::begin_action(
        __trace_composite.as_deref(),
        "Locator",
        $op,
        ::std::option::Option::Some(format!("page@{}", __trace_page.backend_page_id())),
        __trace_params,
      )
    };
    let __trace_span = $crate::trace::open_action(__trace_page.snapshot_before(__trace_span).await).await;
    // Playwright's `Call log:`, attached to the error if the action times out.
    let mut __call_log: ::std::vec::Vec<::std::string::String> =
      ::std::vec![format!("waiting for {}", $crate::locator_generator::as_locator(&$self.selector))];
    if let ::std::option::Option::Some(__s) = __trace_span.as_ref() {
      __s.log(__call_log[0].clone());
    }
    // Input-time capture (target mark + input@ snapshot + point) fires
    // once, on the first successful resolution.
    let mut __trace_input_pending = __trace_span.is_some();
    // Last call-log line, to collapse identical retry messages.
    let mut __trace_last_log = ::std::string::String::new();
    let __op_name: &str = $op;
    let __resolved_timeout: u64 = $timeout_ms.unwrap_or_else(|| $self.frame.page_arc().default_timeout());
    let __deadline: ::std::option::Option<::std::time::Instant> = if __resolved_timeout == 0 {
      ::std::option::Option::None
    } else {
      ::std::option::Option::Some(
        ::std::time::Instant::now() + ::std::time::Duration::from_millis(__resolved_timeout),
      )
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
      ::std::result::Result::Ok(__budget) => __budget.scope(async { 'retry: {

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
          __call_log.push(format!("operation was aborted: {}", __error));
          break 'retry ::std::result::Result::Err(__error.with_call_log(::std::mem::take(&mut __call_log)));
        }
        // Deadline check up-front so we never race into one more attempt after
        // time has already run out.
        if __expired() {
          break 'retry ::std::result::Result::Err(
            $crate::error::FerriError::timeout(__op_name.to_string(), __resolved_timeout)
              .with_call_log(::std::mem::take(&mut __call_log)),
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
              let __left = u64::try_from(__d.saturating_duration_since(::std::time::Instant::now()).as_millis())
                .unwrap_or(__delay_ms);
              __delay_ms.min(__left)
            },
            ::std::option::Option::None => __delay_ms,
          };
          if __sleep_ms > 0 {
            $self.frame.pause_until_change(::std::time::Duration::from_millis(__sleep_ms)).await;
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
          ::std::result::Result::Err(e) => break 'retry ::std::result::Result::Err($crate::error::FerriError::from(e)),
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
            if __call_log.len() == 1 {
              __call_log.push(format!("attempting {} action", __op_name));
            }
            if __trace_input_pending {
              __trace_input_pending = false;
              if let ::std::option::Option::Some(__s) = __trace_span.as_ref() {
                __trace_page.trace_capture_input(__s, &$el, $op).await;
              }
            }
            // Boxed: the guarded attempt would otherwise sit inline in the
            // state machine of every action this macro expands into.
            let __try = __tries;
            __tries = __tries.saturating_add(1);
            match ::std::boxed::Box::pin($crate::actions::with_pointer_context(__try, (!__rframe.is_main_frame()).then(|| __rframe.clone()), $crate::abort::guard($body))).await {
              ::std::result::Result::Ok(val) => break 'retry ::std::result::Result::Ok(val),
              ::std::result::Result::Err(e @ $crate::error::FerriError::Aborted { .. }) => {
                __call_log.push(format!("operation was aborted: {e}"));
                break 'retry ::std::result::Result::Err(e.with_call_log(::std::mem::take(&mut __call_log)));
              },
              ::std::result::Result::Err(e) => {
                let __msg = e.to_string();
                if $crate::locator::retries(&e) {
                  // Retriable: actionability signals
                  // (`error:notvisible` / `error:notenabled` / ...) plus
                  // stale-handle backend errors (the node detached
                  // mid-action). Keep re-resolving until the deadline.
                  let __line = $crate::locator::action_log_line(&__msg);
                  if __line != __trace_last_log {
                    __call_log.push(format!("  {__line}"));
                    __call_log.push(format!("retrying {} action", __op_name));
                    if let ::std::option::Option::Some(__s) = __trace_span.as_ref() {
                      __s.log(__line.clone());
                    }
                    __trace_last_log = __line;
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
    }}).await,
    };
    if let ::std::option::Option::Some(__s) = __trace_span {
      __trace_page
        .snapshot_after_and_finish(__s, __result.as_ref().err())
        .await;
    }
    __trace_page.slow_mo_pause("Locator", $op).await;
    __result
  }};
}

/// The call-log line for a retried attempt, in Playwright's wording:
/// `error:notinviewport` is "element is outside of the viewport",
/// `error:not<state>` is "element is not <state>".
pub(crate) fn action_log_line(msg: &str) -> String {
  // `server/dom.ts::_retryAction`'s lines for `selectOption`'s markers.
  if msg.contains("error:optionsnotfound") {
    return "did not find some options".to_string();
  }
  if msg.contains("error:optionnotenabled") {
    return "option being selected is not enabled".to_string();
  }
  let Some((_, after)) = msg.split_once("error:not") else {
    return msg.strip_prefix("backend error: ").unwrap_or(msg).to_string();
  };
  let state: String = after.chars().take_while(char::is_ascii_alphanumeric).collect();
  match state.as_str() {
    "inviewport" => "element is outside of the viewport".to_string(),
    "connected" => "element is not attached to the DOM".to_string(),
    _ => format!("element is not {state}"),
  }
}

enum PointerPrepVerdict {
  AlreadyDone,
  NotFound,
  Retry(String),
  Ready {
    point: (f64, f64),
    interceptor_installed: bool,
  },
}

fn record_retry_log(call_log: &mut Vec<String>, last_log: &mut String, line: &str, op_name: &str) {
  if call_log.len() == 1 {
    call_log.push(format!("attempting {op_name} action"));
  }
  if line != last_log.as_str() {
    call_log.push(format!("  {line}"));
    call_log.push(format!("retrying {op_name} action"));
    last_log.clear();
    last_log.push_str(line);
  }
}

/// Whether a failed attempt is tried again: never once its input was
/// sent, otherwise as [`is_retryable_action_error`] says.
pub(crate) fn retries(error: &crate::error::FerriError) -> bool {
  !matches!(error, crate::error::FerriError::InputSent { .. }) && is_retryable_action_error(&error.to_string())
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
    // `selectOption` before its options render, or while the one asked
    // for is disabled: Playwright waits for both (`server/dom.ts`).
    || msg.contains("error:optionsnotfound")
    || msg.contains("error:optionnotenabled")
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
    // A WebKit target destroyed under us — the old side of a
    // cross-process navigation. The page swaps to the committed target,
    // so the very next attempt runs on a live session.
    || msg.contains("transport closed")
    // Hit-target interception: another element covers the action point.
    // Retryable so the Locator loop keeps running locator handlers and
    // re-resolving until its deadline (Playwright retries the pointer
    // action on `hitTargetDescription` the same way).
    || msg.contains("intercepts pointer events")
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
    page
      .traced_as(
        "Locator",
        op,
        serde_json::json!({ "selector": self.selector }),
        self.read_untraced(op, js_body, timeout),
      )
      .await
  }

  async fn read_untraced(&self, op: &'static str, js_body: &str, timeout: Option<u64>) -> Result<serde_json::Value> {
    let timeout = timeout.unwrap_or_else(|| self.frame.page_arc().default_timeout());
    let deadline = (timeout != 0).then(|| tokio::time::Instant::now() + std::time::Duration::from_millis(timeout));
    // `retryWithProgressAndBackoff` drops the long waits from the
    // schedule when the timeout is short, so it still gets a few tries.
    let backoffs: Vec<u64> = Self::RETRY_BACKOFFS_MS
      .iter()
      .copied()
      .filter(|&ms| timeout == 0 || ms <= timeout / 5)
      .collect();
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
          let delay = backoffs[attempt.min(backoffs.len() - 1)];
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
    self
      .frame
      .page_arc()
      .context()
      .and_then(|c| crate::trace::recorder_for(&c.composite()))
      .is_some()
  }

  /// Evaluate `expr` in `rframe` (main frame or child frame).
  async fn eval_in_resolved_frame(rframe: &crate::frame::Frame, expr: &str) -> Result<Option<serde_json::Value>> {
    let page = rframe.page_arc();
    if rframe.is_main_frame() {
      page.inner().evaluate(expr).await
    } else {
      page.inner().evaluate_in_frame(expr, rframe.id()).await
    }
  }

  /// Single-eval `locator.fill()` directly inside `UTILITY_WORLD` on CDP:
  /// 1 `Runtime.evaluate(contextId = utility)` + 1 `Input.insertText` (2 RTTs),
  /// skipping main-world `RemoteObjectId`, `DOM.describeNode`, `DOM.resolveNode`,
  /// and `Runtime.releaseObject`. Returns `None` when tracing is active or the
  /// backend has no direct utility-world eval (caller falls back to `retry_resolve!`).
  async fn fill_fast(&self, value: &str, opts: &crate::options::FillOptions) -> Option<Result<()>> {
    if self.is_tracing_active() {
      return None;
    }
    let page_arc = self.frame.page_arc();
    if !matches!(page_arc.inner(), crate::backend::AnyPage::Cdp(_)) {
      return None;
    }
    let res = Box::pin(self.fill_fast_inner(value, opts)).await;
    page_arc.slow_mo_pause("Locator", "fill").await;
    Some(res)
  }

  /// Sleep for one backoff step (clamped to `deadline`) using `pause_until_change`.
  async fn backoff_pause(&self, idx: usize, deadline: Option<std::time::Instant>) {
    let delay_ms = Self::RETRY_BACKOFFS_MS[idx.min(Self::RETRY_BACKOFFS_MS.len() - 1)];
    if delay_ms > 0 {
      let sleep_ms = deadline.map_or(delay_ms, |d| {
        let left =
          u64::try_from(d.saturating_duration_since(std::time::Instant::now()).as_millis()).unwrap_or(delay_ms);
        delay_ms.min(left)
      });
      if sleep_ms > 0 {
        self
          .frame
          .pause_until_change(std::time::Duration::from_millis(sleep_ms))
          .await;
      }
    }
  }

  /// Evaluate `resolveAndFill` once in `UTILITY_WORLD`, lazily injecting the
  /// engine into the utility world on first encounter.
  async fn eval_fill_once(
    &self,
    rframe: &crate::frame::Frame,
    parts: &str,
    value_lit: &str,
    force: bool,
  ) -> Option<Result<String>> {
    let frame_id = (!rframe.is_main_frame()).then(|| rframe.id());
    let frame_id_str = frame_id.map(std::ops::Deref::deref);
    let inner = rframe.page_arc().inner();
    let expr = format!(
      "(() => {{ var fd = window.__fd; if (!(fd instanceof Object)) return '{{\"result\":\"inject\"}}'; return fd.resolveAndFill({parts}, {}, {value_lit}, {force}); }})()",
      self.strict
    );
    let raw = match inner.evaluate_in_utility(frame_id_str, &expr).await? {
      Ok(v) => v.and_then(|val| val.as_str().map(str::to_owned)).unwrap_or_default(),
      Err(e) => return Some(Err(e)),
    };
    if !raw.contains("\"inject\"") {
      return Some(Ok(raw));
    }
    let inject_and_fill = format!(
      "(async () => {{ await {}; return window.__fd.resolveAndFill({parts}, {}, {value_lit}, {force}); }})()",
      crate::selectors::lazy_inject_js(),
      self.strict
    );
    match inner.evaluate_in_utility(frame_id_str, &inject_and_fill).await? {
      Ok(v) => Some(Ok(
        v.and_then(|val| val.as_str().map(str::to_owned)).unwrap_or_default(),
      )),
      Err(e) => Some(Err(e)),
    }
  }

  async fn fill_fast_inner(&self, value: &str, opts: &crate::options::FillOptions) -> Result<()> {
    let timeout = opts.timeout.unwrap_or_else(|| self.frame.page_arc().default_timeout());
    let deadline = (timeout != 0).then(|| std::time::Instant::now() + std::time::Duration::from_millis(timeout));
    let expired = || deadline.is_some_and(|d| std::time::Instant::now() >= d);
    let value_lit = serde_json::to_string(value)?;
    let force = opts.is_force();
    let mut call_log = vec![format!(
      "waiting for {}",
      crate::locator_generator::as_locator(&self.selector)
    )];
    let mut last_log = String::new();
    let budget = crate::operation_budget::OperationBudget::operation(timeout)?;
    budget
      .scope(async {
        if let Some(sig) = crate::abort::current()
          && sig.was_refused()
        {
          return Err(sig.error());
        }
        let outcome = Box::pin(crate::abort::guard(async {
          let mut idx = 0usize;
          loop {
            if expired() {
              return Err(crate::error::FerriError::timeout("fill", timeout));
            }
            crate::locator_handler::perform_checkpoint(self.frame.page_arc()).await;
            self.backoff_pause(idx, deadline).await;
            idx = idx.saturating_add(1);
            let (rframe, rsel) = match self.resolved().await {
              Ok(resolved) => resolved,
              Err(e @ crate::error::FerriError::InvalidSelector { .. }) => return Err(e),
              Err(_) => continue,
            };
            let parts = selectors::build_parts_json(&selectors::parse(&rsel)?);
            let raw = match self.eval_fill_once(&rframe, &parts, &value_lit, force).await {
              Some(Ok(r)) => r,
              Some(Err(e)) if !expired() && !retries(&e) => return Err(e),
              _ => continue,
            };
            let answer: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
            if let Some(count) = answer.get("strict").and_then(serde_json::Value::as_u64) {
              return Err(crate::error::FerriError::strict(
                self.selector.clone(),
                usize::try_from(count).unwrap_or(2),
              ));
            }
            if let Some(error) = answer.get("error").and_then(serde_json::Value::as_str) {
              return Err(crate::error::FerriError::backend(error));
            }
            let inner = rframe.page_arc().inner();
            match answer.get("result").and_then(serde_json::Value::as_str) {
              Some("done") => return Ok(()),
              Some("needsinput") if value.is_empty() => return inner.press_key("Delete", 0).await,
              Some("needsinput") => return inner.insert_text(value).await,
              Some("error:notfound") | None => {},
              Some(state) => {
                record_retry_log(&mut call_log, &mut last_log, &action_log_line(state), "fill");
              },
            }
          }
        }))
        .await;
        match outcome {
          Err(e @ crate::error::FerriError::Aborted { .. }) => {
            call_log.push(format!("operation was aborted: {e}"));
            Err(e.with_call_log(call_log))
          },
          Err(e @ crate::error::FerriError::Timeout { .. }) => Err(e.with_call_log(call_log)),
          other => other,
        }
      })
      .await
  }

  /// Fast single-eval pointer action (`click` / `dblclick` / `setChecked`)
  /// using `window.__fd.resolveAndClickPrep` or `resolveAndCheckPrep` so no
  /// `RemoteObjectId` is created.
  async fn click_fast(&self, opts: &crate::options::ClickOptions, op_name: &'static str) -> Result<()> {
    let page_arc = self.frame.page_arc();
    let res = Box::pin(self.pointer_action_fast(opts, op_name, None)).await;
    page_arc.slow_mo_pause("Locator", op_name).await;
    res
  }

  async fn set_checked_fast(&self, checked: bool, opts: &crate::options::CheckOptions) -> Result<()> {
    let page_arc = self.frame.page_arc();
    let click_opts = opts.clone().into_click_options();
    let res = Box::pin(self.pointer_action_fast(&click_opts, "check", Some(checked))).await;
    page_arc.slow_mo_pause("Locator", "check").await;
    res
  }

  async fn dispatch_prepared_pointer(
    rframe: &crate::frame::Frame,
    opts: &crate::options::ClickOptions,
    args: &crate::backend::BackendClickArgs,
    op_name: &'static str,
    target_checked: Option<bool>,
    point: (f64, f64),
    state: (bool, usize),
  ) -> Result<()> {
    let (interceptor_installed, tries) = state;
    let inner = rframe.page_arc().inner();
    let trial = opts.is_trial();
    let (x, y) = point;
    Box::pin(actions::with_pointer_context(
      tries,
      (!rframe.is_main_frame()).then(|| rframe.clone()),
      async {
        let snap = inner.nav_snapshot();
        let restore = actions::hold_modifiers(inner, &opts.modifiers).await?;
        let dispatch = if trial {
          Ok(())
        } else {
          let (dx, dy) = actions::cross_origin_frame_offset().await?;
          inner.click_at_with(x + dx, y + dy, args).await
        };
        let dispatch = dispatch.and(actions::restore_modifiers(inner, restore).await);
        if let Err(e) = dispatch {
          if interceptor_installed {
            let _ = Self::eval_in_resolved_frame(rframe, "JSON.stringify(window.__fd.finalizeHitInterceptor())").await;
          }
          if trial {
            return Err(e);
          }
          return Err(crate::error::FerriError::InputSent {
            action: op_name.into(),
            message: e.to_string(),
          });
        }
        if let Some(checked) = target_checked {
          if trial {
            return Ok(());
          }
          let fin_js = format!("window.__fd.finalizeCheck({checked}, {interceptor_installed})");
          let Ok(fin_val) = Self::eval_in_resolved_frame(rframe, &fin_js).await else {
            return Ok(());
          };
          let fin: serde_json::Value = fin_val
            .and_then(|v| v.as_str().and_then(|s| serde_json::from_str(s).ok()))
            .unwrap_or_default();
          if let Some(desc) = fin
            .pointer("/hit/hitTargetDescription")
            .and_then(serde_json::Value::as_str)
          {
            return Err(crate::error::FerriError::Backend(format!(
              "{desc} intercepts pointer events"
            )));
          }
          if fin.get("changed").and_then(serde_json::Value::as_bool) != Some(true) {
            return Err(crate::error::FerriError::backend(
              "clicking the checkbox did not change its state",
            ));
          }
          return Ok(());
        }
        if interceptor_installed
          && let Ok(fin_val) =
            Self::eval_in_resolved_frame(rframe, "JSON.stringify(window.__fd.finalizeHitInterceptor())").await
          && let Some(raw_fin) = fin_val.and_then(|v| v.as_str().map(str::to_owned))
          && raw_fin != "\"done\""
          && let Ok(fin) = serde_json::from_str::<serde_json::Value>(&raw_fin)
          && let Some(desc) = fin.get("hitTargetDescription").and_then(serde_json::Value::as_str)
        {
          return Err(crate::error::FerriError::Backend(format!(
            "{desc} intercepts pointer events"
          )));
        }
        if !trial {
          inner.settle_navigation(snap, 2_000).await;
        }
        Ok(())
      },
    ))
    .await
  }

  async fn eval_pointer_prep(
    &self,
    rframe: &crate::frame::Frame,
    rsel: &str,
    opts: &crate::options::ClickOptions,
    target_checked: Option<bool>,
    tries: usize,
  ) -> Result<PointerPrepVerdict> {
    rframe.page_arc().inner().ensure_engine_injected().await?;
    let parts = selectors::build_parts_json(&selectors::parse(rsel)?);
    let pos_lit = match opts.position {
      Some(p) => format!("{{x:{},y:{}}}", p.x, p.y),
      None => "null".to_string(),
    };
    let stable = actions::stable_check().state();
    let force = opts.is_force();
    let trial = opts.is_trial();
    let hit_lit = if !force && !trial { "'mouse'" } else { "null" };
    let scroll_lit = if opts.scrolls() {
      actions::scroll_alignment(tries).unwrap_or("null")
    } else {
      "false"
    };
    let prep_js = match target_checked {
      Some(checked) => format!(
        "window.__fd.resolveAndCheckPrep({parts}, {}, {checked}, {pos_lit}, ['visible','enabled','{stable}'], {scroll_lit}, {hit_lit}, {force}, {trial})",
        self.strict
      ),
      None => format!(
        "window.__fd.resolveAndClickPrep({parts}, {}, {pos_lit}, ['visible','enabled','{stable}'], {scroll_lit}, {hit_lit}, {force}, {trial})",
        self.strict
      ),
    };
    let raw = Self::eval_in_resolved_frame(rframe, &prep_js)
      .await?
      .and_then(|val| val.as_str().map(str::to_owned))
      .unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
    if let Some(count) = parsed.get("strict").and_then(serde_json::Value::as_u64) {
      return Err(crate::error::FerriError::strict(
        self.selector.clone(),
        usize::try_from(count).unwrap_or(2),
      ));
    }
    if let Some(msg) = parsed.get("invalid").and_then(serde_json::Value::as_str) {
      return Err(crate::error::FerriError::invalid_argument("element", msg));
    }
    if parsed.get("alreadyDone").and_then(serde_json::Value::as_bool) == Some(true) {
      return Ok(PointerPrepVerdict::AlreadyDone);
    }
    if let Some(err) = parsed.get("error").and_then(serde_json::Value::as_str) {
      return Err(crate::error::FerriError::backend(err));
    }
    if parsed.get("actionable").and_then(serde_json::Value::as_bool) != Some(true) {
      let reason = parsed
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("notfound");
      return Ok(if reason == "notfound" {
        PointerPrepVerdict::NotFound
      } else {
        PointerPrepVerdict::Retry(action_log_line(&format!("error:not{reason}")))
      });
    }
    let Some(point) = parsed
      .get("point")
      .and_then(|p| Some((p.get("x")?.as_f64()?, p.get("y")?.as_f64()?)))
    else {
      return Ok(PointerPrepVerdict::NotFound);
    };
    let interceptor_installed = if force {
      false
    } else {
      match parsed.get("hit").and_then(serde_json::Value::as_str) {
        Some("ok") => !trial,
        Some("error:notconnected") => return Ok(PointerPrepVerdict::Retry(action_log_line("error:notconnected"))),
        Some(desc) => return Ok(PointerPrepVerdict::Retry(format!("{desc} intercepts pointer events"))),
        None => false,
      }
    };
    Ok(PointerPrepVerdict::Ready {
      point,
      interceptor_installed,
    })
  }

  async fn pointer_action_fast(
    &self,
    opts: &crate::options::ClickOptions,
    op_name: &'static str,
    target_checked: Option<bool>,
  ) -> Result<()> {
    let timeout = opts.timeout.unwrap_or_else(|| self.frame.page_arc().default_timeout());
    let deadline = (timeout != 0).then(|| std::time::Instant::now() + std::time::Duration::from_millis(timeout));
    let expired = || deadline.is_some_and(|d| std::time::Instant::now() >= d);
    let args = crate::backend::BackendClickArgs::from_options(opts);
    let mut call_log = vec![format!(
      "waiting for {}",
      crate::locator_generator::as_locator(&self.selector)
    )];
    let mut last_log = String::new();
    let budget = crate::operation_budget::OperationBudget::operation(timeout)?;
    budget
      .scope(async {
        if let Some(sig) = crate::abort::current()
          && sig.was_refused()
        {
          return Err(sig.error());
        }
        let outcome = Box::pin(crate::abort::guard(async {
          let mut idx = 0usize;
          let mut tries = 0usize;
          loop {
            if expired() {
              return Err(crate::error::FerriError::timeout(op_name, timeout));
            }
            crate::locator_handler::perform_checkpoint(self.frame.page_arc()).await;
            self.backoff_pause(idx, deadline).await;
            idx = idx.saturating_add(1);
            let (rframe, rsel) = match self.resolved().await {
              Ok(resolved) => resolved,
              Err(e @ crate::error::FerriError::InvalidSelector { .. }) => return Err(e),
              Err(_) => continue,
            };
            let (point, interceptor_installed) = match self
              .eval_pointer_prep(&rframe, &rsel, opts, target_checked, tries)
              .await
            {
              Ok(PointerPrepVerdict::AlreadyDone) => return Ok(()),
              Ok(PointerPrepVerdict::Retry(line)) => {
                tries = tries.saturating_add(1);
                record_retry_log(&mut call_log, &mut last_log, &line, op_name);
                continue;
              },
              Ok(PointerPrepVerdict::Ready {
                point,
                interceptor_installed,
              }) => (point, interceptor_installed),
              Err(e) if !expired() && !retries(&e) => return Err(e),
              Ok(PointerPrepVerdict::NotFound) | Err(_) => continue,
            };
            let outcome = Self::dispatch_prepared_pointer(
              &rframe,
              opts,
              &args,
              op_name,
              target_checked,
              point,
              (interceptor_installed, tries),
            )
            .await;
            tries = tries.saturating_add(1);
            match outcome {
              Ok(()) => return Ok(()),
              Err(e) if retries(&e) => {
                record_retry_log(&mut call_log, &mut last_log, &action_log_line(&e.to_string()), op_name);
              },
              Err(e) if !expired() => return Err(e),
              Err(_) => {},
            }
          }
        }))
        .await;
        match outcome {
          Err(e @ crate::error::FerriError::Aborted { .. }) => {
            call_log.push(format!("operation was aborted: {e}"));
            Err(e.with_call_log(call_log))
          },
          Err(e @ crate::error::FerriError::Timeout { .. }) => Err(e.with_call_log(call_log)),
          other => other,
        }
      })
      .await
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

// ── Frame resolution ──────────────────────────────────────────────────────────

/// `frameLocator()` without a selector: the rest of the selector may
/// match in any frame below the one it starts from.
#[doc(hidden)]
pub const ANY_FRAME: &str = "internal:control=any-frame";

/// What an any-frame locator runs when no frame matches yet: a selector
/// that cannot match, so actions keep waiting and counts read zero.
const MATCHES_NOTHING: &str = ":not(*)";

/// The selector after `frameLocator()`'s token, or `None` when the
/// selector does not start in any frame.
fn any_frame_rest(selector: &str) -> Result<Option<&str>> {
  let Some(rest) = selector.trim().strip_prefix(ANY_FRAME) else {
    return Ok(None);
  };
  match rest.trim_start().strip_prefix(">>").map(str::trim) {
    Some(rest) if !rest.is_empty() => Ok(Some(rest)),
    _ => Err(crate::error::FerriError::InvalidSelector {
      selector: selector.to_string(),
      reason: "Selector cannot be empty after frameLocator()".into(),
    }),
  }
}

/// Resolve `frameLocator` enter-frame hops from `start` into the deepest
/// frame plus the selector to run inside it.
async fn resolve_frame_hops(start: crate::frame::Frame, selector: &str) -> Result<(crate::frame::Frame, String)> {
  const MARK: &str = ">> internal:control=enter-frame >>";
  if !selector.contains("internal:control=enter-frame") {
    return Ok((start, selector.to_string()));
  }
  let mut parts = selector.split(MARK).map(str::trim);
  let mut cur = start;
  let mut pending = parts.next().unwrap_or("").to_string();
  for next in parts {
    let page_arc = Arc::clone(cur.page_arc());
    let fid = (!cur.is_main_frame()).then(|| cur.id().to_string());
    let el = crate::selectors::query_one(page_arc.inner(), &pending, false, fid.as_deref()).await?;
    let handle = crate::element_handle::ElementHandle::from_any_element(Arc::clone(&page_arc), el).await?;
    cur = handle
      .content_frame()
      .await?
      .ok_or_else(|| crate::error::FerriError::protocol("frameLocator", "<iframe> has no content frame"))?;
    pending = next.to_string();
  }
  Ok((cur, pending))
}

/// Try `selector` from `root` and every frame below it, keeping the
/// frames it matches in. A start frame whose hops lead nowhere, or whose
/// document is between loads, is skipped as Playwright's no-stall pass
/// skips it.
async fn resolve_in_any_frame(root: &crate::frame::Frame, selector: &str) -> Result<(crate::frame::Frame, String)> {
  let mut starts = vec![root.clone()];
  let mut next = 0;
  while next < starts.len() {
    let children = starts[next].child_frames();
    starts.extend(children);
    next += 1;
  }
  let mut matched: Vec<(crate::frame::Frame, String)> = Vec::new();
  for start in starts {
    let Ok((frame, rest)) = resolve_frame_hops(start, selector).await else {
      continue;
    };
    if matched.iter().any(|(seen, _)| seen.id() == frame.id()) {
      continue;
    }
    if count_in(&frame, &rest).await.is_ok_and(|n| n > 0) {
      matched.push((frame, rest));
    }
  }
  if matched.len() > 1 {
    return Err(crate::error::FerriError::InvalidSelector {
      selector: format!("{ANY_FRAME} >> {selector}"),
      reason: "frameLocator() matched elements in multiple frames.".into(),
    });
  }
  Ok(
    matched
      .pop()
      .unwrap_or_else(|| (root.clone(), MATCHES_NOTHING.to_string())),
  )
}

/// Whether `selector`, starting with [`ANY_FRAME`], matches from `root` in
/// `frame` and in no other frame.
#[doc(hidden)]
pub async fn any_frame_resolves_to(root: &crate::frame::Frame, selector: &str, frame: &crate::frame::Frame) -> bool {
  let Ok(Some(rest)) = any_frame_rest(selector) else {
    return false;
  };
  match Box::pin(resolve_in_any_frame(root, rest)).await {
    Ok((resolved, rest)) => rest != MATCHES_NOTHING && resolved.id() == frame.id(),
    Err(_) => false,
  }
}

/// How many elements `selector` matches in `frame`, without strictness.
async fn count_in(frame: &crate::frame::Frame, selector: &str) -> Result<usize> {
  let parts_json = selectors::build_parts_json(&selectors::parse(selector)?);
  let inner = frame.page_arc().inner();
  let fd = inner.injected_script().await?;
  let js = format!("{fd}.selCount({parts_json})");
  let val = if frame.is_main_frame() {
    inner.evaluate(&js).await
  } else {
    inner.evaluate_in_frame(&js, frame.id()).await
  }?
  .and_then(|v| v.as_u64())
  .unwrap_or(0);
  Ok(usize::try_from(val).unwrap_or(usize::MAX))
}

mod content;
mod frame_locator;
mod interaction;

pub use frame_locator::{FrameLocator, guess_mime_type};
pub(crate) use frame_locator::{
  build_attr_selector, build_role_selector, build_testid_selector, build_text_like_selector,
};
use frame_locator::{escape_for_text_selector, json_quote};
