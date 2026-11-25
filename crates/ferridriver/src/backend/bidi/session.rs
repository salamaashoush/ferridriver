//! `BiDi` session management: WebSocket connection, session creation, browser launch.
//!
//! Native `BiDi` creates a session directly via WebSocket. HTTP-created sessions
//! attach to their negotiated socket without issuing another `session.new`.
//! Firefox exposes native `BiDi` at `ws://host:port/session`.
//! Chrome can also be used via chromedriver's `BiDi` endpoint.

use std::sync::Arc;

use serde_json::json;
use tracing::{debug, info};

use super::transport::BidiTransport;
use crate::error::{FerriError, Result};

/// The profile directory a launched Firefox is using.
///
/// A caller-supplied `userDataDir` is a PERSISTENT profile: ferridriver
/// must not remove it on teardown, or the logins and cookies the caller
/// asked to keep vanish with the browser. Only the throwaway profile
/// ferridriver creates for itself is owned.
pub(crate) struct LaunchedProfile {
  pub path: std::path::PathBuf,
  /// `Some` only for the throwaway profile, whose `Drop` removes it.
  pub owned: Option<tempfile::TempDir>,
  /// Firefox's per-user data root for this launch (`MOZ_APP_DATA`),
  /// unless the caller chose one.
  pub app_data: Option<tempfile::TempDir>,
}

/// A `BiDi` session -- holds the transport and session metadata.
#[derive(Clone)]
pub(crate) struct BidiSession {
  pub session_id: String,
  pub transport: Arc<BidiTransport>,
  pub browser_name: String,
  pub browser_version: String,
  pub classic: Option<Arc<crate::backend::webdriver::session::WebDriverSession>>,
  /// What `session.new` answered, which a served browser's clients are
  /// answered with in turn.
  pub capabilities: serde_json::Value,
  /// Whether the browser implements the `browser` module's user
  /// contexts. Without them every page shares the default context.
  pub user_contexts: bool,
  /// Commands the browser answered `unknown command`.
  unknown_commands: Arc<std::sync::Mutex<rustc_hash::FxHashSet<String>>>,
  /// Whether scripts can run in a sandbox realm; cleared once the browser
  /// shows it has none.
  sandboxes: Arc<std::sync::atomic::AtomicBool>,
  /// What each user context hands the pages the browser opens in it.
  pub(super) scopes: Arc<super::scope::ContextScopes>,
}

// WebKit accepts module subscriptions without expanding them (WebKit bug
// 291371), so lifecycle subscriptions must name their concrete events.

impl BidiSession {
  /// Connect and open a session whose default context uses `proxy`.
  ///
  /// Firefox has no proxy command-line switch, so `launch({ proxy })` lands
  /// here: the `WebDriver` `proxy` capability is the browser-wide equivalent, and
  /// a user context created later with its own proxy still overrides it.
  pub(crate) async fn connect_with_proxy(ws_url: &str, proxy: Option<&crate::options::ProxyConfig>) -> Result<Self> {
    Self::connect_with_options(ws_url, proxy, None).await
  }

  pub(crate) async fn connect_with_options(
    ws_url: &str,
    proxy: Option<&crate::options::ProxyConfig>,
    headers: Option<&rustc_hash::FxHashMap<String, String>>,
  ) -> Result<Self> {
    info!("Connecting BiDi session");
    let transport = Arc::new(match headers {
      Some(headers) => BidiTransport::connect_with_headers(ws_url, Some(headers)).await?,
      None => BidiTransport::connect(ws_url).await?,
    });

    // Create a new session with proper capabilities.
    // webSocketUrl: true tells Firefox to maintain the BiDi WebSocket across navigations.
    // unhandledPromptBehavior: ignore prevents dialogs from blocking automation.
    let mut always_match = json!({
      "acceptInsecureCerts": true,
      "webSocketUrl": true,
      "unhandledPromptBehavior": {
        "default": "ignore"
      }
    });
    if let Some(proxy) = proxy {
      always_match["proxy"] = super::browser::bidi_proxy_capability(proxy);
    }

    let result = transport
      .send_command(
        "session.new",
        json!({ "capabilities": { "alwaysMatch": always_match } }),
      )
      .await?;

    let session_id = result
      .get("sessionId")
      .and_then(|v| v.as_str())
      .unwrap_or("unknown")
      .to_string();
    let capabilities = result.get("capabilities").cloned().unwrap_or_else(|| json!({}));
    Self::finish(transport, session_id, capabilities, None).await
  }

