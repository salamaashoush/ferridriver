//! Browser executable detection, Chrome launch flags, and CDP endpoint discovery.

use crate::error::{FerriError, Result};

pub(super) async fn discover_ws_from_http(http_url: &str) -> Result<String> {
  discover_ws_with_headers(http_url, None).await
}

pub(super) async fn discover_ws_with_headers(
  http_url: &str,
  headers: Option<&rustc_hash::FxHashMap<String, String>>,
) -> Result<String> {
  let budget = crate::operation_budget::OperationBudget::current(10_000)?;
  budget.wait(async {
    let mut url = reqwest::Url::parse(http_url).map_err(|error| FerriError::invalid_argument("url", error.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
      return Err(FerriError::invalid_argument("url", "CDP discovery requires an HTTP or HTTPS endpoint"));
    }
    url.set_path(&format!("{}/json/version", url.path().trim_end_matches('/')));
    let response = crate::backend::webdriver::http_client(headers)?.get(url.clone()).send().await
      .map_err(|error| FerriError::backend(format!("CDP discovery request failed: {error}")))?;
    if !response.status().is_success() {
      return Err(FerriError::backend(format!(
        "CDP discovery returned HTTP {}. Pass the browser WebSocket URL directly or use the profile's DevToolsActivePort file.", response.status()
      )));
    }
    let peer = response.remote_addr();
    let body: serde_json::Value = response.json().await.map_err(|error| FerriError::backend(format!("Parse /json/version: {error}")))?;
    let endpoint = body["webSocketDebuggerUrl"].as_str().ok_or_else(|| FerriError::backend("No webSocketDebuggerUrl in /json/version"))?;
    let websocket = reqwest::Url::parse(endpoint).map_err(|error| FerriError::backend(format!("Invalid debugger endpoint: {error}")))?;
    // Local Chrome can advertise localhost despite listening on only one IP family.
    if websocket.scheme() == "ws" && websocket.host_str() == Some("localhost")
      && let Some(peer) = peer.filter(|address| address.ip().is_loopback())
      && (url.host_str() == Some("localhost") || url.host_str().is_some_and(|host| host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()))) {
      return Ok(pin_ws_authority(endpoint, peer));
    }
    Ok(endpoint.to_owned())
  }).await.map_err(|error| error.error("discovering CDP endpoint"))?
}

/// Replace the host:port authority of a `ws://`/`wss://` URL with `addr`,
/// preserving the scheme and path. Returns the URL unchanged if it does not
/// look like a ws URL with an authority.
pub(super) fn pin_ws_authority(ws_url: &str, addr: std::net::SocketAddr) -> String {
  for scheme in ["ws://", "wss://"] {
    if let Some(rest) = ws_url.strip_prefix(scheme) {
      let path = rest.find('/').and_then(|i| rest.get(i..)).unwrap_or("");
      return format!("{scheme}{addr}{path}");
    }
  }
  ws_url.to_string()
}

/// Discover a running Chrome instance by reading its `DevToolsActivePort` file.
pub(super) fn discover_chrome_ws(channel: &str, explicit_user_data_dir: Option<&str>) -> Result<String> {
  let candidates: Vec<std::path::PathBuf> = if let Some(dir) = explicit_user_data_dir {
    vec![std::path::PathBuf::from(dir)]
  } else {
    chrome_default_user_data_dirs(channel)?
  };

  // A profile directory existing proves only that the browser is installed. Only
  // DevToolsActivePort proves one is running with debugging on, so keep walking
  // the candidates until a readable port file turns up.
  let mut last_err = None;
  for user_data_dir in &candidates {
    let port_file = user_data_dir.join("DevToolsActivePort");
    match std::fs::read_to_string(&port_file) {
      Ok(content) => return parse_devtools_active_port(&content),
      Err(e) => last_err = Some(format!("{}: {e}", port_file.display())),
    }
  }

  let browser = if channel == "chromium" { "Chromium" } else { "Chrome" };
  Err(FerriError::Backend(format!(
    "Cannot read DevToolsActivePort ({}). Ensure {browser} ({channel}) is running with remote debugging \
     enabled at chrome://inspect/#remote-debugging, or pass user_data_dir explicitly.",
    last_err.unwrap_or_else(|| "no candidate profile directories".to_string())
  )))
}

