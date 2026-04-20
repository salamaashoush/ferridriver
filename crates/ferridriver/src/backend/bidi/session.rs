//! `BiDi` session management: WebSocket connection, session creation, browser launch.
//!
//! The `BiDi` protocol connects directly via WebSocket -- no HTTP session endpoint.
//! Firefox exposes native `BiDi` at `ws://host:port/session`.
//! Chrome can also be used via chromedriver's `BiDi` endpoint.

use std::sync::Arc;

use serde_json::json;
use tracing::{debug, info};

use super::transport::BidiTransport;

/// A `BiDi` session -- holds the transport and session metadata.
#[derive(Clone)]
pub(crate) struct BidiSession {
  #[allow(dead_code)]
  pub session_id: String,
  pub transport: Arc<BidiTransport>,
  #[allow(dead_code)]
  pub browser_name: String,
  #[allow(dead_code)]
  pub browser_version: String,
}

// Event subscriptions use top-level module names (e.g. "browsingContext", "network")
// rather than individual event names. This matches Puppeteer's approach and avoids
// issues with unsupported event names breaking the session.

impl BidiSession {
  /// Connect to a `BiDi` endpoint directly via WebSocket.
  ///
  /// This is the native `BiDi` approach:
  /// 1. Connect WebSocket to `ws://host:port/session`
  /// 2. Send `session.new` to create a session
  /// 3. Subscribe to all events
  pub async fn connect(ws_url: &str) -> Result<Self, String> {
    info!("Connecting BiDi session to {ws_url}");

    let transport = Arc::new(BidiTransport::connect(ws_url).await?);

    // Create a new session with proper capabilities.
    // webSocketUrl: true tells Firefox to maintain the BiDi WebSocket across navigations.
    // unhandledPromptBehavior: ignore prevents dialogs from blocking automation.
    let result = transport
      .send_command(
        "session.new",
        json!({
          "capabilities": {
            "alwaysMatch": {
              "acceptInsecureCerts": true,
              "webSocketUrl": true,
              "unhandledPromptBehavior": {
                "default": "ignore"
              }
            }
          }
        }),
      )
      .await?;

    let session_id = result
      .get("sessionId")
      .and_then(|v| v.as_str())
      .unwrap_or("unknown")
      .to_string();
    let capabilities = result.get("capabilities").cloned().unwrap_or(json!({}));
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

    debug!("BiDi session created: id={session_id}, browser={browser_name} {browser_version}");

    // Subscribe to top-level event modules (matching Puppeteer's approach).
    // Using module names instead of individual events ensures we receive ALL
    // events under each module and avoids issues with unsupported event names.
    transport
      .send_command(
        "session.subscribe",
        json!({"events": ["browsingContext", "network", "log", "script", "input"]}),
      )
      .await?;

    info!("BiDi session ready: {browser_name} {browser_version}");
    Ok(Self {
      session_id,
      transport,
      browser_name,
      browser_version,
    })
  }

  /// Connect to a `BiDi` endpoint at the given port.
  /// Constructs `ws://127.0.0.1:{port}/session` and connects.
  #[allow(dead_code, reason = "public library API for external consumers")]
  pub async fn connect_to_port(port: u16) -> Result<Self, String> {
    Self::connect(&format!("ws://127.0.0.1:{port}/session")).await
  }

  /// Launch Firefox and create a `BiDi` session.
  ///
  /// Firefox natively supports `BiDi`: launch with `--remote-debugging-port`,
  /// read the `BiDi` WebSocket URL from stderr, connect directly.
  ///
  /// Returns `(session, child, profile_dir)`. The caller must keep
  /// `profile_dir` alive for the lifetime of the browser — its `Drop` removes
  /// the directory from disk. Firefox is launched with `kill_on_drop(true)`
  /// so the process dies before the dir vanishes.
  pub async fn launch_firefox(
    firefox_path: &str,
    flags: &[String],
    headless: bool,
  ) -> Result<(Self, tokio::process::Child, tempfile::TempDir), String> {
    let profile_dir = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;

    // Write automation preferences to user.js in the profile directory.
    // Matches Playwright's firefoxPreferences + Puppeteer's essentials.
    write_firefox_prefs(profile_dir.path()).map_err(|e| format!("write prefs: {e}"))?;

    let mut command = tokio::process::Command::new(firefox_path);
    command.arg("--remote-debugging-port").arg("0");
    command.arg("--profile").arg(profile_dir.path());
    command.arg("--no-remote");
    if headless {
      command.arg("--headless");
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
    command
      .stdin(std::process::Stdio::null())
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::piped())
      .kill_on_drop(true);

    debug!("Launching Firefox for BiDi: {firefox_path}");
    let mut child = command.spawn().map_err(|e| format!("Firefox launch: {e}"))?;

    // Firefox prints "WebDriver BiDi listening on ws://127.0.0.1:PORT" to stderr
    let ws_url = discover_bidi_ws_url(&mut child).await?;
    debug!("Firefox BiDi endpoint: {ws_url}");

    let session = Self::connect(&ws_url).await?;

    Ok((session, child, profile_dir))
  }

