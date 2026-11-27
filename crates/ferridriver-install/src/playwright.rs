//! Playwright CDN browser build discovery, download, and lookup (`WebKit`, ffmpeg, Android driver).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use ferridriver_error::{FerriError, Result};

use crate::archive::{ANDROID_DRIVER_APKS, extract_zip, firefox_executable_path};
use crate::platform::{
  BuildMarker, PlaywrightBuild, WEBKIT_EXECUTABLE, ffmpeg_build, webkit_archive, webkit_platform_for_host,
};
use crate::playwright_cache::{CachedBrowser, installed_build};
use crate::{
  BrowserInstaller, CacheFamily, DOWNLOAD_RETRIES, InstallProgress, backend_err, ladybird, playwright_cache,
  version_order,
};

/// The latest released `playwright-core`. Its `browsers.json` names the
/// `WebKit` and Android driver builds that release ships, the way Chrome
/// for Testing and Mozilla name their current stable release. Main's names
/// builds no release has shipped yet.
const PW_LATEST_RELEASE_URL: &str = "https://registry.npmjs.org/playwright-core/latest";

#[derive(Debug, Deserialize)]
pub(crate) struct NpmRelease {
  pub(crate) version: String,
}

/// `browsers.json` as tagged for `release`.
pub(crate) fn released_browsers_json_url(release: &NpmRelease) -> Result<String> {
  let version = release.version.trim();
  let plain = !version.is_empty()
    && version.split('.').count() == 3
    && version
      .split('.')
      .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
  if !plain {
    return Err(backend_err(format!(
      "the latest playwright-core is '{version}', which is not a release version"
    )));
  }
  Ok(format!(
    "https://raw.githubusercontent.com/microsoft/playwright/v{version}/packages/playwright-core/browsers.json"
  ))
}

/// The build revision `browsers` names for `name`, taking the override
/// for `platform` when there is one.
pub(crate) fn browser_revision(browsers: PwBrowsersJson, name: &str, platform: &str) -> Result<String> {
  browsers
    .browsers
    .into_iter()
    .find(|b| b.name == name)
    .map(|mut b| b.revision_overrides.remove(platform).unwrap_or(b.revision))
    .ok_or_else(|| backend_err(format!("Playwright browsers.json missing {name} entry")))
}

/// Playwright CDN mirrors. Try in order; on connection failure, fall over.
pub(crate) const PW_CDN_MIRRORS: &[&str] = &[
  "https://cdn.playwright.dev/dbazure/download/playwright",
  "https://playwright.download.prss.microsoft.com/dbazure/download/playwright",
  "https://cdn.playwright.dev",
];

#[derive(Debug, Deserialize)]
pub(crate) struct PwBrowsersJson {
  pub(crate) browsers: Vec<PwBrowserEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PwBrowserEntry {
  pub(crate) name: String,
  pub(crate) revision: String,
  /// Host platforms pinned to an older build, such as `mac14-arm64`.
  #[serde(default)]
  pub(crate) revision_overrides: std::collections::BTreeMap<String, String>,
}

impl BrowserInstaller {
  async fn released_browsers_json(&self) -> Result<PwBrowsersJson> {
    let release: NpmRelease = self
      .client()?
      .get(PW_LATEST_RELEASE_URL)
      .send()
      .await
      .map_err(|e| backend_err(format!("failed to resolve the latest Playwright release: {e}")))?
      .json()
      .await
      .map_err(|e| backend_err(format!("failed to parse the latest Playwright release: {e}")))?;
    self
      .client()?
      .get(released_browsers_json_url(&release)?)
      .send()
      .await
      .map_err(|e| {
        backend_err(format!(
          "failed to fetch Playwright {} browsers.json: {e}",
          release.version
        ))
      })?
      .error_for_status()
      .map_err(|e| {
        backend_err(format!(
          "failed to fetch Playwright {} browsers.json: {e}",
          release.version
        ))
      })?
      .json()
      .await
      .map_err(|e| {
        backend_err(format!(
          "failed to parse Playwright {} browsers.json: {e}",
          release.version
        ))
      })
  }

