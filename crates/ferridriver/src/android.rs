use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

use crate::backend::process::{ChildGroup, drain_child_stderr, output};
use crate::error::{FerriError, Result};

pub use crate::device::DeviceTarget;

mod adb;
mod api;
mod network;
mod wire;
pub(crate) use adb::AdbEndpoint;
pub use api::{Android, AndroidDevice, AndroidDevicesOptions, AndroidFile, AndroidLaunchBrowserOptions, AndroidSocket};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct AndroidOptions {
  pub api_level: u32,
  pub model: String,
  pub sdk_path: Option<PathBuf>,
  pub pkg: String,
  pub apk_path: Option<PathBuf>,
  pub timeout: u64,
  pub accept_licenses: bool,
}

impl Default for AndroidOptions {
  fn default() -> Self {
    Self {
      api_level: 35,
      model: "pixel_7".into(),
      sdk_path: None,
      pkg: "com.android.chrome".into(),
      apk_path: None,
      timeout: 180_000,
      accept_licenses: false,
    }
  }
}

impl AndroidOptions {
  pub(crate) fn image_package(&self) -> Result<String> {
    let abi = match (std::env::consts::OS, std::env::consts::ARCH) {
      ("linux" | "macos", "x86_64") => "x86_64",
      ("macos", "aarch64") => "arm64-v8a",
      (os, arch) => {
        return Err(FerriError::unsupported(format!(
          "Android emulator host {os}/{arch} is not supported"
        )));
      },
    };
    if !(26..=100).contains(&self.api_level) || self.model.is_empty() || self.model.starts_with('-') {
      return Err(FerriError::invalid_argument(
        "device",
        "invalid Android API level or model",
      ));
    }
    Ok(format!("system-images;android-{};google_apis;{abi}", self.api_level))
  }
}

/// The host's loopback as the emulator sees it: `10.0.2.2` is the emulator's alias for `127.0.0.1` on the
/// machine running it. A page keeps the URL it was given; only the connection goes to the alias.
const HOST_LOOPBACK_RULES: [&str; 3] = [
  "MAP localhost 10.0.2.2",
  "MAP *.localhost 10.0.2.2",
  "MAP 127.0.0.1 10.0.2.2",
];

/// The command line of a managed emulator's browser: the launcher's own
/// `DevTools` socket, and the host's loopback behind the emulator's alias.
fn chromium_command_line(pkg: &str, args: &[String]) -> Result<String> {
  command_line(pkg, args, "ferridriver_devtools_remote", true)
}

