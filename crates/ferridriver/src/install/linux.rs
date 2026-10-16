//! What a Linux host is, in Playwright's terms, and what follows from it:
//! the system packages each browser needs and the `WebKit` build to fetch.
//!
//! The apt tables are Playwright's own, copied from
//! `packages/playwright-core/src/server/registry/nativeDeps.ts`, and the
//! distribution detection follows its `hostPlatform.ts`. Playwright
//! publishes one table per release because package names change between
//! them (`libasound2` became `libasound2t64`, `libicu70` is `libicu72` on
//! Debian 12), so a list from one release fails `apt-get` on another. Its
//! arm64 tables are the x64 ones, so the architecture does not pick a
//! table. Arch is not something Playwright supports; its lists are ours.

use super::DependencyGroup;
use crate::error::{FerriError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PackageManager {
  Apt,
  Pacman,
}

/// Playwright's name for this host, such as `ubuntu24.04-arm64`, from the
/// contents of `/etc/os-release` and the CPU architecture.
pub(super) fn host_platform(os_release: &str, arch: &str) -> String {
  let suffix = if arch == "aarch64" { "-arm64" } else { "-x64" };
  let mut id = String::new();
  let mut version = String::new();
  for line in os_release.lines() {
    if let Some(value) = line.strip_prefix("ID=") {
      id = value.trim_matches('"').to_lowercase();
    } else if let Some(value) = line.strip_prefix("VERSION_ID=") {
      version = value.trim_matches('"').to_string();
    }
  }
  let major = version.split('.').next().and_then(|s| s.parse::<u32>().ok());
  let distro = match id.as_str() {
    "ubuntu" | "pop" | "neon" | "tuxedo" => match major {
      Some(..20) => "ubuntu18.04".to_string(),
      Some(20..22) => "ubuntu20.04".to_string(),
      Some(22..24) => "ubuntu22.04".to_string(),
      Some(24..26) | None => "ubuntu24.04".to_string(),
      Some(26..28) => "ubuntu26.04".to_string(),
      Some(_) => format!("ubuntu{version}"),
    },
    "linuxmint" => match major {
      Some(..=20) => "ubuntu20.04".to_string(),
      Some(21) => "ubuntu22.04".to_string(),
      _ => "ubuntu24.04".to_string(),
    },
    "debian" | "raspbian" => match version.as_str() {
      "11" => "debian11".to_string(),
      "12" => "debian12".to_string(),
      // Testing and unstable carry no VERSION_ID; Playwright treats
      // them as the newest release.
      "13" | "" => "debian13".to_string(),
      _ => "ubuntu24.04".to_string(),
    },
    "arch" | "manjaro" | "endeavouros" | "garuda" | "artix" | "cachyos" => "arch".to_string(),
    _ => "ubuntu24.04".to_string(),
  };
  format!("{distro}{suffix}")
}

/// The Playwright `WebKit` archive built for `platform`, from the table in
/// `packages/playwright-core/src/server/registry/index.ts`. Arch has no
/// build of its own and takes Ubuntu 24.04's, as Playwright does for any
/// distribution it does not recognise.
pub(super) fn webkit_archive(platform: &str) -> Option<&'static str> {
  Some(match platform {
    "ubuntu22.04-x64" => "webkit-ubuntu-22.04.zip",
    "ubuntu22.04-arm64" => "webkit-ubuntu-22.04-arm64.zip",
    "ubuntu24.04-x64" | "arch-x64" => "webkit-ubuntu-24.04.zip",
    "ubuntu24.04-arm64" | "arch-arm64" => "webkit-ubuntu-24.04-arm64.zip",
    "ubuntu26.04-x64" => "webkit-ubuntu-26.04.zip",
    "ubuntu26.04-arm64" => "webkit-ubuntu-26.04-arm64.zip",
    "debian12-x64" => "webkit-debian-12.zip",
    "debian12-arm64" => "webkit-debian-12-arm64.zip",
    "debian13-x64" => "webkit-debian-13.zip",
    "debian13-arm64" => "webkit-debian-13-arm64.zip",
    _ => return None,
  })
}

