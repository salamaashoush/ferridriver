//! Playwright-style selector engine.
//!
//! Parses rich selector strings (role=, text=, testid=, css=, etc.) in Rust,
//! then builds a self-contained JS IIFE that executes the query pipeline
//! in the browser context for maximum performance.
//!
//! # Selector Format
//!
//! ```text
//! css=div.container >> role=button[name="Submit"]
//! text="Hello World"
//! role=heading[level=1]
//! testid=login-form
//! label="Email"
//! ```
//!
//! Chaining with `>>` narrows scope: each part's results become the next part's search roots.

use crate::backend::{AnyElement, AnyPage};
use crate::error::{FerriError, Result};

mod parser;
#[cfg(test)]
mod tests;

pub use parser::{Engine, Selector, SelectorPart, build_parts_json, is_rich_selector, parse};

/// Bootstrap JS script to be evaluated on new document.
/// Resets the injection promise so we know we need to re-inject after navigation.
pub const ENGINE_BOOTSTRAP_JS: &str = "window.__fd_promise = null;";

// ─── Selector registry ──────────────────────────────────────────────────────

/// The attribute `getByTestId` reads when nothing overrides it.
pub const DEFAULT_TEST_ID_ATTRIBUTE: &str = "data-testid";

/// One `selectors.register(name, script)` registration.
#[derive(Debug, Clone)]
pub struct SelectorEngineRegistration {
  pub name: String,
  /// A JS expression evaluating to a `SelectorEngine` — what the
  /// injected script's `customEngines` option takes, and what
  /// Playwright's `evaluationScript` produces from a function.
  pub source: String,
  /// Playwright's `{ contentScript: true }`: run the engine in the
  /// page's own world rather than an isolated one. ferridriver has no
  /// isolated world, so `true` is honoured exactly and `false` (the
  /// default) runs in the page's world as well — recorded here so the
  /// distinction is available if isolated worlds land.
  pub content_script: bool,
}

#[derive(Default)]
struct Registry {
  engines: Vec<SelectorEngineRegistration>,
  test_id_attribute: Option<String>,
}

/// Whether anything has ever been registered / overridden.
///
/// Every selector part of every locator call asks whether its engine
/// name is registered, and every `getByTestId` asks for the attribute.
/// Both answers are the default in almost every run, so the flags keep
/// the hot path off the lock entirely — it is taken only once a suite
/// actually calls `selectors.register` or `setTestIdAttribute`.
static HAS_ENGINES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static HAS_TEST_ID_OVERRIDE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Process-global, as Playwright's `selectors` object is per client:
/// engines are registered once (usually from config or a fixture) and
/// every document injected afterwards gets them.
fn registry() -> &'static std::sync::RwLock<Registry> {
  static REGISTRY: std::sync::OnceLock<std::sync::RwLock<Registry>> = std::sync::OnceLock::new();
  REGISTRY.get_or_init(|| std::sync::RwLock::new(Registry::default()))
}

fn read_registry<T>(f: impl FnOnce(&Registry) -> T) -> T {
  f(&registry().read().unwrap_or_else(std::sync::PoisonError::into_inner))
}

/// The second argument of `selectors.register` before a host lowers it:
/// `Function | string | { path?, content? }`.
pub enum SelectorScript {
  /// A function's source (`Function.prototype.toString()`), called with
  /// no argument to produce the engine.
  Function(String),
  /// An expression evaluating to the engine, verbatim.
  Source(String),
  /// A file holding that expression.
  Path(std::path::PathBuf),
}

/// Playwright's `evaluationScript(script, undefined, false)`
/// (`client/clientHelper.ts`) — the rule for turning each of those forms
/// into the one expression the injected script evaluates.
///
/// # Errors
///
/// [`crate::FerriError::InvalidArgument`] when a path cannot be read.
pub fn evaluation_script(script: &SelectorScript) -> crate::Result<String> {
  match script {
    SelectorScript::Function(source) => Ok(format!("({source})(undefined)")),
    SelectorScript::Source(source) => Ok(source.clone()),
    SelectorScript::Path(path) => std::fs::read_to_string(path).map_err(|e| crate::FerriError::InvalidArgument {
      name: "script".to_string(),
      reason: format!("selectors.register: reading {}: {e}", path.display()),
    }),
  }
}

