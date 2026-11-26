//! Backend-agnostic value types, wire protocol/transport enums, and small
//! shared helpers used across all four browser backends.

use std::sync::Arc;

use crate::error::{FerriError, Result};

/// Shared empty JSON object `{}` for CDP commands that take no params.
///
/// CDP `send_command` / `build_command` borrow their params
/// (`&serde_json::Value`), so every no-param call serializes this single
/// static by reference instead of allocating and cloning a fresh `Map`.
/// The hot per-page-enable batch (`Page.enable` / `Runtime.enable` /
/// `Network.enable` / `DOM.enable`, fired on every new target) previously
/// paid five `Map::clone()`s per page; it now borrows this static. Built
/// once via `LazyLock`.
pub(crate) static EMPTY_PARAMS: std::sync::LazyLock<serde_json::Value> =
  std::sync::LazyLock::new(|| serde_json::Value::Object(serde_json::Map::new()));

/// When an operation given `timeout_ms` gives up. `0` is Playwright's no
/// timeout, so its deadline is a year off: never, for a navigation, and
/// inside the range tokio's timer accepts.
#[inline]
pub(crate) fn deadline_for(timeout_ms: u64) -> tokio::time::Instant {
  const NEVER: std::time::Duration = std::time::Duration::from_hours(365 * 24);
  let wait = if timeout_ms == 0 {
    NEVER
  } else {
    std::time::Duration::from_millis(timeout_ms).min(NEVER)
  };
  tokio::time::Instant::now() + wait
}

/// Owned empty JSON object `{}` for the few cold paths that build `params`
/// as an owned value (e.g. the cookie-context branch that conditionally
/// produces a populated map). Hot/no-param paths borrow [`EMPTY_PARAMS`].
pub(crate) fn empty_params() -> serde_json::Value {
  serde_json::Value::Object(serde_json::Map::new())
}

/// Event delivery prefers the newest live wrapper, while retaining weak
/// fallbacks when a temporary `context.pages()` wrapper is dropped.
#[derive(Clone, Default)]
pub struct PageBackref {
  inner: Arc<std::sync::Mutex<Vec<super::shared_page::ErasedWeakPage>>>,
  /// The page that opened this one, kept here rather than on a wrapper:
  /// every wrapper of a popup has to answer `opener()`, and the opener's
  /// wrapper found when the popup appeared is often a passing one (an
  /// input dispatched through `context.pages()`) that is gone a moment
  /// later. Held as the opener's own slot, so nothing pins its page.
  opener: Arc<std::sync::OnceLock<PageBackref>>,
}

impl PageBackref {
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  pub fn set(&self, weak: std::sync::Weak<crate::page::Page>) {
    let erased = super::shared_page::ErasedWeakPage::from_weak(weak);
    if let Ok(mut guard) = self.inner.lock() {
      guard.retain(|existing| existing.strong_count() > 0 && !existing.ptr_eq(&erased));
      guard.push(erased);
    }
  }

  #[must_use]
  pub fn upgrade(&self) -> Option<Arc<crate::page::Page>> {
    let mut guard = self.inner.lock().ok()?;
    while let Some(weak) = guard.last() {
      if let Some(page) = weak.upgrade() {
        return Some(page);
      }
      guard.pop();
    }
    None
  }

  #[must_use]
  pub fn weak(&self) -> std::sync::Weak<crate::page::Page> {
    self.upgrade().as_ref().map(Arc::downgrade).unwrap_or_default()
  }

  /// Whether `other` is the slot of the same page.
  #[must_use]
  pub(crate) fn same_page(&self, other: &Self) -> bool {
    Arc::ptr_eq(&self.inner, &other.inner)
  }

  /// Record the page that opened this one; the first call wins.
  pub(crate) fn set_opener(&self, opener: Self) {
    let _ = self.opener.set(opener);
  }

  /// The slot of the page that opened this one.
  #[must_use]
  pub(crate) fn opener(&self) -> Option<Self> {
    self.opener.get().cloned()
  }
}

/// Frame metadata (backend-agnostic).
pub use ferridriver_driver::FrameInfo;

/// Accessibility tree node (backend-agnostic).
#[derive(Debug, Clone)]
pub struct AxNodeData {
  pub node_id: String,
  pub parent_id: Option<String>,
  pub backend_dom_node_id: Option<i64>,
  pub ignored: bool,
  pub role: Option<String>,
  pub name: Option<String>,
  pub description: Option<String>,
  pub properties: Vec<AxProperty>,
}