  /// Attach to a `BiDi` WebSocket returned by a pre-existing `WebDriver` Classic
  /// session. The HTTP session already ran `session.new`, so sending a second
  /// `session.new` would create a different session or fail on Appium/Safari.
  pub(crate) async fn connect_existing(
    ws_url: &str,
    session_id: String,
    capabilities: serde_json::Value,
    headers: Option<&rustc_hash::FxHashMap<String, String>>,
    classic: Arc<crate::backend::webdriver::session::WebDriverSession>,
  ) -> Result<Self> {
    info!("Connecting to existing BiDi session");
    let transport = Arc::new(BidiTransport::connect_with_headers(ws_url, headers).await?);
    Self::finish(transport, session_id, capabilities, Some(classic)).await
  }

  async fn finish(
    transport: Arc<BidiTransport>,
    session_id: String,
    capabilities: serde_json::Value,
    classic: Option<Arc<crate::backend::webdriver::session::WebDriverSession>>,
  ) -> Result<Self> {
    let browser_name = capabilities
      .get("browserName")
      .and_then(|v| v.as_str())
      .unwrap_or("unknown")
      .to_string();
    let browser_version = capabilities
      .get("browserVersion")
      .and_then(|v| v.as_str())
      .unwrap_or("unknown")
      .to_string();
    let kept_capabilities = capabilities.clone();

    debug!("BiDi session created: id={session_id}, browser={browser_name} {browser_version}");

    let events = if browser_name.eq_ignore_ascii_case("safari") {
      json!([
        "browsingContext.contextCreated",
        "browsingContext.contextDestroyed",
        "browsingContext.domContentLoaded",
        "browsingContext.fragmentNavigated",
        "browsingContext.load",
        "browsingContext.navigationAborted",
        "browsingContext.navigationCommitted",
        "browsingContext.navigationFailed",
        "browsingContext.navigationStarted",
        "browsingContext.userPromptClosed",
        "browsingContext.userPromptOpened",
        "browsingContext.downloadWillBegin",
        "browsingContext.downloadEnd",
        "log.entryAdded",
        "script.realmCreated",
        "script.realmDestroyed",
        "script.message",
        "network",
        "input"
      ])
    } else {
      json!(["browsingContext", "network", "log", "script", "input"])
    };
    transport
      .send_command("session.subscribe", json!({"events": events}))
      .await?;

    for (method, params) in [
      (
        "network.addDataCollector",
        json!({"dataTypes": ["response"], "maxEncodedDataSize":20_000_000}),
      ),
      ("network.addIntercept", json!({"phases":["authRequired"]})),
    ] {
      match transport.send_command(method, params).await {
        Ok(_) => {},
        Err(FerriError::Unsupported(reason)) => tracing::debug!(%reason, "optional BiDi capability unavailable"),
        Err(error) => return Err(error),
      }
    }

    let user_contexts = match transport.send_command("browser.getUserContexts", json!({})).await {
      Ok(_) => true,
      Err(FerriError::Unsupported(reason)) => {
        tracing::debug!(%reason, "BiDi user contexts unavailable; pages share the default context");
        false
      },
      Err(error) => return Err(error),
    };

    info!("BiDi session ready: {browser_name} {browser_version}");
    Ok(Self {
      session_id,
      transport,
      browser_name,
      browser_version,
      classic,
      capabilities: kept_capabilities,
      user_contexts,
      unknown_commands: Arc::default(),
      sandboxes: Arc::new(std::sync::atomic::AtomicBool::new(true)),
      scopes: Arc::default(),
    })
  }

  pub(crate) async fn send_command(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    if method == "input.performActions" && self.classic.is_some() {
      return Box::pin(self.send_input(params)).await;
    }
    if matches!(method, "script.evaluate" | "script.callFunction") && params.pointer("/target/sandbox").is_some() {
      return Box::pin(self.send_sandboxed(method, params)).await;
    }
    self.transport.send_command(method, params).await
  }

