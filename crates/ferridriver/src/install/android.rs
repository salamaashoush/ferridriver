use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::android::{AndroidOptions, AndroidSdk};
use crate::backend::process::output;
use crate::error::{FerriError, Result};

use super::{BrowserInstaller, InstallProgress, backend_err, extract_zip, install_lock, sha256_hex};

impl BrowserInstaller {
  /// # Errors
  ///
  /// Reports unsupported hosts, failed downloads or SDK commands, and licenses
  /// that require explicit acceptance before the requested packages can install.
  pub async fn install_android<F>(&self, options: &AndroidOptions, progress: F) -> Result<String>
  where
    F: Fn(InstallProgress),
  {
    tokio::time::timeout(Duration::from_mins(30), self.prepare_android(options, progress))
      .await
      .map_err(|_| FerriError::timeout("installing Android dependencies", 1_800_000))?
      .map(|sdk| sdk.root.display().to_string())
  }

  pub(crate) async fn prepare_android<F>(&self, options: &AndroidOptions, progress: F) -> Result<AndroidSdk>
  where
    F: Fn(InstallProgress),
  {
    let image = options.image_package()?;
    let root = options
      .sdk_path
      .clone()
      .or_else(|| {
        ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
          .iter()
          .find_map(std::env::var_os)
          .map(PathBuf::from)
      })
      .or_else(|| {
        dirs::home_dir()
          .map(|home| {
            home.join(if cfg!(target_os = "macos") {
              "Library/Android/sdk"
            } else {
              "Android/Sdk"
            })
          })
          .filter(|path| path.is_dir())
      })
      .unwrap_or_else(|| self.cache_dir.join("android/sdk"));
    tokio::fs::create_dir_all(&root).await?;
    let _lock = install_lock(&root.join(".ferridriver-install.lock")).await?;
    progress(InstallProgress::Resolving);
    let tools = match find_tools(&root) {
      Some(path) => path,
      None => self.install_android_tools(&root, &progress).await?,
    };
    let java_home = match find_java() {
      Some(path) => path,
      None => self.install_android_java(&progress).await?,
    };
    let sdk = AndroidSdk {
      root,
      tools,
      java_home,
      adb: None,
    };
    install_packages(&sdk, options, &image).await?;
    progress(InstallProgress::Complete {
      version: image,
      path: sdk.root.display().to_string(),
    });
    Ok(sdk)
  }

  async fn install_android_tools<F>(&self, root: &Path, progress: &F) -> Result<PathBuf>
  where
    F: Fn(InstallProgress),
  {
    let (platform, digest) = match (std::env::consts::OS, std::env::consts::ARCH) {
      ("linux", "x86_64") => (
        "linux",
        "4e4c464f145a7512b57d088ac6c278c03c9eea610886b35a5e0804e74eedf583",
      ),
      ("macos", "x86_64") => (
        "mac_x86_64",
        "c5a6378ab5cf7e0d5701921405115befff13e9ff7417fb588389338f8bd050f3",
      ),
      ("macos", "aarch64") => (
        "mac_arm64",
        "835b62a26162b229b441d1f6d4680383815a270809eb33522c0d480fa5002c4e",
      ),
      _ => return Err(FerriError::unsupported("Android SDK host platform")),
    };
    let parent = root.join("cmdline-tools");
    tokio::fs::create_dir_all(&parent).await?;
    let staging = tempfile::Builder::new().prefix(".ferridriver-").tempdir_in(&parent)?;
    let zip = staging.path().join("tools.zip");
    let url = format!("https://dl.google.com/android/repository/commandlinetools-{platform}-15859902_latest.zip");
    self.download_file(&url, &zip, progress).await?;
    verify_digest(&zip, digest).await?;
    let staging = tokio::task::spawn_blocking(move || {
      extract_zip(&zip, staging.path())?;
      Ok::<_, FerriError>(staging)
    })
    .await
    .map_err(backend_err)??;
    let installed = parent.join("ferridriver-15859902");
    if installed.exists() {
      return Err(backend_err(format!(
        "incomplete Android tools at {}; preserve or repair this directory before retrying",
        installed.display()
      )));
    }
    tokio::fs::rename(staging.path().join("cmdline-tools"), &installed).await?;
    Ok(installed)
  }

  async fn install_android_java<F>(&self, progress: &F) -> Result<PathBuf>
  where
    F: Fn(InstallProgress),
  {
    let parent = self.cache_dir.join("android");
    tokio::fs::create_dir_all(&parent).await?;
    let _lock = install_lock(&parent.join(".java-install.lock")).await?;
    let installed = parent.join("jre-21");
    if let Some(java) = java_in(&installed) {
      return Ok(java);
    }
    let os = if cfg!(target_os = "macos") { "mac" } else { "linux" };
    let arch = if cfg!(target_arch = "aarch64") {
      "aarch64"
    } else {
      "x64"
    };
    let url = format!(
      "https://api.adoptium.net/v3/assets/latest/21/hotspot?architecture={arch}&image_type=jre&os={os}&vendor=eclipse"
    );
    let assets: serde_json::Value = self
      .client
      .get(url)
      .send()
      .await
      .map_err(backend_err)?
      .error_for_status()
      .map_err(backend_err)?
      .json()
      .await
      .map_err(backend_err)?;
    let package = &assets[0]["binary"]["package"];
    let url = package["link"]
      .as_str()
      .ok_or_else(|| backend_err("Adoptium did not return a Java 21 runtime"))?;
    let digest = package["checksum"]
      .as_str()
      .ok_or_else(|| backend_err("Adoptium runtime checksum missing"))?;
    let staging = tempfile::Builder::new().prefix(".jre-").tempdir_in(&parent)?;
    let archive = staging.path().join("java.tar.gz");
    self.download_file(url, &archive, progress).await?;
    verify_digest(&archive, digest).await?;
    let destination = staging.path().join("runtime");
    let extracted = destination.clone();
    let _staging = tokio::task::spawn_blocking(move || {
      let file = std::fs::File::open(archive)?;
      tar::Archive::new(flate2::read::GzDecoder::new(file)).unpack(extracted)?;
      Ok::<_, FerriError>(staging)
    })
    .await
    .map_err(backend_err)??;
    let entry = std::fs::read_dir(destination)?
      .next()
      .transpose()?
      .ok_or_else(|| backend_err("empty Java archive"))?;
    if installed.exists() {
      return Err(backend_err(format!(
        "incomplete Java runtime at {}",
        installed.display()
      )));
    }
    tokio::fs::rename(entry.path(), &installed).await?;
    java_in(&installed).ok_or_else(|| backend_err("downloaded Java runtime has no java executable"))
  }
}

