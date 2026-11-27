//! Installer unit tests.

use super::*;

#[cfg(test)]
mod removal_tests {
  use super::{BrowserInstaller, CacheFamily, version_order};

  /// A cache holding `dirs`, each a directory with a file in it.
  fn cache(dirs: &[&str]) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("cache");
    for dir in dirs {
      std::fs::create_dir_all(root.path().join(dir)).expect("version");
      std::fs::write(root.path().join(dir).join(".downloaded"), "").expect("marker");
    }
    root
  }

  fn left(root: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root)
      .expect("cache")
      .filter_map(Result::ok)
      .map(|entry| entry.file_name().to_string_lossy().into_owned())
      .collect();
    names.sort();
    names
  }

  #[test]
  fn versions_sort_by_their_numbers() {
    let mut versions = vec!["155.0.8059.39", "99.0.1", "158.0b5", "158.0", "157.0.1", "2374"];
    versions.sort_by_key(|v| version_order(v));
    assert_eq!(
      versions,
      ["99.0.1", "155.0.8059.39", "157.0.1", "158.0", "158.0b5", "2374"]
    );
  }

  #[tokio::test]
  async fn only_older_versions_of_the_same_family_go() {
    let root = cache(&[
      "chromium-99.0.4844.51",
      "chromium-154.0.1.2",
      "chromium-155.0.8059.39",
      "chromium-156.0.1.1",
      "chromium-headless-shell-154.0.1.2",
      "chromium-headless-shell-155.0.8059.39",
      "chromedriver-154.0.1.2",
      "firefox-157.0.1",
      "firefox-158.0b5",
      "chromium-",
    ]);
    std::fs::write(root.path().join("chromium-1.0"), "a file, not a version").expect("file");
    let installer = BrowserInstaller::with_cache_dir(root.path().to_path_buf());

    let removed = installer
      .remove_older_versions(CacheFamily::Chromium, "155.0.8059.39")
      .await
      .expect("remove");
    let versions: Vec<&str> = removed.iter().map(|gone| gone.version.as_str()).collect();
    assert_eq!(versions, ["99.0.4844.51", "154.0.1.2"]);
    assert_eq!(removed[1].path, root.path().join("chromium-154.0.1.2"));
    assert_eq!(
      left(root.path()),
      [
        "chromedriver-154.0.1.2",
        "chromium-",
        "chromium-1.0",
        "chromium-155.0.8059.39",
        "chromium-156.0.1.1",
        "chromium-headless-shell-154.0.1.2",
        "chromium-headless-shell-155.0.8059.39",
        "firefox-157.0.1",
        "firefox-158.0b5",
      ]
    );

    // What a launch would pick over the installed one stays.
    let none = installer
      .remove_older_versions(CacheFamily::Firefox, "157.0.1")
      .await
      .expect("remove");
    assert_eq!(none, Vec::new());
    let shell = installer
      .remove_older_versions(CacheFamily::ChromiumHeadlessShell, "155.0.8059.39")
      .await
      .expect("remove");
    assert_eq!(shell.len(), 1);
    assert!(!root.path().join("chromium-headless-shell-154.0.1.2").exists());
  }

  #[tokio::test]
  async fn webkit_revisions_live_in_their_own_directory() {
    let root = cache(&["webkit/webkit-2300", "webkit/webkit-2374", "webkit-2000"]);
    let installer = BrowserInstaller::with_cache_dir(root.path().to_path_buf());
    let removed = installer
      .remove_older_versions(CacheFamily::WebKit, "2374")
      .await
      .expect("remove");
    assert_eq!(removed.len(), 1);
    assert_eq!(left(&root.path().join("webkit")), ["webkit-2374"]);
    // A directory of the same name outside the family's root is not its.
    assert!(root.path().join("webkit-2000").exists());

    let empty = BrowserInstaller::with_cache_dir(root.path().join("no-such-cache"));
    assert_eq!(
      empty
        .remove_older_versions(CacheFamily::AxeCore, "4.12.1")
        .await
        .expect("remove"),
      Vec::new()
    );
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn a_symlink_is_neither_followed_nor_removed() {
    let root = cache(&["firefox-157.0.1"]);
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    std::fs::write(elsewhere.path().join("keep.txt"), "sashoush").expect("file");
    std::os::unix::fs::symlink(elsewhere.path(), root.path().join("firefox-100.0")).expect("link");
    let installer = BrowserInstaller::with_cache_dir(root.path().to_path_buf());
    let removed = installer
      .remove_older_versions(CacheFamily::Firefox, "157.0.1")
      .await
      .expect("remove");
    assert_eq!(removed, Vec::new());
    assert!(elsewhere.path().join("keep.txt").exists());
    assert!(root.path().join("firefox-100.0").exists());
  }
}

