//! Firefox and Chromium/Playwright archive extraction helpers.

use std::path::{Path, PathBuf};

use ferridriver_error::{FerriError, Result};

use crate::backend_err;

// ---------------------------------------------------------------------------
// Firefox platform helpers
// ---------------------------------------------------------------------------

/// Return (`platform_dir`, `archive_filename`, `archive_extension`) for Firefox downloads.
/// Matches Puppeteer's `archive()` and `platformName()` for stable channel.
pub(crate) fn firefox_archive_info(version: &str) -> Result<(String, String, String)> {
  let os = std::env::consts::OS;
  let arch = std::env::consts::ARCH;

  let major: u32 = version.split('.').next().and_then(|s| s.parse().ok()).unwrap_or(0);
  let tar_ext = if major >= 135 { "xz" } else { "bz2" };

  match (os, arch) {
    ("linux", "x86_64") => Ok((
      "linux-x86_64".into(),
      format!("firefox-{version}.tar.{tar_ext}"),
      format!(".tar.{tar_ext}"),
    )),
    ("linux", "aarch64") => Ok((
      "linux-aarch64".into(),
      format!("firefox-{version}.tar.{tar_ext}"),
      format!(".tar.{tar_ext}"),
    )),
    ("macos", "x86_64" | "aarch64") => Ok(("mac".into(), format!("Firefox {version}.dmg"), ".dmg".into())),
    ("windows", "x86_64") => Ok(("win64".into(), format!("Firefox Setup {version}.exe"), ".exe".into())),
    ("windows", "aarch64") => Ok((
      "win64-aarch64".into(),
      format!("Firefox Setup {version}.exe"),
      ".exe".into(),
    )),
    ("windows", "x86") => Ok(("win32".into(), format!("Firefox Setup {version}.exe"), ".exe".into())),
    _ => Err(FerriError::unsupported(format!(
      "unsupported platform for Firefox: {os}-{arch}"
    ))),
  }
}

/// Return the path to the Firefox executable inside the install directory.
/// Matches Puppeteer's `relativeExecutablePath()` for stable channel.
pub(crate) fn firefox_executable_path(install_dir: &Path) -> PathBuf {
  let os = std::env::consts::OS;
  match os {
    "macos" => install_dir.join("Firefox.app/Contents/MacOS/firefox"),
    "windows" => install_dir.join("core").join("firefox.exe"),
    // Linux: tar extracts to firefox/ subdirectory
    _ => install_dir.join("firefox/firefox"),
  }
}

/// Extract a Firefox archive (tar.bz2, tar.xz, or DMG).
pub(crate) fn extract_firefox_archive(archive_path: &Path, dest: &Path) -> Result<()> {
  let path_str = archive_path.to_string_lossy();

  if path_str.ends_with(".tar.bz2") {
    extract_tar_bz2(archive_path, dest)
  } else if path_str.ends_with(".tar.xz") {
    extract_tar_xz(archive_path, dest)
  } else if path_str.ends_with(".dmg") {
    extract_dmg(archive_path, dest)
  } else if path_str.ends_with(".exe") {
    #[cfg(windows)]
    {
      extract_installer(archive_path, dest)
    }
    #[cfg(not(windows))]
    {
      Err(FerriError::unsupported(
        "Firefox's Windows installer is unpacked only on Windows",
      ))
    }
  } else {
    Err(FerriError::unsupported(format!(
      "unsupported Firefox archive format: {path_str}"
    )))
  }
}

/// Unpack Firefox's Windows installer without running it. The installer is
/// a 7-Zip self-extractor (a stub, then a 7z archive whose `core/` is the
/// browser), so its `core/` is read straight out of the archive. Running it
/// with `/ExtractDir`, as Puppeteer does, waits forever where there is no
/// desktop (a service, an SSH session, a CI runner).
#[cfg(windows)]
fn extract_installer(installer: &Path, dest: &Path) -> Result<()> {
  const SIGNATURE: &[u8] = b"7z\xBC\xAF\x27\x1C";
  let bytes = std::fs::read(installer)?;
  // The stub may hold the signature bytes too, so each match is tried.
  let mut archive = bytes
    .windows(SIGNATURE.len())
    .enumerate()
    .filter(|(_, window)| *window == SIGNATURE)
    .find_map(|(start, _)| {
      sevenz_rust2::ArchiveReader::new(std::io::Cursor::new(&bytes[start..]), sevenz_rust2::Password::empty()).ok()
    })
    .ok_or_else(|| backend_err(format!("{} holds no 7z archive", installer.display())))?;
  archive
    .for_each_entries(|entry, reader| {
      let name = entry.name().replace('\\', "/");
      if !name.starts_with("core/") {
        return Ok(true);
      }
      let relative = Path::new(&name);
      if relative
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
      {
        return Err(sevenz_rust2::Error::Other(format!("{name} leaves the archive").into()));
      }
      let target = dest.join(relative);
      if entry.is_directory() {
        std::fs::create_dir_all(&target)?;
      } else {
        if let Some(parent) = target.parent() {
          std::fs::create_dir_all(parent)?;
        }
        std::io::copy(reader, &mut std::fs::File::create(&target)?)?;
      }
      Ok(true)
    })
    .map_err(|error| backend_err(format!("unpacking {}: {error}", installer.display())))
}

/// Extract a .tar.bz2 archive.
fn extract_tar_bz2(archive_path: &Path, dest: &Path) -> Result<()> {
  let file = std::fs::File::open(archive_path)?;
  let decoder = bzip2::read::BzDecoder::new(file);
  let mut archive = tar::Archive::new(decoder);
  archive.unpack(dest)?;
  Ok(())
}

