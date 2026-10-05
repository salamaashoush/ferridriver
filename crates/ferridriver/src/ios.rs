use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::backend::process::{ChildGroup, output};
use crate::error::{FerriError, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct IosOptions {
  pub model: Option<String>,
  pub version: Option<String>,
  pub timeout: u64,
}

impl Default for IosOptions {
  fn default() -> Self {
    Self {
      model: None,
      version: None,
      timeout: 300_000,
    }
  }
}

struct Resources {
  closing: bool,
  udid: Option<String>,
  groups: Vec<ChildGroup>,
  directory: tempfile::TempDir,
}

pub(crate) struct IosOwner {
  resources: Arc<tokio::sync::Mutex<Option<Resources>>>,
  cleanup: tokio::sync::mpsc::UnboundedSender<tokio::sync::oneshot::Sender<std::result::Result<(), String>>>,
}

impl IosOwner {
  pub(crate) fn cleanup_complete(&self) -> bool {
    self.resources.try_lock().is_ok_and(|resources| resources.is_none())
  }

  fn new() -> Result<Arc<Self>> {
    let resources = Arc::new(tokio::sync::Mutex::new(Some(Resources {
      closing: false,
      udid: None,
      groups: Vec::new(),
      directory: tempfile::Builder::new().prefix("ferridriver-ios-").tempdir()?,
    })));
    let (cleanup, mut requests) =
      tokio::sync::mpsc::unbounded_channel::<tokio::sync::oneshot::Sender<std::result::Result<(), String>>>();
    let owned = resources.clone();
    tokio::spawn(async move {
      while let Some(reply) = requests.recv().await {
        let result = cleanup_owned(&owned).await.map_err(|error| error.to_string());
        if let Err(Err(error)) = reply.send(result) {
          tracing::warn!(%error, "iOS cleanup failed after its caller disconnected");
        }
      }
      if let Err(error) = cleanup_owned(&owned).await
        && let Some(resources) = owned.lock().await.take()
      {
        let directory = resources.directory.keep();
        tracing::error!(%error, device = ?resources.udid, directory = %directory.display(), "iOS cleanup failed; retaining device diagnostics");
      }
    });
    Ok(Arc::new(Self { resources, cleanup }))
  }

  pub async fn close(&self) -> Result<()> {
    let (reply, completed) = tokio::sync::oneshot::channel();
    self
      .cleanup
      .send(reply)
      .map_err(|_| FerriError::backend("iOS cleanup worker stopped"))?;
    completed
      .await
      .map_err(|error| FerriError::backend(error.to_string()))?
      .map_err(FerriError::backend)
  }

  async fn create_device(&self, model: String, runtime: String) -> Result<String> {
    let resources = self.resources.clone();
    // Creation can finish in CoreSimulator after its caller is cancelled. Record
    // its UUID before releasing the lock that the cleanup worker must acquire.
    tokio::spawn(async move {
      let mut guard = resources.lock().await;
      let state = guard
        .as_mut()
        .filter(|state| !state.closing)
        .ok_or_else(|| FerriError::target_closed(None))?;
      let name = state
        .directory
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| FerriError::backend("Invalid owned simulator directory"))?;
      let udid = simctl(&["create", name, &model, &runtime]).await?;
      state.udid = Some(udid.clone());
      Ok(udid)
    })
    .await
    .map_err(|error| FerriError::backend(error.to_string()))?
  }
}

async fn cleanup_owned(owned: &tokio::sync::Mutex<Option<Resources>>) -> Result<()> {
  let mut guard = owned.lock().await;
  if let Some(resources) = guard.as_mut() {
    cleanup_resources(resources, simctl).await?;
  }
  *guard = None;
  Ok(())
}