#[cfg(test)]
mod extraction_tests {
  use std::path::Path;

  use super::{extract_tar_xz, extract_zip, link_stays_inside};

  fn write_zip(path: &Path, build: impl FnOnce(&mut zip::ZipWriter<std::fs::File>)) {
    let mut writer = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    build(&mut writer);
    writer.finish().unwrap();
  }

  fn stored() -> zip::write::SimpleFileOptions {
    zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)
  }

  #[test]
  fn symlinks_must_resolve_inside_the_archive() {
    assert!(link_stays_inside(Path::new("lib/libfoo.so"), Path::new("libfoo.so.4")));
    assert!(link_stays_inside(
      Path::new("lib/x/libfoo.so"),
      Path::new("../libfoo.so.4")
    ));
    assert!(!link_stays_inside(
      Path::new("lib/libfoo.so"),
      Path::new("../../etc/passwd")
    ));
    assert!(!link_stays_inside(Path::new("libfoo.so"), Path::new("/etc/passwd")));
  }

  #[test]
  fn an_xz_compressed_tarball_unpacks_its_files() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("firefox.tar.xz");
    let encoder = liblzma::write::XzEncoder::new(std::fs::File::create(&archive).unwrap(), 6);
    let mut tarball = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_size(7);
    header.set_mode(0o644);
    header.set_cksum();
    tarball
      .append_data(&mut header, "firefox/application.ini", &b"Version"[..])
      .unwrap();
    tarball.into_inner().unwrap().finish().unwrap();
    let dest = root.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();
    extract_tar_xz(&archive, &dest).unwrap();
    assert_eq!(std::fs::read(dest.join("firefox/application.ini")).unwrap(), b"Version");
  }

  #[test]
  fn extraction_refuses_entries_outside_the_destination() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("evil.zip");
    write_zip(&archive, |zip| {
      zip.start_file("../escaped", stored()).unwrap();
      std::io::Write::write_all(zip, b"x").unwrap();
    });
    let dest = root.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();
    assert!(extract_zip(&archive, &dest).is_err());
    assert!(!root.path().join("escaped").exists());
  }

  #[cfg(unix)]
  #[test]
  fn extraction_keeps_inner_links_and_drops_privileged_bits() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("build.zip");
    write_zip(&archive, |zip| {
      zip
        .start_file("lib/libfoo.so.4", stored().unix_permissions(0o4755))
        .unwrap();
      std::io::Write::write_all(zip, b"elf").unwrap();
      zip.add_symlink("lib/libfoo.so", "libfoo.so.4", stored()).unwrap();
    });
    let dest = root.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();
    extract_zip(&archive, &dest).unwrap();
    let mode = std::fs::metadata(dest.join("lib/libfoo.so.4"))
      .unwrap()
      .permissions()
      .mode();
    assert_eq!(mode & 0o7777, 0o755, "setuid survived: {mode:o}");
    assert_eq!(std::fs::read(dest.join("lib/libfoo.so")).unwrap(), b"elf");

    let escaping = root.path().join("escaping.zip");
    write_zip(&escaping, |zip| {
      zip.add_symlink("lib/libfoo.so", "../../outside", stored()).unwrap();
    });
    let dest = root.path().join("out2");
    std::fs::create_dir_all(&dest).unwrap();
    assert!(extract_zip(&escaping, &dest).is_err());
  }
}

#[cfg(test)]
mod webkit_platform_tests {
  use super::{
    NpmRelease, PwBrowsersJson, browser_revision, mac_host_platform, released_browsers_json_url, webkit_archive,
  };