/// The package manager and the deduplicated packages that `groups` need
/// on `platform`.
pub(super) fn packages(platform: &str, groups: &[DependencyGroup]) -> Result<(PackageManager, Vec<&'static str>)> {
  let distro = platform
    .strip_suffix("-arm64")
    .or_else(|| platform.strip_suffix("-x64"))
    .unwrap_or(platform);
  let (manager, table) = match distro {
    "ubuntu22.04" => (PackageManager::Apt, &UBUNTU_22_04),
    "ubuntu24.04" => (PackageManager::Apt, &UBUNTU_24_04),
    "ubuntu26.04" => (PackageManager::Apt, &UBUNTU_26_04),
    "debian12" => (PackageManager::Apt, &DEBIAN_12),
    "debian13" => (PackageManager::Apt, &DEBIAN_13),
    "arch" => (PackageManager::Pacman, &ARCH),
    _ => {
      return Err(FerriError::unsupported(format!(
        "no system dependency list for {platform}: lists exist for Ubuntu 22.04, 24.04 and 26.04, Debian 12 and 13, \
         and Arch Linux"
      )));
    },
  };
  let mut packages: Vec<&'static str> = groups.iter().flat_map(|group| table.group(*group)).copied().collect();
  packages.sort_unstable();
  packages.dedup();
  Ok((manager, packages))
}

struct AptTable {
  tools: &'static [&'static str],
  chromium: &'static [&'static str],
  firefox: &'static [&'static str],
  webkit: &'static [&'static str],
}

impl AptTable {
  fn group(&self, group: DependencyGroup) -> &'static [&'static str] {
    match group {
      DependencyGroup::Tools => self.tools,
      DependencyGroup::Chromium => self.chromium,
      DependencyGroup::Firefox => self.firefox,
      DependencyGroup::WebKit => self.webkit,
    }
  }
}

/// pacman names for the same libraries the apt tables name.
const ARCH: AptTable = AptTable {
  tools: &[
    "xorg-server-xvfb",
    "noto-fonts-emoji",
    "fontconfig",
    "freetype2",
    "xorg-fonts-cyrillic",
    "xorg-fonts-type1",
    "ttf-liberation",
    "otf-ipafont",
    "wqy-zenhei",
    "gnu-free-fonts",
  ],
  chromium: &[
    "alsa-lib",
    "at-spi2-core",
    "cairo",
    "libcups",
    "dbus",
    "libdrm",
    "mesa",
    "glib2",
    "nspr",
    "nss",
    "pango",
    "libxcb",
    "libxcomposite",
    "libxdamage",
    "libxfixes",
    "libxrandr",
    "libxkbcommon",
    "libx11",
    "libxext",
    "wayland",
  ],
  firefox: &[
    "alsa-lib",
    "at-spi2-core",
    "ffmpeg",
    "cairo",
    "dbus",
    "fontconfig",
    "freetype2",
    "gdk-pixbuf2",
    "glib2",
    "gtk3",
    "pango",
    "libx11",
    "libxcb",
    "libxcomposite",
    "libxcursor",
    "libxdamage",
    "libxext",
    "libxfixes",
    "libxi",
    "libxrandr",
    "libxrender",
  ],
  webkit: &[
    "woff2",
    "libvpx",
    "opus",
    "libwebp",
    "harfbuzz-icu",
    "enchant",
    "hyphen",
    "libsecret",
    "libnotify",
    "mesa",
    "libxslt",
    "libevent",
    "libgudev",
    "libsoup3",
    "libavif",
    "gst-plugins-base",
    "gst-plugins-base-libs",
    "gst-plugins-good",
    "gst-plugins-bad",
    "gst-libav",
    "gtk4",
  ],
};