/// `selectors.register(name, script, options)`.
///
/// Divergence, and the reason for it: Playwright refuses ANY second
/// registration of a name, which it can afford because each of its
/// workers is a separate process with its own `selectors`. ferridriver's
/// workers share one process, so a spec file that registers an engine at
/// module scope — the ordinary way to do it — registers once per worker.
/// An IDENTICAL registration is therefore a no-op here; only a
/// conflicting one (same name, different script) raises, which is the
/// mistake the upstream error exists to catch.
///
/// # Errors
///
/// [`crate::FerriError::InvalidArgument`] when the name is registered
/// with a different script — Playwright's message verbatim.
pub fn register_selector_engine(name: &str, source: &str, content_script: bool) -> crate::Result<()> {
  let mut registry = registry().write().unwrap_or_else(std::sync::PoisonError::into_inner);
  if let Some(existing) = registry.engines.iter().find(|e| e.name == name) {
    if existing.source == source && existing.content_script == content_script {
      return Ok(());
    }
    return Err(crate::FerriError::InvalidArgument {
      name: "name".to_string(),
      reason: format!("selectors.register: \"{name}\" selector engine has been already registered"),
    });
  }
  registry.engines.push(SelectorEngineRegistration {
    name: name.to_string(),
    source: source.to_string(),
    content_script,
  });
  HAS_ENGINES.store(true, std::sync::atomic::Ordering::Release);
  Ok(())
}

/// Every engine registered so far, in registration order.
#[must_use]
pub fn registered_selector_engines() -> Vec<SelectorEngineRegistration> {
  if !HAS_ENGINES.load(std::sync::atomic::Ordering::Acquire) {
    return Vec::new();
  }
  read_registry(|r| r.engines.clone())
}

/// `selectors.setTestIdAttribute(name)` — the default every context
/// starts from. A context can still override it (`testIdAttribute` in
/// the `use` bag), which is what keeps two projects in one process from
/// fighting over the value.
pub fn set_default_test_id_attribute(name: &str) {
  registry()
    .write()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .test_id_attribute = Some(name.to_string());
  HAS_TEST_ID_OVERRIDE.store(true, std::sync::atomic::Ordering::Release);
}

/// The process-wide default test-id attribute.
#[must_use]
pub fn default_test_id_attribute() -> std::borrow::Cow<'static, str> {
  if !HAS_TEST_ID_OVERRIDE.load(std::sync::atomic::Ordering::Acquire) {
    return std::borrow::Cow::Borrowed(DEFAULT_TEST_ID_ATTRIBUTE);
  }
  read_registry(|r| {
    r.test_id_attribute
      .clone()
      .map_or(std::borrow::Cow::Borrowed(DEFAULT_TEST_ID_ATTRIBUTE), |a| {
        std::borrow::Cow::Owned(a)
      })
  })
}

/// Clear every registration. Tests only: the registry is process-global,
/// so a test that registers an engine would otherwise leak it into the
/// next one.
#[doc(hidden)]
pub fn reset_selector_registry() {
  let mut registry = registry().write().unwrap_or_else(std::sync::PoisonError::into_inner);
  registry.engines.clear();
  registry.test_id_attribute = None;
  HAS_ENGINES.store(false, std::sync::atomic::Ordering::Release);
  HAS_TEST_ID_OVERRIDE.store(false, std::sync::atomic::Ordering::Release);
}