  /// Input over `BiDi`, or over Classic on a browser without
  /// `input.performActions`.
  async fn send_input(&self, params: serde_json::Value) -> Result<serde_json::Value> {
    if let Some(result) = self.try_command("input.performActions", params.clone()).await? {
      return Ok(result);
    }
    let context = params
      .get("context")
      .and_then(serde_json::Value::as_str)
      .ok_or_else(|| FerriError::invalid_argument("context", "input requires a browsing context"))?;
    Box::pin(self.classic_page(context)?.perform_actions(params)).await?;
    Ok(json!({}))
  }

  /// Run a script in the sandbox its target names, or in the page's own
  /// realm on a browser that has no sandboxes. A browser that refuses the
  /// sandbox but runs the same script without it has none; that is
  /// remembered, so later scripts go straight to the page realm.
  async fn send_sandboxed(&self, method: &str, mut params: serde_json::Value) -> Result<serde_json::Value> {
    let without_sandbox = |params: &mut serde_json::Value| {
      if let Some(target) = params.get_mut("target").and_then(serde_json::Value::as_object_mut) {
        target.remove("sandbox");
      }
    };
    if !self.sandboxes.load(std::sync::atomic::Ordering::Acquire) {
      without_sandbox(&mut params);
      return self.transport.send_command(method, params).await;
    }
    match self.transport.send_raw(method, params.clone()).await? {
      Ok(result) => Ok(result),
      Err(error) if error.error == "unsupported operation" => {
        without_sandbox(&mut params);
        let result = self.transport.send_command(method, params).await?;
        tracing::debug!(%method, %error, "BiDi sandboxes unavailable; scripts run in the page realm");
        self.sandboxes.store(false, std::sync::atomic::Ordering::Release);
        Ok(result)
      },
      Err(error) => Err(error.into_error(method)),
    }
  }

  /// Send `method`, answering `None` when the browser does not implement
  /// it. The answer is remembered, so a caller with another way to do the
  /// same thing pays the round trip once.
  ///
  /// # Errors
  /// Returns every other error `method` fails with.
  pub(crate) async fn try_command(&self, method: &str, params: serde_json::Value) -> Result<Option<serde_json::Value>> {
    if self.lacks(method) {
      return Ok(None);
    }
    match self.transport.send_raw(method, params).await? {
      Ok(result) => Ok(Some(result)),
      Err(error) if error.error == "unknown command" => {
        tracing::debug!(%method, %error, "BiDi command unavailable; using the fallback");
        self
          .unknown_commands
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .insert(method.to_owned());
        Ok(None)
      },
      Err(error) => Err(error.into_error(method)),
    }
  }

  /// Whether the browser has answered `method` as a command it does not know.
  pub(crate) fn lacks(&self, method: &str) -> bool {
    self
      .unknown_commands
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .contains(method)
  }

  /// The Classic `WebDriver` view of top-level `context`, for what the
  /// browser serves there and not over `BiDi`. A top-level browsing
  /// context's id is its window handle.
  ///
  /// # Errors
  /// Returns `FerriError::Unsupported` for a session not created over HTTP.
  pub(crate) fn classic_page(&self, context: &str) -> Result<crate::backend::webdriver::page::WebDriverPage> {
    let classic = self
      .classic
      .as_ref()
      .ok_or_else(|| FerriError::unsupported("the fallback needs a session created through WebDriver Classic"))?;
    Ok(crate::backend::webdriver::page::WebDriverPage::new(
      Arc::clone(classic),
      crate::backend::webdriver::session::Target {
        window: Some(context.to_owned()),
        ..Default::default()
      },
      30_000,
    ))
  }

