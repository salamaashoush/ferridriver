//! What build am I?
//!
//! One place every surface reads: `--version`, the upgrade check, and the
//! `User-Agent` the release API sees. They agree by construction, so a canary
//! tester and the logs they send back cannot disagree about which binary ran.
//! The values are baked in by `build.rs`.

/// Version reported to users. Canary builds carry a `-canary.<sha>` suffix.
pub const VERSION: &str = env!("FERRIDRIVER_VERSION");

/// The release line this build was cut from, without any canary suffix.
/// Release tags are keyed on this, never on [`VERSION`].
pub const RELEASE_VERSION: &str = env!("FERRIDRIVER_RELEASE_VERSION");

/// Release channel: `stable` or `canary`. Decides which releases
/// `ferridriver upgrade` considers by default.
pub const CHANNEL: &str = env!("FERRIDRIVER_CHANNEL");

/// Short commit this build was cut from. Empty where there was no git.
pub const GIT_SHA: &str = env!("FERRIDRIVER_GIT_SHA");

/// `-dirty` when the working tree had uncommitted tracked changes.
pub const DIRTY: &str = env!("FERRIDRIVER_BUILD_DIRTY");

/// Target triple this build was compiled for.
pub const TARGET: &str = env!("FERRIDRIVER_BUILD_TARGET");

/// Whether this is a canary build.
#[must_use]
pub fn is_canary() -> bool {
  CHANNEL == "canary"
}

/// The line `--version` prints: the version, then everything needed to
/// identify the exact artifact.
///
/// Assembled by `build.rs`, because clap's `long_version` is an attribute
/// that takes a `&'static str` and cannot call a function.
pub const LONG_VERSION: &str = env!("FERRIDRIVER_LONG_VERSION");

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_version_always_starts_with_the_release_line() {
    assert!(VERSION.starts_with(RELEASE_VERSION), "{VERSION} vs {RELEASE_VERSION}");
  }

  #[test]
  fn the_channel_and_the_suffix_agree() {
    assert_eq!(is_canary(), CHANNEL == "canary");
    assert_eq!(is_canary(), VERSION.contains("-canary"));
  }

  #[test]
  fn the_long_version_names_the_channel() {
    assert!(LONG_VERSION.contains(CHANNEL), "{LONG_VERSION}");
    assert!(LONG_VERSION.starts_with(VERSION), "{LONG_VERSION}");
  }
}