/// Cached default lazy-inject IIFE when no custom selector engines and no
/// `testIdAttribute` override are registered (~180 KB saved per `new_page` /
/// `retire_context`).
static DEFAULT_LAZY_INJECT_JS: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Borrowed when no custom engines or test-id overrides are active, owned
/// otherwise.
#[must_use]
pub(crate) fn lazy_inject_js() -> std::borrow::Cow<'static, str> {
  if !HAS_ENGINES.load(std::sync::atomic::Ordering::Acquire)
    && !HAS_TEST_ID_OVERRIDE.load(std::sync::atomic::Ordering::Acquire)
  {
    let cached = DEFAULT_LAZY_INJECT_JS.get_or_init(|| build_lazy_inject_js_with(DEFAULT_TEST_ID_ATTRIBUTE));
    return std::borrow::Cow::Borrowed(cached.as_str());
  }
  std::borrow::Cow::Owned(build_lazy_inject_js_with(&default_test_id_attribute()))
}

/// Unified lazy-injection IIFE.
/// 1. Checks if already ready.
/// 2. Checks if currently injecting.
/// 3. Otherwise, starts injection and stores the promise.
///
/// Returns the `InjectedScript` instance.
#[must_use]
pub fn build_lazy_inject_js() -> String {
  lazy_inject_js().into_owned()
}

/// [`build_lazy_inject_js`] for a context whose `testIdAttribute` differs
/// from the process default.
///
/// The attribute reaches the page only for the strict-mode error text and
/// codegen: a `getByTestId` selector already carries the attribute name in
/// its body (`internal:testid=[<attr>=<value>]`), which is why the
/// functional path is decided Rust-side and this copy is cosmetic.
/// Registered selector engines, by contrast, only exist if the injected
/// script is told about them, so they ride along here — every new
/// document is injected afresh and picks up whatever is registered by
/// then.
#[must_use]
pub fn build_lazy_inject_js_with(test_id_attribute: &str) -> String {
  let engine_js = build_inject_js();
  let options = serde_json::json!({
    "testIdAttributeName": test_id_attribute,
    "customEngines": registered_selector_engines()
      .into_iter()
      .map(|e| serde_json::json!({ "name": e.name, "source": e.source }))
      .collect::<Vec<_>>(),
  });
  // The engine JS directly creates window.__fd at the end of its IIFE,
  // reading `window.__fdOptions` as it goes.
  //
  // An injection that races a navigation can run in the outgoing
  // document and still set `window.__fd` on the new one, through the
  // shared WindowProxy. That engine's functions belong to a dead realm, so
  // its promises never settle; `instanceof` against this realm's `Object`
  // tells it apart, and it is injected over rather than waited on.
  format!(
    r"(async () => {{
      if (window.__fd instanceof Object) return window.__fd;
      if (window.__fd === undefined && window.__fd_promise instanceof Promise) return await window.__fd_promise;
      window.__fd_promise = (async () => {{
        try {{
          window.__fdOptions = {options};
          {engine_js}
          return window.__fd;
        }} catch (e) {{
          window.__fd_promise = null;
          throw e;
        }}
      }})();
      return await window.__fd_promise;
    }})()"
  )
}

/// Result of a selector query -- lightweight info returned from JS.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct MatchedElement {
  pub index: usize,
  pub tag: String,
  pub text: String,
}

/// Whether `selectors.register` claimed this engine name.
#[must_use]
pub fn is_registered_selector_engine(name: &str) -> bool {
  if !HAS_ENGINES.load(std::sync::atomic::Ordering::Acquire) {
    return false;
  }
  let bare = name.strip_suffix(":light").unwrap_or(name);
  read_registry(|r| r.engines.iter().any(|e| e.name == bare))
}

// ─── JS Query Builder ───────────────────────────────────────────────────────

/// JS to inject the unified runtime once. Idempotent -- safe to call multiple times.
#[must_use]
pub fn build_inject_js() -> String {
  // Bundled Playwright-compatible selector engine + actionability checks + ferridriver helpers.
  // Built from crates/ferridriver/src/injected/ via `bun build.ts`.
  ENGINE_JS.to_string()
}

/// Build a lightweight query call (runtime must already be injected).
fn build_query_js(selector: &Selector, fd: &str) -> String {
  let parts_json = build_parts_json(selector);
  format!("{fd}.sel({parts_json})")
}