/// The command-line file Chromium on Android reads at start, for `pkg`
/// listening for `DevTools` on the abstract socket `socket`. `emulator`
/// adds the rules that send the device's loopback to the machine running
/// it, which only an emulator has an address for.
pub(crate) fn command_line(pkg: &str, args: &[String], socket: &str, emulator: bool) -> Result<String> {
  if pkg
    .split('.')
    .any(|part| part.is_empty() || !part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
  {
    return Err(FerriError::invalid_argument(
      "pkg",
      "expected an Android application package identifier",
    ));
  }
  // Playwright's Android launch (`server/android/android.ts::_defaultArgs`): its own three switches, then
  // `chromiumSwitches({ android: true })`, which is every desktop switch but `--disable-sync`. The desktop
  // launcher adds `--enable-automation` itself, so it is not part of that set either.
  let mut switches: Vec<String> = vec![
    "--disable-fre".into(),
    format!("--remote-debugging-socket-name={socket}"),
  ];
  switches.extend(
    crate::state::CHROMIUM_SWITCHES
      .iter()
      .filter(|switch| !["--disable-sync", "--enable-automation"].contains(switch))
      .map(|switch| (*switch).to_owned()),
  );
  // Chromium's native navigation blur suppresses CDP input after DOM load and actionability checks.
  switches.push("--disable-features=AndroidNavigationBlurTransitionAnimation".into());
  let mut resolver_rules: Vec<&str> = Vec::new();
  let mut positional: Vec<&str> = Vec::new();
  for arg in args {
    if arg.is_empty() || arg.contains('\0') {
      return Err(FerriError::invalid_argument(
        "args",
        "Android browser arguments must be nonempty and contain no NUL bytes",
      ));
    }
    if !positional.is_empty() {
      positional.push(arg);
      continue;
    }
    if arg == "--" {
      positional.push(arg);
      continue;
    }
    let switch = arg.strip_prefix("--").or_else(|| arg.strip_prefix('-'));
    let (name, value) = switch.map_or(("", None), |flag| {
      flag
        .split_once('=')
        .map_or((flag, None), |(name, value)| (name, Some(value)))
    });
    match name {
      "remote-debugging-socket-name" | "remote-debugging-port" | "remote-debugging-pipe" => {
        return Err(FerriError::invalid_argument(
          "args",
          "managed Android owns its debugging socket",
        ));
      },
      // One spelling, so the lists merge with the defaults above instead of replacing them.
      "enable-features" | "disable-features" => {
        if let Some(features) = value.filter(|features| !features.is_empty()) {
          switches.push(format!("--{name}={features}"));
        }
      },
      "host-resolver-rules" => {
        resolver_rules.extend(
          value
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|rule| !rule.is_empty()),
        );
      },
      _ => switches.push(arg.clone()),
    }
  }
  crate::state::merge_feature_switches(&mut switches);
  // The emulator's own loopback is the device, where nothing a script started is listening. Chromium takes
  // the first rule that matches and an EXCLUDE before any of them, so the caller's rules decide first.
  if emulator {
    resolver_rules.extend(HOST_LOOPBACK_RULES);
  }
  if !resolver_rules.is_empty() {
    switches.push(format!("--host-resolver-rules={}", resolver_rules.join(", ")));
  }
  let flags = std::iter::once("_")
    .chain(switches.iter().map(String::as_str))
    .chain(positional);
  let line = flags
    .map(|arg| {
      // Chromium treats backslashes literally except before quotes; close quotes before trailing backslashes.
      let prefix = arg.trim_end_matches('\\');
      if prefix.is_empty() {
        return arg.to_owned();
      }
      format!("\"{}\"{}", prefix.replace('"', "\\\""), &arg[prefix.len()..])
    })
    .collect::<Vec<_>>()
    .join(" ");
  if line.encode_utf16().count() > 96 * 1024 {
    return Err(FerriError::invalid_argument(
      "args",
      "Android Chromium limits its command-line file to 98304 UTF-16 code units",
    ));
  }
  Ok(line)
}

pub(crate) struct AndroidSdk {
  pub root: PathBuf,
  pub tools: PathBuf,
  pub java_home: PathBuf,
  pub adb: Option<AdbEndpoint>,
}

impl AndroidSdk {
  pub fn command(&self, program: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    command
      .env("ANDROID_HOME", &self.root)
      .env("ANDROID_SDK_ROOT", &self.root);
    command.env("JAVA_HOME", &self.java_home).kill_on_drop(true);
    if let Some(endpoint) = &self.adb {
      command
        .env("ANDROID_ADB_SERVER_ADDRESS", "127.0.0.1")
        .env("ANDROID_ADB_SERVER_PORT", endpoint.port.to_string())
        .env("ADB_SERVER_SOCKET", format!("tcp:127.0.0.1:{}", endpoint.port))
        .env("HOME", &endpoint.home)
        .env("ADB_VENDOR_KEYS", endpoint.home.join(".android/adbkey"));
    }
    command
  }

  fn adb_command(&self) -> tokio::process::Command {
    let mut command = self.command(&self.root.join("platform-tools/adb"));
    if let Some(endpoint) = &self.adb {
      // A numeric host prevents adb from spawning an unowned replacement daemon.
      command.args(["-H", "127.0.0.1", "-P", &endpoint.port.to_string()]);
    }
    command
  }

  async fn adb(&self, serial: &str, args: &[&str]) -> Result<String> {
    tracing::debug!(serial, ?args, "Android command");
    let mut command = self.adb_command();
    command.args(["-s", serial]).args(args);
    output(command, None, Some(Duration::from_secs(15))).await
  }
}

pub(crate) struct AndroidOwner {
  group: tokio::sync::Mutex<Option<ChildGroup>>,
  adb_group: tokio::sync::Mutex<Option<ChildGroup>>,
  cleanup: tokio::sync::Mutex<()>,
  directory: crate::backend::async_tempdir::AsyncTempDir,
}

impl AndroidOwner {
  fn new(directory: tempfile::TempDir) -> Arc<Self> {
    let owner = Arc::new(Self {
      group: tokio::sync::Mutex::new(None),
      adb_group: tokio::sync::Mutex::new(None),
      cleanup: tokio::sync::Mutex::new(()),
      directory: crate::backend::async_tempdir::AsyncTempDir::new(directory),
    });
    crate::state::allocation::retain_android(&owner);
    owner
  }

