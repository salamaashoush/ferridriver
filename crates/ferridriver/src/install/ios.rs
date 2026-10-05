use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::backend::process::output;
use crate::error::{FerriError, Result};

use super::{BrowserInstaller, InstallProgress, backend_err, install_lock, sha256_hex};

const NODE_VERSION: &str = "24.21.0";
const REMOTE_DEBUGGER_REVISION: &str = "64e4914bb763be134ec577176118ecda242b04d6";
const REMOTE_DEBUGGER_DIGEST: &str = "2083d055769674d3fa97956449185ac26c40d0a25daffe151f60f089606052d3";
const PAGE_ROUTING_PATCH: &str = include_str!("appium-remote-debugger.patch");
const SIMULATOR_REVISION: &str = "75a05126445ad079d9b33fed4cd4a736f3ab3ee5";
const SIMULATOR_DIGEST: &str = "b7babc1f9f7fb1ab3f26f384de1909d85b4e2322e3604b270d9d8660908cffbe";
const SIMULATOR_PATCH: &str = include_str!("appium-ios-simulator.patch");
const WDA_REVISION: &str = "3e8aa7de81f254dbb0876baa9e9173c16b55b3a0";
const WDA_DIGEST: &str = "b42e618429451d5ad1db26ac0f1cde263795e9b13544912914e5374d65a07624";
const WDA_PATCH: &str = include_str!("appium-webdriveragent.patch");
const XCUITEST_REVISION: &str = "65585c013009dd3f3dd4e85cb89f7dca185d017c";
const XCUITEST_DIGEST: &str = "a7c82efe6ad7be4e31a73fadf3271a32ec2700e374918c9aa7ea4fb97c4fdab8";
const XCUITEST_PATCH: &str = include_str!("appium-xcuitest-driver.patch");

impl BrowserInstaller {
  /// # Errors
  ///
  /// Requires macOS with configured Xcode and reports dependency installation
  /// failures without replacing existing installations.
  pub async fn install_ios<F>(&self, progress: F) -> Result<String>
  where
    F: Fn(InstallProgress),
  {
    if !cfg!(target_os = "macos") {
      return Err(FerriError::unsupported(
        "Managed iOS simulators require macOS and Xcode; connect to a remote iOS endpoint on this platform",
      ));
    }
    tokio::time::timeout(Duration::from_mins(30), self.prepare_ios(progress))
      .await
      .map_err(|_| FerriError::timeout("installing iOS dependencies", 1_800_000))?
  }

