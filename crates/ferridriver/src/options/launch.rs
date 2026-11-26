//! Navigation, browser selection, launch, and connect option structs.

use super::context::{BrowserContextOptions, ProxyConfig};
use super::{LoadState, ViewportConfig};

/// Navigation options for goto/reload/goBack/goForward.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GotoOptions {
  /// When to consider navigation complete. Default: [`LoadState::Load`].
  pub wait_until: Option<LoadState>,
  /// Maximum navigation timeout in milliseconds.
  pub timeout: Option<u64>,
  /// HTTP `Referer` header to send with the navigation request. Mirrors
  /// `page.goto(url, { referer })`. If both this and
  /// `extraHTTPHeaders.referer` are set, this wins.
  pub referer: Option<String>,
}

/// Which browser product. Three `BrowserType` instances exposed as
/// `chromium`, `firefox`, and `webkit` on the top-level module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowserKind {
  /// Google Chrome / Chromium
  Chromium,
  /// Mozilla Firefox
  Firefox,
  /// Apple `WebKit` (macOS only)
  WebKit,
  Safari,
  /// Ladybird, driven through the `WebDriver` service it ships.
  Ladybird,
  /// Lightpanda, the headless browser that serves CDP itself.
  Lightpanda,
}

impl std::str::FromStr for BrowserKind {
  type Err = crate::error::FerriError;
  fn from_str(value: &str) -> Result<Self, Self::Err> {
    match value {
      "chromium" => Ok(Self::Chromium),
      "firefox" => Ok(Self::Firefox),
      "webkit" => Ok(Self::WebKit),
      "safari" => Ok(Self::Safari),
      "ladybird" => Ok(Self::Ladybird),
      "lightpanda" => Ok(Self::Lightpanda),
      _ => Err(crate::error::FerriError::invalid_argument(
        "browser",
        format!("unknown browser {value:?}; expected chromium, firefox, webkit, safari, ladybird, or lightpanda"),
      )),
    }
  }
}