#[derive(Debug, Clone)]
pub struct AxProperty {
  pub name: String,
  pub value: Option<serde_json::Value>,
}

/// One cookie as the browser stores it: the engine's record, which is
/// Playwright's `Cookie` / `SetNetworkCookieParam` shape.
pub type CookieData = ferrijs_fetch::Cookie;
/// Cookie `SameSite` attribute (`Strict | Lax | None`).
pub use ferrijs_fetch::SameSite;

/// Options for setting a cookie (matches Playwright's `SetNetworkCookieParam`).
/// Use `url` to derive domain/path automatically, or set `domain`/`path` directly.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCookieParams {
  pub name: String,
  pub value: String,
  /// URL to derive domain/path from. Mutually exclusive with domain/path.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub url: Option<String>,
  #[serde(default)]
  pub domain: String,
  #[serde(default)]
  pub path: String,
  #[serde(default)]
  pub secure: bool,
  #[serde(default)]
  pub http_only: bool,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub expires: Option<f64>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub same_site: Option<SameSite>,
}

impl From<SetCookieParams> for CookieData {
  fn from(p: SetCookieParams) -> Self {
    Self {
      name: p.name,
      value: p.value,
      domain: p.domain,
      path: if p.path.is_empty() { "/".to_string() } else { p.path },
      secure: p.secure,
      http_only: p.http_only,
      expires: p.expires,
      same_site: p.same_site,
      url: p.url,
    }
  }
}

/// Options for clearing cookies (matches Playwright's `ClearNetworkCookieOptions`).
/// All fields are optional filters -- only cookies matching ALL specified filters are cleared.
/// If no filters are specified, all cookies are cleared.
///
/// Playwright: `context.clearCookies(options?: { name?: string | RegExp,
/// domain?: string | RegExp, path?: string | RegExp })` — a string filter
/// is an exact match, a `RegExp` filter `.test()`s the field
/// (`/tmp/playwright/packages/playwright-core/src/server/browserContext.ts::clearCookies`).
#[derive(Debug, Clone, Default)]
pub struct ClearCookieOptions {
  /// Filter by cookie name.
  pub name: Option<crate::options::StringOrRegex>,
  /// Filter by domain.
  pub domain: Option<crate::options::StringOrRegex>,
  /// Filter by path.
  pub path: Option<crate::options::StringOrRegex>,
}

impl ClearCookieOptions {
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.name.is_none() && self.domain.is_none() && self.path.is_none()
  }

  /// Compile the filters once so per-cookie matching doesn't re-build
  /// regexes.
  ///
  /// # Errors
  ///
  /// Returns [`crate::error::FerriError::InvalidArgument`] when a regex
  /// filter fails to compile.
  pub fn compile(&self) -> Result<CompiledCookieFilter> {
    fn field(f: Option<&crate::options::StringOrRegex>) -> Result<Option<CookieFieldMatcher>> {
      match f {
        None => Ok(None),
        Some(crate::options::StringOrRegex::String(s)) => Ok(Some(CookieFieldMatcher::Exact(s.clone()))),
        Some(crate::options::StringOrRegex::Regex { source, flags }) => Ok(Some(CookieFieldMatcher::Regex(
          crate::url_matcher::compile_js_regex(source, flags)?,
        ))),
      }
    }
    Ok(CompiledCookieFilter {
      name: field(self.name.as_ref())?,
      domain: field(self.domain.as_ref())?,
      path: field(self.path.as_ref())?,
    })
  }
}

/// One compiled `clearCookies` field filter: exact string equality or a
/// JS-`RegExp`-derived regex test.
#[derive(Debug)]
enum CookieFieldMatcher {
  Exact(String),
  Regex(regex::Regex),
}

impl CookieFieldMatcher {
  fn matches(&self, value: &str) -> bool {
    match self {
      Self::Exact(s) => s == value,
      Self::Regex(re) => re.is_match(value),
    }
  }
}

/// Pre-compiled [`ClearCookieOptions`] — see [`ClearCookieOptions::compile`].
#[derive(Debug)]
pub struct CompiledCookieFilter {
  name: Option<CookieFieldMatcher>,
  domain: Option<CookieFieldMatcher>,
  path: Option<CookieFieldMatcher>,
}

impl CompiledCookieFilter {
  /// Whether a cookie matches ALL specified filters (unspecified fields
  /// always match).
  #[must_use]
  pub fn matches(&self, cookie: &CookieData) -> bool {
    self.name.as_ref().is_none_or(|m| m.matches(&cookie.name))
      && self.domain.as_ref().is_none_or(|m| m.matches(&cookie.domain))
      && self.path.as_ref().is_none_or(|m| m.matches(&cookie.path))
  }
}