  async fn prepare_ios<F>(&self, progress: F) -> Result<String>
  where
    F: Fn(InstallProgress),
  {
    progress(InstallProgress::Resolving);
    validate_xcode().await?;

    let parent = self.cache_dir.join("ios");
    tokio::fs::create_dir_all(&parent).await?;
    let _lock = install_lock(&parent.join(".install.lock")).await?;
    ensure_runtime().await?;
    let revision = format!(
      "appium-3.7.0-xcuitest-12.12.3-{}-{}-",
      std::env::consts::ARCH,
      dependency_revision()
    );
    if let Some(root) = cached_installation(&parent, &revision).await? {
      verify_installation(&root).await?;
      progress(InstallProgress::Complete {
        version: "Appium 3.7.0 / XCUITest 12.12.3".into(),
        path: root.display().to_string(),
      });
      return Ok(root.display().to_string());
    }
    // Preserve failed attempts while allowing a later install to complete independently.
    let root = tempfile::Builder::new().prefix(&revision).tempdir_in(&parent)?.keep();
    let marker = root.join("ready.json");
    let (arch, digest) = node_archive(std::env::consts::ARCH)?;
    let node_name = format!("node-v{NODE_VERSION}-darwin-{arch}");
    self
      .install_ios_archive(
        &format!("https://nodejs.org/dist/v{NODE_VERSION}/{node_name}.tar.gz"),
        digest,
        &root.join("node.tar.gz"),
        &root,
        &progress,
      )
      .await?;
    tokio::fs::rename(root.join(node_name), root.join("node")).await?;
    for (name, revision, digest, patch) in [
      (
        "appium-remote-debugger",
        REMOTE_DEBUGGER_REVISION,
        REMOTE_DEBUGGER_DIGEST,
        PAGE_ROUTING_PATCH,
      ),
      (
        "appium-ios-simulator",
        SIMULATOR_REVISION,
        SIMULATOR_DIGEST,
        SIMULATOR_PATCH,
      ),
      ("WebDriverAgent", WDA_REVISION, WDA_DIGEST, WDA_PATCH),
      (
        "appium-xcuitest-driver",
        XCUITEST_REVISION,
        XCUITEST_DIGEST,
        XCUITEST_PATCH,
      ),
    ] {
      self
        .build_ios_dependency(&root, name, revision, digest, patch, &progress)
        .await?;
    }

    tokio::fs::write(
      root.join("package.json"),
      serde_json::to_vec_pretty(&serde_json::json!({
        "private": true,
        "dependencies": {"appium": "3.7.0", "appium-xcuitest-driver": "file:./appium-xcuitest-driver-12.12.3.tgz"}
      }))?,
    )
    .await?;
    npm(&root, &root, &["install", "--package-lock=true"]).await?;
    // XCUITest's published bundle includes this dependency; npm root overrides do not replace it.
    npm(
      &root,
      &root.join("node_modules/appium-xcuitest-driver"),
      &[
        "install",
        "--package-lock=true",
        "../../appium-remote-debugger-17.4.2.tgz",
        "../../appium-ios-simulator-9.1.3.tgz",
        "../../appium-webdriveragent-16.12.8.tgz",
      ],
    )
    .await?;
    // Verify the dependency actually resolved by XCUITest, not a root-level copy.
    verify_installation(&root).await?;
    register_driver(&root).await?;
    tokio::fs::write(marker, serde_json::to_vec_pretty(&installation_metadata())?).await?;
    progress(InstallProgress::Complete {
      version: "Appium 3.7.0 / XCUITest 12.12.3".into(),
      path: root.display().to_string(),
    });
    Ok(root.display().to_string())
  }

  async fn build_ios_dependency<F>(
    &self,
    root: &Path,
    name: &str,
    revision: &str,
    digest: &str,
    patch_source: &str,
    progress: &F,
  ) -> Result<()>
  where
    F: Fn(InstallProgress),
  {
    self
      .install_ios_archive(
        &format!("https://codeload.github.com/appium/{name}/tar.gz/{revision}"),
        digest,
        &root.join(format!("{name}.tar.gz")),
        root,
        progress,
      )
      .await?;
    let source = root.join(format!("{name}-{revision}"));
    let mut patch = tokio::process::Command::new("/usr/bin/patch");
    patch.args(["--batch", "--forward", "-p1"]).current_dir(&source);
    output(patch, Some(patch_source.as_bytes()), Some(Duration::from_secs(30))).await?;
    let mut args = vec!["install", "--package-lock=true"];
    if name == "appium-xcuitest-driver" {
      args.extend(["--no-save", "../appium-remote-debugger-17.4.2.tgz"]);
    }
    npm(root, &source, &args).await?;
    npm(root, &source, &["run", "build"]).await?;
    npm(root, &source, &["pack", "--pack-destination", ".."]).await?;
    Ok(())
  }

  async fn install_ios_archive<F>(
    &self,
    url: &str,
    digest: &str,
    archive: &Path,
    destination: &Path,
    progress: &F,
  ) -> Result<()>
  where
    F: Fn(InstallProgress),
  {
    self.download_file(url, archive, progress).await?;
    if sha256_hex(&tokio::fs::read(archive).await?) != digest {
      return Err(backend_err("iOS dependency archive checksum mismatch"));
    }
    let archive = archive.to_path_buf();
    let destination = destination.to_path_buf();
    tokio::task::spawn_blocking(move || {
      tar::Archive::new(flate2::read::GzDecoder::new(std::fs::File::open(archive)?)).unpack(destination)
    })
    .await
    .map_err(backend_err)??;
    Ok(())
  }
}

