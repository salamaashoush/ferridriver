//! Lightpanda's release builds: one static executable per platform on its
//! GitHub releases, each asset carrying the SHA-256 GitHub computed.

use super::{BrowserInstaller, InstallProgress, backend_err};
use ferridriver_error::{FerriError, Result};

const LIGHTPANDA_LATEST_URL: &str = "https://api.github.com/repos/lightpanda-io/browser/releases/latest";

#[derive(serde::Deserialize)]
struct GithubRelease {
  tag_name: String,
  assets: Vec<GithubAsset>,
}

#[derive(serde::Deserialize)]
struct GithubAsset {
  name: String,
  browser_download_url: String,
  #[serde(default)]
  digest: Option<String>,
}

impl BrowserInstaller {
  /// Install the newest Lightpanda release into the cache.
  ///
  /// # Errors
  ///
  /// Returns an error when the release cannot be resolved, the host has
  /// no published build (Windows runs it through WSL), or the download
  /// fails or does not match the SHA-256 GitHub lists for it.
  pub async fn install_lightpanda<F>(&self, progress: F) -> Result<String>
  where
    F: Fn(InstallProgress),
  {
    progress(InstallProgress::Resolving);
    let os = match std::env::consts::OS {
      "macos" => "macos",
      "linux" => "linux",
      other => {
        return Err(FerriError::unsupported(format!(
          "Lightpanda publishes no {other} build; run it in WSL on Windows"
        )));
      },
    };
    let asset_name = format!("lightpanda-{}-{os}", std::env::consts::ARCH);
    let mut request = self
      .client()?
      .get(LIGHTPANDA_LATEST_URL)
      .header(reqwest::header::ACCEPT, "application/vnd.github+json");
    // Unauthenticated API calls share a small hourly limit per address,
    // which a CI fleet behind one NAT runs through.
    if let Ok(token) = std::env::var("GITHUB_TOKEN")
      && !token.is_empty()
    {
      request = request.bearer_auth(token);
    }
    let release: GithubRelease = request
      .send()
      .await
      .and_then(reqwest::Response::error_for_status)
      .map_err(|e| backend_err(format!("failed to resolve the latest Lightpanda release: {e}")))?
      .json()
      .await
      .map_err(|e| backend_err(format!("failed to parse the latest Lightpanda release: {e}")))?;
    let version = release.tag_name;
    let asset = release
      .assets
      .into_iter()
      .find(|asset| asset.name == asset_name)
      .ok_or_else(|| FerriError::unsupported(format!("Lightpanda {version} has no {asset_name} build")))?;
    let sha256 = asset
      .digest
      .as_deref()
      .and_then(|digest| digest.strip_prefix("sha256:"))
      .ok_or_else(|| backend_err(format!("GitHub lists no SHA-256 for {asset_name}")))?
      .to_owned();

    let install_dir = self.cache_dir.join(format!("lightpanda-{version}"));
    let marker = install_dir.join(".downloaded");
    let executable = install_dir.join("lightpanda");
    if marker.exists() && executable.is_file() {
      let path = executable.to_string_lossy().into_owned();
      progress(InstallProgress::AlreadyInstalled {
        version,
        path: path.clone(),
      });
      return Ok(path);
    }
    if install_dir.exists() {
      tokio::fs::remove_dir_all(&install_dir).await?;
    }
    tokio::fs::create_dir_all(&install_dir).await?;
    self
      .download_verified(
        &asset.browser_download_url,
        &executable,
        &sha256,
        "Lightpanda",
        &progress,
      )
      .await?;
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
    }
    tokio::fs::write(&marker, version.as_bytes()).await?;
    let path = executable.to_string_lossy().into_owned();
    progress(InstallProgress::Complete {
      version,
      path: path.clone(),
    });
    Ok(path)
  }
}