pub(super) fn parse_devtools_active_port(content: &str) -> Result<String> {
  let lines: Vec<&str> = content.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
  let (Some(port_line), Some(path)) = (lines.first(), lines.get(1)) else {
    return Err(FerriError::Backend(format!(
      "Invalid DevToolsActivePort content: {content:?}"
    )));
  };

  let port: u16 = port_line
    .parse()
    .map_err(|_| FerriError::Backend(format!("Invalid port '{port_line}' in DevToolsActivePort")))?;

  Ok(format!("ws://127.0.0.1:{port}{path}"))
}

/// Profile directories to search for a running browser, best match first.
///
/// Returns every directory that exists rather than the first, because an
/// installed-but-idle Chrome must not shadow a Chromium that is actually
/// running with debugging enabled.
pub(super) fn chrome_default_user_data_dirs(channel: &str) -> Result<Vec<std::path::PathBuf>> {
  let home = std::env::var("HOME")
    .or_else(|_| std::env::var("USERPROFILE"))
    .map_err(|_| FerriError::backend("Cannot determine home directory"))?;

  let os = std::env::consts::OS;
  let suffix = match channel {
    "stable" | "chrome" => "",
    "beta" => " Beta",
    "dev" => " Dev",
    "canary" => " Canary",
    "chromium" => {
      let path = match os {
        "linux" => std::path::PathBuf::from(&home).join(".config/chromium"),
        "macos" => std::path::PathBuf::from(&home).join("Library/Application Support/Chromium"),
        "windows" => {
          let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| format!("{home}/AppData/Local"));
          std::path::PathBuf::from(local_app_data).join("Chromium/User Data")
        },
        _ => return Err(FerriError::unsupported(format!("OS: {os}"))),
      };
      return Ok(vec![path]);
    },
    other => {
      return Err(FerriError::invalid_argument(
        "channel",
        format!("unknown Chrome channel: {other}"),
      ));
    },
  };

  let chrome_path = match os {
    "linux" => {
      let dir_name = if suffix.is_empty() {
        "google-chrome".to_string()
      } else {
        format!("google-chrome{}", suffix.to_lowercase().replace(' ', "-"))
      };
      std::path::PathBuf::from(&home).join(".config").join(dir_name)
    },
    "macos" => std::path::PathBuf::from(&home)
      .join("Library/Application Support")
      .join(format!("Google/Chrome{suffix}")),
    "windows" => {
      let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| format!("{home}/AppData/Local"));
      std::path::PathBuf::from(local_app_data).join(format!("Google/Chrome{suffix}/User Data"))
    },
    _ => {
      return Err(FerriError::unsupported(format!("OS: {os}")));
    },
  };

  let chromium_path = match os {
    "linux" => Some(std::path::PathBuf::from(&home).join(".config/chromium")),
    "macos" => Some(std::path::PathBuf::from(&home).join("Library/Application Support/Chromium")),
    "windows" => {
      let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| format!("{home}/AppData/Local"));
      Some(std::path::PathBuf::from(local_app_data).join("Chromium/User Data"))
    },
    _ => None,
  };

  let found: Vec<_> = std::iter::once(chrome_path.clone())
    .chain(chromium_path.clone())
    .filter(|p| p.exists())
    .collect();

  if found.is_empty() {
    return Err(FerriError::Backend(match chromium_path {
      Some(c) => format!(
        "Chrome user data dir not found at {} or {}",
        chrome_path.display(),
        c.display()
      ),
      None => format!("Chrome user data dir not found: {}", chrome_path.display()),
    }));
  }

  Ok(found)
}

/// Common Chrome/Chromium launch flags used by cdp-pipe and cdp-raw backends.
#[must_use]
/// Build Chrome flags matching Playwright's launch sequence exactly.
/// Order: chromiumSwitches → headless flags → sandbox → user args.
pub fn chrome_flags(headless: bool, extra_args: &[String]) -> Vec<String> {
  chrome_flags_with(headless, extra_args, None, false)
}