  /// Install the Playwright `WebKit` binary.
  ///
  /// Reads the `WebKit` revision from the `browsers.json` of the latest
  /// released Playwright, downloads the matching archive from the
  /// Playwright CDN, and extracts it to
  /// `~/.cache/ferridriver/webkit/webkit-<rev>/`.
  ///
  /// The launcher search order picks this up automatically. Override
  /// with the `FERRIDRIVER_WEBKIT` environment variable when needed.
  ///
  /// # Errors
  ///
  /// Returns an error if Playwright's `browsers.json` is unreachable,
  /// the host platform has no published `WebKit` build, the download fails
  /// after all retries, or extraction fails.
  pub async fn install_webkit<F>(&self, progress: F) -> Result<String>
  where
    F: Fn(InstallProgress),
  {
    progress(InstallProgress::Resolving);

    let browsers = self.released_browsers_json().await?;

    let platform = webkit_platform_for_host()?;
    let archive = webkit_archive(&platform)
      .ok_or_else(|| FerriError::unsupported(format!("no Playwright WebKit build for {platform}")))?;
    let revision = browser_revision(browsers, "webkit", &platform)?;

    let install_dir = self.cache_dir.join("webkit").join(format!("webkit-{revision}"));
    self
      .install_playwright_build(
        PlaywrightBuild {
          name: "webkit",
          label: "WebKit",
          executable: install_dir.join(WEBKIT_EXECUTABLE),
          install_dir,
          revision,
          archive,
          marker: BuildMarker::Archive,
        },
        &progress,
      )
      .await
  }

  /// Install Playwright's ffmpeg, the build `playwright install ffmpeg`
  /// fetches: the one video recording encodes with. It decodes the
  /// screencast's JPEG frames and writes VP8 into `WebM`, and nothing else.
  ///
  /// # Errors
  ///
  /// Returns an error if Playwright's `browsers.json` is unreachable, the
  /// host platform has no published ffmpeg build, the download fails after
  /// all retries, or extraction fails.
  pub async fn install_ffmpeg<F>(&self, progress: F) -> Result<String>
  where
    F: Fn(InstallProgress),
  {
    progress(InstallProgress::Resolving);
    let browsers = self.released_browsers_json().await?;
    let platform = webkit_platform_for_host()?;
    let (archive, executable) = ffmpeg_build(&platform)
      .ok_or_else(|| FerriError::unsupported(format!("no Playwright ffmpeg build for {platform}")))?;
    let revision = browser_revision(browsers, "ffmpeg", &platform)?;
    let install_dir = self.cache_dir.join(format!("ffmpeg-{revision}"));
    self
      .install_playwright_build(
        PlaywrightBuild {
          name: "ffmpeg",
          label: "ffmpeg",
          executable: install_dir.join(executable),
          install_dir,
          revision,
          archive,
          marker: BuildMarker::Archive,
        },
        &progress,
      )
      .await
  }