  /// Launch Firefox and create a `BiDi` session.
  ///
  /// Firefox natively supports `BiDi`: launch with `--remote-debugging-port`,
  /// read the `BiDi` WebSocket URL from stderr, connect directly.
  ///
  /// `user_data_dir` selects a PERSISTENT profile: the directory is
  /// created if missing and never removed, so cookies and logins survive
  /// a restart. Without it a throwaway profile is created and owned.
  ///
  /// Returns `(session, group, profile)`. The caller must keep
  /// `profile` alive for the lifetime of the browser — the owned variant's
  /// `Drop` removes the directory from disk. Firefox is owned by its
  /// process group from the moment it is spawned, so its whole tree dies
  /// before the dir vanishes, a failed startup included.
  pub(crate) async fn launch_firefox(
    firefox_path: &str,
    flags: &[String],
    headless: bool,
    env: &rustc_hash::FxHashMap<String, String>,
    user_data_dir: Option<&std::path::Path>,
    proxy: Option<&crate::options::ProxyConfig>,
    firefox_user_prefs: Option<&rustc_hash::FxHashMap<String, serde_json::Value>>,
  ) -> Result<(Self, crate::backend::process::ChildGroup, LaunchedProfile)> {
    super::profile::validate_preferences(firefox_user_prefs)?;
    // Prefix the throwaway profile dir so test-harness cleanup can
    // `pkill -f` any leaked Firefox processes by their `--profile` arg —
    // mirrors the `ferridriver-pipe-` / `ferridriver-raw-` prefixes
    // used by the CDP launches in `backend::cdp::mod`.
    let profile_dir = if let Some(dir) = user_data_dir {
      std::fs::create_dir_all(dir).map_err(|e| format!("user data dir {}: {e}", dir.display()))?;
      LaunchedProfile {
        path: dir.to_path_buf(),
        owned: None,
        app_data: None,
      }
    } else {
      let owned = tempfile::Builder::new()
        .prefix("ferridriver-firefox-")
        .tempdir()
        .map_err(|e| format!("tempdir: {e}"))?;
      LaunchedProfile {
        path: owned.path().to_path_buf(),
        owned: Some(owned),
        app_data: None,
      }
    };
    let mut profile_dir = profile_dir;
    // Before it reads `--profile`, Firefox creates its per-user data root
    // and writes `profiles.ini`, crash reports and pending pings there. A
    // launch keeps all of that out of the user's own Firefox data, and
    // starts where that directory is protected (it fails with "Could not
    // find profile folder." otherwise).
    if !env.contains_key("MOZ_APP_DATA") && std::env::var_os("MOZ_APP_DATA").is_none() {
      profile_dir.app_data = Some(
        tempfile::Builder::new()
          .prefix("ferridriver-firefox-data-")
          .tempdir()
          .map_err(|e| format!("tempdir: {e}"))?,
      );
    }

    let profile_guard = super::profile::ProfileGuard::acquire(&profile_dir.path)?;

    // Pre-create the per-profile downloads dir and pin Firefox to it
    // via `browser.download.dir` + `folderList=2`. Without these prefs
    // Firefox falls back to the user's `~/Downloads` for the
    // `suggestedFilename` calculation it ships in
    // `browsingContext.downloadWillBegin`, so a `Content-Disposition:
    // filename="x.txt"` payload comes back as `x(NN).txt` whenever
    // `~/Downloads/x.txt` already exists on the host (it picks the
    // first free suffix). `browser.setDownloadBehavior`'s
    // `destinationFolder` only redirects the on-disk write, not the
    // suggested-filename deduplication scan.
    let downloads_dir = profile_dir.path.join("downloads");
    std::fs::create_dir_all(&downloads_dir).map_err(|e| format!("downloads dir: {e}"))?;

    // Write automation preferences to user.js in the profile directory.
    // Matches Playwright's firefoxPreferences + Puppeteer's essentials.
    super::profile::write_preferences(
      &profile_dir.path,
      &firefox_default_prefs(&downloads_dir)?,
      firefox_user_prefs,
    )?;
    #[cfg(unix)]
    profile_guard.allow_firefox_start()?;
    #[cfg(windows)]
    let _profile_guard = profile_guard.allow_firefox_start();

    let mut command = tokio::process::Command::new(firefox_path);
    command.envs(env);
    command.arg("--remote-debugging-port").arg("0");
    command.arg("--profile").arg(&profile_dir.path);
    command.arg("--no-remote");
    if headless {
      command.arg("--headless");
      // The flag alone still lets macOS realise native widgets: a right
      // click with no `preventDefault` painted a real Firefox context
      // menu over the user's desktop, which then outlived the browser
      // when the run was killed with the menu open. `MOZ_HEADLESS` is
      // the switch Firefox itself checks for full headless widgetry.
      command.env("MOZ_HEADLESS", "1");
    }

    // Translate Chrome-style --window-size=W,H to Firefox's --width/--height flags,
    // and forward any other extra flags.
    for flag in flags {
      if let Some(dims) = flag.strip_prefix("--window-size=") {
        if let Some((w, h)) = dims.split_once(',') {
          command.arg("--width").arg(w);
          command.arg("--height").arg(h);
        }
      } else if flag != "--headless" {
        command.arg(flag);
      }
    }

    command.env("MOZ_CRASHREPORTER_DISABLE", "1");
    // Release Firefox honours the remote-settings server pref below only
    // with this set. Without it Firefox fetches Mozilla's at startup,
    // through a launch proxy too, before the BiDi session exists to answer
    // the proxy's challenge, and `session.new` can wait a minute on it.
    command.env("MOZ_REMOTE_SETTINGS_DEVTOOLS", "1");
    if let Some(app_data) = &profile_dir.app_data {
      command.env("MOZ_APP_DATA", app_data.path());
      // On Linux Firefox's startup cache and fontconfig's font cache go
      // under XDG_CACHE_HOME (`~/.cache` by default), which MOZ_APP_DATA
      // does not move.
      if cfg!(target_os = "linux") && !env.contains_key("XDG_CACHE_HOME") {
        command.env("XDG_CACHE_HOME", app_data.path().join("cache"));
      }
    }
    command
      .stdin(std::process::Stdio::null())
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::piped())
      .kill_on_drop(true);