/// One engine-level capture, as `screenshotter.ts::_screenshot` hands it
/// to a page delegate's `takeScreenshot`. Everything above the engine —
/// option validation, page preparation, masks, the background override
/// and the rect arithmetic — happens in the `screenshotter` module.
#[derive(Debug, Clone, Copy)]
pub struct ScreenshotCapture {
  pub format: ImageFormat,
  /// A rect in document coordinates; set for full-page and element
  /// captures.
  pub document_rect: Option<crate::options::ClipRect>,
  /// A rect in viewport coordinates; set for viewport captures.
  pub viewport_rect: Option<crate::options::ClipRect>,
  /// Already defaulted: jpeg 80, webp 100, `None` for png.
  pub quality: Option<u8>,
  /// Whether the captured rect fits the viewport; when it does not the
  /// engine has to capture beyond it.
  pub fits_viewport: bool,
  pub scale: ScreenshotScale,
}

/// An RGBA background override (`setDefaultBackgroundColorOverride`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackgroundColor {
  pub r: u8,
  pub g: u8,
  pub b: u8,
  pub a: f64,
}

impl BackgroundColor {
  pub const TRANSPARENT: Self = Self {
    r: 0,
    g: 0,
    b: 0,
    a: 0.0,
  };
}

/// `scale` option for screenshots — mirrors Playwright's `"css" | "device"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotScale {
  /// One image pixel per CSS pixel — small, DPR-independent output.
  Css,
  /// One image pixel per device pixel — sharp, larger on Retina.
  Device,
}

/// Re-encode a PNG capture as WebP the way Playwright's `encodeWebp`
/// does for engines without a WebP encoder: lossless when `quality` is
/// absent or 100, lossy at that quality otherwise.
pub(crate) fn encode_webp_from_png(png: &[u8], quality: Option<u8>) -> Result<Vec<u8>> {
  let image = image::load_from_memory_with_format(png, image::ImageFormat::Png)
    .map_err(|e| FerriError::backend(format!("screenshot png decode: {e}")))?
    .to_rgba8();
  let webp = webp::Encoder::from_rgba(image.as_raw(), image.width(), image.height());
  let bytes = match quality {
    Some(q) if q < 100 => webp.encode(f32::from(q)),
    _ => webp.encode_lossless(),
  };
  Ok(bytes.to_vec())
}

/// Image format for screenshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
  Png,
  Jpeg,
  Webp,
}

/// Performance metric (backend-agnostic).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MetricData {
  pub name: String,
  pub value: f64,
}

/// Backend-ready click arguments. Produced by
/// `BackendClickArgs::from_options` from the user's
/// [`crate::options::ClickOptions`]. Every per-backend `click_at_with`
/// receives this struct — keeps the wire-level per-backend impls free
/// of `ClickOptions` parsing and defaulting logic. The modifiers are
/// not here: every mouse event carries the page keyboard's held ones.
#[derive(Debug, Clone)]
pub struct BackendClickArgs {
  pub button: crate::options::MouseButton,
  pub click_count: u32,
  /// Milliseconds to sleep between `mousedown` and `mouseup`.
  pub delay_ms: u64,
  /// Interpolated `mousemove` events between the current cursor and
  /// `(x, y)`. `1` = single move at destination.
  pub steps: u32,
}

impl BackendClickArgs {
  #[must_use]
  pub fn from_options(opts: &crate::options::ClickOptions) -> Self {
    Self {
      button: opts.resolved_button(),
      click_count: opts.resolved_click_count(),
      delay_ms: opts.resolved_delay_ms(),
      steps: opts.resolved_steps(),
    }
  }

  /// Convenience factory for the "no options, just click once with left"
  /// default path — wire for existing `click_at` call sites.
  #[must_use]
  pub fn default_left() -> Self {
    Self {
      button: crate::options::MouseButton::Left,
      click_count: 1,
      delay_ms: 0,
      steps: 1,
    }
  }
}

/// Backend-ready hover arguments. Produced by
/// [`crate::actions::hover_with_opts`] from the user's
/// [`crate::options::HoverOptions`].
#[derive(Debug, Clone, Copy)]
pub struct BackendHoverArgs {
  /// Interpolated `mousemove` events between the current cursor and
  /// `(x, y)`. `1` = single move at destination.
  pub steps: u32,
}