/// [`chrome_flags`] with Playwright's `ignoreDefaultArgs`.
///
/// Every built-in switch is a policy decision that can conflict with a
/// caller's environment (a proxy that needs background networking, an
/// extension that needs the sandbox). Playwright has always let a
/// caller drop them; ferridriver carried the option on `LaunchPlan` and
/// then applied it nowhere, so the flags were unconditional.
#[must_use]
pub fn chrome_flags_with(
  headless: bool,
  extra_args: &[String],
  ignore: Option<&crate::options::IgnoreDefaultArgs>,
  sandbox: bool,
) -> Vec<String> {
  use crate::options::IgnoreDefaultArgs;

  let drop_all = matches!(ignore, Some(IgnoreDefaultArgs::All));
  let dropped: &[String] = match ignore {
    Some(IgnoreDefaultArgs::Some(list)) => list,
    _ => &[],
  };
  // Compare on the switch NAME so `--foo=bar` is dropped by `--foo`.
  let is_dropped = |flag: &str| {
    let name = flag.split('=').next().unwrap_or(flag);
    dropped.iter().any(|d| {
      let d_name = d.split('=').next().unwrap_or(d);
      d_name == name
    })
  };

  let mut flags: Vec<String> = Vec::with_capacity(40 + extra_args.len());
  let loads_extension = extra_args.iter().any(|arg| arg.starts_with("--load-extension="));

  // 1. Base chromiumSwitches (from Playwright's chromiumSwitches.ts)
  if !drop_all {
    for f in CHROMIUM_SWITCHES {
      if loads_extension && *f == "--disable-extensions" {
        continue;
      }
      if !is_dropped(f) {
        flags.push((*f).into());
      }
    }
  }

  // 2. Always added after base switches
  if !drop_all && !is_dropped("--enable-unsafe-swiftshader") {
    flags.push("--enable-unsafe-swiftshader".into());
  }

  // 3. Headless flags (Playwright adds these when headless=true).
  // Playwright passes bare `--headless` too — Chrome maps to
  // `--headless=old` on full chrome. The 2x perf gap on Regular
  // Chrome lives elsewhere, not in this flag (verified via
  // playwright-core/lib/server/chromium/chromium.js:288).
  //
  // Filtered by `ignoreDefaultArgs` like every other section: Playwright
  // builds the headless switches inside `defaultArgs()`, so
  // `ignoreDefaultArgs: true` drops them there too. Gating only sections
  // 1/2/4 left the caller with a browser that was still forced headless
  // after asking for no defaults at all.
  if headless && !drop_all {
    for f in ["--headless", "--hide-scrollbars", "--mute-audio"] {
      if !is_dropped(f) {
        flags.push(f.into());
      }
    }
    // No colour scheme is pinned here: a page is emulated light unless
    // asked otherwise, and `colorScheme: null` hands it back to whatever
    // the browser does on this host, as it does in Playwright.
    if !is_dropped("--blink-settings") {
      flags.push(
        "--blink-settings=primaryHoverType=2,availableHoverTypes=2,primaryPointerType=4,availablePointerTypes=4".into(),
      );
    }
  }

  // 4. Sandbox control (Playwright disables by default unless chromiumSandbox=true)
  if !sandbox && !drop_all && !is_dropped("--no-sandbox") {
    flags.push("--no-sandbox".into());
  }

  // 5. User-provided args. Never filtered: `ignoreDefaultArgs` names
  // DEFAULTS to drop, so an explicitly-passed arg still wins.
  for arg in extra_args {
    flags.push(arg.clone());
  }

  merge_feature_switches(&mut flags);
  flags
}