    // Put Firefox into its own session+process group so content/plugin
    // subprocesses die together with the parent on teardown.
    #[cfg(unix)]
    crate::backend::process::spawn_in_own_session(command.as_std_mut());

    debug!("Launching Firefox for BiDi: {firefox_path}");
    let mut child =
      crate::backend::process::spawn_exclusive(|| command.spawn()).map_err(|e| format!("Firefox launch: {e}"))?;
    let stderr = child.stderr.take();
    // Owned before the BiDi handshake below. Firefox does not exit when
    // its parent dies, and the handshake takes long enough that a kill
    // landing inside it used to strand a whole browser.
    let mut group =
      crate::backend::process::ChildGroup::recorded(child, Some(&profile_dir.path), profile_dir.owned.is_some());

    // Firefox prints "WebDriver BiDi listening on ws://127.0.0.1:PORT" to stderr
    let ws_url = discover_bidi_ws_url(stderr, &mut group).await?;
    debug!("Firefox BiDi endpoint: {ws_url}");

    let session = Box::pin(Self::connect_with_proxy(&ws_url, proxy)).await?;

    Ok((session, group, profile_dir))
  }

  /// Launch a browser and create a `BiDi` session.
  /// Currently supports Firefox (native `BiDi`). Chrome does not have built-in
  /// `BiDi` support -- use the CDP backend for Chrome instead.
  pub(crate) async fn launch(
    browser_path: &str,
    flags: &[String],
    headless: bool,
    env: &rustc_hash::FxHashMap<String, String>,
    user_data_dir: Option<&std::path::Path>,
    proxy: Option<&crate::options::ProxyConfig>,
    firefox_user_prefs: Option<&rustc_hash::FxHashMap<String, serde_json::Value>>,
  ) -> Result<(Self, crate::backend::process::ChildGroup, LaunchedProfile)> {
    let path_lower = browser_path.to_lowercase();
    if path_lower.contains("firefox") {
      Box::pin(Self::launch_firefox(
        browser_path,
        flags,
        headless,
        env,
        user_data_dir,
        proxy,
        firefox_user_prefs,
      ))
      .await
    } else {
      Err(FerriError::unsupported(format!(
        "BiDi backend requires Firefox (found: {browser_path}). \
         Chrome does not have built-in BiDi support -- use the CDP backend for Chrome. \
         Set FIREFOX_PATH or install Firefox."
      )))
    }
  }
}