const UBUNTU_22_04: AptTable = AptTable {
  tools: &[
    "xvfb",
    "fonts-noto-color-emoji",
    "fonts-unifont",
    "libfontconfig1",
    "libfreetype6",
    "xfonts-cyrillic",
    "xfonts-scalable",
    "fonts-liberation",
    "fonts-ipafont-gothic",
    "fonts-wqy-zenhei",
    "fonts-tlwg-loma-otf",
    "fonts-freefont-ttf",
  ],
  chromium: &[
    "libasound2",
    "libatk-bridge2.0-0",
    "libatk1.0-0",
    "libatspi2.0-0",
    "libcairo2",
    "libcups2",
    "libdbus-1-3",
    "libdrm2",
    "libgbm1",
    "libglib2.0-0",
    "libnspr4",
    "libnss3",
    "libpango-1.0-0",
    "libwayland-client0",
    "libx11-6",
    "libxcb1",
    "libxcomposite1",
    "libxdamage1",
    "libxext6",
    "libxfixes3",
    "libxkbcommon0",
    "libxrandr2",
  ],
  firefox: &[
    "ffmpeg",
    "libasound2",
    "libatk1.0-0",
    "libcairo-gobject2",
    "libcairo2",
    "libdbus-1-3",
    "libdbus-glib-1-2",
    "libfontconfig1",
    "libfreetype6",
    "libgdk-pixbuf-2.0-0",
    "libglib2.0-0",
    "libgtk-3-0",
    "libpango-1.0-0",
    "libpangocairo-1.0-0",
    "libx11-6",
    "libx11-xcb1",
    "libxcb-shm0",
    "libxcb1",
    "libxcomposite1",
    "libxcursor1",
    "libxdamage1",
    "libxext6",
    "libxfixes3",
    "libxi6",
    "libxrandr2",
    "libxrender1",
    "libxtst6",
  ],
  webkit: &[
    "libsoup-3.0-0",
    "libenchant-2-2",
    "gstreamer1.0-libav",
    "gstreamer1.0-plugins-bad",
    "gstreamer1.0-plugins-base",
    "gstreamer1.0-plugins-good",
    "libicu70",
    "libatk-bridge2.0-0",
    "libatk1.0-0",
    "libcairo2",
    "libdbus-1-3",
    "libdrm2",
    "libegl1",
    "libepoxy0",
    "libevdev2",
    "libffi7",
    "libfontconfig1",
    "libfreetype6",
    "libgbm1",
    "libgdk-pixbuf-2.0-0",
    "libgles2",
    "libglib2.0-0",
    "libglx0",
    "libgstreamer-gl1.0-0",
    "libgstreamer-plugins-base1.0-0",
    "libgstreamer1.0-0",
    "libgtk-4-1",
    "libgudev-1.0-0",
    "libharfbuzz-icu0",
    "libharfbuzz0b",
    "libhyphen0",
    "libjpeg-turbo8",
    "liblcms2-2",
    "libmanette-0.2-0",
    "libnotify4",
    "libopengl0",
    "libopenjp2-7",
    "libopus0",
    "libpango-1.0-0",
    "libpng16-16",
    "libproxy1v5",
    "libsecret-1-0",
    "libwayland-client0",
    "libwayland-egl1",
    "libwayland-server0",
    "libwebpdemux2",
    "libwoff1",
    "libx11-6",
    "libxcomposite1",
    "libxdamage1",
    "libxkbcommon0",
    "libxml2",
    "libxslt1.1",
    "libx264-163",
    "libatomic1",
    "libevent-2.1-7",
    "libavif13",
  ],
};