async fn cleanup_resources(
  resources: &mut Resources,
  control: impl std::ops::AsyncFn(&[&str]) -> Result<String>,
) -> Result<()> {
  resources.closing = true;
  for group in &mut resources.groups {
    group.shutdown().await?;
  }
  resources.groups.clear();
  if let Some(udid) = resources.udid.as_deref() {
    let shutdown = control(&["shutdown", udid]).await;
    let listing: Value = serde_json::from_str(&control(&["list", "devices", "--json"]).await?)?;
    let state = simulator_state(&listing, udid)?;
    if state.is_some_and(|state| state != "Shutdown") {
      shutdown?;
      return Err(FerriError::backend("Owned iOS simulator did not shut down"));
    }
    if state.is_some() {
      control(&["delete", udid]).await?;
    }
    resources.udid = None;
  }
  match tokio::fs::remove_dir_all(resources.directory.path()).await {
    Ok(()) => {},
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
    Err(error) => return Err(error.into()),
  }
  Ok(())
}

fn simulator_state<'a>(listing: &'a Value, udid: &str) -> Result<Option<&'a str>> {
  let invalid = || FerriError::protocol("simctl list devices", "invalid device catalog");
  for devices in listing["devices"].as_object().ok_or_else(invalid)?.values() {
    for device in devices.as_array().ok_or_else(invalid)? {
      if device["udid"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(invalid)?
        == udid
      {
        return device["state"].as_str().map(Some).ok_or_else(invalid);
      }
    }
  }
  Ok(None)
}

async fn simctl(args: &[&str]) -> Result<String> {
  let mut command = tokio::process::Command::new("/usr/bin/xcrun");
  command.arg("simctl").args(args);
  output(command, None, Some(Duration::from_secs(60)))
    .await
    .map(|value| value.trim().to_owned())
}

pub(crate) async fn launch(options: &IosOptions, headless: bool) -> Result<crate::backend::AnyBrowser> {
  if options.timeout == 0 {
    return Err(FerriError::invalid_argument(
      "device.timeout",
      "iOS launch timeout must be positive",
    ));
  }
  let deadline = tokio::time::Instant::now()
    .checked_add(Duration::from_millis(options.timeout))
    .ok_or_else(|| FerriError::invalid_argument("device.timeout", "iOS launch timeout is too large"))?;
  let installation = PathBuf::from(
    tokio::time::timeout_at(deadline, crate::install::BrowserInstaller::new().install_ios(|_| {}))
      .await
      .map_err(|_| FerriError::timeout("provisioning iOS dependencies", options.timeout))??,
  );
  let owner = IosOwner::new()?;
  crate::state::allocation::retain_ios(&owner);
  let result = tokio::time::timeout_at(deadline, launch_owned(options, headless, &installation, &owner))
    .await
    .unwrap_or_else(|_| Err(FerriError::timeout("launching iOS Safari", options.timeout)));
  match result {
    Ok(browser) => Ok(crate::backend::AnyBrowser::WebDriver(browser)),
    Err(error) => {
      if crate::state::allocation::has_pending_sessions() {
        return Err(error);
      }
      if let Err(cleanup) = owner.close().await {
        return Err(FerriError::backend(format!(
          "{error}; simulator cleanup failed: {cleanup}"
        )));
      }
      Err(error)
    },
  }
}