/// Chromium reads only the last `--enable-features` and the last
/// `--disable-features`, so a caller's own switch would silently drop the
/// defaults (Playwright appends them the same way and loses them too). Each
/// kind becomes one switch where it first appeared, and a feature named
/// again later moves to that later list, so the caller wins feature by
/// feature over the defaults before it.
///
/// `WebMCP` also brings what an automation client needs from it on builds
/// that gate those separately: `WebMCPTesting` for `getTools()` and
/// `DevToolsWebMCPSupport` for the `DevTools` domain. Chrome 153 enables both
/// with `WebMCP` already; a caller who disables one keeps it disabled.
pub(crate) fn merge_feature_switches(flags: &mut Vec<String>) {
  const ENABLE: &str = "--enable-features=";
  const DISABLE: &str = "--disable-features=";
  // A feature may carry a field trial or parameters: `Name<Trial:key/value`.
  fn name(feature: &str) -> &str {
    let feature = feature.trim_start_matches('*');
    feature.split(['<', ':']).next().unwrap_or(feature)
  }
  let mut lists: [Vec<String>; 2] = [Vec::new(), Vec::new()];
  let mut slots: [Option<usize>; 2] = [None, None];
  let mut kept = Vec::with_capacity(flags.len());
  for flag in flags.drain(..) {
    let (kind, value) = if let Some(value) = flag.strip_prefix(ENABLE) {
      (0, value)
    } else if let Some(value) = flag.strip_prefix(DISABLE) {
      (1, value)
    } else {
      kept.push(flag);
      continue;
    };
    slots[kind].get_or_insert(kept.len());
    for feature in value.split(',').filter(|feature| !feature.is_empty()) {
      for list in &mut lists {
        list.retain(|named| name(named) != name(feature));
      }
      lists[kind].push(feature.to_owned());
    }
  }
  let [enabled, disabled] = &mut lists;
  if enabled.iter().any(|feature| name(feature) == "WebMCP") {
    for companion in ["WebMCPTesting", "DevToolsWebMCPSupport"] {
      if !enabled
        .iter()
        .chain(disabled.iter())
        .any(|feature| name(feature) == companion)
      {
        enabled.push(companion.to_owned());
      }
    }
  }
  let mut switches = Vec::new();
  for (slot, switch, list) in [(slots[0], ENABLE, enabled), (slots[1], DISABLE, disabled)] {
    if let Some(slot) = slot
      && !list.is_empty()
    {
      switches.push((slot, format!("{switch}{}", list.join(","))));
    }
  }
  // Later slots first, so inserting one does not shift the other.
  switches.sort_by_key(|(slot, _)| std::cmp::Reverse(*slot));
  for (slot, switch) in switches {
    kept.insert(slot, switch);
  }
  *flags = kept;
}

/// Chrome switches matching Playwright's `chromiumSwitches()` exactly.
/// See: playwright/packages/playwright-core/src/server/chromium/chromiumSwitches.ts
pub(crate) const CHROMIUM_SWITCHES: &[&str] = &[
  "--disable-field-trial-config",
  "--disable-background-networking",
  "--disable-background-timer-throttling",
  "--disable-backgrounding-occluded-windows",
  "--disable-back-forward-cache",
  "--disable-breakpad",
  "--disable-client-side-phishing-detection",
  "--disable-component-extensions-with-background-pages",
  "--disable-component-update",
  "--no-default-browser-check",
  "--disable-default-apps",
  "--disable-dev-shm-usage",
  "--disable-edgeupdater",
  "--disable-extensions",
  "--disable-features=AvoidUnnecessaryBeforeUnloadCheckSync,BlockInsecurePrivateNetworkRequests,BoundaryEventDispatchTracksNodeRemoval,DestroyProfileOnBrowserClose,DialMediaRouteProvider,GlobalMediaControls,HttpsUpgrades,LensOverlay,MediaRouter,PaintHolding,PrivateNetworkAccessSendPreflights,ThirdPartyStoragePartitioning,Translate,AutoDeElevate,RenderDocument,OptimizationHints,msForceBrowserSignIn,msEdgeUpdateLaunchServicesPreferredVersion",
  "--enable-features=CDPScreenshotNewSurface",
  "--allow-pre-commit-input",
  "--disable-hang-monitor",
  "--disable-ipc-flooding-protection",
  "--disable-popup-blocking",
  "--disable-prompt-on-repost",
  "--disable-renderer-backgrounding",
  "--force-color-profile=srgb",
  "--metrics-recording-only",
  "--no-first-run",
  "--password-store=basic",
  "--use-mock-keychain",
  "--no-service-autorun",
  "--export-tagged-pdf",
  "--disable-search-engine-choice-screen",
  "--unsafely-disable-devtools-self-xss-warnings",
  "--edge-skip-compat-layer-relaunch",
  "--enable-automation",
  "--disable-infobars",
  "--disable-sync",
];