/// The injected JS engine — bundled from `src/injected/` TypeScript sources.
/// Rebuild with: `cd crates/ferridriver/src/injected && bun build.ts`
const ENGINE_JS: &str = include_str!("../injected/dist/engine.min.js");

pub(crate) const UTILITY_SCRIPT_JS: &str = include_str!("../injected/dist/utility-script.min.js");
pub(crate) const MCP_SUPPORT_JS: &str = include_str!("../injected/dist/mcp-support.min.js");
pub(crate) const AX_SUPPORT_JS: &str = include_str!("../injected/dist/ax-support.min.js");

/// Playwright's `StorageScript` class, as an expression. It collects and
/// restores an origin's `localStorage`, `IndexedDB` and private file
/// system from inside a page of that origin.
pub(crate) const STORAGE_SCRIPT_JS: &str = include_str!("../injected/dist/storage.min.js");

/// `expression` run with a `StorageScript` for `browser` bound to
/// `script`, its result awaited and stringified so every backend ships
/// it back as one flat string.
pub(crate) fn storage_script_call(browser: crate::backend::BackendKind, expression: &str) -> String {
  let name = match browser {
    crate::backend::BackendKind::Bidi => "firefox",
    crate::backend::BackendKind::WebKit => "webkit",
    crate::backend::BackendKind::CdpPipe
    | crate::backend::BackendKind::CdpWs
    | crate::backend::BackendKind::WebDriver => "chromium",
  };
  format!(
    "(async () => {{ const script = new ({STORAGE_SCRIPT_JS})({name:?}); return JSON.stringify(await ({expression})); }})()"
  )
}

/// On-demand devtools bundle (`window.__fdDevtools`): `Highlight`,
/// `generateSelector`, and `asLocator`. Evaluated only by highlight,
/// screenshot-mask, picker, recorder, `normalize_selector`, and
/// `generate_locator` paths — keeping ~43 KB out of the always-injected engine.
#[doc(hidden)]
pub const RECORDER_SUPPORT_JS: &str = include_str!("../injected/dist/recorder-support.min.js");

/// On-demand ARIA snapshot & template bundle (`window.__fdAria`). Evaluated
/// only by `ariaSnapshot*`, `toMatchAriaSnapshot`, and timed-out assertion
/// error context — keeping ~20 KB plus the `yaml` parser out of the
/// always-injected engine.
pub(crate) const ARIA_SUPPORT_JS: &str = include_str!("../injected/dist/aria-support.min.js");

/// Ensure the on-demand devtools bundle (`window.__fdDevtools`) is loaded in
/// `frame_id` (or the main frame when `None`).
pub(crate) async fn ensure_devtools_support(page: &AnyPage, frame_id: Option<&str>) -> Result<()> {
  let installed = match frame_id {
    Some(fid) => {
      page
        .evaluate_in_frame("window.__fdDevtools instanceof Object", fid)
        .await?
    },
    None => page.evaluate("window.__fdDevtools instanceof Object").await?,
  }
  .and_then(|v| v.as_bool())
  .unwrap_or(false);
  if !installed {
    run_void(page, RECORDER_SUPPORT_JS, frame_id).await?;
  }
  Ok(())
}

/// Ensure the on-demand ARIA bundle (`window.__fdAria`) is loaded in
/// `frame_id` (or the main frame when `None`).
pub(crate) async fn ensure_aria_support(page: &AnyPage, frame_id: Option<&str>) -> Result<()> {
  let installed = match frame_id {
    Some(fid) => page.evaluate_in_frame("window.__fdAria instanceof Object", fid).await?,
    None => page.evaluate("window.__fdAria instanceof Object").await?,
  }
  .and_then(|v| v.as_bool())
  .unwrap_or(false);
  if !installed {
    run_void(page, ARIA_SUPPORT_JS, frame_id).await?;
  }
  Ok(())
}

// ─── Query functions ────────────────────────────────────────────────────────