  pub(crate) fn cleanup_complete(&self) -> bool {
    self.directory.cleanup_complete()
      && self.group.try_lock().is_ok_and(|group| group.is_none())
      && self.adb_group.try_lock().is_ok_and(|group| group.is_none())
  }

  pub async fn close(&self) -> Result<()> {
    let _cleanup = self.cleanup.lock().await;
    for resource in [&self.group, &self.adb_group] {
      let mut resource = resource.lock().await;
      if let Some(group) = resource.as_mut() {
        group.shutdown().await?;
      }
      *resource = None;
    }
    self.directory.remove_now().await
  }
}

// Keep the device startup future opaque to the browser state referenced by its returned page handles.
pub(crate) fn launch<'a>(
  options: &'a AndroidOptions,
  headless: bool,
  args: &'a [String],
) -> futures::future::BoxFuture<'a, Result<crate::backend::AnyBrowser>> {
  Box::pin(async move {
    let mut owner = None;
    let result = {
      let future = Box::pin(async {
        let flags = chromium_command_line(&options.pkg, args)?;
        if let Some(apk) = &options.apk_path {
          let metadata = tokio::fs::metadata(apk)
            .await
            .map_err(|error| FerriError::invalid_argument("apkPath", error.to_string()))?;
          if !metadata.is_file() {
            return Err(FerriError::invalid_argument("apkPath", "expected an APK file"));
          }
        }
        let sdk = crate::install::BrowserInstaller::new()
          .prepare_android(options, |_| {})
          .await?;
        Box::pin(launch_prepared(options, headless, &flags, sdk, &mut owner)).await
      });
      if options.timeout == 0 {
        future.await
      } else {
        tokio::time::timeout(Duration::from_millis(options.timeout), future)
          .await
          .unwrap_or_else(|_| Err(FerriError::timeout("launching Android browser", options.timeout)))
      }
    };
    if let Err(error) = result {
      if let Some(owner) = owner
        && let Err(cleanup) = owner.close().await
      {
        return Err(FerriError::backend(format!("{error}; cleanup failed: {cleanup}")));
      }
      return Err(error);
    }
    result
  })
}

async fn validate_apk_package(sdk: &AndroidSdk, apk: &Path, pkg: &str, home: &Path) -> Result<()> {
  let mut inspect = sdk.command(&sdk.tools.join("bin/apkanalyzer"));
  inspect
    .env("HOME", home)
    .env("ANDROID_USER_HOME", home)
    .args(["manifest", "application-id"])
    .arg(apk);
  let declared = output(inspect, None, Some(Duration::from_secs(30))).await?;
  if declared.trim() != pkg {
    return Err(FerriError::invalid_argument(
      "apkPath",
      format!(
        "APK package {} does not match selected package {}",
        declared.trim(),
        pkg
      ),
    ));
  }
  Ok(())
}

async fn launch_prepared(
  options: &AndroidOptions,
  headless: bool,
  flags: &str,
  mut sdk: AndroidSdk,
  owner_slot: &mut Option<Arc<AndroidOwner>>,
) -> Result<crate::backend::AnyBrowser> {
  let temp = tempfile::Builder::new().prefix("ferridriver-android-").tempdir()?;
  let root = temp.path().to_owned();
  let owner = AndroidOwner::new(temp);
  *owner_slot = Some(Arc::clone(&owner));
  let avd_home = root.join("avd");
  let user_home = root.join("user");
  if let Some(apk) = &options.apk_path {
    tokio::fs::create_dir_all(&user_home).await?;
    validate_apk_package(&sdk, apk, &options.pkg, &user_home).await?;
  }
  tokio::fs::create_dir_all(&avd_home).await?;
  tokio::fs::create_dir_all(&user_home).await?;
  let mut create = sdk.command(&sdk.tools.join("bin/avdmanager"));
  create
    .env("ANDROID_AVD_HOME", &avd_home)
    .env("ANDROID_USER_HOME", &user_home);
  create.args([
    "create",
    "avd",
    "--name",
    "ferridriver",
    "--package",
    &options.image_package()?,
    "--device",
    &options.model,
  ]);
  output(create, Some(b"no\n"), Some(Duration::from_secs(60))).await?;

  adb::start_server(&mut sdk, &user_home, &root, &owner).await?;
  let (console, device) = adb::reserve_emulator_ports()?;
  let console_port = console.local_addr()?.port();
  let device_port = device.local_addr()?.port();
  let report = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
  let mut command = sdk.command(&sdk.root.join("emulator/emulator"));
  command
    .env("ANDROID_AVD_HOME", &avd_home)
    .env("ANDROID_USER_HOME", &user_home);
  command.args([
    "-avd",
    "ferridriver",
    "-no-snapshot",
    "-no-boot-anim",
    "-gpu",
    "software",
    "-memory",
    "2048",
  ]);
  command.args([
    "-report-console",
    &format!("tcp:{},max=30", report.local_addr()?.port()),
    "-ports",
    &format!("{console_port},{device_port}"),
  ]);
  if headless {
    command.arg("-no-window");
  }
  command
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped());
  #[cfg(unix)]
  command.process_group(0);
  drop((console, device));
  let mut child = crate::backend::process::spawn_exclusive(|| command.spawn())?;
  let stderr = drain_child_stderr(&mut child);
  let group = ChildGroup::new(child);
  *owner.group.lock().await = Some(group);
  Box::pin(connect_device(&sdk, options, flags, &owner, report, stderr)).await
}