/// Navigation lifecycle target — which CDP event to wait for after Page.navigate.
/// Matches Playwright's `waitUntil` semantics exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavLifecycle {
  /// `Page.frameNavigated` — response committed, new document started.
  Commit,
  /// `Page.lifecycleEvent` name="`DOMContentLoaded`" — HTML parsed, DOM ready.
  DomContentLoaded,
  /// `Page.lifecycleEvent` name="load" — all resources loaded.
  Load,
}

impl NavLifecycle {
  /// Parse from a `waitUntil` string (Playwright / MCP convention).
  /// Unknown values default to `Load`.
  #[must_use]
  pub fn parse_lifecycle(s: &str) -> Self {
    match s {
      "commit" => Self::Commit,
      "domcontentloaded" => Self::DomContentLoaded,
      _ => Self::Load,
    }
  }
}

/// Wire protocol spoken to the browser, independent of the transport layer
/// (`pipe` vs `ws` for CDP).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
  /// Chrome `DevTools` Protocol (Chromium, Chrome, Lightpanda).
  Cdp,
  /// `WebDriver` `BiDi` (Firefox, Ladybird, Chromium, Safari).
  Bidi,
  /// Playwright `WebKit` Inspector protocol.
  #[serde(rename = "webkit")]
  WebKit,
  /// W3C Classic `WebDriver`.
  #[serde(rename = "webdriver", alias = "web-driver")]
  WebDriver,
}

impl Protocol {
  pub const ALL: &'static [&'static str] = &["cdp", "bidi", "webkit", "webdriver"];

  /// # Errors
  /// Rejects unknown protocol names.
  pub fn parse(value: &str) -> Result<Self> {
    match value {
      "cdp" => Ok(Self::Cdp),
      "bidi" => Ok(Self::Bidi),
      "webkit" => Ok(Self::WebKit),
      "webdriver" | "web-driver" => Ok(Self::WebDriver),
      _ => Err(FerriError::invalid_argument(
        "protocol",
        format!("unknown protocol {value:?}; expected {}", Self::ALL.join(", ")),
      )),
    }
  }

  #[must_use]
  pub const fn name(self) -> &'static str {
    match self {
      Self::Cdp => "cdp",
      Self::Bidi => "bidi",
      Self::WebKit => "webkit",
      Self::WebDriver => "webdriver",
    }
  }
}

/// Which backend (protocol + transport) to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
  /// Chrome `DevTools` Protocol over pipes (`--remote-debugging-pipe`).
  #[serde(rename = "cdp-pipe", alias = "cdp")]
  CdpPipe,
  /// Chrome `DevTools` Protocol over WebSocket.
  #[serde(rename = "cdp-ws", alias = "cdp-raw")]
  CdpWs,
  /// Playwright `WebKit` Inspector protocol (cross-platform, bundled binary).
  #[serde(rename = "webkit")]
  WebKit,
  /// `WebDriver` `BiDi` protocol (cross-browser: Firefox, Ladybird, Chromium, Safari).
  Bidi,
  #[serde(rename = "webdriver", alias = "web-driver")]
  WebDriver,
}