/// Query all elements matching a rich selector inside the execution
/// context of `frame_id` (or the main frame when `None`). Returns
/// lightweight info; injects the engine JS on first use.
///
/// # Errors
///
/// Returns an error if selector parsing or JS evaluation fails.
pub async fn query_all(page: &AnyPage, selector: &str, frame_id: Option<&str>) -> Result<Vec<MatchedElement>> {
  let parsed = parse(selector)?;
  page.ensure_engine_injected().await?;
  let fd = "window.__fd";
  let js = build_query_js(&parsed, fd);
  let result_str = match frame_id {
    Some(fid) => page.evaluate_in_frame(&js, fid).await,
    None => page.evaluate(&js).await,
  }?
  .and_then(|v| v.as_str().map(std::string::ToString::to_string))
  .unwrap_or_else(|| "[]".into());

  // Stream directly into `Vec<MatchedElement>` on the happy array path,
  // avoiding an intermediate `serde_json::Value` tree for large match sets.
  if result_str.trim_start().starts_with('{') {
    let val: serde_json::Value =
      serde_json::from_str(&result_str).map_err(|e| FerriError::Backend(format!("Parse selector results: {e}")))?;
    if let Some(err) = val.get("error").and_then(|e| e.as_str()) {
      return Err(FerriError::evaluation(err.to_string()));
    }
  }
  let elements: Vec<MatchedElement> =
    serde_json::from_str(&result_str).map_err(|e| FerriError::Backend(format!("Parse selector results: {e}")))?;
  Ok(elements)
}

/// Query a single element in `frame_id`'s execution context. If
/// `strict=true`, errors when 0 or >1 matches.
///
/// # Errors
///
/// Returns an error if selector parsing fails, no element is found, or (in strict mode)
/// multiple elements match.
pub async fn query_one(page: &AnyPage, selector: &str, strict: bool, frame_id: Option<&str>) -> Result<AnyElement> {
  let parsed = parse(selector)?;
  let parts_json = build_parts_json(&parsed);

  if strict {
    let matches = query_all(page, selector, frame_id).await?;
    if matches.is_empty() {
      return Err(FerriError::element_not_found(selector));
    }
    if matches.len() > 1 {
      cleanup_tags(page).await;
      return Err(FerriError::strict(selector, matches.len()));
    }
    // The query_all JS tags the match with `data-fd-sel='0'`; we resolve
    // that tag in the SAME frame so we get an element bound to the right
    // execution context.
    let fd = "window.__fd";
    let tagged_js = format!("{fd}.selOne([{{\"engine\":\"css\",\"body\":\"[data-fd-sel='0']\"}}])");
    let el = page
      .evaluate_to_element(&tagged_js, frame_id)
      .await
      .map_err(|_| FerriError::invalid_selector(selector, "could not resolve matched element"))?;
    cleanup_tags(page).await;
    return Ok(el);
  }

  page.ensure_engine_injected().await?;
  let fd = "window.__fd";
  let js = format!("{fd}.selOne({parts_json})");
  page
    .evaluate_to_element(&js, frame_id)
    .await
    .map_err(|_| FerriError::element_not_found(selector))
}

/// Query a single element using pre-built JS (avoids re-parsing the selector).
/// The `sel_js` should be the output of `build_selone_js()`. Element
/// resolution always runs in a frame's execution context — `frame_id`
/// is `None` for the main frame, `Some(id)` for an iframe. Mirrors
/// Playwright's frame-bound resolution model.
///
/// # Errors
///
/// Returns an error if no element is found or JS evaluation fails.
pub async fn query_one_prebuilt(
  page: &AnyPage,
  sel_js: &str,
  selector_display: &str,
  frame_id: Option<&str>,
) -> Result<AnyElement> {
  // Surface the underlying error verbatim when it carries
  // recognisable signal — most notably `strict mode violation: <N>`
  // thrown by the engine's `selOne(parts, strict=true)`. Falling back
  // to a generic "No element found" message would swallow that signal
  // and the locator retry loop would spin until timeout instead of
  // converting the strict-mode breach into a typed
  // `FerriError::StrictModeViolation`.
  page.evaluate_to_element(sel_js, frame_id).await.map_err(|err| {
    let msg = err.to_string();
    if msg.contains("strict mode violation") {
      err
    } else {
      FerriError::element_not_found(selector_display)
    }
  })
}