impl BrowserKind {
  /// Product name string: `"chromium"` / `"firefox"` / `"webkit"`.
  /// Matches `BrowserType.name()`.
  #[must_use]
  pub fn name(self) -> &'static str {
    match self {
      Self::Chromium => "chromium",
      Self::Firefox => "firefox",
      Self::WebKit => "webkit",
      Self::Safari => "safari",
      Self::Ladybird => "ladybird",
      Self::Lightpanda => "lightpanda",
    }
  }

  /// Default backend protocol for this product. Chromium runs over the
  /// CDP pipe transport (lowest latency), Firefox over `WebDriver`
  /// `BiDi`, and `WebKit` over the native macOS host IPC.
  #[must_use]
  pub fn default_backend(self) -> crate::backend::BackendKind {
    match self {
      Self::Chromium => crate::backend::BackendKind::CdpPipe,
      Self::Firefox | Self::Ladybird => crate::backend::BackendKind::Bidi,
      Self::Safari => crate::backend::BackendKind::WebDriver,
      Self::Lightpanda => crate::backend::BackendKind::CdpWs,
      Self::WebKit => crate::backend::BackendKind::WebKit,
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserSelection {
  pub browser: BrowserKind,
  pub backend: crate::backend::BackendKind,
}

impl BrowserSelection {
  /// # Errors
  /// Rejects a protocol that cannot drive the requested browser.
  pub fn resolve(
    browser: Option<BrowserKind>,
    backend: Option<crate::backend::BackendKind>,
  ) -> crate::error::Result<Self> {
    Self::resolve_with_transport(browser, backend, None)
  }

  /// Resolve browser, backend protocol, and optional CDP wire transport together.
  ///
  /// # Errors
  /// Rejects a protocol or transport that cannot drive the requested browser.
  pub fn resolve_with_transport(
    browser: Option<BrowserKind>,
    backend: Option<crate::backend::BackendKind>,
    transport: Option<ChromiumTransport>,
  ) -> crate::error::Result<Self> {
    use crate::backend::BackendKind;
    let browser = browser.unwrap_or(match backend {
      Some(BackendKind::Bidi) => BrowserKind::Firefox,
      Some(BackendKind::WebKit) => BrowserKind::WebKit,
      _ => BrowserKind::Chromium,
    });
    let base_backend = backend.unwrap_or_else(|| browser.default_backend());
    let backend = base_backend.with_transport(transport)?;
    let compatible = matches!(
      (browser, backend),
      (
        BrowserKind::Chromium,
        BackendKind::CdpPipe | BackendKind::CdpWs | BackendKind::Bidi | BackendKind::WebDriver
      ) | (
        BrowserKind::Firefox | BrowserKind::Safari | BrowserKind::Ladybird,
        BackendKind::Bidi | BackendKind::WebDriver
      ) | (BrowserKind::WebKit, BackendKind::WebKit)
        | (BrowserKind::Lightpanda, BackendKind::CdpWs)
    );
    if !compatible {
      return Err(crate::error::FerriError::invalid_argument(
        "backend",
        format!(
          "{} cannot use {}; omit backend to select its driver automatically",
          browser.name(),
          backend.name()
        ),
      ));
    }
    Ok(Self { browser, backend })
  }
}

/// Public launch options bag, the `browserType.launch(options)`
/// parameter. Selection of which browser to launch happens via the
/// `BrowserType` instance you call `.launch(...)` on (`chromium()`,
/// `firefox()`, `webkit()`); this bag only carries the per-launch knobs.
#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
  /// Run in headless mode. Defaults to `true` (default).
  pub headless: Option<bool>,
  /// Path to a browser executable to run instead of the bundled one.
  pub executable_path: Option<String>,
  /// Extra command-line arguments to pass to the browser.
  pub args: Vec<String>,
  /// Chromium unpacked extension directories to load at launch.
  /// Chromium-only; other browser products ignore this list.
  pub extensions: Vec<std::path::PathBuf>,
  /// Browser distribution channel (e.g. `"chrome"`, `"chrome-beta"`,
  /// `"msedge"`). Currently surface-only — the bundled-browser resolver
  /// reads this when selecting between the headless shell and a
  /// channel-specific Chrome install.
  pub channel: Option<String>,
  /// Environment variables to set when spawning the browser process.
  pub env: Option<rustc_hash::FxHashMap<String, String>>,
  /// Slow down operations by this many ms (debugging).
  pub slow_mo: Option<u64>,
  /// Maximum time to wait for the browser to start. `0` means no
  /// timeout. Defaults to `30_000`.
  pub timeout: Option<u64>,
  /// Directory to use for downloads (per-context override is on
  /// [`BrowserContextOptions`] / persistent-context launch).
  pub downloads_path: Option<std::path::PathBuf>,
  /// If `true`, do not pass the bundled "default args"; if a list of
  /// strings, drop the named ones. Chromium-only: it is the only launch
  /// path that injects a switch list, and the others answer
  /// `Unsupported` rather than pretend to filter nothing.
  ///
  pub ignore_default_args: Option<IgnoreDefaultArgs>,
  /// Per-process signal handling — defaults all three to
  /// `true` (close the browser on SIGHUP / SIGINT / SIGTERM).
  pub handle_sighup: Option<bool>,
  pub handle_sigint: Option<bool>,
  pub handle_sigterm: Option<bool>,
  /// Enable Chromium sandboxing. Defaults to `false`.
  pub chromium_sandbox: Option<bool>,
  /// Firefox `about:config` user-prefs map.
  pub firefox_user_prefs: Option<rustc_hash::FxHashMap<String, serde_json::Value>>,
  /// Network proxy applied at the browser level.
  pub proxy: Option<ProxyConfig>,
  /// Tracing artifact directory.
  pub traces_dir: Option<std::path::PathBuf>,
}

/// `ignoreDefaultArgs?: boolean | string[]` shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IgnoreDefaultArgs {
  /// `true` — drop ALL default args.
  All,
  /// String list — drop just these.
  Some(Vec<String>),
}

/// Launch settings that apply to ONE named browser instance, layered
/// over the state's base plan.
///
/// A consumer (the MCP server's config, the test runner's project
/// routing) supplies these per instance name. Before this, the callback
/// could only contribute extra arguments, so an instance could not have
/// its own profile directory, binary, headless mode, backend or
/// environment — which is why every launch got a throwaway profile and
/// lost its cookies on restart.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InstanceOverrides {
  pub kind: Option<BrowserKind>,
  /// Extra browser arguments, appended to the base args.
  pub args: Vec<String>,
  /// `--user-data-dir` to launch with (persistent profile).
  pub user_data_dir: Option<String>,
  /// Browser binary for this instance.
  pub executable_path: Option<String>,
  /// Headless override.
  pub headless: Option<bool>,
  /// Backend override: instances may run different engines in one
  /// process, since each instance owns its own browser handle.
  pub backend: Option<crate::backend::BackendKind>,
  /// Extra environment variables for the browser process.
  pub env: rustc_hash::FxHashMap<String, String>,
  /// Built-in switches to drop for this instance.
  pub ignore_default_args: Option<IgnoreDefaultArgs>,
  /// The proxy this instance's `args` already point the browser at,
  /// carried whole because no flag can hold its credentials: they answer
  /// the proxy's challenges, and a context without a proxy of its own
  /// sends its request client through it. Not lowered into flags again.
  pub launched_proxy: Option<ProxyConfig>,
}

