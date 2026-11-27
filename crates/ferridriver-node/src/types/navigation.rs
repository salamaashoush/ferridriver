//! Navigation, lifecycle, emulation, frame, and launch/connect option bags.

use napi_derive::napi;

use super::{JsRegExpLike, f64_to_u64, string_or_regex_to_rust};

/// Navigation options (waitUntil, timeout, referer).
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct GotoOptions {
  /// When to consider navigation complete: "load", "domcontentloaded", "networkidle", "commit"
  pub wait_until: Option<String>,
  /// Maximum navigation timeout in milliseconds.
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
  /// HTTP `Referer` header to send with the navigation request. Mirrors
  /// Playwright's `page.goto(url, { referer })`.
  pub referer: Option<String>,
}

impl From<GotoOptions> for ferridriver::options::GotoOptions {
  fn from(o: GotoOptions) -> Self {
    Self {
      wait_until: o.wait_until.as_deref().map(ferridriver::options::LoadState::from),
      timeout: o.timeout.map(f64_to_u64),
      referer: o.referer,
    }
  }
}

/// Options for `page.waitForURL(url, { timeout?, waitUntil? })`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct WaitForUrlOptions {
  /// Maximum wait time in milliseconds.
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
  /// Lifecycle state to await once the URL matches: "load",
  /// "domcontentloaded", "networkidle", "commit".
  pub wait_until: Option<String>,
}

impl From<WaitForUrlOptions> for ferridriver::options::WaitForUrlOptions {
  fn from(o: WaitForUrlOptions) -> Self {
    Self {
      timeout: o.timeout.map(f64_to_u64),
      wait_until: o.wait_until.as_deref().map(ferridriver::options::LoadState::from),
    }
  }
}

/// Options for `frame.waitForNavigation({ timeout?, url?, waitUntil? })`.
#[napi(object, object_to_js = false)]
#[derive(Default)]
pub struct WaitForNavigationOptions {
  /// Maximum wait time in milliseconds.
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
  /// Only a navigation to a URL this matches ends the wait.
  #[napi(ts_type = "string | RegExp")]
  pub url: Option<napi::Either<String, JsRegExpLike>>,
  /// Lifecycle state to await once the navigation commits: "load",
  /// "domcontentloaded", "networkidle", "commit".
  pub wait_until: Option<String>,
}

impl TryFrom<WaitForNavigationOptions> for ferridriver::options::WaitForNavigationOptions {
  type Error = napi::Error;

  fn try_from(o: WaitForNavigationOptions) -> napi::Result<Self> {
    Ok(Self {
      timeout: o.timeout.map(f64_to_u64),
      url: o.url.map(string_or_regex_to_rust).transpose()?,
      wait_until: o.wait_until.as_deref().map(ferridriver::options::LoadState::from),
    })
  }
}

/// Options for `frame.setContent(html, { timeout?, waitUntil? })`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct SetContentOptions {
  /// Maximum time for the document to reach `waitUntil`, in milliseconds.
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
  /// When the content counts as set: "load", "domcontentloaded",
  /// "networkidle", "commit".
  pub wait_until: Option<String>,
}

impl From<SetContentOptions> for ferridriver::options::SetContentOptions {
  fn from(o: SetContentOptions) -> Self {
    Self {
      timeout: o.timeout.map(f64_to_u64),
      wait_until: o.wait_until.as_deref().map(ferridriver::options::LoadState::from),
    }
  }
}

/// Options for `frame.addScriptTag({ content?, path?, type?, url? })`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct AddScriptTagOptions {
  /// Raw JavaScript for an inline script.
  pub content: Option<String>,
  /// A file whose content becomes an inline script.
  pub path: Option<String>,
  /// The script's `type`, `module` for an ES module.
  #[napi(js_name = "type")]
  pub script_type: Option<String>,
  /// The `src` of an external script.
  pub url: Option<String>,
}

impl From<AddScriptTagOptions> for ferridriver::options::AddScriptTagOptions {
  fn from(o: AddScriptTagOptions) -> Self {
    Self {
      content: o.content,
      path: o.path.map(std::path::PathBuf::from),
      script_type: o.script_type,
      url: o.url,
    }
  }
}

/// Options for `frame.addStyleTag({ content?, path?, url? })`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct AddStyleTagOptions {
  /// Raw CSS for an inline stylesheet.
  pub content: Option<String>,
  /// A file whose content becomes an inline stylesheet.
  pub path: Option<String>,
  /// The `href` of an external stylesheet.
  pub url: Option<String>,
}

impl From<AddStyleTagOptions> for ferridriver::options::AddStyleTagOptions {
  fn from(o: AddStyleTagOptions) -> Self {
    Self {
      content: o.content,
      path: o.path.map(std::path::PathBuf::from),
      url: o.url,
    }
  }
}

/// Options for `frame.$(selector, { strict? })`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct QuerySelectorOptions {
  /// Whether more than one match is an error. The context's
  /// `strictSelectors` decides when this is left out.
  pub strict: Option<bool>,
}