async fn install_packages(sdk: &AndroidSdk, options: &AndroidOptions, image: &str) -> Result<()> {
  let image_path = sdk.root.join(image.replace(';', "/"));
  let packages = [
    ("platform-tools", sdk.root.join("platform-tools/adb")),
    ("emulator", sdk.root.join("emulator/emulator")),
    (image, image_path.join("package.xml")),
  ];
  let missing: Vec<&str> = packages
    .iter()
    .filter(|(_, path)| !path.is_file())
    .map(|(name, _)| *name)
    .collect();
  if !missing.is_empty() {
    let mut command = sdk.command(&sdk.tools.join("bin/sdkmanager"));
    command.arg(format!("--sdk_root={}", sdk.root.display())).args(&missing);
    let input = options.accept_licenses.then(|| "y\n".repeat(256));
    output(
      command,
      input.as_deref().map(str::as_bytes),
      Some(Duration::from_mins(30)),
    )
    .await?;
    if let Some((name, _)) = packages.iter().find(|(_, path)| !path.is_file()) {
      return Err(FerriError::backend(format!(
        "Android package {name} was not installed; review the Android SDK licenses and run ferridriver install android --accept-licenses to accept them"
      )));
    }
  }
  Ok(())
}

async fn verify_digest(path: &Path, expected: &str) -> Result<()> {
  if sha256_hex(&tokio::fs::read(path).await?) != expected {
    return Err(backend_err("Android dependency archive checksum mismatch"));
  }
  Ok(())
}

fn find_tools(root: &Path) -> Option<PathBuf> {
  let parent = root.join("cmdline-tools");
  let latest = parent.join("latest");
  if latest.join("bin/avdmanager").is_file() && latest.join("bin/sdkmanager").is_file() {
    return Some(latest);
  }
  std::fs::read_dir(parent)
    .ok()?
    .filter_map(std::result::Result::ok)
    .map(|entry| entry.path())
    .find(|path| path.join("bin/avdmanager").is_file() && path.join("bin/sdkmanager").is_file())
}

fn java_in(path: &Path) -> Option<PathBuf> {
  [path.to_owned(), path.join("Contents/Home")]
    .into_iter()
    .find(|path| path.join("bin/java").is_file())
}

fn find_java() -> Option<PathBuf> {
  let mut candidates: Vec<PathBuf> = std::env::var_os("JAVA_HOME").map(PathBuf::from).into_iter().collect();
  candidates.extend(
    [
      "/Applications/Android Studio.app/Contents/jbr",
      "/opt/android-studio/jbr",
      "/usr/lib/jvm/default",
    ]
    .map(PathBuf::from),
  );
  if let Some(home) = dirs::home_dir() {
    candidates.push(home.join(".local/share/JetBrains/Toolbox/apps/android-studio/jbr"));
  }
  candidates.into_iter().find_map(|path| java_in(&path))
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;

  #[tokio::test]
  async fn zero_exit_without_installed_packages_is_an_error() {
    let directory = tempfile::tempdir().unwrap();
    let sdk = fake_sdk(directory.path()).unwrap();
    let options = AndroidOptions::default();
    let image = options.image_package().unwrap();
    let error = install_packages(&sdk, &options, &image).await.unwrap_err();
    assert!(error.to_string().contains("not installed"));
    assert!(!sdk.root.join("accepted").exists());
  }

  #[tokio::test]
  async fn explicit_acceptance_installs_missing_packages_and_reuses_them() {
    let directory = tempfile::tempdir().unwrap();
    let sdk = fake_sdk(directory.path()).unwrap();
    let options = AndroidOptions {
      accept_licenses: true,
      ..Default::default()
    };
    let image = options.image_package().unwrap();
    install_packages(&sdk, &options, &image).await.unwrap();
    assert!(sdk.root.join("accepted").is_file());
    assert!(sdk.root.join(image.replace(';', "/")).join("package.xml").is_file());
    install_packages(&sdk, &AndroidOptions::default(), &image)
      .await
      .unwrap();
    assert_eq!(std::fs::read_to_string(sdk.root.join("calls")).unwrap(), "call\n");
  }

  fn fake_sdk(root: &Path) -> Result<AndroidSdk> {
    let tools = root.join("cmdline-tools/test");
    std::fs::create_dir_all(tools.join("bin"))?;
    let script = tools.join("bin/sdkmanager");
    std::os::unix::fs::symlink(
      Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/android/sdkmanager.sh"),
      &script,
    )?;
    Ok(AndroidSdk {
      root: root.to_owned(),
      tools,
      java_home: root.join("java"),
      adb: None,
    })
  }
}