/// Connect-to-server options bag for `browserType.connect(wsEndpoint, options)`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ConnectOptions {
  pub headers: Option<rustc_hash::FxHashMap<String, String>>,
  pub slow_mo: Option<u64>,
  pub timeout: Option<u64>,
  pub expose_network: Option<String>,
  /// Extra W3C capabilities used when `connect()` targets a `WebDriver` HTTP
  /// endpoint. `browserName` defaults to the `BrowserType` product name.
  pub capabilities: Option<serde_json::Value>,
}

/// Connect-over-CDP options bag for `browserType.connectOverCDP(endpointURL, options)`.
/// Chromium-only.
#[derive(Debug, Clone, Default)]
pub struct ConnectOverCdpOptions {
  pub headers: Option<rustc_hash::FxHashMap<String, String>>,
  pub slow_mo: Option<u64>,
  pub timeout: Option<u64>,
}

/// Persistent-context launch options bag for
/// `browserType.launchPersistentContext(userDataDir, options)`. Composed
/// of the launch knobs plus a full [`BrowserContextOptions`] applied to
/// the implicit default context.
#[derive(Debug, Clone, Default)]
pub struct LaunchPersistentContextOptions {
  /// Per-launch knobs (mirror [`LaunchOptions`] exactly).
  pub launch: LaunchOptions,
  /// Per-context knobs applied to the default context that ships with
  /// the persistent profile.
  pub context: BrowserContextOptions,
}

/// Per-`BrowserType` instance configuration carried by `chromium()` /
/// `firefox()` / `webkit()` factories. The single field that varies
/// today is `transport` for Chromium — `chromium` is
/// always pipe-only; ferridriver lets callers override to the
/// `WebSocket` transport (`CdpWs`) for backend-coverage testing.
#[derive(Debug, Clone, Default)]
pub struct BrowserTypeOptions {
  /// Transport override for Chromium. Ignored for Firefox / `WebKit`.
  pub transport: Option<ChromiumTransport>,
}

/// Wire transport for the CDP backend (`pipe` or `ws`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChromiumTransport {
  /// CDP over Unix pipe (fd 3/4). Default for Chromium — lowest latency.
  Pipe,
  /// CDP over WebSocket. Used by `connectOverCDP`, Lightpanda, and explicitly
  /// selectable via `chromium({ transport: 'ws' })` or `transport = "ws"`.
  Ws,
}

impl ChromiumTransport {
  pub const ALL: &'static [&'static str] = &["pipe", "ws"];

  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Pipe => "pipe",
      Self::Ws => "ws",
    }
  }
}

impl std::str::FromStr for ChromiumTransport {
  type Err = crate::error::FerriError;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "pipe" => Ok(Self::Pipe),
      "ws" | "websocket" => Ok(Self::Ws),
      _ => Err(crate::error::FerriError::invalid_argument(
        "transport",
        format!("unknown transport {s:?}; expected {}", Self::ALL.join(", ")),
      )),
    }
  }
}

/// Internal launch plan. Carries fields that are NOT exposed on the
/// public [`LaunchOptions`] (which mirrors verbatim) but
/// that the runtime needs in order to launch / connect to the right
/// backend. Constructed by `BrowserType` from the public options bag
/// and the per-instance kind/transport, then handed to
/// [`crate::state::BrowserState::with_plan`].
#[derive(Debug, Clone)]
pub struct LaunchPlan {
  pub backend: crate::backend::BackendKind,
  pub kind: BrowserKind,
  pub headless: bool,
  pub executable_path: Option<String>,
  pub args: Vec<String>,
  pub channel: Option<String>,
  pub env: Option<rustc_hash::FxHashMap<String, String>>,
  pub user_data_dir: Option<String>,
  pub ws_endpoint: Option<String>,
  pub connection_headers: Option<rustc_hash::FxHashMap<String, String>>,
  pub auto_connect: Option<AutoConnectOptions>,
  pub default_viewport: Option<ViewportConfig>,
  pub slow_mo: Option<u64>,
  pub timeout: Option<u64>,
  pub downloads_path: Option<std::path::PathBuf>,
  pub ignore_default_args: Option<IgnoreDefaultArgs>,
  pub handle_sighup: Option<bool>,
  pub handle_sigint: Option<bool>,
  pub handle_sigterm: Option<bool>,
  pub chromium_sandbox: Option<bool>,
  pub firefox_user_prefs: Option<rustc_hash::FxHashMap<String, serde_json::Value>>,
  pub proxy: Option<ProxyConfig>,
  pub traces_dir: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone)]