/// `browserContext.grantPermissions(permissions, options?)`'s bag.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct GrantPermissionsOptions {
  /// Grant only for this origin (`https://example.com`); every origin
  /// when left out.
  pub origin: Option<String>,
}

/// Playwright's `TimeoutOptions`: the bag of `locator.textContent` and
/// the other reads.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct TimeoutOptions {
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
}

impl From<TimeoutOptions> for ferridriver::options::TimeoutOptions {
  fn from(o: TimeoutOptions) -> Self {
    Self {
      timeout: o.timeout.map(f64_to_u64),
    }
  }
}

/// The bag of `page.textContent(selector, options?)` and the other
/// selector reads: `StrictOptions & TimeoutOptions`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct SelectorReadOptions {
  pub strict: Option<bool>,
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
}

impl From<SelectorReadOptions> for ferridriver::options::SelectorReadOptions {
  fn from(o: SelectorReadOptions) -> Self {
    Self {
      strict: o.strict,
      timeout: o.timeout.map(f64_to_u64),
    }
  }
}

/// Options for `page.waitForLoadState(state?, { timeout? })`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct WaitForLoadStateOptions {
  /// Maximum wait time in milliseconds.
  pub timeout: Option<f64>,
  #[napi(ts_type = "AbortSignal")]
  pub signal: Option<crate::abort::Signal>,
}

impl From<WaitForLoadStateOptions> for ferridriver::options::WaitForLoadStateOptions {
  fn from(o: WaitForLoadStateOptions) -> Self {
    Self {
      timeout: o.timeout.map(f64_to_u64),
    }
  }
}

/// Options for `page.consoleMessages` / `page.pageErrors`. Playwright:
/// `{ filter?: 'all' | 'since-navigation' }` (default `since-navigation`).
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct ObservedFilterOptions {
  #[napi(ts_type = "'all' | 'since-navigation'")]
  pub filter: Option<String>,
}

/// Options for `page.close({ runBeforeUnload?, reason? })`. Playwright parity.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct PageCloseOptions {
  /// When `true`, fires the page's `beforeunload` handlers before
  /// unloading. CDP switches from `Target.closeTarget` (force) to
  /// `Page.close`; `BiDi` passes `promptUnload: true`; `WebKit` dispatches
  /// a synthetic `BeforeUnloadEvent` on the window.
  pub run_before_unload: Option<bool>,
  /// Human-readable reason surfaced on subsequent `TargetClosed` errors
  /// emitted to in-flight operations on this page.
  pub reason: Option<String>,
}

impl From<PageCloseOptions> for ferridriver::options::PageCloseOptions {
  fn from(o: PageCloseOptions) -> Self {
    Self {
      run_before_unload: o.run_before_unload,
      reason: o.reason,
    }
  }
}

/// Options for `browser.close({ reason? })`. Playwright parity.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct BrowserCloseOptions {
  /// Human-readable reason surfaced on `TargetClosed` errors emitted to
  /// in-flight operations on this browser's pages/contexts.
  pub reason: Option<String>,
}

impl From<BrowserCloseOptions> for ferridriver::options::BrowserCloseOptions {
  fn from(o: BrowserCloseOptions) -> Self {
    Self { reason: o.reason }
  }
}

/// Playwright-parity `page.emulateMedia` options per
/// `/tmp/playwright/packages/playwright-core/types/types.d.ts:2580`.
///
/// Every field is `Option<Either<String, Null>>` so we can surface all
/// three states Playwright's TS signature requires: absent (field not
/// present on the object → don't change), JS `null` (reset the override),
/// or a string value (apply it). Plain `Option<String>` would conflate
/// undefined and null — napi-rs would either reject null or silently fold
/// it into None, breaking the contract.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct EmulateMediaOptions {
  #[napi(ts_type = "null | 'screen' | 'print'")]
  pub media: Option<napi::Either<String, napi::bindgen_prelude::Null>>,
  #[napi(ts_type = "null | 'light' | 'dark' | 'no-preference'")]
  pub color_scheme: Option<napi::Either<String, napi::bindgen_prelude::Null>>,
  #[napi(ts_type = "null | 'reduce' | 'no-preference'")]
  pub reduced_motion: Option<napi::Either<String, napi::bindgen_prelude::Null>>,
  #[napi(ts_type = "null | 'active' | 'none'")]
  pub forced_colors: Option<napi::Either<String, napi::bindgen_prelude::Null>>,
  #[napi(ts_type = "null | 'no-preference' | 'more'")]
  pub contrast: Option<napi::Either<String, napi::bindgen_prelude::Null>>,
}

pub(crate) fn lower_override(
  v: Option<napi::Either<String, napi::bindgen_prelude::Null>>,
) -> ferridriver::options::MediaOverride {
  match v {
    None => ferridriver::options::MediaOverride::Unchanged,
    Some(napi::Either::A(s)) => ferridriver::options::MediaOverride::Set(s),
    Some(napi::Either::B(_)) => ferridriver::options::MediaOverride::Disabled,
  }
}

