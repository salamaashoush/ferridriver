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
mod network;
pub(crate) use adb::AdbEndpoint;

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

fn chromium_command_line(pkg: &str, args: &[String]) -> Result<String> {
  if pkg
    .split('.')
    .any(|part| part.is_empty() || !part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
  {
    return Err(FerriError::invalid_argument(
      "pkg",
      "expected an Android application package identifier",
    ));
  }
  let mut flags = vec![
    "_",
    "--disable-fre",
    "--no-first-run",
    "--no-default-browser-check",
    "--remote-debugging-socket-name=ferridriver_devtools_remote",
  ];
  // Chromium's native navigation blur suppresses CDP input after DOM load and actionability checks.
  let mut disabled_features = vec!["AndroidNavigationBlurTransitionAnimation"];
  let mut feature_names: rustc_hash::FxHashSet<&str> = disabled_features.iter().copied().collect();
  let mut parsing_switches = true;
  for arg in args {
    if arg.is_empty() || arg.contains('\0') {
      return Err(FerriError::invalid_argument(
        "args",
        "Android browser arguments must be nonempty and contain no NUL bytes",
      ));
    }
    let switch = arg.strip_prefix("--").or_else(|| arg.strip_prefix('-'));
    if parsing_switches
      && let Some(flag) = switch
      && [
        "remote-debugging-socket-name",
        "remote-debugging-port",
        "remote-debugging-pipe",
      ]
      .contains(&flag.split('=').next().unwrap_or_default())
    {
      return Err(FerriError::invalid_argument(
        "args",
        "managed Android owns its debugging socket",
      ));
    }
    if arg == "--" {
      parsing_switches = false;
    }
    if parsing_switches
      && let Some(features) = switch.and_then(|flag| {
        flag
          .strip_prefix("disable-features=")
          .or_else(|| (flag == "disable-features").then_some(""))
      })
    {
      for feature in features.split(',').map(str::trim).filter(|feature| !feature.is_empty()) {
        if feature_names.insert(feature) {
          disabled_features.push(feature);
        }
      }
      continue;
    }
    flags.push(arg);
  }
  let disabled_features = format!("--disable-features={}", disabled_features.join(","));
  flags.insert(1, &disabled_features);
  let line = flags
    .into_iter()
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
  let mut child = command.spawn()?;
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
  let mut browser = Box::pin(start_chrome(sdk, &serial, &options.pkg, flags)).await?;
  network::wait_until_ready(&browser).await?;
  browser.android = Some(owner.clone());
  tracing::info!(%serial, pkg = options.pkg, "managed Android browser is ready");
  Ok(crate::backend::AnyBrowser::CdpWs(browser))
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
    let animation = line
      .find("--disable-features=AndroidNavigationBlurTransitionAnimation")
      .unwrap();
    assert!(animation < line.find("\"--\"").unwrap(), "{line}");
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
    assert!(
      switches.contains("\"--disable-features=AndroidNavigationBlurTransitionAnimation,OtherFeature,SecondFeature\"")
    );
    assert_eq!(switches.matches("disable-features=").count(), 1);
    assert_eq!(
      positional.trim(),
      "\"--disable-features=positional\" \"--remote-debugging-port=9222\""
    );
    for flag in ["--disable-features", "--disable-features=", "-disable-features="] {
      let line = chromium_command_line("org.chromium.chrome", &[flag.into()]).unwrap();
      assert_eq!(line.matches("disable-features").count(), 1, "{line}");
      assert!(line.contains("--disable-features=AndroidNavigationBlurTransitionAnimation"));
    }
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