  #[test]
  fn darwin_releases_map_to_mac_platforms() {
    for (release, arch, platform) in [
      ("17.7.0", "x86_64", "mac10.13"),
      ("18.0.0", "x86_64", "mac10.14"),
      ("19.6.0", "x86_64", "mac10.15"),
      ("20.6.0", "aarch64", "mac11-arm64"),
      ("23.6.0", "aarch64", "mac14-arm64"),
      ("24.1.0", "x86_64", "mac15"),
      ("25.0.0", "aarch64", "mac26-arm64"),
      ("27.2.0", "aarch64", "mac26-arm64"),
    ] {
      assert_eq!(mac_host_platform(release, arch), platform, "{release} {arch}");
    }
  }

  #[test]
  fn each_mac_release_takes_the_build_made_for_it() {
    assert_eq!(webkit_archive("mac14-arm64"), Some("webkit-mac-14-arm64.zip"));
    assert_eq!(webkit_archive("mac15"), Some("webkit-mac-15.zip"));
    assert_eq!(webkit_archive("mac26-arm64"), Some("webkit-mac-26-arm64.zip"));
    assert_eq!(webkit_archive("mac13-arm64"), None);
  }

  #[test]
  fn browser_builds_come_from_the_latest_release_tag_not_main() {
    let release: NpmRelease = serde_json::from_str(r#"{"name":"playwright-core","version":"1.65.0"}"#).unwrap();
    assert_eq!(
      released_browsers_json_url(&release).unwrap(),
      "https://raw.githubusercontent.com/microsoft/playwright/v1.65.0/packages/playwright-core/browsers.json"
    );
    for unreleased in ["1.66.0-alpha-2026-11-06", "1.65.0-beta-1793921730000", "main", ""] {
      let release = NpmRelease {
        version: unreleased.to_string(),
      };
      assert!(released_browsers_json_url(&release).is_err(), "{unreleased}");
    }
  }

  #[test]
  fn a_revision_is_the_platform_override_or_the_default() {
    let json = r#"{"browsers":[{"name":"webkit","revision":"2388","revisionOverrides":{"mac14-arm64":"2251"}},{"name":"android","revision":"1001"}]}"#;
    let parse = || serde_json::from_str::<PwBrowsersJson>(json).unwrap();
    assert_eq!(browser_revision(parse(), "webkit", "mac14-arm64").unwrap(), "2251");
    assert_eq!(browser_revision(parse(), "webkit", "mac26-arm64").unwrap(), "2388");
    assert_eq!(browser_revision(parse(), "android", "").unwrap(), "1001");
    assert!(browser_revision(parse(), "chromium", "mac26-arm64").is_err());
  }

  #[test]
  fn revision_overrides_are_read_per_platform() {
    let json = r#"{"browsers":[{"name":"webkit","revision":"2387","revisionOverrides":{"mac14-arm64":"2251"}}]}"#;
    let mut browsers: PwBrowsersJson = serde_json::from_str(json).unwrap();
    let mut webkit = browsers.browsers.remove(0);
    assert_eq!(webkit.revision_overrides.remove("mac14-arm64").as_deref(), Some("2251"));
    assert_eq!(webkit.revision, "2387");
  }
}

#[cfg(test)]
mod client_tests {
  use std::path::PathBuf;

  use super::BrowserInstaller;

  #[test]
  fn an_installer_builds_its_client_only_when_a_download_needs_it() {
    let installer = BrowserInstaller::with_cache_dir(PathBuf::from("/cache"));
    assert!(installer.client.get().is_none());
    let first: *const reqwest::Client = installer.client().expect("client");
    let second: *const reqwest::Client = installer.client().expect("client");
    assert!(std::ptr::eq(first, second));
  }
}

#[cfg(test)]
mod webkit_revision_tests {
  use std::io::Write as _;
  use std::path::{Path, PathBuf};
  use std::sync::{Arc, Mutex};

  use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

  use super::playwright_cache::{CachedBrowser, INSTALLATION_COMPLETE, newest_build_in};
  use super::{
    BrowserInstaller, DOWNLOAD_RETRIES, InstallProgress, WEBKIT_EXECUTABLE, webkit_archive, webkit_platform_for_host,
  };

  /// A host that serves `builds` by path and records every path asked for.
  struct Host {
    address: String,
    asked: Arc<Mutex<Vec<String>>>,
  }