async fn launch_owned(
  options: &IosOptions,
  headless: bool,
  installation: &std::path::Path,
  owner: &Arc<IosOwner>,
) -> Result<crate::backend::webdriver::browser::WebDriverBrowser> {
  let runtimes: Value = serde_json::from_str(&simctl(&["list", "runtimes", "--json"]).await?)?;
  let types: Value = serde_json::from_str(&simctl(&["list", "devicetypes", "--json"]).await?)?;
  let (model, runtime) = select_device(options, &runtimes, &types)?;
  let udid = owner.create_device(model, runtime).await?;
  simctl(&["boot", &udid]).await?;
  simctl(&["bootstatus", &udid, "-b"]).await?;
  let endpoint = start_appium(installation, owner).await?;
  if !headless {
    start_ui(&udid, owner).await?;
  }
  let ports = [
    std::net::TcpListener::bind("127.0.0.1:0")?,
    std::net::TcpListener::bind("127.0.0.1:0")?,
  ];
  let wda_port = ports[0].local_addr()?.port();
  let mjpeg_port = ports[1].local_addr()?.port();
  let home = dirs::home_dir().ok_or_else(|| FerriError::backend("Cannot locate the Simulator device set"))?;
  let directory = owner
    .resources
    .lock()
    .await
    .as_ref()
    .filter(|state| !state.closing)
    .ok_or_else(|| FerriError::target_closed(None))?
    .directory
    .path()
    .to_owned();
  let capabilities = json!({
    "platformName":"iOS", "appium:automationName":"XCUITest", "appium:udid":udid,
    "appium:simulatorDevicesSetPath":home.join("Library/Developer/CoreSimulator/Devices"),
    "appium:isHeadless":true, "appium:noReset":false,
    // Keep owned sessions alive until the Rust browser releases them.
    "appium:newCommandTimeout":0,
    // Safari 26's toolbar tutorial consumes the first native tap. State 3 marks
    // that tutorial as already shown, before Safari first opens on our device.
    "appium:safariGlobalPreferences":{
      "WBSOnboardingStatesDefaultsKeyV0.2":{"TipForMoreButton":3}
    },
    "appium:wdaLocalPort":wda_port, "appium:mjpegServerPort":mjpeg_port,
    "appium:derivedDataPath":directory.join("wda"), "appium:wdaLaunchTimeout":options.timeout,
    "appium:wdaStartupRetries":1
  });
  drop(ports);
  let mut browser = crate::backend::webdriver::browser::WebDriverBrowser::connect(
    &endpoint,
    "safari",
    Some(&capabilities),
    None,
    Some(options.timeout),
  )
  .await?;
  browser.ios_owner = Some(owner.clone());
  Ok(browser)
}

fn select_device(options: &IosOptions, runtimes: &Value, types: &Value) -> Result<(String, String)> {
  let arch = if std::env::consts::ARCH == "aarch64" {
    "arm64"
  } else {
    std::env::consts::ARCH
  };
  let mut candidates = runtimes["runtimes"]
    .as_array()
    .into_iter()
    .flatten()
    .filter(|runtime| runtime["isAvailable"] == true)
    .filter(|runtime| {
      runtime["identifier"]
        .as_str()
        .is_some_and(|id| id.starts_with("com.apple.CoreSimulator.SimRuntime.iOS-"))
    })
    .filter(|runtime| {
      options
        .version
        .as_ref()
        .is_none_or(|version| runtime["version"] == *version)
    })
    .filter(|runtime| {
      runtime.get("supportedArchitectures").is_none_or(|values| {
        values
          .as_array()
          .is_some_and(|values| values.iter().any(|value| value == arch))
      })
    })
    .filter_map(|runtime| Some((version_number(runtime["version"].as_str()?)?, runtime)))
    .collect::<Vec<_>>();
  candidates.sort_by_key(|(version, _)| std::cmp::Reverse(*version));
  for (version, runtime) in candidates {
    let model = types["devicetypes"]
      .as_array()
      .into_iter()
      .flatten()
      .filter(|model| model["productFamily"] == "iPhone" || model["productFamily"] == "iPad")
      .filter(|model| {
        options
          .model
          .as_ref()
          .map_or(model["productFamily"] == "iPhone", |name| {
            model["name"] == *name || model["identifier"] == *name
          })
      })
      .filter(|model| {
        model["minRuntimeVersion"]
          .as_u64()
          .is_none_or(|minimum| version >= minimum)
      })
      .filter(|model| {
        model["maxRuntimeVersion"]
          .as_u64()
          .is_none_or(|maximum| version <= maximum)
      })
      .find_map(|model| model["identifier"].as_str());
    if let (Some(model), Some(runtime)) = (model, runtime["identifier"].as_str()) {
      return Ok((model.to_owned(), runtime.to_owned()));
    }
  }
  Err(FerriError::invalid_argument(
    "device",
    "No compatible available iOS runtime and simulator model match the requested version/model",
  ))
}