async fn connect_device(
  sdk: &AndroidSdk,
  options: &AndroidOptions,
  flags: &str,
  owner: &Arc<AndroidOwner>,
  report: tokio::net::TcpListener,
  stderr: crate::backend::process::StderrTail,
) -> Result<crate::backend::AnyBrowser> {
  let (socket, _) = tokio::time::timeout(Duration::from_secs(30), report.accept())
    .await
    .map_err(|_| FerriError::backend(format!("emulator did not report its port: {:?}", stderr.lines())))??;
  let mut bytes = Vec::new();
  socket.take(32).read_to_end(&mut bytes).await?;
  let port: u16 = String::from_utf8_lossy(&bytes)
    .trim_matches(|c: char| c.is_whitespace() || c == '\0')
    .parse()
    .map_err(|_| FerriError::protocol("Android emulator", "invalid console port"))?;
  let serial = format!("emulator-{port}");
  tracing::info!(%serial, "waiting for managed Android device to boot");
  let mut wait = sdk.adb_command();
  wait.args(["-s", &serial, "wait-for-device"]);
  output(wait, None, None).await?;
  loop {
    if sdk.adb(&serial, &["shell", "getprop", "sys.boot_completed"]).await? == "1" {
      break;
    }
    if owner
      .group
      .lock()
      .await
      .as_mut()
      .is_none_or(|group| !group.is_running())
    {
      return Err(FerriError::backend(format!(
        "emulator exited during boot: {:?}",
        stderr.lines()
      )));
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
  }
  sdk
    .adb(&serial, &["shell", "cmd", "wifi", "set-wifi-enabled", "enabled"])
    .await?;
  sdk
    .adb(
      &serial,
      &["shell", "cmd", "wifi", "connect-network", "AndroidWifi", "open"],
    )
    .await?;
  if let Some(apk) = &options.apk_path {
    let mut install = sdk.adb_command();
    install
      .args(["-s", &serial, "install", "--no-streaming", "-r"])
      .arg(apk);
    output(install, None, None).await?;
  }
  enable_profile_overlay(sdk, &serial, &options.model).await?;
  let mut browser = Box::pin(start_chrome(sdk, &serial, &options.pkg, flags)).await?;
  network::wait_until_ready(&browser).await?;
  browser.android = Some(owner.clone());
  tracing::info!(%serial, pkg = options.pkg, "managed Android browser is ready");
  Ok(crate::backend::AnyBrowser::CdpWs(browser))
}

/// Whether `cmd overlay list` offers `overlay`, and if so whether it is on. A line is a state marker and a
/// package: `[x]` enabled, `[ ]` disabled, `---` unusable on this device and so not offered.
fn profile_overlay_state(listing: &str, overlay: &str) -> Option<bool> {
  listing.lines().find_map(|line| {
    let (marker, package) = line.trim().rsplit_once(' ')?;
    match marker {
      "[x]" if package == overlay => Some(true),
      "[ ]" if package == overlay => Some(false),
      _ => None,
    }
  })
}

/// Turn on the framework overlay of the emulator's hardware profile before the browser starts.
///
/// The emulator enables it by itself a few seconds after boot completes. A framework overlay changes every
/// application's assets, Android answers that by recreating each running activity, and Chrome's takes its
/// tabs with it: a browser started in between lost every page it had and refused new ones until its activity
/// was back. Enabled here first, the emulator's own request finds nothing to change.
async fn enable_profile_overlay(sdk: &AndroidSdk, serial: &str, model: &str) -> Result<()> {
  let overlay = format!("com.android.internal.emulation.{model}");
  let mut requested = false;
  loop {
    let listing = sdk.adb(serial, &["shell", "cmd", "overlay", "list"]).await?;
    match profile_overlay_state(&listing, &overlay) {
      // A profile without an overlay of its own has nothing the emulator will turn on later.
      None | Some(true) => return Ok(()),
      Some(false) if !requested => {
        sdk
          .adb(
            serial,
            &["shell", "cmd", "overlay", "enable-exclusive", "--category", &overlay],
          )
          .await?;
        requested = true;
      },
      Some(false) => tokio::time::sleep(Duration::from_millis(100)).await,
    }
  }
}

async fn resolve_chrome_activity(sdk: &AndroidSdk, serial: &str, pkg: &str) -> Result<String> {
  let resolved = loop {
    let resolved = sdk
      .adb(
        serial,
        &[
          "shell",
          "cmd",
          "package",
          "resolve-activity",
          "--brief",
          "-a",
          "android.intent.action.MAIN",
          "-p",
          pkg,
        ],
      )
      .await?;
    if resolved == "No activity found" {
      tokio::time::sleep(Duration::from_millis(100)).await;
      continue;
    }
    break resolved;
  };
  let activity = resolved
    .lines()
    .last()
    .filter(|name| {
      name
        .strip_prefix(pkg)
        .is_some_and(|component| component.starts_with('/'))
        && name
          .chars()
          .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '/' | '_'))
    })
    .ok_or_else(|| FerriError::backend(format!("Android could not resolve Chrome's main activity: {resolved}")))?;
  Ok(activity.to_owned())
}