impl BackendKind {
  pub const ALL: &'static [&'static str] = &["cdp-pipe", "cdp-ws", "bidi", "webkit", "webdriver"];

  /// # Errors
  /// Rejects unknown protocol names.
  pub fn parse(value: &str) -> Result<Self> {
    match value {
      "cdp" | "cdp-pipe" => Ok(Self::CdpPipe),
      "cdp-ws" | "cdp-raw" => Ok(Self::CdpWs),
      "bidi" => Ok(Self::Bidi),
      "webkit" => Ok(Self::WebKit),
      "web-driver" | "webdriver" => Ok(Self::WebDriver),
      _ => Err(FerriError::invalid_argument(
        "backend",
        format!("unknown backend {value:?}; expected {}", Self::ALL.join(", ")),
      )),
    }
  }

  /// Build a backend kind from a wire [`Protocol`] and an optional CDP transport.
  ///
  /// # Errors
  /// Rejects a transport override paired with a non-CDP protocol.
  pub fn from_protocol(protocol: Protocol, transport: Option<crate::options::ChromiumTransport>) -> Result<Self> {
    match (protocol, transport) {
      (Protocol::Cdp, None | Some(crate::options::ChromiumTransport::Pipe)) => Ok(Self::CdpPipe),
      (Protocol::Cdp, Some(crate::options::ChromiumTransport::Ws)) => Ok(Self::CdpWs),
      (Protocol::Bidi, None) => Ok(Self::Bidi),
      (Protocol::WebKit, None) => Ok(Self::WebKit),
      (Protocol::WebDriver, None) => Ok(Self::WebDriver),
      (other, Some(t)) => Err(FerriError::invalid_argument(
        "transport",
        format!(
          "transport '{}' only applies to CDP backends, not {}",
          t.as_str(),
          other.name()
        ),
      )),
    }
  }

  /// The wire [`Protocol`] this backend speaks, independent of transport.
  #[must_use]
  pub const fn protocol(self) -> Protocol {
    match self {
      Self::CdpPipe | Self::CdpWs => Protocol::Cdp,
      Self::WebKit => Protocol::WebKit,
      Self::Bidi => Protocol::Bidi,
      Self::WebDriver => Protocol::WebDriver,
    }
  }

  /// The CDP wire transport, or `None` for non-CDP backends.
  #[must_use]
  pub const fn transport(self) -> Option<crate::options::ChromiumTransport> {
    match self {
      Self::CdpPipe => Some(crate::options::ChromiumTransport::Pipe),
      Self::CdpWs => Some(crate::options::ChromiumTransport::Ws),
      Self::WebKit | Self::Bidi | Self::WebDriver => None,
    }
  }

  /// Apply an explicit CDP transport override to this backend.
  ///
  /// # Errors
  /// Rejects a transport override on a non-CDP backend.
  pub fn with_transport(self, transport: Option<crate::options::ChromiumTransport>) -> Result<Self> {
    match transport {
      None => Ok(self),
      Some(t) => Self::from_protocol(self.protocol(), Some(t)),
    }
  }

  #[must_use]
  pub fn as_str(self) -> &'static str {
    self.name()
  }

  #[must_use]
  pub fn kind(self) -> Self {
    self
  }

  #[must_use]
  pub fn name(self) -> &'static str {
    match self {
      Self::CdpPipe => "cdp-pipe",
      Self::CdpWs => "cdp-ws",
      Self::WebKit => "webkit",
      Self::Bidi => "bidi",
      Self::WebDriver => "webdriver",
    }
  }
}

/// Create a configured profile directory before the browser is told to
/// use it.
///
/// Chromium creates a missing `--user-data-dir` itself; the others do
/// not. Playwright levels this and says why: "Firefox bails if the
/// profile directory does not exist, Chrome creates it. We ensure
/// consistent behavior here." (`server/browserType.ts:165-169`, with
/// `mode: 0o700`).
pub(crate) fn ensure_user_data_dir(dir: Option<&str>) -> crate::error::Result<()> {
  let Some(dir) = dir else {
    return Ok(());
  };
  let path = std::path::Path::new(dir);
  if path.is_dir() {
    return Ok(());
  }
  std::fs::create_dir_all(path).map_err(|e| {
    crate::error::FerriError::backend(format!(
      "could not create the configured user data directory `{dir}`: {e}"
    ))
  })?;
  // The profile holds cookies and credentials, so it is the owner's
  // alone — the mode Playwright sets for the same reason.
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
      crate::error::FerriError::backend(format!(
        "could not restrict the user data directory `{dir}` to its owner: {e}"
      ))
    })?;
  }
  Ok(())
}

/// A key combo split into its keys as Playwright splits one
/// (`server/input.ts::split`): a `+` that starts a key is that key, so
/// `"+"` is the plus key and `"Shift++"` is Shift and plus, where a plain
/// `split('+')` leaves empty names behind.
pub(crate) fn split_key_combo(combo: &str) -> Vec<&str> {
  let mut keys = Vec::new();
  let mut rest = combo;
  loop {
    // A `+` that starts `rest` is the key itself, so the separator is
    // the next `+` after the first character.
    let first = rest.chars().next().map_or(0, char::len_utf8);
    let Some(at) = rest.get(first..).and_then(|tail| tail.find('+')).map(|at| at + first) else {
      break;
    };
    let (key, tail) = rest.split_at(at);
    keys.push(key);
    rest = tail.get(1..).unwrap_or_default();
  }
  keys.push(rest);
  keys
}

#[cfg(test)]
mod key_combo_tests {
  use super::split_key_combo;

  #[test]
  fn a_plus_that_starts_a_key_is_the_plus_key() {
    assert_eq!(split_key_combo("+"), ["+"]);
    assert_eq!(split_key_combo("Shift++"), ["Shift", "+"]);
    assert_eq!(split_key_combo("Control+Shift+T"), ["Control", "Shift", "T"]);
    assert_eq!(split_key_combo("a"), ["a"]);
    assert_eq!(split_key_combo("Control+="), ["Control", "="]);
  }
}