/// Resolve the Chrome binary to use, respecting env vars and headless mode.
///
/// Follows Playwright's resolution strategy:
/// - `executablePath` (handled by caller before this function) always wins.
/// - Explicit env vars (`CHROMIUM_HEADLESS_SHELL_PATH`, `CHROMIUM_PATH`) override auto-detection.
/// - When headless and no explicit path is set, prefer Chrome Headless Shell (lighter, faster).
/// - Fall back to full Chrome/Chromium otherwise.
///
/// Precedence (headless=true):
/// 1. `CHROMIUM_HEADLESS_SHELL_PATH` env var
/// 2. `CHROMIUM_PATH` env var (explicit user override always wins over auto-detection)
/// 3. ferridriver's own cache: its headless shell, then its Chromium
/// 4. Playwright's headless shell
/// 5. Auto-detect regular Chrome (`detect_chromium()`)
///
/// A build in ferridriver's cache outranks any build in Playwright's, so a
/// Playwright install elsewhere on the machine never changes the browser.
///
/// Precedence (headless=false):
/// 1. `CHROMIUM_PATH` env var
/// 2. Auto-detect regular Chrome (`detect_chromium()`)
#[must_use]
pub fn resolve_chromium(headless: bool) -> String {
  if headless {
    // Explicit headless shell path
    if let Ok(p) = std::env::var("CHROMIUM_HEADLESS_SHELL_PATH")
      && std::path::Path::new(&p).exists()
    {
      return p;
    }

    // Explicit chrome path -- user chose a specific binary, respect it
    if let Ok(p) = std::env::var("CHROMIUM_PATH")
      && std::path::Path::new(&p).exists()
    {
      return p;
    }

    let installer = crate::install::BrowserInstaller::new();
    if let Some(p) = installer
      .find_installed_headless_shell()
      .or_else(|| installer.find_installed_chromium())
      .or_else(|| playwright_build(crate::install::CachedBrowser::ChromiumHeadlessShell))
    {
      return p;
    }
  }

  // Headed mode, or no headless shell found: use regular Chrome
  detect_chromium()
}

/// Auto-detect Chrome Headless Shell binary on the system.
///
/// ferridriver's own cache first, then Playwright's. Does NOT check env vars
/// (that's `resolve_chromium()`'s job).
#[must_use]
pub fn detect_chromium_headless_shell() -> Option<String> {
  crate::install::BrowserInstaller::new()
    .find_installed_headless_shell()
    .or_else(|| playwright_build(crate::install::CachedBrowser::ChromiumHeadlessShell))
}

pub(super) fn playwright_build(browser: crate::install::CachedBrowser) -> Option<String> {
  crate::install::find_playwright_browser(browser).map(|path| path.to_string_lossy().into_owned())
}