const UBUNTU_24_04: AptTable = AptTable {
  tools: &[
    "xvfb",
    "fonts-noto-color-emoji",
    "fonts-unifont",
    "libfontconfig1",
    "libfreetype6",
    "xfonts-cyrillic",
    "xfonts-scalable",
    "fonts-liberation",
    "fonts-ipafont-gothic",
    "fonts-wqy-zenhei",
    "fonts-tlwg-loma-otf",
    "fonts-freefont-ttf",
  ],
  chromium: &[
    "libasound2t64",
    "libatk-bridge2.0-0t64",
    "libatk1.0-0t64",
    "libatspi2.0-0t64",
    "libcairo2",
    "libcups2t64",
    "libdbus-1-3",
    "libdrm2",
    "libgbm1",
    "libglib2.0-0t64",
    "libnspr4",
    "libnss3",
    "libpango-1.0-0",
    "libx11-6",
    "libxcb1",
    "libxcomposite1",
    "libxdamage1",
    "libxext6",
    "libxfixes3",
    "libxkbcommon0",
    "libxrandr2",
  ],
  firefox: &[
    "libasound2t64",
    "libatk1.0-0t64",
    "libavcodec60",
    "libcairo-gobject2",
    "libcairo2",
    "libdbus-1-3",
    "libfontconfig1",
    "libfreetype6",
    "libgdk-pixbuf-2.0-0",
    "libglib2.0-0t64",
    "libgtk-3-0t64",
    "libpango-1.0-0",
    "libpangocairo-1.0-0",
    "libx11-6",
    "libx11-xcb1",
    "libxcb-shm0",
    "libxcb1",
    "libxcomposite1",
    "libxcursor1",
    "libxdamage1",
    "libxext6",
    "libxfixes3",
    "libxi6",
    "libxrandr2",
    "libxrender1",
  ],
  webkit: &[
    "gstreamer1.0-libav",
    "gstreamer1.0-plugins-bad",
    "gstreamer1.0-plugins-base",
    "gstreamer1.0-plugins-good",
    "libicu74",
    "libatomic1",
    "libatk-bridge2.0-0t64",
    "libatk1.0-0t64",
    "libcairo-gobject2",
    "libcairo2",
    "libdbus-1-3",
    "libdrm2",
    "libenchant-2-2",
    "libepoxy0",
    "libevent-2.1-7t64",
    "libflite1",
    "libfontconfig1",
    "libfreetype6",
    "libgbm1",
    "libgdk-pixbuf-2.0-0",
    "libgles2",
    "libglib2.0-0t64",
    "libgstreamer-gl1.0-0",
    "libgstreamer-plugins-bad1.0-0",
    "libgstreamer-plugins-base1.0-0",
    "libgstreamer1.0-0",
    "libgtk-4-1",
    "libharfbuzz-icu0",
    "libharfbuzz0b",
    "libhyphen0",
    "libicu74",
    "libjpeg-turbo8",
    "liblcms2-2",
    "libmanette-0.2-0",
    "libopus0",
    "libpango-1.0-0",
    "libpangocairo-1.0-0",
    "libpng16-16t64",
    "libsecret-1-0",
    "libvpx9",
    "libwayland-client0",
    "libwayland-egl1",
    "libwayland-server0",
    "libwebp7",
    "libwebpdemux2",
    "libwoff1",
    "libx11-6",
    "libxkbcommon0",
    "libxml2",
    "libxslt1.1",
    "libx264-164",
    "libavif16",
  ],
};

const UBUNTU_26_04: AptTable = AptTable {
  tools: &[
    "xvfb",
    "fonts-noto-color-emoji",
    "fonts-unifont",
    "libfontconfig1",
    "libfreetype6",
    "xfonts-cyrillic",
    "xfonts-scalable",
    "fonts-liberation",
    "fonts-ipafont-gothic",
    "fonts-wqy-zenhei",
    "fonts-tlwg-loma-otf",
    "fonts-freefont-ttf",
  ],
  chromium: &[
    "libasound2t64",
    "libatk-bridge2.0-0t64",
    "libatk1.0-0t64",
    "libatspi2.0-0t64",
    "libcairo2",
    "libcups2t64",
    "libdbus-1-3",
    "libdrm2",
    "libgbm1",
    "libglib2.0-0t64",
    "libnspr4",
    "libnss3",
    "libpango-1.0-0",
    "libx11-6",
    "libxcb1",
    "libxcomposite1",
    "libxdamage1",
    "libxext6",
    "libxfixes3",
    "libxkbcommon0",
    "libxrandr2",
  ],
  firefox: &[
    "libasound2t64",
    "libatk1.0-0t64",
    "libavcodec62",
    "libcairo-gobject2",
    "libcairo2",
    "libdbus-1-3",
    "libfontconfig1",
    "libfreetype6",
    "libgdk-pixbuf-2.0-0",
    "libglib2.0-0t64",
    "libgtk-3-0t64",
    "libpango-1.0-0",
    "libpangocairo-1.0-0",
    "libx11-6",
    "libx11-xcb1",
    "libxcb-shm0",
    "libxcb1",
    "libxcomposite1",
    "libxcursor1",
    "libxdamage1",
    "libxext6",
    "libxfixes3",
    "libxi6",
    "libxrandr2",
    "libxrender1",
  ],
  webkit: &[
    "gstreamer1.0-libav",
    "gstreamer1.0-plugins-bad",
    "gstreamer1.0-plugins-base",
    "gstreamer1.0-plugins-good",
    "libicu78",
    "libatomic1",
    "libatk-bridge2.0-0t64",
    "libatk1.0-0t64",
    "libcairo-gobject2",
    "libcairo2",
    "libdbus-1-3",
    "libdrm2",
    "libenchant-2-2",
    "libepoxy0",
    "libevent-2.1-7t64",
    "libflite1",
    "libfontconfig1",
    "libfreetype6",
    "libgbm1",
    "libgdk-pixbuf-2.0-0",
    "libgles2",
    "libglib2.0-0t64",
    "libgstreamer-gl1.0-0",
    "libgstreamer-plugins-bad1.0-0",
    "libgstreamer-plugins-base1.0-0",
    "libgstreamer1.0-0",
    "libgtk-4-1",
    "libharfbuzz-icu0",
    "libharfbuzz0b",
    "libhyphen0",
    "libjpeg-turbo8",
    "liblcms2-2",
    "libmanette-0.2-0",
    "libopus0",
    "libpango-1.0-0",
    "libpangocairo-1.0-0",
    "libpng16-16t64",
    "libsecret-1-0",
    "libvpx12",
    "libwayland-client0",
    "libwayland-egl1",
    "libwayland-server0",
    "libwebp7",
    "libwebpdemux2",
    "libwoff1",
    "libx11-6",
    "libxkbcommon0",
    "libxml2-16",
    "libxslt1.1",
    "libx264-165",
    "libavif16",
  ],
};