async fn configure_chrome(sdk: &AndroidSdk, serial: &str, pkg: &str, flags: &str) -> Result<()> {
  sdk.adb(serial, &["shell", "am", "force-stop", pkg]).await?;
  let api_level = sdk.adb(serial, &["shell", "getprop", "ro.build.version.sdk"]).await?;
  let api_level: u32 = api_level
    .parse()
    .map_err(|_| FerriError::protocol("Android version", "invalid SDK level"))?;
  if api_level >= 33 {
    sdk
      .adb(
        serial,
        &["shell", "pm", "grant", pkg, "android.permission.POST_NOTIFICATIONS"],
      )
      .await?;
  }
  sdk
    .adb(serial, &["shell", "am", "set-debug-app", "--persistent", pkg])
    .await?;
  let command_line = tempfile::NamedTempFile::new()?;
  tokio::fs::write(command_line.path(), flags).await?;
  let mut push = sdk.adb_command();
  push
    .args(["-s", serial, "push"])
    .arg(command_line.path())
    .arg("/data/local/tmp/chrome-command-line");
  output(push, None, Some(Duration::from_secs(15))).await?;
  sdk
    .adb(
      serial,
      &["shell", "chmod", "644", "/data/local/tmp/chrome-command-line"],
    )
    .await?;
  Ok(())
}

