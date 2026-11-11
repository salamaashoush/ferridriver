//! Playwright's device descriptors — `devices['iPhone 15']`.
//!
//! The table is generated from the vendored
//! `deviceDescriptorsSource.json` by `build.rs`, so it costs nothing at
//! startup and a descriptor that stops matching this shape fails the
//! build rather than a run. See `VENDOR.md` for the pin and the re-sync
//! recipe.

/// The three engines a descriptor can name, spelled as Playwright
/// spells them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceBrowser {
  Chromium,
  Firefox,
  #[serde(rename = "webkit")]
  WebKit,
}

impl DeviceBrowser {
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::Chromium => "chromium",
      Self::Firefox => "firefox",
      Self::WebKit => "webkit",
    }
  }
}

impl std::fmt::Display for DeviceBrowser {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(self.as_str())
  }
}

/// `{ width, height }` — a descriptor's viewport or screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeviceSize {
  pub width: i64,
  pub height: i64,
}

/// One entry of Playwright's device registry.
///
/// `screen` is absent from upstream's `DeviceDescriptor` TYPE but present
/// in its DATA, and a `use: { ...devices[name] }` spread carries it — so
/// it is carried here too rather than dropped on the way through.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceDescriptor {
  pub user_agent: &'static str,
  pub viewport: DeviceSize,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub screen: Option<DeviceSize>,
  pub device_scale_factor: f64,
  pub is_mobile: bool,
  pub has_touch: bool,
  pub default_browser_type: DeviceBrowser,
}

/// The vendored table exactly as upstream ships it.
///
/// The JS `devices` object is `JSON.parse`d from this string rather than
/// rebuilt field by field, so a `use: { ...devices[name] }` spread
/// carries every key upstream's own spread carries — `screen` included —
/// and the two halves of this module cannot describe different devices.
pub const SOURCE: &str = include_str!("deviceDescriptorsSource.json");

include!(concat!(env!("OUT_DIR"), "/devices_table.rs"));

/// Every descriptor, sorted by device name.
#[must_use]
pub fn all() -> &'static [(&'static str, DeviceDescriptor)] {
  DEVICES
}

/// One descriptor by its exact name — `"iPhone 15"`, `"Desktop Safari"`.
///
/// Playwright's `devices` is a plain object, so lookup is exact and
/// case-sensitive; an unknown name is `None` rather than a fallback.
#[must_use]
pub fn get(name: &str) -> Option<&'static DeviceDescriptor> {
  DEVICES
    .binary_search_by_key(&name, |(n, _)| *n)
    .ok()
    .map(|i| &DEVICES[i].1)
}

#[cfg(test)]
mod tests {
  use super::{DeviceBrowser, all, get};

  #[test]
  fn the_table_holds_every_vendored_device() {
    assert_eq!(all().len(), 207);
  }

  #[test]
  fn names_are_sorted_so_lookup_can_binary_search() {
    let mut sorted: Vec<&str> = all().iter().map(|(n, _)| *n).collect();
    let read = sorted.clone();
    sorted.sort_unstable();
    assert_eq!(read, sorted);
  }

  #[test]
  fn a_descriptor_carries_every_field_upstream_ships() {
    let d = get("iPhone 15").expect("iPhone 15");
    assert!(d.user_agent.contains("iPhone; CPU iPhone OS"));
    assert_eq!(d.viewport.width, 393);
    assert_eq!(d.viewport.height, 659);
    assert_eq!(d.screen.expect("iPhone 15 has a screen").height, 852);
    assert!((d.device_scale_factor - 3.0).abs() < f64::EPSILON);
    assert!(d.is_mobile);
    assert!(d.has_touch);
    assert_eq!(d.default_browser_type, DeviceBrowser::WebKit);
  }

  #[test]
  fn a_desktop_descriptor_is_not_mobile_and_names_its_engine() {
    let d = get("Desktop Firefox").expect("Desktop Firefox");
    assert_eq!(d.viewport.width, 1280);
    assert_eq!(d.screen.expect("Desktop Firefox has a screen").width, 1920);
    assert!(!d.is_mobile);
    assert!(!d.has_touch);
    assert_eq!(d.default_browser_type, DeviceBrowser::Firefox);
  }

  #[test]
  fn an_optional_screen_stays_absent_rather_than_becoming_the_viewport() {
    // 92 of the 207 descriptors carry no `screen`; inventing one from
    // the viewport would emulate a device upstream never described.
    let d = get("Blackberry PlayBook").expect("Blackberry PlayBook");
    assert!(d.screen.is_none());
    assert_eq!(
      d.viewport,
      super::DeviceSize {
        width: 600,
        height: 1024
      }
    );
  }

  #[test]
  fn an_unknown_name_is_none_rather_than_a_fallback() {
    assert!(get("iphone 15").is_none());
    assert!(get("Nokia 3310").is_none());
  }

  #[test]
  fn every_engine_spelling_round_trips() {
    for b in [DeviceBrowser::Chromium, DeviceBrowser::Firefox, DeviceBrowser::WebKit] {
      let json = serde_json::to_string(&b).expect("serialize");
      assert_eq!(json, format!("\"{}\"", b.as_str()));
      let back: DeviceBrowser = serde_json::from_str(&json).expect("deserialize");
      assert_eq!(back, b);
    }
  }
}