/// Build the JS expression for `selOne` from a selector string.
/// Call once, then pass to `query_one_prebuilt` in a retry loop.
///
/// `strict = true` makes the engine throw a recognisable
/// `strict mode violation: <count>` error when the selector matches
/// more than one element — mirrors Playwright's
/// `injected.querySelector(parsed, root, strict)` pattern. The host
/// catches the exception and converts it to a typed
/// `FerriError::StrictModeViolation`. Skipping the separate
/// `query_all` + `cleanup_tags` round-trips that the strict check
/// would otherwise need (~2 RTTs per locator action).
///
/// # Errors
///
/// Returns an error if the selector string cannot be parsed.
pub fn build_selone_js(selector: &str, fd: &str, strict: bool) -> Result<String> {
  let parsed = parse(selector)?;
  let parts_json = build_parts_json(&parsed);
  let strict_lit = if strict { "true" } else { "false" };
  Ok(format!("{fd}.selOne({parts_json},{strict_lit})"))
}

/// Resolve `selector` to a single element in `frame_id`'s execution
/// context and return the canonical selector Playwright's
/// recorder/codegen would emit for it (`injected.generateSelectorSimple`).
/// Mirrors `Frame.resolveSelector` in
/// `/tmp/playwright/packages/playwright-core/src/server/frames.ts:1274`.
///
/// # Errors
///
/// Returns an error if selector parsing fails, no element matches, or
/// (strict, like Playwright's `query`) more than one element matches.
/// Decode the `{ok} | {strict} | {none}` result the injected
/// `resolveOne` returns.
///
/// The shapes are data rather than exceptions because a thrown message
/// does not survive evaluation: the host sees `Uncaught` and the match
/// count is gone, so a strict violation was indistinguishable from any
/// other failure.
fn decode_resolve_one(raw: Option<serde_json::Value>, selector: &str) -> Result<String> {
  let text = raw
    .and_then(|v| v.as_str().map(std::string::ToString::to_string))
    .ok_or_else(|| FerriError::invalid_selector(selector, "no result from the injected engine"))?;
  let value: serde_json::Value =
    serde_json::from_str(&text).map_err(|e| FerriError::invalid_selector(selector, e.to_string()))?;

  if let Some(ok) = value.get("ok").and_then(serde_json::Value::as_str) {
    return Ok(ok.to_string());
  }
  if let Some(count) = value.get("strict").and_then(serde_json::Value::as_u64) {
    return Err(FerriError::strict(
      selector,
      usize::try_from(count).unwrap_or(usize::MAX),
    ));
  }
  Err(FerriError::element_not_found(selector))
}

/// Resolve `selector` to a single element in `frame_id`'s execution
/// context and return the canonical selector Playwright's
/// recorder/codegen would emit for it (`injected.generateSelectorSimple`).
/// Mirrors `Frame.resolveSelector` in
/// `/tmp/playwright/packages/playwright-core/src/server/frames.ts:1274`.
///
/// # Errors
///
/// Returns an error if selector parsing fails, no element matches, or
/// (strict, like Playwright's `query`) more than one element matches.
pub async fn normalize_selector(page: &AnyPage, selector: &str, frame_id: Option<&str>) -> Result<String> {
  let parsed = parse(selector)?;
  let parts_json = build_parts_json(&parsed);
  page.ensure_engine_injected().await?;
  ensure_devtools_support(page, frame_id).await?;
  let js = format!("window.__fd.normalizeSelector({parts_json})");
  let result = match frame_id {
    Some(fid) => page.evaluate_in_frame(&js, fid).await,
    None => page.evaluate(&js).await,
  };
  decode_resolve_one(result?, selector)
}