async fn start_chrome(
  sdk: &AndroidSdk,
  serial: &str,
  pkg: &str,
  flags: &str,
) -> Result<crate::backend::cdp::CdpBrowser<crate::backend::cdp::ws::WsTransport>> {
  if !sdk
    .adb(serial, &["shell", "pm", "path", pkg])
    .await?
    .starts_with("package:")
  {
    return Err(FerriError::backend(format!(
      "Android browser package {pkg} is not installed; provide device.apkPath"
    )));
  }
  let activity = resolve_chrome_activity(sdk, serial, pkg).await?;
  configure_chrome(sdk, serial, pkg, flags).await?;
  sdk
    .adb(
      serial,
      &[
        "shell",
        "am",
        "start",
        "-a",
        "android.intent.action.VIEW",
        "-d",
        "about:blank",
        "-n",
        &activity,
      ],
    )
    .await?;
  let local_port = sdk
    .adb(
      serial,
      &["forward", "tcp:0", "localabstract:ferridriver_devtools_remote"],
    )
    .await?;
  let client = reqwest::Client::builder()
    .timeout(Duration::from_secs(2))
    .build()
    .map_err(|error| FerriError::backend(error.to_string()))?;
  let url = format!("http://127.0.0.1:{local_port}/json/version");
  let ws = loop {
    if let Ok(response) = client.get(&url).send().await
      && let Ok(value) = response.json::<serde_json::Value>().await
      && let Some(ws) = value.get("webSocketDebuggerUrl").and_then(serde_json::Value::as_str)
    {
      break ws.to_owned();
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  };
  crate::backend::cdp::CdpBrowser::connect(&ws).await
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;

  fn disabled_features(line: &str) -> Vec<&str> {
    line
      .split("\"--disable-features=")
      .nth(1)
      .and_then(|rest| rest.split('"').next())
      .map(|features| features.split(',').collect())
      .unwrap_or_default()
  }

  #[test]
  fn browser_arguments_preserve_chromium_quote_and_backslash_rules() {
    let line = chromium_command_line(
      "org.chromium.chrome",
      &[
        "--label=sashoush \"quoted\" C:\\".into(),
        "--literal=$(sashoush); 'text'".into(),
        "\\\\".into(),
        "--after=tail".into(),
      ],
    )
    .unwrap();
    assert!(line.contains(r#""--label=sashoush \"quoted\" C:"\"#), "{line}");
    assert!(line.contains(r#""--literal=$(sashoush); 'text'""#), "{line}");
    assert!(line.contains(" \\\\ "), "{line}");
    assert_eq!(
      line
        .matches("--remote-debugging-socket-name=ferridriver_devtools_remote")
        .count(),
      1
    );
  }

  #[test]
  fn profile_overlay_is_found_by_its_whole_name_and_its_marker() {
    let listing = "android\n[ ] com.android.internal.emulation.pixel_7_pro\n[x] com.android.internal.systemui.navbar.gestural\n--- com.android.internal.emulation.pixel_3\n[ ] com.android.internal.emulation.pixel_7\n\ncom.android.systemui\n[x] com.android.systemui.emulation.pixel_7\n";
    assert_eq!(
      profile_overlay_state(listing, "com.android.internal.emulation.pixel_7"),
      Some(false)
    );
    assert_eq!(
      profile_overlay_state(listing, "com.android.internal.emulation.pixel_3"),
      None
    );
    assert_eq!(
      profile_overlay_state(listing, "com.android.internal.systemui.navbar.gestural"),
      Some(true)
    );
    assert_eq!(
      profile_overlay_state(listing, "com.android.internal.emulation.medium_phone"),
      None
    );
    assert_eq!(
      profile_overlay_state("", "com.android.internal.emulation.pixel_7"),
      None
    );
  }

  #[test]
  fn transport_switch_precedes_the_argument_terminator() {
    let line = chromium_command_line("org.chromium.chrome", &["--".into(), "about:blank".into()]).unwrap();
    let socket = line.find("--remote-debugging-socket-name=").unwrap();
    let terminator = line.find("\"--\"").unwrap();
    assert!(
      socket < terminator,
      "managed socket became a positional argument: {line}"
    );
  }

  #[test]
  fn managed_browser_disables_native_navigation_input_suppression() {
    let line = chromium_command_line("org.chromium.chrome", &["--".into(), "about:blank".into()]).unwrap();
    let (switches, _) = line.split_once("\"--\"").unwrap();
    assert!(
      disabled_features(switches).contains(&"AndroidNavigationBlurTransitionAnimation"),
      "{line}"
    );
  }

  #[test]
  fn browser_feature_arguments_preserve_native_input_and_positional_arguments() {
    let line = chromium_command_line(
      "org.chromium.chrome",
      &[
        "--disable-features=OtherFeature,AndroidNavigationBlurTransitionAnimation".into(),
        "-disable-features=SecondFeature,OtherFeature".into(),
        "--".into(),
        "--disable-features=positional".into(),
        "--remote-debugging-port=9222".into(),
      ],
    )
    .unwrap();
    let (switches, positional) = line.split_once("\"--\"").unwrap();
    let disabled = disabled_features(switches);
    for feature in [
      "AndroidNavigationBlurTransitionAnimation",
      "OtherFeature",
      "SecondFeature",
    ] {
      assert_eq!(disabled.iter().filter(|named| **named == feature).count(), 1, "{line}");
    }
    assert_eq!(switches.matches("disable-features=").count(), 1);
    assert_eq!(
      positional.trim(),
      "\"--disable-features=positional\" \"--remote-debugging-port=9222\""
    );
    for flag in ["--disable-features", "--disable-features=", "-disable-features="] {
      let line = chromium_command_line("org.chromium.chrome", &[flag.into()]).unwrap();
      assert_eq!(line.matches("disable-features").count(), 1, "{line}");
      assert!(
        disabled_features(&line).contains(&"AndroidNavigationBlurTransitionAnimation"),
        "{line}"
      );
    }
  }

  #[test]
  fn managed_browser_starts_with_the_desktop_switches_and_merges_feature_lists() {
    let line = chromium_command_line(
      "org.chromium.chrome",
      &[
        "--enable-features=WebMCP".into(),
        "--enable-features=PaintHolding".into(),
      ],
    )
    .unwrap();
    for switch in [
      "--disable-back-forward-cache",
      "--disable-field-trial-config",
      "--disable-popup-blocking",
    ] {
      assert_eq!(line.matches(&format!("\"{switch}\"")).count(), 1, "{line}");
    }
    for absent in ["--disable-sync", "--enable-automation"] {
      assert!(!line.contains(absent), "{line}");
    }
    assert_eq!(line.matches("--no-first-run").count(), 1, "{line}");
    assert_eq!(line.matches("enable-features=").count(), 1, "{line}");
    let enabled = line
      .split("\"--enable-features=")
      .nth(1)
      .and_then(|rest| rest.split('"').next())
      .unwrap();
    for feature in ["CDPScreenshotNewSurface", "WebMCP", "PaintHolding"] {
      assert!(enabled.split(',').any(|named| named == feature), "{line}");
    }
    assert!(!disabled_features(&line).contains(&"PaintHolding"), "{line}");
  }

  #[test]
  fn host_loopback_reaches_the_host_unless_the_caller_rules_otherwise() {
    let line = chromium_command_line("org.chromium.chrome", &[]).unwrap();
    assert!(
      line
        .contains("\"--host-resolver-rules=MAP localhost 10.0.2.2, MAP *.localhost 10.0.2.2, MAP 127.0.0.1 10.0.2.2\""),
      "{line}"
    );

    let line = chromium_command_line(
      "org.chromium.chrome",
      &[
        "--host-resolver-rules=EXCLUDE 127.0.0.1, MAP app.example.com 10.0.2.2".into(),
        "-host-resolver-rules=MAP localhost 192.0.2.1".into(),
        "--".into(),
        "--host-resolver-rules=positional".into(),
      ],
    )
    .unwrap();
    let (switches, positional) = line.split_once(" \"--\" ").unwrap();
    assert_eq!(switches.matches("host-resolver-rules=").count(), 1, "{line}");
    assert!(
      switches.contains(
        "\"--host-resolver-rules=EXCLUDE 127.0.0.1, MAP app.example.com 10.0.2.2, MAP localhost 192.0.2.1, \
         MAP localhost 10.0.2.2, MAP *.localhost 10.0.2.2, MAP 127.0.0.1 10.0.2.2\""
      ),
      "{line}"
    );
    assert_eq!(positional, "\"--host-resolver-rules=positional\"");
  }

  #[test]
  fn a_device_that_is_no_emulator_keeps_its_own_loopback() {
    let line = command_line("com.android.chrome", &[], "ferridriver_7_devtools_remote", false).unwrap();
    assert!(
      line.contains("\"--remote-debugging-socket-name=ferridriver_7_devtools_remote\""),
      "{line}"
    );
    assert!(!line.contains("host-resolver-rules"), "{line}");
    let line = command_line(
      "com.android.chrome",
      &["--host-resolver-rules=MAP app.example.com 192.0.2.1".into()],
      "ferridriver_7_devtools_remote",
      false,
    )
    .unwrap();
    assert!(
      line.contains("\"--host-resolver-rules=MAP app.example.com 192.0.2.1\""),
      "{line}"
    );
  }

  #[test]
  fn managed_browser_rejects_invalid_package_and_transport_arguments() {
    for pkg in ["", "com..chrome", "-chrome", "com.chrome;bad", "com.chrome/Activity"] {
      assert!(chromium_command_line(pkg, &[]).is_err(), "{pkg}");
    }
    for arg in [
      "",
      "--flag=\0",
      "--remote-debugging-port=9222",
      "-remote-debugging-port=9222",
      "--remote-debugging-pipe",
      "--remote-debugging-socket-name=other",
    ] {
      assert!(
        chromium_command_line("org.chromium.chrome", &[arg.into()]).is_err(),
        "{arg:?}"
      );
    }
    assert!(chromium_command_line("org.chromium.chrome", &["x".repeat(96 * 1024)]).is_err());
  }

  #[tokio::test]
  async fn invalid_browser_inputs_fail_before_sdk_provisioning() {
    let root = tempfile::tempdir().unwrap();
    let sdk = root.path().join("unused-sdk");
    for options in [
      AndroidOptions {
        pkg: "com.chrome;bad".into(),
        sdk_path: Some(sdk.clone()),
        ..Default::default()
      },
      AndroidOptions {
        apk_path: Some(root.path().join("missing.apk")),
        sdk_path: Some(sdk.clone()),
        ..Default::default()
      },
      AndroidOptions {
        apk_path: Some(root.path().to_owned()),
        sdk_path: Some(sdk.clone()),
        ..Default::default()
      },
    ] {
      let result = tokio::time::timeout(Duration::from_secs(1), launch(&options, true, &[]))
        .await
        .unwrap();
      assert!(matches!(result, Err(FerriError::InvalidArgument { .. })));
      assert!(!sdk.exists());
    }
  }

  #[tokio::test]
  async fn waits_for_chrome_activity_after_boot_completion() {
    let directory = tempfile::tempdir().unwrap();
    let bin = directory.path().join("platform-tools");
    std::fs::create_dir(&bin).unwrap();
    let adb = bin.join("adb");
    // Immutable executables avoid rust-lang/rust#114554 during parallel forks.
    std::os::unix::fs::symlink(
      Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/android/adb.sh"),
      &adb,
    )
    .unwrap();
    let sdk = AndroidSdk {
      root: directory.path().to_owned(),
      tools: directory.path().to_owned(),
      java_home: directory.path().to_owned(),
      adb: None,
    };
    let activity = tokio::time::timeout(
      Duration::from_secs(2),
      resolve_chrome_activity(&sdk, "emulator-test", "com.android.chrome"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(activity, "com.android.chrome/com.google.android.apps.chrome.Main");
  }

  #[tokio::test]
  async fn failed_avd_setup_keeps_its_early_owner_available_for_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let sdk = AndroidSdk {
      root: directory.path().to_owned(),
      tools: directory.path().to_owned(),
      java_home: directory.path().to_owned(),
      adb: None,
    };
    let mut retained = None;
    assert!(
      launch_prepared(&AndroidOptions::default(), true, "_", sdk, &mut retained)
        .await
        .is_err()
    );
    let owner = retained.unwrap();
    assert!(!owner.cleanup_complete());
    assert!(owner.group.lock().await.is_none());
    assert!(owner.adb_group.lock().await.is_none());
    owner.close().await.unwrap();
    assert!(owner.cleanup_complete());
  }

  #[tokio::test]
  async fn cancelling_a_device_wait_kills_its_command_group() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("unexpected");
    let mut command = tokio::process::Command::new("sh");
    command
      .arg("-c")
      .arg("(sleep 0.3; touch \"$1\") & wait")
      .arg("android-test")
      .arg(&marker);
    let result = tokio::time::timeout(Duration::from_millis(100), output(command, None, None)).await;
    assert!(result.is_err());
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!marker.exists());
  }

  #[tokio::test]
  async fn command_deadline_kills_helpers_before_they_write() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("unexpected");
    let mut command = tokio::process::Command::new("sh");
    command
      .arg("-c")
      .arg("(sleep 0.3; touch \"$1\") & wait")
      .arg("android-test")
      .arg(&marker);
    let error = output(command, None, Some(Duration::from_millis(100)))
      .await
      .unwrap_err();
    assert!(error.to_string().contains("deadline"));
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!marker.exists());
  }

  #[tokio::test]
  async fn command_reports_failure_and_drains_both_pipes() {
    let mut command = tokio::process::Command::new("sh");
    command.args(["-c", "printf 'sdk-out'; printf 'sdk-error' >&2; exit 17"]);
    let error = output(command, None, Some(Duration::from_secs(5)))
      .await
      .unwrap_err()
      .to_string();
    assert!(error.contains("17"));
    assert!(error.contains("sdk-out"));
    assert!(error.contains("sdk-error"));
  }

  #[test]
  fn target_rejects_unknown_options_and_invalid_models() {
    assert!(
      serde_json::from_value::<DeviceTarget>(serde_json::json!({
        "platform":"android", "apiLevl":35
      }))
      .is_err()
    );
    assert!(
      AndroidOptions {
        model: "--force".into(),
        ..Default::default()
      }
      .image_package()
      .is_err()
    );
  }
}