fn version_number(version: &str) -> Option<u64> {
  let mut parts = version.split('.');
  let major: u64 = parts.next()?.parse().ok()?;
  let minor: u64 = parts.next().unwrap_or("0").parse().ok()?;
  let patch: u64 = parts.next().unwrap_or("0").parse().ok()?;
  if major > 65535 || minor > 255 || patch > 255 || parts.next().is_some() {
    return None;
  }
  Some((major << 16) | (minor << 8) | patch)
}

async fn start_appium(installation: &std::path::Path, owner: &IosOwner) -> Result<String> {
  let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
  let endpoint = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
  let mut command = tokio::process::Command::new(installation.join("node/bin/node"));
  let mut paths = vec![installation.join("node/bin")];
  paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
  command
    .arg(installation.join("node_modules/appium/index.js"))
    .args([
      "--address",
      "127.0.0.1",
      "--port",
      &listener.local_addr()?.port().to_string(),
    ])
    .env(
      "PATH",
      std::env::join_paths(paths).map_err(|error| FerriError::backend(error.to_string()))?,
    )
    .env("APPIUM_HOME", installation.join("home"))
    .env("APPIUM_WDA_INHERIT_PROCESS_GROUP", "1");
  drop(listener);
  let (index, stderr) = spawn_owned(command, owner).await?;
  let client = reqwest::Client::builder()
    .no_proxy()
    .timeout(Duration::from_secs(2))
    .build()
    .map_err(|error| FerriError::backend(error.to_string()))?;
  loop {
    if !owner
      .resources
      .lock()
      .await
      .as_mut()
      .filter(|state| !state.closing)
      .ok_or_else(|| FerriError::target_closed(None))?
      .groups
      .get_mut(index)
      .ok_or_else(|| FerriError::target_closed(None))?
      .is_running()
    {
      return Err(FerriError::backend(format!(
        "Appium exited during startup: {}",
        stderr.as_error_context().unwrap_or_default()
      )));
    }
    if let Ok(response) = client.get(format!("{endpoint}/status")).send().await
      && response.status().is_success()
    {
      return Ok(endpoint);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

async fn start_ui(udid: &str, owner: &IosOwner) -> Result<()> {
  let mut xcode = tokio::process::Command::new("/usr/bin/xcode-select");
  xcode.arg("-p");
  let developer = output(xcode, None, Some(Duration::from_secs(10))).await?;
  let mut command = tokio::process::Command::new(
    PathBuf::from(developer.trim()).join("Applications/Simulator.app/Contents/MacOS/Simulator"),
  );
  command.args(["-CurrentDeviceUDID", udid]);
  spawn_owned(command, owner).await.map(|_| ())
}

async fn spawn_owned(
  mut command: tokio::process::Command,
  owner: &IosOwner,
) -> Result<(usize, crate::backend::process::StderrTail)> {
  let mut resources = owner.resources.lock().await;
  let state = resources
    .as_mut()
    .filter(|state| !state.closing)
    .ok_or_else(|| FerriError::target_closed(None))?;
  command
    .kill_on_drop(true)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped());
  #[cfg(unix)]
  command.process_group(0);
  let mut child = command.spawn()?;
  let stderr = crate::backend::process::drain_child_output(&mut child);
  let index = state.groups.len();
  state.groups.push(ChildGroup::new(child));
  Ok((index, stderr))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn failed_directory_cleanup_retains_resources_for_an_explicit_retry() {
    let owner = IosOwner::new().unwrap();
    let path = owner
      .resources
      .lock()
      .await
      .as_ref()
      .unwrap()
      .directory
      .path()
      .to_owned();
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, b"sashoush cleanup fixture").unwrap();
    let error = owner.close().await;
    let retained = owner.resources.lock().await.is_some();
    assert!(matches!(
      spawn_owned(tokio::process::Command::new("unavailable-command"), &owner).await,
      Err(FerriError::TargetClosed { .. })
    ));
    assert!(matches!(
      owner
        .create_device("unavailable-model".into(), "unavailable-runtime".into())
        .await,
      Err(FerriError::TargetClosed { .. })
    ));
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let retry = owner.close().await;
    let removed = !path.exists();
    if !removed {
      std::fs::remove_dir(&path).unwrap();
    }
    assert!(error.is_err());
    assert!(retained, "failed cleanup discarded the owned resources");
    assert!(retry.is_ok(), "{retry:?}");
    assert!(removed, "retry returned without removing the owned directory");
    assert!(owner.resources.lock().await.is_none());
  }

  fn catalog() -> (Value, Value) {
    (
      json!({"runtimes":[
        {"identifier":"com.apple.CoreSimulator.SimRuntime.iOS-26-2","version":"26.2","isAvailable":true},
        {"identifier":"com.apple.CoreSimulator.SimRuntime.iOS-18-7","version":"18.7","isAvailable":true},
        {"identifier":"com.apple.CoreSimulator.SimRuntime.iOS-27","version":"27.0","isAvailable":false}
      ]}),
      json!({"devicetypes":[
        {"name":"iPhone older","identifier":"older","productFamily":"iPhone","minRuntimeVersion":1_179_648,"maxRuntimeVersion":1_245_183},
        {"name":"iPhone current","identifier":"current","productFamily":"iPhone","minRuntimeVersion":1_703_936},
        {"name":"iPad current","identifier":"tablet","productFamily":"iPad","minRuntimeVersion":1_703_936}
      ]}),
    )
  }

  #[tokio::test]
  async fn simulator_cleanup_retries_deletion_but_not_a_confirmed_absent_device() {
    for removed_remotely in [false, true] {
      let mut resources = Resources {
        closing: false,
        udid: Some("owned".into()),
        groups: Vec::new(),
        directory: tempfile::tempdir().unwrap(),
      };
      let directory = resources.directory.path().to_owned();
      let deletes = std::cell::Cell::new(0);
      let control = async |args: &[&str]| match args {
        ["shutdown", "owned"] => Ok(String::new()),
        ["list", "devices", "--json"] => Ok(if removed_remotely && deletes.get() != 0 {
          json!({"devices":{}}).to_string()
        } else {
          json!({"devices":{"runtime":[{"udid":"owned","state":"Shutdown"}]}}).to_string()
        }),
        ["delete", "owned"] => {
          deletes.set(deletes.get() + 1);
          if deletes.get() == 1 {
            Err(FerriError::backend("simulator deletion unavailable"))
          } else {
            Ok(String::new())
          }
        },
        other => panic!("unexpected command: {other:?}"),
      };
      let error = cleanup_resources(&mut resources, &control).await.unwrap_err();
      assert!(error.to_string().contains("simulator deletion unavailable"));
      assert_eq!(resources.udid.as_deref(), Some("owned"));
      assert!(resources.closing);
      assert!(directory.is_dir());
      cleanup_resources(&mut resources, &control).await.unwrap();
      assert!(resources.udid.is_none());
      assert!(!directory.exists());
      assert_eq!(deletes.get(), if removed_remotely { 1 } else { 2 });
    }
  }

  #[tokio::test]
  async fn directory_retry_does_not_repeat_successful_simulator_deletion() {
    let mut resources = Resources {
      closing: false,
      udid: Some("owned".into()),
      groups: Vec::new(),
      directory: tempfile::tempdir().unwrap(),
    };
    let directory = resources.directory.path().to_owned();
    let calls = std::cell::RefCell::new(Vec::new());
    let control = async |args: &[&str]| {
      calls.borrow_mut().push(args[0].to_owned());
      match args {
        ["shutdown", "owned"] => Ok(String::new()),
        ["list", "devices", "--json"] => {
          Ok(json!({"devices":{"runtime":[{"udid":"owned","state":"Shutdown"}]}}).to_string())
        },
        ["delete", "owned"] => {
          std::fs::remove_dir(&directory).unwrap();
          std::fs::write(&directory, b"sashoush cleanup fixture").unwrap();
          Ok(String::new())
        },
        other => panic!("unexpected command: {other:?}"),
      }
    };
    assert!(cleanup_resources(&mut resources, &control).await.is_err());
    assert!(resources.udid.is_none());
    assert!(directory.is_file());
    std::fs::remove_file(&directory).unwrap();
    std::fs::create_dir(&directory).unwrap();
    cleanup_resources(&mut resources, &control).await.unwrap();
    assert!(!directory.exists());
    assert_eq!(*calls.borrow(), ["shutdown", "list", "delete"]);
  }

  #[test]
  fn malformed_simulator_catalogs_do_not_confirm_a_device_is_absent() {
    for listing in [
      json!({}),
      json!({"devices":[]}),
      json!({"devices":{"runtime":null}}),
      json!({"devices":{"runtime":[{}]}}),
      json!({"devices":{"runtime":[{"udid":""}]}}),
      json!({"devices":{"runtime":[{"udid":"owned"}]}}),
    ] {
      assert!(simulator_state(&listing, "owned").is_err(), "{listing}");
    }
    assert_eq!(simulator_state(&json!({"devices":{}}), "owned").unwrap(), None);
  }

  #[test]
  fn selects_latest_compatible_runtime_and_model() {
    let (runtimes, types) = catalog();
    assert_eq!(
      select_device(&IosOptions::default(), &runtimes, &types).unwrap(),
      ("current".into(), "com.apple.CoreSimulator.SimRuntime.iOS-26-2".into())
    );
    let options = IosOptions {
      model: Some("iPhone older".into()),
      ..Default::default()
    };
    assert_eq!(
      select_device(&options, &runtimes, &types).unwrap(),
      ("older".into(), "com.apple.CoreSimulator.SimRuntime.iOS-18-7".into())
    );
  }

  #[test]
  fn accepts_explicit_tablet_and_rejects_incompatible_requests() {
    let (runtimes, types) = catalog();
    let mut options = IosOptions {
      model: Some("tablet".into()),
      ..Default::default()
    };
    assert_eq!(select_device(&options, &runtimes, &types).unwrap().0, "tablet");
    options.version = Some("18.7".into());
    assert!(select_device(&options, &runtimes, &types).is_err());
    options.version = Some("27.0".into());
    assert!(select_device(&options, &runtimes, &types).is_err());
  }

  #[test]
  fn rejects_wrong_architecture_and_malformed_versions() {
    let (mut runtimes, types) = catalog();
    for runtime in runtimes["runtimes"].as_array_mut().unwrap() {
      runtime["supportedArchitectures"] = json!(["unavailable-architecture"]);
    }
    assert!(select_device(&IosOptions::default(), &runtimes, &types).is_err());
    for value in ["", "26.x", "26.0.0.1", "65536", "26.256", "26.0.256"] {
      assert_eq!(version_number(value), None, "{value}");
    }
    assert_eq!(version_number("26.2"), Some(1_704_448));
  }

  #[tokio::test]
  async fn dropping_owner_cleans_its_directory() {
    let owner = IosOwner::new().unwrap();
    let path = owner
      .resources
      .lock()
      .await
      .as_ref()
      .unwrap()
      .directory
      .path()
      .to_owned();
    let resources = owner.resources.clone();
    drop(owner);
    tokio::time::timeout(Duration::from_secs(5), async {
      while resources.lock().await.is_some() {
        tokio::task::yield_now().await;
      }
    })
    .await
    .unwrap();
    assert!(!path.exists());
    assert!(resources.lock().await.is_none());
  }

  #[tokio::test]
  async fn cancelling_close_does_not_cancel_cleanup_waiting_for_creation() {
    let owner = IosOwner::new().unwrap();
    let guard = owner.resources.lock().await;
    let path = guard.as_ref().unwrap().directory.path().to_owned();
    let closing = owner.clone();
    let (queued, receipt) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
      let mut cleanup = std::pin::pin!(closing.close());
      assert!(futures::poll!(&mut cleanup).is_pending());
      queued.send(()).unwrap();
      cleanup.await
    });
    receipt.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(path.exists());
    drop(guard);
    tokio::time::timeout(Duration::from_secs(5), owner.close())
      .await
      .unwrap()
      .unwrap();
    assert!(!path.exists());
  }
}