pub struct AutoConnectOptions {
  pub channel: String,
  pub user_data_dir: Option<String>,
}

impl Default for LaunchPlan {
  fn default() -> Self {
    Self {
      backend: crate::backend::BackendKind::CdpPipe,
      kind: BrowserKind::Chromium,
      headless: true,
      executable_path: None,
      args: Vec::new(),
      channel: None,
      env: None,
      user_data_dir: None,
      ws_endpoint: None,
      connection_headers: None,
      auto_connect: None,
      default_viewport: Some(ViewportConfig::default()),
      slow_mo: None,
      timeout: None,
      downloads_path: None,
      ignore_default_args: None,
      handle_sighup: None,
      handle_sigint: None,
      handle_sigterm: None,
      chromium_sandbox: None,
      firefox_user_prefs: None,
      proxy: None,
      traces_dir: None,
    }
  }
}

impl LaunchPlan {
  /// Build a launch plan from the public [`LaunchOptions`] plus the
  /// per-`BrowserType` selection (`kind` + optional Chromium transport
  /// override).
  #[must_use]
  pub fn from_public(kind: BrowserKind, transport: Option<ChromiumTransport>, opts: LaunchOptions) -> Self {
    let backend = match (kind, transport) {
      (BrowserKind::Chromium, Some(ChromiumTransport::Ws)) => crate::backend::BackendKind::CdpWs,
      _ => kind.default_backend(),
    };
    let mut args = opts.args;
    if kind == BrowserKind::Chromium && !opts.extensions.is_empty() {
      let paths = opts
        .extensions
        .iter()
        .map(|path| path.to_string_lossy())
        .collect::<Vec<_>>()
        .join(",");
      args.push(format!("--load-extension={paths}"));
    }
    Self {
      backend,
      kind,
      headless: opts.headless.unwrap_or(kind != BrowserKind::Safari),
      executable_path: opts.executable_path,
      args,
      channel: opts.channel,
      env: opts.env,
      user_data_dir: None,
      ws_endpoint: None,
      connection_headers: None,
      auto_connect: None,
      default_viewport: Some(ViewportConfig::default()),
      slow_mo: opts.slow_mo,
      timeout: opts.timeout,
      downloads_path: opts.downloads_path,
      ignore_default_args: opts.ignore_default_args,
      handle_sighup: opts.handle_sighup,
      handle_sigint: opts.handle_sigint,
      handle_sigterm: opts.handle_sigterm,
      chromium_sandbox: opts.chromium_sandbox,
      firefox_user_prefs: opts.firefox_user_prefs,
      proxy: opts.proxy,
      traces_dir: opts.traces_dir,
    }
  }
}

impl Default for ViewportConfig {
  fn default() -> Self {
    Self {
      width: crate::state::DEFAULT_VIEWPORT_WIDTH,
      height: crate::state::DEFAULT_VIEWPORT_HEIGHT,
      device_scale_factor: 1.0,
      is_mobile: false,
      has_touch: false,
      is_landscape: false,
    }
  }
}

/// Per-context video-recording configuration: `recordVideo: { dir,
/// size? }` option on `browserType.launch` + `browser.newContext`.
/// Enabling it starts `CDP` screencast / `BiDi` polyfill recording on
/// every new page in the context; the file is finalised when the page
/// closes and surfaced via `page.video().path()`.
#[derive(Debug, Clone)]
pub struct RecordVideoOptions {
  /// Directory where the video file is written. One file per page.
  /// Filenames are derived from the page's created-at timestamp.
  pub dir: std::path::PathBuf,
  /// Optional explicit video dimensions. When `None`, ferridriver
  /// defaults to `800x450` (fallback when no viewport is set) unless the
  /// caller supplies a size. Values are forced to an even number of
  /// pixels so `libx264` accepts them without `yuv420p`-conversion
  /// warnings.
  pub size: Option<VideoSize>,
}