const DEBIAN_12: AptTable = AptTable {
  tools: &[
    "xvfb",
    "fonts-noto-color-emoji",
    "fonts-unifont",
    "libfontconfig1",
    "libfreetype6",
    "xfonts-scalable",
    "fonts-liberation",
    "fonts-ipafont-gothic",
    "fonts-wqy-zenhei",
    "fonts-tlwg-loma-otf",
    "fonts-freefont-ttf",
  ],
  chromium: &[
    "libasound2",
    "libatk-bridge2.0-0",
    "libatk1.0-0",
    "libatspi2.0-0",
    "libcairo2",
    "libcups2",
    "libdbus-1-3",
    "libdrm2",
    "libgbm1",
    "libglib2.0-0",
    "libnspr4",
    "libnss3",
    "libpango-1.0-0",
    "libx11-6",
    "libxcb1",
    "libxcomposite1",
    "libxdamage1",
    "libxext6",
    "libxfixes3",
    "libxkbcommon0",
    "libxrandr2",
  ],
  firefox: &[
    "libasound2",
    "libatk1.0-0",
    "libavcodec59",
    "libcairo-gobject2",
    "libcairo2",
    "libdbus-1-3",
    "libdbus-glib-1-2",
    "libfontconfig1",
    "libfreetype6",
    "libgdk-pixbuf-2.0-0",
    "libglib2.0-0",
    "libgtk-3-0",
    "libharfbuzz0b",
    "libpango-1.0-0",
    "libpangocairo-1.0-0",
    "libx11-6",
    "libx11-xcb1",
    "libxcb-shm0",
    "libxcb1",
    "libxcomposite1",
    "libxcursor1",
    "libxdamage1",
    "libxext6",
    "libxfixes3",
    "libxi6",
    "libxrandr2",
    "libxrender1",
    "libxtst6",
  ],
  webkit: &[
    "libsoup-3.0-0",
    "gstreamer1.0-libav",
    "gstreamer1.0-plugins-bad",
    "gstreamer1.0-plugins-base",
    "gstreamer1.0-plugins-good",
    "libatk-bridge2.0-0",
    "libatk1.0-0",
    "libcairo2",
    "libdbus-1-3",
    "libdrm2",
    "libegl1",
    "libenchant-2-2",
    "libepoxy0",
    "libevdev2",
    "libfontconfig1",
    "libfreetype6",
    "libgbm1",
    "libgdk-pixbuf-2.0-0",
    "libgles2",
    "libglib2.0-0",
    "libglx0",
    "libgstreamer-gl1.0-0",
    "libgstreamer-plugins-base1.0-0",
    "libgstreamer1.0-0",
    "libgtk-4-1",
    "libgudev-1.0-0",
    "libharfbuzz-icu0",
    "libharfbuzz0b",
    "libhyphen0",
    "libicu72",
    "libjpeg62-turbo",
    "liblcms2-2",
    "libmanette-0.2-0",
    "libnotify4",
    "libopengl0",
    "libopenjp2-7",
    "libopus0",
    "libpango-1.0-0",
    "libpng16-16",
    "libproxy1v5",
    "libsecret-1-0",
    "libwayland-client0",
    "libwayland-egl1",
    "libwayland-server0",
    "libwebp7",
    "libwebpdemux2",
    "libwoff1",
    "libx11-6",
    "libxcomposite1",
    "libxdamage1",
    "libxkbcommon0",
    "libxml2",
    "libxslt1.1",
    "libatomic1",
    "libevent-2.1-7",
    "libavif15",
  ],
};