/// Read Firefox stderr to find the `BiDi` WebSocket URL.
/// Firefox prints: "`WebDriver` `BiDi` listening on ws://127.0.0.1:PORT"
async fn discover_bidi_ws_url(
  stderr: Option<tokio::process::ChildStderr>,
  group: &mut crate::backend::process::ChildGroup,
) -> Result<String> {
  use tokio::io::AsyncBufReadExt;

  let stderr = stderr.ok_or_else(|| FerriError::backend("Firefox: no stderr handle"))?;
  let mut reader = tokio::io::BufReader::new(stderr);
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);

  let mut line = String::new();
  // What Firefox printed before the URL, so a startup failure carries
  // Firefox's own reason ("Could not find profile folder.") instead of
  // only an exit status.
  let mut output: std::collections::VecDeque<String> = std::collections::VecDeque::new();
  let exited = |status: std::process::ExitStatus, output: &std::collections::VecDeque<String>| {
    let said: Vec<&str> = output.iter().map(String::as_str).collect();
    FerriError::Backend(if said.is_empty() {
      format!("Firefox exited during startup with status: {status}")
    } else {
      format!(
        "Firefox exited during startup with status: {status}\n{}",
        said.join("\n")
      )
    })
  };
  loop {
    if tokio::time::Instant::now() >= deadline {
      return Err(FerriError::timeout(
        "waiting for Firefox BiDi WebSocket URL in stderr",
        15_000,
      ));
    }

    line.clear();
    let read_result = tokio::time::timeout(std::time::Duration::from_secs(1), reader.read_line(&mut line)).await;

    match read_result {
      Ok(Ok(0)) => {
        // EOF
        if !group.is_running() {
          return Err(exited(group.wait().await?, &output));
        }
        // stderr closed but the process is still alive — read_line
        // returns 0 immediately from here on, so pace the exit checks
        // instead of hot-spinning until the deadline.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
      },
      Ok(Ok(_)) => {
        // Look for the BiDi WebSocket URL
        // Firefox prints: "WebDriver BiDi listening on ws://127.0.0.1:PORT"
        // The actual BiDi endpoint is at ws://host:port/session
        if let Some(pos) = line.find("ws://")
          && let Some(url) = line.get(pos..)
        {
          let mut ws_url = url.trim().to_string();
          // Ensure the URL ends with /session (Firefox's BiDi endpoint)
          if !ws_url.ends_with("/session") {
            if ws_url.ends_with('/') {
              ws_url.push_str("session");
            } else {
              ws_url.push_str("/session");
            }
          }
          return Ok(ws_url);
        }
        let said = line.trim();
        if !said.is_empty() {
          if output.len() == 20 {
            output.pop_front();
          }
          output.push_back(said.to_string());
        }
      },
      Ok(Err(e)) => return Err(FerriError::Backend(format!("Firefox stderr read error: {e}"))),
      Err(_) => {
        // Timeout on this read, check if process died
        if !group.is_running() {
          return Err(exited(group.wait().await?, &output));
        }
      },
    }
  }
}

/// Based on Playwright's `playwright.cfg` and Puppeteer's Firefox defaults.
/// The profile's `user.js`, one `user_pref("key", value);` per line,
/// organised by category as Playwright's profile is.
const FIREFOX_PREFS: &str = r#"// ── Process model ──────────────────────────────────────────────────────
// Force single content process for reliable mouse event dispatch (Playwright + Puppeteer).
user_pref("fission.webContentIsolationStrategy", 0);
user_pref("fission.bfcacheInParent", false);
user_pref("fission.autostart", false);
user_pref("dom.ipc.processCount", 1);
user_pref("dom.ipc.processPrelaunchEnabled", false);

// ── Input events ──────────────────────────────────────────────────────
// Remove minimum tick/time restrictions for synthetic input events.
user_pref("dom.input_events.security.minNumTicks", 0);
user_pref("dom.input_events.security.minTimeElapsedInMS", 0);

// ── Startup & UI ──────────────────────────────────────────────────────
user_pref("browser.startup.homepage", "about:blank");
user_pref("browser.startup.page", 0);
user_pref("browser.newtabpage.enabled", false);
user_pref("browser.shell.checkDefaultBrowser", false);
user_pref("browser.tabs.warnOnClose", false);
user_pref("browser.tabs.warnOnCloseOtherTabs", false);
user_pref("browser.tabs.warnOnOpen", false);
user_pref("browser.warnOnQuit", false);
user_pref("browser.sessionstore.resume_from_crash", false);
user_pref("browser.uitour.enabled", false);
user_pref("toolkit.cosmeticAnimations.enabled", false);
user_pref("browser.rights.3.shown", true);