  /// Launch a browser and create a `BiDi` session.
  /// Currently supports Firefox (native `BiDi`). Chrome does not have built-in
  /// `BiDi` support -- use the CDP backend for Chrome instead.
  pub async fn launch(
    browser_path: &str,
    flags: &[String],
    headless: bool,
  ) -> Result<(Self, tokio::process::Child, tempfile::TempDir), String> {
    let path_lower = browser_path.to_lowercase();
    if path_lower.contains("firefox") {
      Box::pin(Self::launch_firefox(browser_path, flags, headless)).await
    } else {
      Err(format!(
        "BiDi backend requires Firefox (found: {browser_path}). \
         Chrome does not have built-in BiDi support -- use the CDP backend for Chrome. \
         Set FIREFOX_PATH or install Firefox."
      ))
    }
  }

  /// End the `BiDi` session gracefully.
  #[allow(dead_code)]
  pub async fn end(&self) -> Result<(), String> {
    let _ = self.transport.send_command("session.end", json!({})).await;
    Ok(())
  }
}

/// Read Firefox stderr to find the `BiDi` WebSocket URL.
/// Firefox prints: "`WebDriver` `BiDi` listening on ws://127.0.0.1:PORT"
async fn discover_bidi_ws_url(child: &mut tokio::process::Child) -> Result<String, String> {
  use tokio::io::AsyncBufReadExt;

  let stderr = child.stderr.take().ok_or("Firefox: no stderr handle")?;
  let mut reader = tokio::io::BufReader::new(stderr);
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);

  let mut line = String::new();
  loop {
    if tokio::time::Instant::now() >= deadline {
      return Err("Timeout waiting for Firefox BiDi WebSocket URL in stderr".into());
    }

    line.clear();
    let read_result = tokio::time::timeout(std::time::Duration::from_secs(1), reader.read_line(&mut line)).await;

    match read_result {
      Ok(Ok(0)) => {
        // EOF
        if let Ok(Some(status)) = child.try_wait() {
          return Err(format!("Firefox exited during startup with status: {status}"));
        }
      },
      Ok(Ok(_)) => {
        // Look for the BiDi WebSocket URL
        // Firefox prints: "WebDriver BiDi listening on ws://127.0.0.1:PORT"
        // The actual BiDi endpoint is at ws://host:port/session
        if let Some(pos) = line.find("ws://") {
          let mut ws_url = line[pos..].trim().to_string();
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
      },
      Ok(Err(e)) => return Err(format!("Firefox stderr read error: {e}")),
      Err(_) => {
        // Timeout on this read, check if process died
        if let Ok(Some(status)) = child.try_wait() {
          return Err(format!("Firefox exited during startup with status: {status}"));
        }
      },
    }
  }
}

/// Write Firefox automation preferences to `user.js` in the given profile directory.
/// Based on Playwright's `playwright.cfg` and Puppeteer's Firefox defaults.
fn write_firefox_prefs(profile_dir: &std::path::Path) -> std::io::Result<()> {
  use std::io::Write;

  let prefs_path = profile_dir.join("user.js");
  let mut f = std::fs::File::create(prefs_path)?;

  // Each line: user_pref("key", value);
  // Organised by category matching Playwright's structure.
  write!(
    f,
    r#"// ── Process model ──────────────────────────────────────────────────────
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
user_pref("browser.translations.enable", false);

// ── Extensions ────────────────────────────────────────────────────────
user_pref("extensions.autoDisableScopes", 0);
user_pref("extensions.enabledScopes", 5);
user_pref("extensions.update.enabled", false);
user_pref("extensions.screenshots.disabled", true);
user_pref("extensions.blocklist.enabled", false);

// ── Network (isolate from external services) ──────────────────────────
user_pref("network.captive-portal-service.enabled", false);
user_pref("network.connectivity-service.enabled", false);
user_pref("network.dns.disablePrefetch", true);
user_pref("network.http.speculative-parallel-limit", 0);
user_pref("network.cookie.CHIPS.enabled", false);
user_pref("browser.pocket.enabled", false);

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
user_pref("services.settings.server", "http://dummy.test/dummy/blocklist/");
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
"#
  )?;

  Ok(())
}