  impl Host {
    async fn serving(builds: Vec<(String, Vec<u8>)>) -> Self {
      let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listener");
      let address = format!("http://{}", listener.local_addr().expect("address"));
      let asked = Arc::new(Mutex::new(Vec::new()));
      let log = Arc::clone(&asked);
      tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
          let mut request = Vec::new();
          let mut chunk = [0_u8; 1024];
          while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            match socket.read(&mut chunk).await {
              Ok(0) | Err(_) => break,
              Ok(read) => request.extend_from_slice(&chunk[..read]),
            }
          }
          let line = String::from_utf8_lossy(&request)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string();
          let path = line.split(' ').nth(1).unwrap_or_default().to_string();
          log.lock().expect("log").push(path.clone());
          let response = match builds.iter().find(|(served, _)| *served == path) {
            Some((_, body)) => [
              format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
              )
              .into_bytes(),
              body.clone(),
            ]
            .concat(),
            None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
          };
          let _ = socket.write_all(&response).await;
          let _ = socket.shutdown().await;
        }
      });
      Self { address, asked }
    }

    fn asked(&self) -> Vec<String> {
      self.asked.lock().expect("log").clone()
    }
  }

  fn archive_path(revision: &str) -> String {
    let platform = webkit_platform_for_host().expect("platform");
    let archive = webkit_archive(&platform).expect("archive");
    format!("/builds/webkit/{revision}/{archive}")
  }

  fn webkit_zip() -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().unix_permissions(0o644);
    zip.start_file(WEBKIT_EXECUTABLE, options).expect("entry");
    zip.write_all(b"#!/bin/sh\n").expect("contents");
    zip.add_directory("minibrowser-wpe", options).expect("directory");
    zip.finish().expect("archive").into_inner()
  }

  fn installer(root: &Path, hosts: &[&Host]) -> BrowserInstaller {
    BrowserInstaller::with_cache_dir(root.join("ferridriver"))
      .with_download_hosts(hosts.iter().map(|host| host.address.clone()).collect())
  }

  fn playwright_cache(root: &Path) -> PathBuf {
    root.join("ms-playwright")
  }

  #[tokio::test]
  async fn a_pinned_revision_lands_in_playwright_s_cache_marked_complete() {
    let host = Host::serving(vec![(archive_path("2370"), webkit_zip())]).await;
    let root = tempfile::tempdir().expect("root");
    let cache = playwright_cache(root.path());
    let installed = installer(root.path(), &[&host])
      .install_webkit_revision("2370", &cache, |_| {})
      .await
      .expect("installed");
    let build = cache.join("webkit-2370");
    assert_eq!(PathBuf::from(&installed), build.join(WEBKIT_EXECUTABLE));
    assert!(build.join(INSTALLATION_COMPLETE).is_file());
    assert!(build.join("minibrowser-wpe").is_dir());
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      let mode = std::fs::metadata(&installed).expect("executable").permissions().mode();
      assert_eq!(mode & 0o777, 0o755);
    }
    assert_eq!(host.asked(), [archive_path("2370")]);
    assert_eq!(
      newest_build_in(&cache, CachedBrowser::WebKit),
      Some(build.join(WEBKIT_EXECUTABLE))
    );
  }

  #[tokio::test]
  async fn a_build_already_marked_complete_is_kept_without_a_download() {
    let host = Host::serving(vec![(archive_path("2370"), webkit_zip())]).await;
    let root = tempfile::tempdir().expect("root");
    let cache = playwright_cache(root.path());
    let installer = installer(root.path(), &[&host]);
    installer
      .install_webkit_revision("2370", &cache, |_| {})
      .await
      .expect("first install");
    let seen = Mutex::new(Vec::new());
    let kept = installer
      .install_webkit_revision("2370", &cache, |event| seen.lock().expect("events").push(event))
      .await
      .expect("second install");
    assert_eq!(PathBuf::from(kept), cache.join("webkit-2370").join(WEBKIT_EXECUTABLE));
    assert_eq!(host.asked().len(), 1);
    let seen = seen.into_inner().expect("events");
    assert!(
      matches!(seen.as_slice(), [InstallProgress::AlreadyInstalled { version, .. }] if version == "2370"),
      "{seen:?}"
    );
  }

  #[tokio::test]
  async fn an_install_interrupted_before_its_marker_is_replaced() {
    let host = Host::serving(vec![(archive_path("2370"), webkit_zip())]).await;
    let root = tempfile::tempdir().expect("root");
    let cache = playwright_cache(root.path());
    let build = cache.join("webkit-2370");
    std::fs::create_dir_all(&build).expect("build");
    std::fs::write(build.join(WEBKIT_EXECUTABLE), "#!/bin/sh\n").expect("executable");
    std::fs::write(build.join("half-written.so"), "").expect("leftover");
    let seen = Mutex::new(Vec::new());
    installer(root.path(), &[&host])
      .install_webkit_revision("2370", &cache, |event| seen.lock().expect("events").push(event))
      .await
      .expect("installed");
    assert!(!build.join("half-written.so").exists());
    assert!(build.join(INSTALLATION_COMPLETE).is_file());
    assert_eq!(host.asked().len(), 1);
    assert!(matches!(
      seen.into_inner().expect("events").last(),
      Some(InstallProgress::Complete { version, .. }) if version == "2370"
    ));
  }

  #[tokio::test]
  async fn a_revision_that_is_not_a_build_number_is_refused_before_any_download() {
    let host = Host::serving(vec![(archive_path("2370"), webkit_zip())]).await;
    let root = tempfile::tempdir().expect("root");
    let cache = playwright_cache(root.path());
    for revision in ["", "23a0", "../2370", "2370/..", "-1"] {
      let refused = installer(root.path(), &[&host])
        .install_webkit_revision(revision, &cache, |_| {})
        .await
        .expect_err("not a build number");
      assert!(
        refused.to_string().contains("is not a WebKit build number"),
        "{refused}"
      );
    }
    assert_eq!(host.asked(), Vec::<String>::new());
    assert!(!cache.exists());
  }

  #[tokio::test]
  async fn a_host_without_the_build_hands_over_to_the_next() {
    let empty = Host::serving(Vec::new()).await;
    let serving = Host::serving(vec![(archive_path("2370"), webkit_zip())]).await;
    let root = tempfile::tempdir().expect("root");
    let cache = playwright_cache(root.path());
    installer(root.path(), &[&empty, &serving])
      .install_webkit_revision("2370", &cache, |_| {})
      .await
      .expect("installed from the second host");
    assert_eq!(empty.asked().len(), DOWNLOAD_RETRIES as usize);
    assert_eq!(serving.asked(), [archive_path("2370")]);
  }

  #[tokio::test]
  async fn no_host_with_the_build_fails_and_leaves_no_build_behind() {
    let empty = Host::serving(Vec::new()).await;
    let root = tempfile::tempdir().expect("root");
    let cache = playwright_cache(root.path());
    let failed = installer(root.path(), &[&empty])
      .install_webkit_revision("2370", &cache, |_| {})
      .await
      .expect_err("no host has the build");
    assert!(
      failed
        .to_string()
        .contains("WebKit download failed after exhausting mirrors"),
      "{failed}"
    );
    assert!(failed.to_string().contains("404"), "{failed}");
    assert!(!cache.join("webkit-2370").exists());
    assert_eq!(newest_build_in(&cache, CachedBrowser::WebKit), None);
  }

  #[tokio::test]
  async fn without_a_download_host_nothing_is_fetched() {
    let root = tempfile::tempdir().expect("root");
    let cache = playwright_cache(root.path());
    let failed = BrowserInstaller::with_cache_dir(root.path().join("ferridriver"))
      .with_download_hosts(Vec::new())
      .install_webkit_revision("2370", &cache, |_| {})
      .await
      .expect_err("no host");
    assert!(
      failed.to_string().contains("no download host is configured"),
      "{failed}"
    );
  }

  #[test]
  fn playwright_s_cdn_is_the_default_download_host() {
    let installer = BrowserInstaller::with_cache_dir(PathBuf::from("/cache"));
    assert_eq!(
      installer.download_hosts,
      super::PW_CDN_MIRRORS
        .iter()
        .map(|host| (*host).to_string())
        .collect::<Vec<_>>()
    );
  }
}