/// Resolve `selector` to a single element and return the locator
/// EXPRESSION a developer would paste into a test, rather than the
/// selector string [`normalize_selector`] returns.
///
/// `page.getByRole('button', { name: 'Sign in' })` instead of
/// `internal:role=button[name="Sign in"s]`. Mirrors Playwright's
/// `playwright.locator(element)` console helper.
///
/// # Errors
///
/// Returns an error if selector parsing fails, no element matches, or
/// more than one element matches.
pub async fn generate_locator(
  page: &AnyPage,
  selector: &str,
  frame_id: Option<&str>,
  language: ferridriver_types::OutputLanguage,
) -> Result<String> {
  let parsed = parse(selector)?;
  let parts_json = build_parts_json(&parsed);
  page.ensure_engine_injected().await?;
  ensure_devtools_support(page, frame_id).await?;
  let js = format!(
    "window.__fd.generateLocator({parts_json},{})",
    serde_json::json!(language.as_injected_language())
  );
  let result = match frame_id {
    Some(fid) => page.evaluate_in_frame(&js, fid).await,
    None => page.evaluate(&js).await,
  };
  decode_resolve_one(result?, selector)
}

/// Parse a `strict mode violation: <count>` exception message thrown
/// by the engine-side `selOne(parts, strict=true)` and return the
/// match count. Used by [`crate::locator::Locator`] to convert a
/// page-side strict violation into a typed
/// [`crate::error::FerriError::StrictModeViolation`] without paying
/// the separate `query_all` round-trip the previous implementation
/// did up-front. Mirrors Playwright's exception flow from
/// `injected.querySelector(parsed, root, strict)` —
/// `/tmp/playwright/packages/injected/src/injectedScript.ts:278`.
#[must_use]
pub fn parse_strict_violation_count<E: std::fmt::Display + ?Sized>(err: &E) -> Option<usize> {
  // Engine output (ferridriver's bundled selOne): `strict mode
  // violation: <N>` — `<N>` is the match count and appears immediately
  // after the colon.
  let message = err.to_string();
  let needle = "strict mode violation:";
  let (_, tail) = message.split_once(needle)?;
  let tail = tail.trim();
  let digits_end = tail.bytes().take_while(u8::is_ascii_digit).count();
  tail.get(..digits_end)?.parse().ok()
}

/// Set the element highlights `frame_id` shows to exactly `entries`
/// (selector, optional CSS), through `injected.setHighlights`. An empty
/// list clears them.
///
/// # Errors
///
/// Returns an error if a selector does not parse or JS evaluation fails.
pub async fn set_highlights(
  page: &AnyPage,
  entries: &[(String, Option<String>)],
  frame_id: Option<&str>,
) -> Result<()> {
  let mut list = Vec::with_capacity(entries.len());
  for (selector, style) in entries {
    let parts: serde_json::Value = serde_json::from_str(&build_parts_json(&parse(selector)?))?;
    list.push(serde_json::json!({ "parts": parts, "cssStyle": style }));
  }
  if !list.is_empty() {
    page.ensure_engine_injected().await?;
    ensure_devtools_support(page, frame_id).await?;
  }
  let js = format!(
    "window.__fd && window.__fd.setHighlights({})",
    serde_json::Value::Array(list)
  );
  run_void(page, &js, frame_id).await
}

/// Hide the element highlight overlay in `frame_id`'s execution context.
/// Mirrors Playwright's `frame._hideHighlight(selector)` ->
/// `injected.hideHighlight()`
/// (`/tmp/playwright/packages/playwright-core/src/server/frames.ts:1351`).
/// Playwright's `removeHighlight` is per-selector, but its public
/// `hideHighlight` tears the whole overlay down; `Locator.hideHighlight`
/// uses the latter, so we mirror that and drop the entire overlay.
///
/// # Errors
///
/// Returns an error if JS evaluation fails.
pub async fn hide_highlight(page: &AnyPage, frame_id: Option<&str>) -> Result<()> {
  page.ensure_engine_injected().await?;
  run_void(page, "window.__fd.hideHighlight()", frame_id).await
}