/// Detect Chrome/Chromium binary on the system.
///
/// Automation-capable builds win over the platform's installed browser, and
/// not merely because they are better tested: a Chrome enrolled in cloud
/// management can carry a `RemoteDebuggingAllowed=false` policy, under which
/// it starts normally, refuses to open the debugging port, and never writes
/// `DevToolsActivePort` — indistinguishable from a hang. Chrome for Testing
/// and Chromium builds are never enrolled. Both launchers here always pass
/// their own `--user-data-dir`, so preferring one costs no profile.
#[must_use]
pub fn detect_chromium() -> String {
  if let Ok(p) = std::env::var("CHROMIUM_PATH")
    && std::path::Path::new(&p).exists()
  {
    return p;
  }

  // ferridriver's own `install chromium` download.
  if let Some(p) = crate::install::BrowserInstaller::new().find_installed_chromium() {
    return p;
  }

  if let Some(p) = playwright_build(crate::install::CachedBrowser::Chromium) {
    return p;
  }

  // Chrome for Testing installed as a normal app, and plain Chromium — both
  // unenrolled, so they outrank the managed bundles further down.
  #[cfg(target_os = "macos")]
  {
    let unmanaged = [
      "Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
      "Chromium.app/Contents/MacOS/Chromium",
    ];
    for bundle in &unmanaged {
      let sys = std::path::PathBuf::from("/Applications").join(bundle);
      if sys.exists() {
        return sys.to_string_lossy().to_string();
      }
      if let Ok(home) = std::env::var("HOME") {
        let user = std::path::PathBuf::from(&home).join("Applications").join(bundle);
        if user.exists() {
          return user.to_string_lossy().to_string();
        }
      }
    }
  }

  if let Ok(path_var) = std::env::var("PATH") {
    let names = [
      "chromium-browser",
      "chromium",
      "google-chrome-stable",
      "google-chrome",
      "microsoft-edge",
      "chrome",
    ];
    for name in &names {
      for dir in path_var.split(':') {
        let candidate = std::path::PathBuf::from(dir).join(name);
        if candidate.exists() {
          return candidate.to_string_lossy().to_string();
        }
      }
    }
  }

  #[cfg(target_os = "macos")]
  {
    // Chromium and Chrome for Testing were already tried above; what is left
    // is the platform's own browser, which may be enrolled.
    let bundles = [
      "Google Chrome.app/Contents/MacOS/Google Chrome",
      "Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
      "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    ];
    for bundle in &bundles {
      let sys = std::path::PathBuf::from("/Applications").join(bundle);
      if sys.exists() {
        return sys.to_string_lossy().to_string();
      }
      if let Ok(home) = std::env::var("HOME") {
        let user = std::path::PathBuf::from(&home).join("Applications").join(bundle);
        if user.exists() {
          return user.to_string_lossy().to_string();
        }
      }
    }
  }

  #[cfg(target_os = "linux")]
  {
    let paths = [
      "/usr/bin/chromium-browser",
      "/usr/bin/chromium",
      "/snap/bin/chromium",
      "/usr/bin/google-chrome-stable",
      "/usr/bin/google-chrome",
      "/usr/bin/microsoft-edge",
    ];
    for path in &paths {
      if std::path::Path::new(path).exists() {
        return path.to_string();
      }
    }
  }

  "chromium".to_string()
}

/// The Ladybird browser executable: `LADYBIRD_PATH`, then ferridriver's
/// own install, then the system's.
///
/// # Errors
///
/// Returns a backend error naming the install command when no Ladybird
/// is found.
pub fn detect_ladybird() -> Result<String> {
  if let Ok(path) = std::env::var("LADYBIRD_PATH")
    && std::path::Path::new(&path).exists()
  {
    return Ok(path);
  }
  if let Some(path) = crate::install::BrowserInstaller::new().find_installed_ladybird() {
    return Ok(path);
  }
  let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
  let mut candidates: Vec<std::path::PathBuf> = Vec::new();
  if cfg!(target_os = "macos") {
    candidates.push("/Applications/Ladybird.app/Contents/MacOS/Ladybird".into());
    if let Some(home) = &home {
      candidates.push(home.join("Applications/Ladybird.app/Contents/MacOS/Ladybird"));
    }
  } else if cfg!(target_os = "linux") {
    candidates.push("/opt/ladybird/bin/Ladybird".into());
  }
  candidates
    .into_iter()
    .find(|path| path.is_file())
    .map(|path| path.to_string_lossy().into_owned())
    .ok_or_else(|| FerriError::backend("Ladybird not found. Run `ferridriver install ladybird` or set LADYBIRD_PATH"))
}

/// The Lightpanda executable: `LIGHTPANDA_PATH`, then ferridriver's own
/// install, then one on `PATH` (Homebrew and the AUR install it there).
///
/// # Errors
///
/// Returns a backend error naming the install command when no Lightpanda
/// is found.
pub fn detect_lightpanda() -> Result<String> {
  if let Ok(path) = std::env::var("LIGHTPANDA_PATH")
    && std::path::Path::new(&path).exists()
  {
    return Ok(path);
  }
  if let Some(path) = crate::install::BrowserInstaller::new().find_installed_lightpanda() {
    return Ok(path);
  }
  std::env::var_os("PATH")
    .into_iter()
    .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
    .map(|dir| dir.join("lightpanda"))
    .find(|path| path.is_file())
    .map(|path| path.to_string_lossy().into_owned())
    .ok_or_else(|| {
      FerriError::backend("Lightpanda not found. Run `ferridriver install lightpanda` or set LIGHTPANDA_PATH")
    })
}

/// Detect Firefox binary on the system.
///
/// Search order (matches Chrome detection pattern):
/// 1. `FIREFOX_PATH` environment variable
/// 2. ferridriver's own browser cache (installed via `install_firefox()`)
/// 3. Playwright's browser cache
/// 4. System-installed Firefox (platform-specific paths)
/// 5. `which firefox` fallback
///
/// # Errors
///
/// Returns an error if no Firefox binary can be found.
pub fn detect_firefox() -> Result<String> {
  // 1. Env var (highest priority)
  if let Ok(p) = std::env::var("FIREFOX_PATH")
    && std::path::Path::new(&p).exists()
  {
    return Ok(p);
  }

  // 2. ferridriver's own cache
  if let Some(p) = crate::install::BrowserInstaller::new().find_installed_firefox() {
    return Ok(p);
  }

  // 3. Playwright's Firefox cache
  if let Some(p) = playwright_build(crate::install::CachedBrowser::Firefox) {
    return Ok(p);
  }

  // 4. System-installed Firefox
  #[cfg(target_os = "macos")]
  {
    let paths = [
      "/Applications/Firefox.app/Contents/MacOS/firefox",
      "/Applications/Firefox Nightly.app/Contents/MacOS/firefox",
      "/Applications/Firefox Developer Edition.app/Contents/MacOS/firefox",
    ];
    for path in &paths {
      if std::path::Path::new(path).exists() {
        return Ok(path.to_string());
      }
    }
  }

  #[cfg(target_os = "linux")]
  {
    // Skip snap-wrapped Firefox builds: Ubuntu 24.04+ ships a snap as
    // /usr/bin/firefox (and the explicit /snap/bin/firefox). Snap's
    // confinement blocks the WebDriver BiDi remote-debugging port and the
    // shim never prints the WebSocket URL on stderr, so detection hangs
    // until the 15s discovery timeout. Treat snap wrappers as
    // "not installed" so callers fall back to ferridriver's own download.
    let paths = [
      "/usr/bin/firefox",
      "/usr/bin/firefox-esr",
      "/snap/bin/firefox",
      "/usr/lib/firefox/firefox",
      "/usr/lib64/firefox/firefox",
    ];
    for path in &paths {
      let p = std::path::Path::new(path);
      if !p.exists() {
        continue;
      }
      let resolved = std::fs::canonicalize(p).map_or_else(|_| path.to_string(), |c| c.to_string_lossy().to_string());
      if resolved.contains("/snap/") {
        continue;
      }
      return Ok(path.to_string());
    }
  }

  #[cfg(target_os = "windows")]
  {
    let paths = [
      r"C:\Program Files\Mozilla Firefox\firefox.exe",
      r"C:\Program Files (x86)\Mozilla Firefox\firefox.exe",
    ];
    for path in &paths {
      if std::path::Path::new(path).exists() {
        return Ok(path.to_string());
      }
    }
  }

  // 5. which/where fallback
  let cmd = if cfg!(windows) { "where" } else { "which" };
  if let Ok(output) = std::process::Command::new(cmd).arg("firefox").output()
    && output.status.success()
  {
    let p = String::from_utf8_lossy(&output.stdout)
      .lines()
      .next()
      .unwrap_or("")
      .trim()
      .to_string();
    if !p.is_empty() && std::path::Path::new(&p).exists() {
      return Ok(p);
    }
  }

  Err(FerriError::backend(
    "Firefox not found. Install with `ferridriver install firefox` or set FIREFOX_PATH.",
  ))
}