fn installation_metadata() -> serde_json::Value {
  serde_json::json!({
    "appium": "3.7.0", "xcuitest": "12.12.3", "node": NODE_VERSION,
    "remoteDebuggerRevision": REMOTE_DEBUGGER_REVISION,
    "patchSha256": sha256_hex(PAGE_ROUTING_PATCH.as_bytes()),
    "simulatorRevision": SIMULATOR_REVISION,
    "simulatorPatchSha256": sha256_hex(SIMULATOR_PATCH.as_bytes()),
    "wdaRevision": WDA_REVISION,
    "wdaPatchSha256": sha256_hex(WDA_PATCH.as_bytes()),
    "xcuitestRevision": XCUITEST_REVISION,
    "xcuitestPatchSha256": sha256_hex(XCUITEST_PATCH.as_bytes())
  })
}

fn dependency_revision() -> String {
  sha256_hex(
    format!(
      "{NODE_VERSION}\0{REMOTE_DEBUGGER_REVISION}\0{SIMULATOR_REVISION}\0{WDA_REVISION}\0{XCUITEST_REVISION}\0{PAGE_ROUTING_PATCH}\0{SIMULATOR_PATCH}\0{WDA_PATCH}\0{XCUITEST_PATCH}"
    )
    .as_bytes(),
  )
}

async fn validate_xcode() -> Result<()> {
  let mut xcode = tokio::process::Command::new("/usr/bin/xcrun");
  xcode.args(["--find", "simctl"]);
  output(xcode, None, Some(Duration::from_secs(30))).await?;
  let mut setup = tokio::process::Command::new("/usr/bin/xcodebuild");
  setup.arg("-checkFirstLaunchStatus");
  output(setup, None, Some(Duration::from_secs(30))).await?;
  Ok(())
}

async fn cached_installation(parent: &Path, revision: &str) -> Result<Option<PathBuf>> {
  let mut entries = tokio::fs::read_dir(parent).await?;
  while let Some(entry) = entries.next_entry().await? {
    if entry.file_name().to_string_lossy().starts_with(revision)
      && entry.file_type().await?.is_dir()
      && entry.path().join("ready.json").is_file()
    {
      return Ok(Some(entry.path()));
    }
  }
  Ok(None)
}

fn node_archive(arch: &str) -> Result<(&'static str, &'static str)> {
  match arch {
    "x86_64" => Ok((
      "x64",
      "1462cb3b3046b815cf8ea436d3da450ec1a9f11dac7e5a46b0ada5305d7e8097",
    )),
    "aarch64" => Ok((
      "arm64",
      "bed7eea5325e1108f32ce5228ddd6a5f0f08a499ee42aa7442aea583702f6057",
    )),
    _ => Err(FerriError::unsupported(format!(
      "iOS dependency host architecture {arch}"
    ))),
  }
}

async fn ensure_runtime() -> Result<()> {
  if installed_runtime().await? {
    return Ok(());
  }
  let mut command = tokio::process::Command::new("/usr/bin/xcodebuild");
  command.args(["-downloadPlatform", "iOS"]);
  output(command, None, Some(Duration::from_mins(25))).await?;
  if !installed_runtime().await? {
    return Err(backend_err(
      "Xcode did not install an available iOS simulator runtime for this host",
    ));
  }
  Ok(())
}

async fn installed_runtime() -> Result<bool> {
  let mut command = tokio::process::Command::new("/usr/bin/xcrun");
  command.args(["simctl", "list", "runtimes", "--json"]);
  let listing = output(command, None, Some(Duration::from_secs(30))).await?;
  has_runtime(&serde_json::from_str(&listing)?, std::env::consts::ARCH)
}

fn has_runtime(listing: &serde_json::Value, arch: &str) -> Result<bool> {
  let arch = if arch == "aarch64" { "arm64" } else { arch };
  let runtimes = listing["runtimes"]
    .as_array()
    .ok_or_else(|| backend_err("simctl did not return a runtime list"))?;
  Ok(runtimes.iter().any(|runtime| {
    runtime["isAvailable"] == true
      && runtime["identifier"]
        .as_str()
        .is_some_and(|id| id.starts_with("com.apple.CoreSimulator.SimRuntime.iOS-"))
      && runtime.get("supportedArchitectures").is_none_or(|architectures| {
        architectures
          .as_array()
          .is_some_and(|values| values.iter().any(|value| value.as_str() == Some(arch)))
      })
  }))
}