async fn run_void(page: &AnyPage, js: &str, frame_id: Option<&str>) -> Result<()> {
  match frame_id {
    Some(fid) => page.evaluate_in_frame(js, fid).await,
    None => page.evaluate(js).await,
  }?;
  Ok(())
}

/// Clean up any leftover selector tags (call after operations).
pub async fn cleanup_tags(page: &AnyPage) {
  let _ = page
    .evaluate(
      "(function() { \
        document.querySelectorAll('[data-fd-sel]').forEach(function(e) { \
            e.removeAttribute('data-fd-sel'); \
        }); \
    })()",
    )
    .await;
}

/// Build the one-round-trip read expression a Locator uses for
/// `textContent`, `getAttribute`, `isChecked` and the rest.
///
/// Every outcome other than the value comes back as DATA: a strict-mode
/// breach, no match, and an exception the read threw. These run through
/// a plain `evaluate`, where a thrown message degrades to `Uncaught`, so
/// the waiting loop could not tell a settled failure (more elements, or
/// an element the read rejects) from "not ready yet" and would spin
/// until the timeout.
///
/// `selAll` rather than `selOne`: the engine materialises every match
/// either way (`selOne` calls the same `executeSelector` and takes the
/// first), so the count costs nothing extra.
#[must_use]
pub fn build_read_js(parts_json: &str, strict: bool, js_body: &str, fd: &str) -> String {
  // A non-strict read cannot breach, so it keeps the original shape and
  // the value crosses unwrapped. Wrapping it unconditionally measured 7%
  // slower per read, because the envelope serialised the value and the
  // backend then serialised that again.
  if !strict {
    return format!(
      "(function() {{ var el = {fd}.selOne({parts_json}); if (!el) return {{{MISSING_KEY}: 1}}; \
       try {{ {js_body} }} catch (e) {{ return {{{ERROR_KEY}: String(e && e.message || e)}}; }} }})()"
    );
  }
  format!(
    "(function() {{ \
       var __a = {fd}.selAll({parts_json}); \
       if (__a.length > 1) return {{{STRICT_KEY}: __a.length}}; \
       if (!__a.length) return {{{MISSING_KEY}: 1}}; \
       var el = __a[0]; \
       try {{ return {{{VALUE_KEY}: (function() {{ {js_body} }})()}}; }} \
       catch (e) {{ return {{{ERROR_KEY}: String(e && e.message || e)}}; }} \
     }})()"
  )
}

/// Keys of the envelope a strict [`build_read_js`] returns. Prefixed
/// because the value beside them is arbitrary, and a plain `v` could be
/// mistaken for one of the value's own fields.
const STRICT_KEY: &str = "__fdStrict";
const VALUE_KEY: &str = "__fdValue";
/// What the read answers when nothing matched, which a read of a `null`
/// property must not be mistaken for.
const MISSING_KEY: &str = "__fdMissing";
const ERROR_KEY: &str = "__fdError";

/// Decode what [`build_read_js`] returned: `None` when no element
/// matched, the value the read produced (which may be `null`) otherwise.
///
/// # Errors
///
/// [`FerriError::StrictModeViolation`] when the selector matched more
/// than one element and the locator was strict, and
/// [`FerriError::Evaluation`] with the message the read threw.
pub fn decode_read_result(raw: Option<serde_json::Value>, selector: &str) -> Result<Option<serde_json::Value>> {
  let Some(mut value) = raw else {
    return Ok(None);
  };
  if value.get(MISSING_KEY).is_some() {
    return Ok(None);
  }
  if let Some(message) = value.get(ERROR_KEY).and_then(serde_json::Value::as_str) {
    return Err(FerriError::evaluation(message));
  }
  if let Some(count) = value.get(STRICT_KEY).and_then(serde_json::Value::as_u64) {
    return Err(FerriError::strict(
      selector,
      usize::try_from(count).unwrap_or(usize::MAX),
    ));
  }
  match value.as_object_mut().and_then(|map| map.remove(VALUE_KEY)) {
    Some(inner) => Ok(Some(inner)),
    // A non-strict read crosses unwrapped.
    None => Ok(Some(value)),
  }
}