// ── Media defaults ────────────────────────────────────────────────────
// No reduced motion unless a test asks, as Playwright emulates by default.
// BiDi cannot override it per page, so without this a host with animations
// turned off (Windows runners) leaks `prefers-reduced-motion: reduce`.
user_pref("ui.prefersReducedMotion", 0);

// ── Updates & telemetry (all disabled) ────────────────────────────────
user_pref("app.update.enabled", false);
user_pref("app.update.auto", false);
user_pref("app.update.mode", 0);
user_pref("app.update.service.enabled", false);
user_pref("app.update.checkInstallTime", false);
user_pref("app.update.disabledForTesting", true);
user_pref("app.normandy.enabled", false);
user_pref("app.normandy.api_url", "");
user_pref("datareporting.policy.dataSubmissionEnabled", false);
user_pref("datareporting.healthreport.service.enabled", false);
user_pref("datareporting.healthreport.uploadEnabled", false);
user_pref("toolkit.telemetry.enabled", false);
user_pref("toolkit.telemetry.server", "");
user_pref("telemetry.fog.test.localhost_port", -1);
user_pref("datareporting.usage.uploadEnabled", false);
user_pref("datareporting.healthreport.documentServerURI", "http://dummy.test/dummy/healthreport/");
user_pref("datareporting.healthreport.logging.consoleEnabled", false);
user_pref("datareporting.healthreport.service.firstRun", false);
user_pref("datareporting.policy.dataSubmissionPolicyBypassNotification", true);
user_pref("browser.translations.enable", false);

// ── Extensions ────────────────────────────────────────────────────────
user_pref("extensions.autoDisableScopes", 0);
user_pref("extensions.enabledScopes", 5);
user_pref("extensions.update.enabled", false);
user_pref("extensions.screenshots.disabled", true);
user_pref("extensions.blocklist.enabled", false);
user_pref("extensions.update.notifyUser", false);
user_pref("extensions.getAddons.cache.enabled", false);
user_pref("extensions.installDistroAddons", false);
user_pref("extensions.webservice.discoverURL", "http://dummy.test/dummy/discoveryURL");

// ── Network (isolate from external services) ──────────────────────────
user_pref("network.captive-portal-service.enabled", false);
user_pref("network.connectivity-service.enabled", false);
user_pref("network.dns.disablePrefetch", true);
user_pref("network.http.speculative-parallel-limit", 0);
user_pref("network.cookie.CHIPS.enabled", false);
user_pref("browser.pocket.enabled", false);
user_pref("network.sntp.pools", "dummy.test");
user_pref("browser.search.update", false);
user_pref("browser.urlbar.suggest.searches", false);
user_pref("browser.pagethumbnails.capturing_disabled", true);
user_pref("browser.newtabpage.activity-stream.feeds.system.topstories", false);
user_pref("browser.newtabpage.activity-stream.showSponsoredTopSites", false);
user_pref("browser.newtabpage.activity-stream.newtabWallpapers.enabled", false);
user_pref("browser.newtabpage.activity-stream.newtabWallpapers.v2.enabled", false);
user_pref("browser.newtabpage.activity-stream.discoverystream.region-weather-config", "");
user_pref("browser.translations.automaticallyPopup", false);
user_pref("browser.startup.homepage_override.mstone", "ignore");
user_pref("browser.usedOnWindows10.introURL", "");
// Loopback follows a configured proxy unless its bypass list names it, as on
// Chromium and WebKit. Firefox otherwise never proxies loopback. Hijacked
// localhost stays a secure context, as it is when reached directly.
user_pref("network.proxy.allow_hijacking_localhost", true);
user_pref("network.proxy.testing_localhost_is_secure_when_hijacked", true);

// ── Security (relaxed for testing) ────────────────────────────────────
user_pref("browser.safebrowsing.blockedURIs.enabled", false);
user_pref("browser.safebrowsing.downloads.enabled", false);
user_pref("browser.safebrowsing.passwords.enabled", false);
user_pref("browser.safebrowsing.malware.enabled", false);
user_pref("browser.safebrowsing.phishing.enabled", false);
user_pref("security.fileuri.strict_origin_policy", false);
user_pref("signon.autofillForms", false);
user_pref("signon.rememberSignons", false);
user_pref("privacy.trackingprotection.enabled", false);
user_pref("dom.security.https_first", false);