fn node_command(root: &Path) -> Result<tokio::process::Command> {
  let bin = root.join("node/bin");
  let mut paths = vec![bin.clone()];
  paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
  let mut command = tokio::process::Command::new(bin.join("node"));
  command
    .env("PATH", std::env::join_paths(paths).map_err(backend_err)?)
    .env("APPIUM_HOME", root.join("home"))
    .env("npm_config_cache", root.join("npm-cache"))
    .current_dir(root);
  Ok(command)
}

async fn register_driver(root: &Path) -> Result<()> {
  let mut command = node_command(root)?;
  // Local registration links the already-built package, whose tarball omits the TypeScript build config.
  command.env("npm_config_ignore_scripts", "true");
  command
    .arg(root.join("node_modules/appium/index.js"))
    .args(["driver", "install", "--source", "local"])
    .arg(root.join("node_modules/appium-xcuitest-driver"));
  output(command, None, Some(Duration::from_mins(5))).await?;
  Ok(())
}

async fn npm(root: &Path, cwd: &Path, args: &[&str]) -> Result<()> {
  let mut command = node_command(root)?;
  command
    .arg(root.join("node/lib/node_modules/npm/bin/npm-cli.js"))
    .arg("--prefix")
    .arg(cwd)
    .args(args)
    .args(["--no-audit", "--no-fund"])
    .current_dir(cwd);
  output(command, None, Some(Duration::from_mins(15))).await?;
  Ok(())
}