impl From<EmulateMediaOptions> for ferridriver::options::EmulateMediaOptions {
  fn from(o: EmulateMediaOptions) -> Self {
    Self {
      media: lower_override(o.media),
      color_scheme: lower_override(o.color_scheme),
      reduced_motion: lower_override(o.reduced_motion),
      forced_colors: lower_override(o.forced_colors),
      contrast: lower_override(o.contrast),
    }
  }
}

/// Selector bag form of [`FrameSelectorArg`] — matches Playwright's
/// object form `{ name?, url? }` (the `string` form is handled by
/// [`FrameSelectorArg`]'s `Either::A`).
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct FrameSelectorBag {
  pub name: Option<String>,
  pub url: Option<String>,
}

/// Playwright's `page.frame(frameSelector)` union —
/// `string | { name?, url? }`. napi-rs resolves the union at the JS
/// boundary; the `ts_args_type` on the call site forces the generated
/// `.d.ts` to the precise shape.
pub(crate) type FrameSelectorArg = napi::Either<String, FrameSelectorBag>;

impl From<FrameSelectorBag> for ferridriver::options::FrameSelector {
  fn from(b: FrameSelectorBag) -> Self {
    Self {
      name: b.name.filter(|s| !s.is_empty()),
      url: b.url.filter(|s| !s.is_empty()),
    }
  }
}

/// Launch options for the browser. Mirrors Playwright's `LaunchOptions`
/// (`/tmp/playwright/packages/playwright-core/types/types.d.ts:15192`).
/// Browser selection happens via the `BrowserType` you call `.launch`
/// on (`chromium()`, `firefox()`, `webkit()`); this bag carries only
/// the per-launch knobs.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
  /// Run in headless mode. Defaults to `true`.
  pub headless: Option<bool>,
  /// Path to the browser executable.
  pub executable_path: Option<String>,
  /// Additional browser arguments.
  pub args: Option<Vec<String>>,
  /// Chromium unpacked extension directories to load at launch.
  pub extensions: Option<Vec<String>>,
  /// Browser distribution channel ("chrome", "chrome-beta", "msedge", ...).
  pub channel: Option<String>,
  /// Environment variables visible to the browser.
  #[napi(ts_type = "{ [key: string]: string | undefined }")]
  pub env: Option<rustc_hash::FxHashMap<String, Option<String>>>,
  /// Slow down operations by this many ms (debugging).
  pub slow_mo: Option<u32>,
  /// Maximum time to wait for the browser to start (ms). `0` disables.
  pub timeout: Option<u32>,
  /// Where to save downloads (per-launch override).
  pub downloads_path: Option<String>,
  /// Tracing artifact directory.
  pub traces_dir: Option<String>,
  /// `true` drops every bundled switch; a list drops the named ones.
  /// Chromium-only, because it is the only launch path that injects a
  /// switch list.
  ///
  /// `--enable-automation` is one of them, so dropping the lot also
  /// stops Chrome reporting its own command line over CDP.
  #[napi(ts_type = "boolean | Array<string>")]
  pub ignore_default_args: Option<napi::Either<bool, Vec<String>>>,
  /// Proxy every context of this browser uses unless it names its own.
  ///
  /// Lowered per engine: Chromium takes `--proxy-server`, `WebKit` takes
  /// `--proxy`, and Firefox takes the `WebDriver` `proxy` capability -- it has no
  /// proxy switch at all.
  pub proxy: Option<crate::context::NapiProxyConfig>,
  /// Keep Chromium's sandbox; it is turned off unless this is `true`.
  pub chromium_sandbox: Option<bool>,
  /// Firefox preferences (`about:config`) the launched browser starts with.
  #[napi(ts_type = "{ [key: string]: string | number | boolean }")]
  pub firefox_user_prefs: Option<rustc_hash::FxHashMap<String, serde_json::Value>>,
}

/// Options for `chromium()` / `firefox()` / `webkit()` factory.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct BrowserTypeOptions {
  /// Wire transport. Chromium-only. `"pipe"` (default) drives CDP
  /// over a Unix pipe; `"ws"` drives CDP over WebSocket. Ignored on
  /// `firefox` / `webkit`.
  pub transport: Option<String>,
}

/// Options for `BrowserType.connect(wsEndpoint, options?)`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct ConnectOptions {
  pub headers: Option<crate::types::HashMap<String, String>>,
  pub slow_mo: Option<u32>,
  pub timeout: Option<u32>,
  pub expose_network: Option<String>,
  pub capabilities: Option<serde_json::Value>,
}

/// Options for `BrowserType.connectOverCDP(endpointURL, options?)`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct ConnectOverCdpOptions {
  pub headers: Option<crate::types::HashMap<String, String>>,
  pub slow_mo: Option<u32>,
  pub timeout: Option<u32>,
}