  /// Download one build from the Playwright CDN, falling over between its
  /// mirrors, and unpack it into its own directory.
  async fn install_playwright_build<F>(&self, build: PlaywrightBuild, progress: &F) -> Result<String>
  where
    F: Fn(InstallProgress),
  {
    let PlaywrightBuild {
      name,
      label,
      revision,
      archive,
      install_dir,
      executable,
      marker,
    } = build;
    let marker_file = install_dir.join(marker.file_name());

    if marker.says_installed(&marker_file, archive).await && executable.exists() {
      let path = executable.to_string_lossy().to_string();
      progress(InstallProgress::AlreadyInstalled {
        version: revision.clone(),
        path: path.clone(),
      });
      return Ok(path);
    }

    if install_dir.exists() {
      let _ = tokio::fs::remove_dir_all(&install_dir).await;
    }

    let tmp_dir = self.cache_dir.join(".tmp");
    tokio::fs::create_dir_all(&tmp_dir).await?;
    let zip_path = tmp_dir.join(format!("{name}-{revision}.zip"));

    self
      .download_build(
        &format!("builds/{name}/{revision}/{archive}"),
        &zip_path,
        label,
        progress,
      )
      .await?;

    progress(InstallProgress::Extracting);
    tokio::fs::create_dir_all(&install_dir).await?;
    let install_dir_clone = install_dir.clone();
    let zip_path_clone = zip_path.clone();
    tokio::task::spawn_blocking(move || extract_zip(&zip_path_clone, &install_dir_clone))
      .await
      .map_err(|e| backend_err(format!("extract task failed: {e}")))??;
    let _ = tokio::fs::remove_file(&zip_path).await;

    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      if executable.exists() {
        let _ = std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755));
      }
    }

    if !executable.exists() {
      return Err(backend_err(format!(
        "extraction completed but {label} not found at: {}",
        executable.display()
      )));
    }

    marker.write(&marker_file, archive).await?;

    let path = executable.to_string_lossy().to_string();
    progress(InstallProgress::Complete {
      version: revision,
      path: path.clone(),
    });
    Ok(path)
  }

  /// Install `WebKit` build `revision` into Playwright's browser cache at
  /// `cache`, laid out as `playwright install` lays it out:
  /// `<cache>/webkit-<revision>`, marked complete by
  /// `INSTALLATION_COMPLETE`. Playwright, and ferridriver's own launcher
  /// through the Playwright cache, both find a build installed this way.
  ///
  /// For a project pinned to one `playwright-core`, whose `browsers.json`
  /// names the revision. [`Self::install_webkit`] instead installs the
  /// latest release's build into ferridriver's own cache. A build already
  /// marked complete is kept, and a directory left without the marker by an
  /// interrupted install is replaced.
  ///
  /// # Errors
  ///
  /// Returns an error if `revision` is not a build number, the host
  /// platform has no published `WebKit` build, the download fails from
  /// every host after all retries, or extraction fails.
  pub async fn install_webkit_revision<F>(&self, revision: &str, cache: &Path, progress: F) -> Result<String>
  where
    F: Fn(InstallProgress),
  {
    if revision.is_empty() || !revision.bytes().all(|byte| byte.is_ascii_digit()) {
      return Err(FerriError::invalid_argument(
        "revision",
        format!("'{revision}' is not a WebKit build number"),
      ));
    }
    let platform = webkit_platform_for_host()?;
    let archive = webkit_archive(&platform)
      .ok_or_else(|| FerriError::unsupported(format!("no Playwright WebKit build for {platform}")))?;

    if let Some(installed) = installed_build(cache, CachedBrowser::WebKit, revision) {
      let path = installed.to_string_lossy().to_string();
      progress(InstallProgress::AlreadyInstalled {
        version: revision.to_string(),
        path: path.clone(),
      });
      return Ok(path);
    }

    let install_dir = playwright_cache::build_dir(cache, CachedBrowser::WebKit, revision);
    self
      .install_playwright_build(
        PlaywrightBuild {
          name: "webkit",
          label: "WebKit",
          executable: install_dir.join(WEBKIT_EXECUTABLE),
          install_dir,
          revision: revision.to_string(),
          archive,
          marker: BuildMarker::PlaywrightComplete,
        },
        &progress,
      )
      .await
  }

  /// Download `build_path` from the first download host that serves it,
  /// retrying each host before falling over to the next.
  async fn download_build<F>(&self, build_path: &str, dest: &Path, what: &str, progress: &F) -> Result<()>
  where
    F: Fn(InstallProgress),
  {
    let mut last_error = format!("no download host is configured for {build_path}");
    for host in &self.download_hosts {
      let url = format!("{}/{build_path}", host.trim_end_matches('/'));
      for attempt in 1..=DOWNLOAD_RETRIES {
        progress(InstallProgress::Downloading {
          bytes_downloaded: 0,
          total_bytes: None,
        });
        match self.download_file(&url, dest, progress).await {
          Ok(()) => return Ok(()),
          Err(e) => {
            last_error = format!("{host} attempt {attempt}/{DOWNLOAD_RETRIES}: {e}");
            // The next attempt writes the file afresh; a partial one it
            // cannot remove is overwritten there.
            if let Err(error) = tokio::fs::remove_file(dest).await
              && error.kind() != std::io::ErrorKind::NotFound
            {
              tracing::debug!(target: "ferridriver::install", "removing {}: {error}", dest.display());
            }
          },
        }
      }
    }
    Err(backend_err(format!(
      "{what} download failed after exhausting mirrors: {last_error}"
    )))
  }

  /// Download Playwright's Android driver: the two APKs `_android` installs
  /// on a device for its selector actions and its `input`. The same
  /// archive, from the same mirrors, that `playwright install android`
  /// fetches.
  ///
  /// # Errors
  ///
  /// Returns an error if the registry or every mirror is unreachable, or
  /// the archive does not hold both APKs.
  pub async fn install_android_driver<F>(&self, progress: F) -> Result<String>
  where
    F: Fn(InstallProgress),
  {
    progress(InstallProgress::Resolving);
    let browsers = self.released_browsers_json().await?;
    let revision = browser_revision(browsers, "android", "")?;

    let install_dir = self.cache_dir.join(format!("android-driver-{revision}"));
    let marker_file = install_dir.join(".downloaded");
    let complete = |dir: &Path| ANDROID_DRIVER_APKS.iter().all(|apk| dir.join(apk).exists());
    if marker_file.exists() && complete(&install_dir) {
      let path = install_dir.to_string_lossy().to_string();
      progress(InstallProgress::AlreadyInstalled {
        version: revision,
        path: path.clone(),
      });
      return Ok(path);
    }
    if install_dir.exists() {
      let _ = tokio::fs::remove_dir_all(&install_dir).await;
    }

    let tmp_dir = self.cache_dir.join(".tmp");
    tokio::fs::create_dir_all(&tmp_dir).await?;
    let zip_path = tmp_dir.join(format!("android-driver-{revision}.zip"));
    self
      .download_build(
        &format!("builds/android/{revision}/android.zip"),
        &zip_path,
        "Android driver",
        &progress,
      )
      .await?;

    progress(InstallProgress::Extracting);
    tokio::fs::create_dir_all(&install_dir).await?;
    let install_dir_clone = install_dir.clone();
    let zip_path_clone = zip_path.clone();
    tokio::task::spawn_blocking(move || extract_zip(&zip_path_clone, &install_dir_clone))
      .await
      .map_err(|e| backend_err(format!("extract task failed: {e}")))??;
    let _ = tokio::fs::remove_file(&zip_path).await;
    if !complete(&install_dir) {
      return Err(backend_err(format!(
        "extraction completed but the driver APKs are not in {}",
        install_dir.display()
      )));
    }
    let _ = tokio::fs::write(&marker_file, revision.as_bytes()).await;

    let path = install_dir.to_string_lossy().to_string();
    progress(InstallProgress::Complete {
      version: revision,
      path: path.clone(),
    });
    Ok(path)
  }

  /// The directory of an installed Android driver, the newest when there
  /// are several, or `None` when [`Self::install_android_driver`] has not
  /// run.
  #[must_use]
  pub fn find_installed_android_driver(&self) -> Option<PathBuf> {
    let entries = std::fs::read_dir(&self.cache_dir).ok()?;
    let mut candidates: Vec<_> = entries
      .filter_map(std::result::Result::ok)
      .filter(|e| e.file_name().to_string_lossy().starts_with("android-driver-"))
      .collect();
    candidates.sort_by_key(|entry| std::cmp::Reverse(version_order(&entry.file_name().to_string_lossy())));
    candidates
      .into_iter()
      .map(|entry| entry.path())
      .find(|dir| dir.join(".downloaded").exists() && ANDROID_DRIVER_APKS.iter().all(|apk| dir.join(apk).exists()))
  }

  /// Return the path to an installed `WebKit`, or `None` if not installed.
  #[must_use]
  pub fn find_installed_webkit(&self) -> Option<String> {
    let root = CacheFamily::WebKit.root(&self.cache_dir);
    let entries = std::fs::read_dir(root).ok()?;
    let mut candidates: Vec<_> = entries
      .filter_map(std::result::Result::ok)
      .filter(|e| e.file_name().to_string_lossy().starts_with("webkit-"))
      .collect();
    candidates.sort_by_key(|entry| std::cmp::Reverse(version_order(&entry.file_name().to_string_lossy())));

    candidates.into_iter().find_map(|entry| {
      let exe = entry.path().join(WEBKIT_EXECUTABLE);
      (entry.path().join(".downloaded").exists() && exe.is_file()).then(|| exe.to_string_lossy().to_string())
    })
  }

  /// Return the path to an installed ffmpeg, or `None` if not installed.
  #[must_use]
  pub fn find_installed_ffmpeg(&self) -> Option<String> {
    let platform = webkit_platform_for_host().ok()?;
    let (_, executable) = ffmpeg_build(&platform)?;
    let entries = std::fs::read_dir(&self.cache_dir).ok()?;
    let mut candidates: Vec<_> = entries
      .filter_map(std::result::Result::ok)
      .filter(|e| {
        CacheFamily::Ffmpeg
          .version_of(&e.file_name().to_string_lossy())
          .is_some()
      })
      .collect();
    candidates.sort_by_key(|entry| std::cmp::Reverse(version_order(&entry.file_name().to_string_lossy())));
    candidates.into_iter().find_map(|entry| {
      let exe = entry.path().join(executable);
      (entry.path().join(".downloaded").exists() && exe.is_file()).then(|| exe.to_string_lossy().to_string())
    })
  }

  /// The newest Lightpanda `ferridriver install lightpanda` put in the cache.
  #[must_use]
  pub fn find_installed_lightpanda(&self) -> Option<String> {
    let entries = std::fs::read_dir(&self.cache_dir).ok()?;
    let mut candidates: Vec<_> = entries
      .filter_map(std::result::Result::ok)
      .filter(|e| e.file_name().to_string_lossy().starts_with("lightpanda-"))
      .collect();
    candidates.sort_by_key(|entry| std::cmp::Reverse(version_order(&entry.file_name().to_string_lossy())));
    candidates.into_iter().find_map(|entry| {
      let exe = entry.path().join("lightpanda");
      (entry.path().join(".downloaded").exists() && exe.is_file()).then(|| exe.to_string_lossy().into_owned())
    })
  }

  /// The newest Ladybird `ferridriver install ladybird` put in the cache.
  #[must_use]
  pub fn find_installed_ladybird(&self) -> Option<String> {
    let entries = std::fs::read_dir(&self.cache_dir).ok()?;
    let mut candidates: Vec<_> = entries
      .filter_map(std::result::Result::ok)
      .filter(|e| e.file_name().to_string_lossy().starts_with("ladybird-"))
      .collect();
    candidates.sort_by_key(|entry| std::cmp::Reverse(version_order(&entry.file_name().to_string_lossy())));
    candidates.into_iter().find_map(|entry| {
      let exe = ladybird::executable_path(&entry.path());
      (entry.path().join(".downloaded").exists() && exe.exists()).then(|| exe.to_string_lossy().into_owned())
    })
  }

  /// Return the path to an installed Firefox, or `None` if not installed.
  #[must_use]
  pub fn find_installed_firefox(&self) -> Option<String> {
    let entries = std::fs::read_dir(&self.cache_dir).ok()?;
    let mut candidates: Vec<_> = entries
      .filter_map(std::result::Result::ok)
      .filter(|e| e.file_name().to_string_lossy().starts_with("firefox-"))
      .collect();
    candidates.sort_by_key(|entry| std::cmp::Reverse(version_order(&entry.file_name().to_string_lossy())));

    for entry in candidates {
      let marker = entry.path().join(".downloaded");
      let exe = firefox_executable_path(&entry.path());
      if marker.exists() && exe.exists() {
        return Some(exe.to_string_lossy().to_string());
      }
    }
    None
  }
}
