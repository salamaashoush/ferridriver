//! Per-page media emulation state shared across page wrappers.

use std::sync::Arc;

/// The colour scheme a page is emulated with when neither its context
/// nor `page.emulateMedia` names one. A browser left to itself follows
/// the host's appearance, so a dark-mode machine would otherwise render
/// and match `prefers-color-scheme` differently from every other host.
pub(crate) const DEFAULT_COLOR_SCHEME: &str = "light";

/// A page's media emulation: what its context names, and what
/// `page.emulateMedia` has layered over that. Kept on the backend page,
/// which every `Page` wrapper of it shares, so neither a fresh wrapper
/// nor a context re-applying its options undoes a page's own override.
#[derive(Debug, Default)]
pub(crate) struct MediaEmulation {
  pub(crate) context: crate::options::EmulateMediaOptions,
  pub(crate) page: crate::options::EmulateMediaOptions,
}

impl MediaEmulation {
  /// The state the page should be in on a backend that emulates
  /// `features`, and whether anyone asked for any of it. A feature nobody
  /// names gets its default where the backend can emulate it and is left
  /// to the browser where it cannot; `null` for a feature the backend never
  /// emulates asks for what the page already has, so nothing is sent.
  pub(crate) fn effective(&self, features: MediaFeatures) -> (crate::options::EmulateMediaOptions, bool) {
    let mut media = self.page.layered_over(&self.context);
    let requested = !media.is_empty();
    for (field, feature, default) in [
      (&mut media.media, MediaFeature::Media, None),
      (
        &mut media.color_scheme,
        MediaFeature::ColorScheme,
        Some(DEFAULT_COLOR_SCHEME),
      ),
      (
        &mut media.reduced_motion,
        MediaFeature::ReducedMotion,
        Some("no-preference"),
      ),
      (&mut media.forced_colors, MediaFeature::ForcedColors, Some("none")),
      (&mut media.contrast, MediaFeature::Contrast, Some("no-preference")),
    ] {
      resolve_feature(field, features.emulates(feature), default);
    }
    (media, requested)
  }
}

fn resolve_feature(field: &mut crate::options::MediaOverride, emulated: bool, default: Option<&str>) {
  use crate::options::MediaOverride;
  match (&*field, emulated, default) {
    (MediaOverride::Unchanged, true, Some(default)) => *field = MediaOverride::Set(default.to_owned()),
    (MediaOverride::Disabled, false, _) => *field = MediaOverride::Unchanged,
    _ => {},
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaFeature {
  Media,
  ColorScheme,
  ReducedMotion,
  ForcedColors,
  Contrast,
}

/// The media features a backend emulates for one page.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MediaFeatures(pub(crate) &'static [MediaFeature]);

impl MediaFeatures {
  pub(crate) const ALL: Self = Self(&[
    MediaFeature::Media,
    MediaFeature::ColorScheme,
    MediaFeature::ReducedMotion,
    MediaFeature::ForcedColors,
    MediaFeature::Contrast,
  ]);
  pub(crate) const NONE: Self = Self(&[]);

  fn emulates(self, feature: MediaFeature) -> bool {
    self.0.contains(&feature)
  }
}

pub(crate) type SharedMediaEmulation = Arc<tokio::sync::Mutex<MediaEmulation>>;

#[cfg(test)]
mod media_emulation_tests {
  use super::{MediaEmulation, MediaFeature, MediaFeatures};
  use crate::options::{EmulateMediaOptions, MediaOverride};

  fn scheme(value: MediaOverride) -> EmulateMediaOptions {
    EmulateMediaOptions {
      color_scheme: value,
      ..Default::default()
    }
  }

  fn color_scheme(emulation: &MediaEmulation, features: MediaFeatures) -> (MediaOverride, bool) {
    let (media, requested) = emulation.effective(features);
    (media.color_scheme, requested)
  }

  #[test]
  fn nobody_naming_a_feature_gets_its_default_unasked() {
    let (media, requested) = MediaEmulation::default().effective(MediaFeatures::ALL);
    assert_eq!(media.color_scheme, MediaOverride::Set("light".into()));
    assert_eq!(media.reduced_motion, MediaOverride::Set("no-preference".into()));
    assert_eq!(media.forced_colors, MediaOverride::Set("none".into()));
    assert_eq!(media.contrast, MediaOverride::Set("no-preference".into()));
    assert_eq!(media.media, MediaOverride::Unchanged);
    assert!(!requested);
  }

  #[test]
  fn the_page_wins_over_its_context_and_null_is_kept() {
    let emulation = MediaEmulation {
      context: scheme(MediaOverride::Set("dark".into())),
      page: EmulateMediaOptions::default(),
    };
    assert_eq!(
      color_scheme(&emulation, MediaFeatures::ALL),
      (MediaOverride::Set("dark".into()), true)
    );
    let emulation = MediaEmulation {
      page: scheme(MediaOverride::Disabled),
      ..emulation
    };
    assert_eq!(
      color_scheme(&emulation, MediaFeatures::ALL),
      (MediaOverride::Disabled, true)
    );
    let emulation = MediaEmulation {
      context: scheme(MediaOverride::Disabled),
      page: EmulateMediaOptions::default(),
    };
    assert_eq!(color_scheme(&emulation, MediaFeatures::ALL).0, MediaOverride::Disabled);
  }

  #[test]
  fn another_feature_alone_still_gets_light_and_counts_as_asked() {
    let emulation = MediaEmulation {
      context: EmulateMediaOptions::default(),
      page: EmulateMediaOptions {
        reduced_motion: MediaOverride::Set("reduce".into()),
        ..Default::default()
      },
    };
    let (media, requested) = emulation.effective(MediaFeatures::ALL);
    assert_eq!(media.color_scheme, MediaOverride::Set("light".into()));
    assert_eq!(media.reduced_motion, MediaOverride::Set("reduce".into()));
    assert!(requested);
  }

  #[test]
  fn a_feature_the_backend_cannot_emulate_gets_no_default_and_null_is_a_no_op() {
    let only_scheme = MediaFeatures(&[MediaFeature::ColorScheme]);
    let (media, _) = MediaEmulation::default().effective(only_scheme);
    assert_eq!(media.color_scheme, MediaOverride::Set("light".into()));
    assert_eq!(media.reduced_motion, MediaOverride::Unchanged);
    let emulation = MediaEmulation {
      context: EmulateMediaOptions {
        reduced_motion: MediaOverride::Disabled,
        contrast: MediaOverride::Set("more".into()),
        ..Default::default()
      },
      page: EmulateMediaOptions::default(),
    };
    let (media, requested) = emulation.effective(only_scheme);
    assert_eq!(media.reduced_motion, MediaOverride::Unchanged);
    assert_eq!(media.contrast, MediaOverride::Set("more".into()));
    assert!(requested);
    assert!(MediaEmulation::default().effective(MediaFeatures::NONE).0.is_empty());
  }
}