// ── Timeouts & hangs ──────────────────────────────────────────────────
user_pref("dom.max_script_run_time", 0);
user_pref("dom.max_chrome_script_run_time", 0);
user_pref("dom.ipc.reportProcessHangs", false);
user_pref("hangmonitor.timeout", 0);
user_pref("apz.content_response_timeout", 60000);
user_pref("toolkit.startup.max_resumed_crashes", -1);

// ── Background pages (a tab behind a popup must keep running) ─────────
// Without these a page that is not the front tab gets its timers clamped
// and its animation frames throttled to about one a second, and every
// action that waits on a frame (a click's stability and hit checks) takes
// that long.
user_pref("dom.min_background_timeout_value", 0);
user_pref("dom.min_background_timeout_value_without_budget_throttling", 0);
user_pref("dom.timeout.enable_budget_timer_throttling", false);
user_pref("layout.testing.top-level-always-active", true);
user_pref("browser.tabs.disableBackgroundZombification", false);

// ── Remote / BiDi (essential) ─────────────────────────────────────────
user_pref("remote.enabled", true);
user_pref("remote.bidi.dismiss_file_pickers.enabled", true);

// ── Miscellaneous ─────────────────────────────────────────────────────
user_pref("dom.disable_open_during_load", false);
user_pref("dom.iframe_lazy_loading.enabled", false);
user_pref("dom.file.createInChild", true);
user_pref("dom.push.serverURL", "");
user_pref("focusmanager.testmode", true);
user_pref("geo.provider.testing", true);
user_pref("geo.wifi.scan", false);
user_pref("general.useragent.updates.enabled", false);
user_pref("services.settings.server", "data:,#remote-settings-dummy/v1");
user_pref("services.sync.enabled", false);
user_pref("media.gmp-manager.updateEnabled", false);
user_pref("media.sanity-test.disabled", true);
user_pref("devtools.jsonview.enabled", false);
user_pref("webgl.forbid-software", false);
user_pref("ui.systemUsesDarkTheme", 0);
user_pref("plugin.state.flash", 0);
user_pref("javascript.options.showInConsole", true);
user_pref("network.cookie.sameSite.laxByDefault", false);
user_pref("network.http.prompt-temp-redirect", false);
user_pref("network.manage-offline-status", false);
user_pref("security.notification_enable_delay", 0);
user_pref("security.certerrors.mitm.priming.enabled", false);
user_pref("startup.homepage_welcome_url", "about:blank");
user_pref("startup.homepage_welcome_url.additional", "");
user_pref("screenshots.browser.component.enabled", false);
"#;

fn firefox_default_prefs(downloads_dir: &std::path::Path) -> std::io::Result<Vec<u8>> {
  let mut f = Vec::new();

  write_firefox_download_prefs(&mut f, downloads_dir)?;

  f.extend_from_slice(FIREFOX_PREFS.as_bytes());

  Ok(f)
}

/// Append download-related preferences to `user.js` so Firefox routes
/// downloads to a profile-scoped folder. Without these prefs Firefox
/// falls back to the host user's `~/Downloads` when computing the
/// `suggestedFilename` it ships in `browsingContext.downloadWillBegin`,
/// which makes the value depend on whichever leftover files happen to
/// be lying around in the developer's downloads dir.
fn write_firefox_download_prefs(f: &mut impl std::io::Write, downloads_dir: &std::path::Path) -> std::io::Result<()> {
  let dir = serde_json::to_string(&downloads_dir.to_string_lossy()).map_err(std::io::Error::other)?;
  writeln!(
    f,
    "// ── Downloads (profile-scoped to keep suggestedFilename deterministic)\n\
user_pref(\"browser.download.folderList\", 2);\n\
user_pref(\"browser.download.dir\", {dir});\n\
user_pref(\"browser.download.lastDir\", {dir});\n\
user_pref(\"browser.download.useDownloadDir\", true);\n\
user_pref(\"browser.download.manager.showWhenStarting\", false);\n\
user_pref(\"browser.download.alwaysOpenPanel\", false);\n\
user_pref(\"browser.helperApps.alwaysAsk.force\", false);\n\
user_pref(\"browser.helperApps.neverAsk.saveToDisk\", \"application/octet-stream, application/pdf\");"
  )
}