const DEBIAN_13: AptTable = AptTable {
  tools: &[
    "xvfb",
    "fonts-noto-color-emoji",
    "fonts-unifont",
    "libfontconfig1",
    "libfreetype6",
    "xfonts-scalable",
    "fonts-liberation",
    "fonts-ipafont-gothic",
    "fonts-wqy-zenhei",
    "fonts-tlwg-loma-otf",
    "fonts-freefont-ttf",
  ],
  chromium: &[
    "libasound2t64",
    "libatk-bridge2.0-0t64",
    "libatk1.0-0t64",
    "libatspi2.0-0t64",
    "libcairo2",
    "libcups2t64",
    "libdbus-1-3",
    "libdrm2",
    "libgbm1",
    "libglib2.0-0t64",
    "libnspr4",
    "libnss3",
    "libpango-1.0-0",
    "libx11-6",
    "libxcb1",
    "libxcomposite1",
    "libxdamage1",
    "libxext6",
    "libxfixes3",
    "libxkbcommon0",
    "libxrandr2",
  ],
  firefox: &[
    "libasound2",
    "libatk1.0-0t64",
    "libavcodec61",
    "libcairo-gobject2",
    "libcairo2",
    "libdbus-1-3",
    "libdbus-glib-1-2",
    "libfontconfig1",
    "libfreetype6",
    "libgdk-pixbuf-2.0-0",
    "libglib2.0-0t64",
    "libgtk-3-0t64",
    "libharfbuzz0b",
    "libpango-1.0-0",
    "libpangocairo-1.0-0",
    "libx11-6",
    "libx11-xcb1",
    "libxcb-shm0",
    "libxcb1",
    "libxcomposite1",
    "libxcursor1",
    "libxdamage1",
    "libxext6",
    "libxfixes3",
    "libxi6",
    "libxrandr2",
    "libxrender1",
    "libxtst6",
  ],
  webkit: &[
    "libsoup-3.0-0",
    "gstreamer1.0-libav",
    "gstreamer1.0-plugins-bad",
    "gstreamer1.0-plugins-base",
    "gstreamer1.0-plugins-good",
    "libatk-bridge2.0-0t64",
    "libatk1.0-0t64",
    "libcairo2",
    "libdbus-1-3",
    "libdrm2",
    "libegl1",
    "libenchant-2-2",
    "libepoxy0",
    "libevdev2",
    "libfontconfig1",
    "libfreetype6",
    "libgbm1",
    "libgdk-pixbuf-2.0-0",
    "libgles2",
    "libglib2.0-0t64",
    "libglx0",
    "libgstreamer-gl1.0-0",
    "libgstreamer-plugins-base1.0-0",
    "libgstreamer1.0-0",
    "libgtk-4-1",
    "libgudev-1.0-0",
    "libharfbuzz-icu0",
    "libharfbuzz0b",
    "libhyphen0",
    "libicu76",
    "libjpeg62-turbo",
    "liblcms2-2",
    "libmanette-0.2-0",
    "libnotify4",
    "libopengl0",
    "libopenjp2-7",
    "libopus0",
    "libpango-1.0-0",
    "libpng16-16t64",
    "libproxy1v5",
    "libsecret-1-0",
    "libwayland-client0",
    "libwayland-egl1",
    "libwayland-server0",
    "libwebp7",
    "libwebpdemux2",
    "libwoff1",
    "libx11-6",
    "libxcomposite1",
    "libxdamage1",
    "libxkbcommon0",
    "libxml2",
    "libxslt1.1",
    "libatomic1",
    "libevent-2.1-7t64",
    "libavif16",
  ],
};
#[cfg(test)]
mod tests {
  use super::*;