/// Extract a .tar.xz archive.
pub(crate) fn extract_tar_xz(archive_path: &Path, dest: &Path) -> Result<()> {
  let file = std::fs::File::open(archive_path)?;
  let decoder = liblzma::read::XzDecoder::new(file);
  let mut archive = tar::Archive::new(decoder);
  archive.unpack(dest)?;
  Ok(())
}

/// Extract a .dmg (macOS disk image) by mounting, copying, and unmounting.
/// Same approach as Puppeteer: hdiutil attach -> cp -R -> hdiutil detach.
pub(crate) fn extract_dmg(dmg_path: &Path, dest: &Path) -> Result<()> {
  // Mount the DMG
  let output = std::process::Command::new("hdiutil")
    .args(["attach", "-nobrowse", "-noautoopen"])
    .arg(dmg_path)
    .output()?;

  if !output.status.success() {
    return Err(backend_err(format!(
      "hdiutil attach failed: {}",
      String::from_utf8_lossy(&output.stderr)
    )));
  }

  let stdout = String::from_utf8_lossy(&output.stdout);
  let mount_path = stdout
    .lines()
    .filter_map(|line| {
      let parts: Vec<&str> = line.split('\t').collect();
      parts.last().map(|p| p.trim().to_string())
    })
    .find(|p| p.starts_with("/Volumes/"))
    .ok_or_else(|| backend_err("could not find volume mount path in hdiutil output"))?;

  // Find the .app directory inside the mount
  let result = (|| -> Result<()> {
    let entries = std::fs::read_dir(&mount_path)?;
    let app_name = entries
      .filter_map(std::result::Result::ok)
      .find(|e| e.file_name().to_string_lossy().ends_with(".app"))
      .ok_or_else(|| backend_err("no .app found in mounted DMG"))?;

    let source = std::path::Path::new(&mount_path).join(app_name.file_name());

    // cp -R to destination
    let status = std::process::Command::new("cp")
      .args(["-R"])
      .arg(&source)
      .arg(dest)
      .status()?;

    if !status.success() {
      return Err(backend_err("cp -R failed to copy Firefox.app"));
    }
    Ok(())
  })();

  // Always detach the DMG
  let _ = std::process::Command::new("hdiutil")
    .args(["detach", &mount_path, "-quiet"])
    .status();

  result
}

// ---------------------------------------------------------------------------
// Zip extraction (for Chrome)
// ---------------------------------------------------------------------------

/// What Playwright's Android driver archive holds: the driver and the
/// instrumentation target it runs in.
pub const ANDROID_DRIVER_APKS: [&str; 2] = ["android-driver.apk", "android-driver-target.apk"];

/// Whether a symlink at `link` (relative to the archive root) pointing at
/// `target` resolves inside the archive.
#[cfg(any(unix, test))]
pub(crate) fn link_stays_inside(link: &Path, target: &Path) -> bool {
  use std::path::Component;
  if target.is_absolute() {
    return false;
  }
  let mut depth: usize = link.parent().map_or(0, |parent| parent.components().count());
  for component in target.components() {
    match component {
      Component::Normal(_) => depth += 1,
      Component::CurDir => {},
      Component::ParentDir => match depth.checked_sub(1) {
        Some(up) => depth = up,
        None => return false,
      },
      Component::RootDir | Component::Prefix(_) => return false,
    }
  }
  true
}

pub(crate) fn extract_zip(zip_path: &Path, dest: &Path) -> Result<()> {
  let file = std::fs::File::open(zip_path)?;
  let mut archive = zip::ZipArchive::new(file).map_err(|e| backend_err(format!("failed to read zip archive: {e}")))?;

  for i in 0..archive.len() {
    let mut entry = archive
      .by_index(i)
      .map_err(|e| backend_err(format!("failed to read zip entry {i}: {e}")))?;

    // An entry named `/etc/x` or `../x` would land outside `dest`.
    let relative = entry
      .enclosed_name()
      .ok_or_else(|| backend_err(format!("zip entry {i} has a path outside the archive")))?;
    let out_path = dest.join(&relative);

    if entry.is_dir() {
      std::fs::create_dir_all(&out_path)?;
      continue;
    }

    if let Some(parent) = out_path.parent() {
      std::fs::create_dir_all(parent)?;
    }

    // Symlink entries: file content IS the link target. Required for
    // Playwright WebKit bundles (libwebkitgtk-6.0.so.4 → ...so.4.17.1).
    // ZIP encodes symlinks as regular file entries with the unix mode
    // bits S_IFLNK (0o120000) set.
    #[cfg(unix)]
    {
      use std::io::Read;
      use std::os::unix::fs::PermissionsExt;
      if let Some(mode) = entry.unix_mode() {
        if mode & 0o170_000 == 0o120_000 {
          let mut target = String::new();
          entry.read_to_string(&mut target)?;
          if !link_stays_inside(&relative, Path::new(&target)) {
            return Err(backend_err(format!(
              "zip entry {} links outside the archive: {target}",
              relative.display()
            )));
          }
          // Replace any pre-existing entry — zip ordering is not stable.
          let _ = std::fs::remove_file(&out_path);
          std::os::unix::fs::symlink(&target, &out_path)?;
          continue;
        }
        let mut out_file = std::fs::File::create(&out_path)?;
        std::io::copy(&mut entry, &mut out_file)?;
        // Permission bits only: setuid, setgid and sticky from an archive
        // are never wanted on a browser build.
        let _ = std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(mode & 0o777));
        continue;
      }
    }

    let mut out_file = std::fs::File::create(&out_path)?;
    std::io::copy(&mut entry, &mut out_file)?;
  }

  Ok(())
}