async fn verify_installation(root: &Path) -> Result<()> {
  let mut command = node_command(root)?;
  command.args([
    "--input-type=module",
    "-e",
    r"
import {createRequire} from 'node:module';
import {readFileSync} from 'node:fs';
import {dirname,join} from 'node:path';
const require=createRequire(join(process.cwd(),'node_modules/appium-xcuitest-driver/package.json'));
const pkg=require.resolve('appium-remote-debugger/package.json');
const source=readFileSync(join(dirname(pkg),'build/lib/rpc/rpc-client.js'),'utf8');
if (!source.includes('_pageTargetsBySenderId') || !source.includes('forwardDidClose')) {
  throw new Error('XCUITest did not resolve the patched Inspector dependency');
}
const executionSource=readFileSync(join(dirname(pkg),'build/lib/mixins/execute.js'),'utf8');
const navigationSource=readFileSync(join(dirname(pkg),'build/lib/mixins/navigate.js'),'utf8');
if (!navigationSource.includes('Page.navigate') || !navigationSource.includes('err.code !== -32601')) {
  throw new Error('XCUITest did not resolve the capability-based Safari navigation implementation');
}
const windowSource=readFileSync(join('node_modules','appium-xcuitest-driver','build/lib/commands/window.js'),'utf8');
if (!executionSource.includes('options.emulateUserGesture') || !windowSource.includes('createNewWindow')) {
  throw new Error('XCUITest did not resolve the Safari window-creation implementation');
}
const simulatorPkg=require.resolve('appium-ios-simulator/package.json');
const simulatorSource=readFileSync(join(dirname(simulatorPkg),'build/lib/simulator-xcode-14.js'),'utf8');
if (!simulatorSource.includes('runOpts.isHeadless && this.devicesSetPath')) {
  throw new Error('XCUITest did not resolve the isolated Simulator dependency');
}
const wdaPkg=require.resolve('appium-webdriveragent/package.json');
const wdaSource=readFileSync(join(dirname(wdaPkg),'build/lib/xcodebuild.js'),'utf8');
if (!wdaSource.includes('APPIUM_WDA_INHERIT_PROCESS_GROUP') ||
    !wdaSource.includes('start(this.detachProcess ? true : null)')) {
  throw new Error('XCUITest did not resolve the owned WebDriverAgent dependency');
}
for (const [name,version] of [['appium','3.7.0'],['appium-xcuitest-driver','12.12.3']]) {
  if (JSON.parse(readFileSync(join('node_modules',name,'package.json'),'utf8')).version!==version) {
    throw new Error('Unexpected installed version: '+name);
  }
}
",
  ]);
  output(command, None, Some(Duration::from_secs(30))).await?;
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn node_hosts_are_pinned_and_unsupported_hosts_are_rejected() {
    assert_eq!(node_archive("x86_64").unwrap().0, "x64");
    assert_eq!(node_archive("aarch64").unwrap().0, "arm64");
    assert!(matches!(node_archive("riscv64"), Err(FerriError::Unsupported { .. })));
  }

  #[test]
  fn commands_use_private_tools_and_extension_home() {
    let root = PathBuf::from("/tmp/sashoush ios setup");
    let command = node_command(&root).unwrap();
    let command = command.as_std();
    assert_eq!(command.get_program(), root.join("node/bin/node"));
    assert_eq!(command.get_current_dir(), Some(root.as_path()));
    let env: std::collections::BTreeMap<_, _> = command.get_envs().collect();
    assert_eq!(
      env[std::ffi::OsStr::new("APPIUM_HOME")],
      Some(root.join("home").as_os_str())
    );
    assert_eq!(
      std::env::split_paths(env[std::ffi::OsStr::new("PATH")].unwrap()).next(),
      Some(root.join("node/bin"))
    );
  }

  #[test]
  fn runtime_detection_requires_available_ios_for_the_host_architecture() {
    let mut runtime = serde_json::json!({
      "identifier":"com.apple.CoreSimulator.SimRuntime.iOS-26-2",
      "isAvailable":true, "supportedArchitectures":["arm64"]
    });
    let listing = |runtime| serde_json::json!({"runtimes":[runtime]});
    assert!(has_runtime(&listing(runtime.clone()), "aarch64").unwrap());
    assert!(!has_runtime(&listing(runtime.clone()), "x86_64").unwrap());
    runtime["isAvailable"] = false.into();
    assert!(!has_runtime(&listing(runtime.clone()), "aarch64").unwrap());
    runtime["isAvailable"] = true.into();
    runtime["identifier"] = "com.apple.CoreSimulator.SimRuntime.tvOS-26-2".into();
    assert!(!has_runtime(&listing(runtime), "aarch64").unwrap());
    assert!(!has_runtime(&serde_json::json!({"runtimes":[]}), "x86_64").unwrap());
    assert!(has_runtime(&serde_json::json!({}), "x86_64").is_err());
  }

  #[tokio::test]
  async fn cache_ignores_incomplete_attempts_and_other_revisions_without_changing_them() {
    let directory = tempfile::tempdir().unwrap();
    let incomplete = directory.path().join("current-incomplete");
    let old = directory.path().join("old-complete");
    std::fs::create_dir(&incomplete).unwrap();
    std::fs::write(incomplete.join("npm.log"), "interrupted installation").unwrap();
    std::fs::create_dir(&old).unwrap();
    std::fs::write(old.join("ready.json"), "{}").unwrap();
    assert_eq!(cached_installation(directory.path(), "current-").await.unwrap(), None);
    let complete = directory.path().join("current-complete");
    std::fs::create_dir(&complete).unwrap();
    std::fs::write(complete.join("ready.json"), "{}").unwrap();
    assert_eq!(
      cached_installation(directory.path(), "current-").await.unwrap(),
      Some(complete)
    );
    assert_eq!(
      std::fs::read_to_string(incomplete.join("npm.log")).unwrap(),
      "interrupted installation"
    );
    assert!(old.join("ready.json").is_file());
  }

  #[cfg(not(target_os = "macos"))]
  #[tokio::test]
  async fn unsupported_host_does_not_create_cache_or_download() {
    let temporary = tempfile::tempdir().unwrap();
    let cache = temporary.path().join("absent");
    let installer = BrowserInstaller::with_cache_dir(cache.clone());
    assert!(matches!(
      installer.install_ios(|_| {}).await,
      Err(FerriError::Unsupported { .. })
    ));
    assert!(!cache.exists());
  }
}