  fn os_release(id: &str, version: &str) -> String {
    format!("NAME=\"x\"\nID={id}\nVERSION_ID=\"{version}\"\n")
  }

  #[test]
  fn hosts_map_to_playwright_platforms() {
    assert_eq!(
      host_platform(&os_release("ubuntu", "24.04"), "x86_64"),
      "ubuntu24.04-x64"
    );
    assert_eq!(
      host_platform(&os_release("ubuntu", "24.10"), "aarch64"),
      "ubuntu24.04-arm64"
    );
    assert_eq!(
      host_platform(&os_release("ubuntu", "26.04"), "x86_64"),
      "ubuntu26.04-x64"
    );
    assert_eq!(host_platform(&os_release("pop", "22.04"), "x86_64"), "ubuntu22.04-x64");
    assert_eq!(
      host_platform(&os_release("linuxmint", "21.3"), "x86_64"),
      "ubuntu22.04-x64"
    );
    assert_eq!(host_platform(&os_release("debian", "12"), "aarch64"), "debian12-arm64");
    assert_eq!(host_platform("ID=debian\n", "x86_64"), "debian13-x64");
    assert_eq!(host_platform("ID=arch\nBUILD_ID=rolling\n", "x86_64"), "arch-x64");
    assert_eq!(host_platform(&os_release("fedora", "41"), "x86_64"), "ubuntu24.04-x64");
  }

  #[test]
  fn only_the_requested_browsers_are_installed() {
    let (manager, chromium) =
      packages("ubuntu24.04-x64", &[DependencyGroup::Tools, DependencyGroup::Chromium]).unwrap();
    assert_eq!(manager, PackageManager::Apt);
    assert!(chromium.contains(&"libnss3"));
    assert!(chromium.contains(&"xvfb"));
    assert!(
      !chromium.contains(&"libgtk-4-1"),
      "WebKit's GTK came along: {chromium:?}"
    );
    assert!(
      !chromium.contains(&"libgtk-3-0t64"),
      "Firefox's GTK came along: {chromium:?}"
    );

    let (_, all) = packages(
      "ubuntu24.04-x64",
      &[
        DependencyGroup::Tools,
        DependencyGroup::Chromium,
        DependencyGroup::Firefox,
        DependencyGroup::WebKit,
      ],
    )
    .unwrap();
    assert!(all.contains(&"libgtk-4-1") && all.contains(&"libgtk-3-0t64"));
    assert!(all.windows(2).all(|pair| pair[0] < pair[1]), "sorted and deduplicated");
  }

  #[test]
  fn debian_gets_debian_package_names() {
    let (_, packages) = packages("debian12-arm64", &[DependencyGroup::Chromium, DependencyGroup::WebKit]).unwrap();
    for ubuntu_only in ["libicu70", "libjpeg-turbo8", "libffi7", "libx264-163"] {
      assert!(
        !packages.contains(&ubuntu_only),
        "{ubuntu_only} is not a Debian 12 package"
      );
    }
    assert!(packages.contains(&"libasound2"));
  }

  #[test]
  fn webkit_builds_follow_the_release_and_architecture() {
    assert_eq!(webkit_archive("debian12-arm64"), Some("webkit-debian-12-arm64.zip"));
    assert_eq!(webkit_archive("ubuntu26.04-x64"), Some("webkit-ubuntu-26.04.zip"));
    assert_eq!(webkit_archive("arch-x64"), Some("webkit-ubuntu-24.04.zip"));
    assert_eq!(webkit_archive("ubuntu20.04-x64"), None);
  }

  #[test]
  fn releases_without_a_list_say_so() {
    let error = packages("ubuntu20.04-x64", &[DependencyGroup::Chromium]).unwrap_err();
    assert!(error.to_string().contains("ubuntu20.04-x64"), "{error}");
    let (manager, _) = packages("arch-arm64", &[DependencyGroup::Chromium]).unwrap();
    assert_eq!(manager, PackageManager::Pacman);
  }
}