/// Video frame dimensions for [`RecordVideoOptions::size`]. Matches
/// `recordVideo.size: { width, height }` shape.
#[derive(Debug, Clone, Copy)]
pub struct VideoSize {
  pub width: u32,
  pub height: u32,
}

impl Default for VideoSize {
  fn default() -> Self {
    Self {
      width: 800,
      height: 450,
    }
  }
}

/// Matches Playwright's `constructURLBasedOnBaseURL`, including its invalid-input fallback.
#[must_use]
pub fn construct_url_with_base(base: Option<&str>, given: &str) -> String {
  let resolved = match base {
    Some(base) => reqwest::Url::parse(base).and_then(|base| base.join(given)),
    None => reqwest::Url::parse(given),
  };
  resolved.map_or_else(|_| given.to_owned(), Into::into)
}

#[cfg(test)]
mod string_or_regex_tests {
  use crate::options::StringOrRegex;

  fn parts(value: &StringOrRegex) -> (&str, &str) {
    match value {
      StringOrRegex::Regex { source, flags } => (source, flags),
      StringOrRegex::String(_) => panic!("expected the regex variant"),
    }
  }

  #[test]
  fn a_rust_regex_becomes_an_ecmascript_source_and_flags() {
    let plain = StringOrRegex::from(regex::Regex::new(r"^/todos$").unwrap());
    assert_eq!(parts(&plain), (r"^/todos$", ""));

    let flagged = StringOrRegex::from(&regex::Regex::new(r"(?is)ship\s+it").unwrap());
    assert_eq!(parts(&flagged), (r"ship\s+it", "is"));

    let named = StringOrRegex::from(regex::Regex::new(r"(?P<id>\d+)").unwrap());
    assert_eq!(parts(&named), (r"(?<id>\d+)", ""));

    // A group that is not purely flags stays in the pattern.
    let group = StringOrRegex::from(regex::Regex::new(r"(?:a|b)c").unwrap());
    assert_eq!(parts(&group), (r"(?:a|b)c", ""));
  }

  #[test]
  fn rust_side_matching_follows_playwright() {
    let exact = StringOrRegex::from("Count: 2");
    assert!(exact.matches("Count: 2"));
    assert!(!exact.matches("Count: 22"));
    assert!(exact.matches_with("count: 2", true));

    let re = StringOrRegex::regex(r"\bdone\b", "");
    assert!(re.matches("item done"));
    assert!(!re.matches("undone"));
    assert!(StringOrRegex::regex("DONE", "").matches_with("done", true));

    assert!(StringOrRegex::from("a b").matches_normalized("  a \n b ", false, false));
    assert!(StringOrRegex::from("b").matches_normalized("a  b c", false, true));

    // Lookbehind is JavaScript-only: refused by `compile`, matches nothing.
    let js_only = StringOrRegex::regex("(?<=a)b", "");
    assert!(js_only.compile().is_some_and(|r| r.is_err()));
    assert!(!js_only.matches("ab"));
    assert!(StringOrRegex::from("x").compile().is_none());
  }

  #[test]
  fn displays_as_playwright_prints_it() {
    assert_eq!(StringOrRegex::from("hi").to_string(), "\"hi\"");
    assert_eq!(StringOrRegex::regex("a+", "i").to_string(), "/a+/i");
  }
}

#[cfg(test)]
mod url_resolution_tests {
  use super::construct_url_with_base;

  #[test]
  fn resolves_queries_fragments_paths_and_absolute_urls() {
    let base = Some("https://example.com/one/two?old=1#before");
    for (given, expected) in [
      ("?next=2", "https://example.com/one/two?next=2"),
      ("#after", "https://example.com/one/two?old=1#after"),
      ("", "https://example.com/one/two?old=1"),
      ("../three", "https://example.com/three"),
      ("//other.example/path", "https://other.example/path"),
      ("/space here", "https://example.com/space%20here"),
      ("HTTP://EXAMPLE.COM:80/a/../b", "http://example.com/b"),
      ("mailto:hello@example.com", "mailto:hello@example.com"),
    ] {
      assert_eq!(construct_url_with_base(base, given), expected);
    }
    assert_eq!(
      construct_url_with_base(None, "https://example.com"),
      "https://example.com/"
    );
    assert_eq!(construct_url_with_base(None, "relative"), "relative");
    assert_eq!(
      construct_url_with_base(Some("invalid"), "https://example.com"),
      "https://example.com"
    );
    assert_eq!(construct_url_with_base(base, "http://["), "http://[");
  }
}
