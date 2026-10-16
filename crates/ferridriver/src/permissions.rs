//! Playwright's permission names, as each engine's protocol spells them
//! (`crBrowser.ts::doGrantPermissions`, `wkPage.ts::_grantPermissions`,
//! `bidiBrowser.ts::doGrantPermissions`).

use crate::error::{FerriError, Result};

fn unknown(permission: &str) -> FerriError {
  FerriError::invalid_argument("permissions", format!("Unknown permission: {permission}"))
}

/// `Browser.PermissionType`s for `permissions`. `legacy` is the fallback
/// Playwright retries with for browsers that predate the newer
/// local-network names.
pub(crate) fn chromium(permissions: &[String], legacy: bool) -> Result<Vec<&'static str>> {
  let mut out = Vec::with_capacity(permissions.len());
  for permission in permissions {
    let mapped: &[&'static str] = match permission.as_str() {
      "geolocation" => &["geolocation"],
      "midi" => &["midi"],
      "notifications" => &["notifications"],
      "camera" => &["videoCapture"],
      "microphone" => &["audioCapture"],
      "background-sync" => &["backgroundSync"],
      "ambient-light-sensor" | "accelerometer" | "gyroscope" | "magnetometer" => &["sensors"],
      "clipboard-read" => &["clipboardReadWrite"],
      "clipboard-write" => &["clipboardSanitizedWrite"],
      "payment-handler" => &["paymentHandler"],
      "midi-sysex" => &["midiSysex"],
      "storage-access" => &["storageAccess"],
      "local-fonts" => &["localFonts"],
      "local-network-access" if legacy => &["localNetworkAccess"],
      "local-network-access" => &["localNetworkAccess", "localNetwork", "loopbackNetwork"],
      "screen-wake-lock" => &["wakeLockScreen"],
      other => return Err(unknown(other)),
    };
    out.extend_from_slice(mapped);
  }
  Ok(out)
}

/// `Emulation.grantPermissions` names for Playwright's `WebKit` build.
pub(crate) fn webkit(permissions: &[String]) -> Result<Vec<&'static str>> {
  permissions
    .iter()
    .map(|permission| match permission.as_str() {
      "geolocation" => Ok("geolocation"),
      "notifications" => Ok("notifications"),
      "clipboard-read" => Ok("clipboard-read"),
      "screen-wake-lock" => Ok("screen-wake-lock"),
      "camera" => Ok("camera"),
      "microphone" => Ok("microphone"),
      other => Err(unknown(other)),
    })
    .collect()
}

/// W3C permission names for `permissions.setPermission`: Playwright's,
/// except the one that names two descriptors.
pub(crate) fn bidi(permissions: &[String]) -> Vec<String> {
  permissions
    .iter()
    .flat_map(|permission| match permission.as_str() {
      "local-network-access" => vec!["local-network".to_string(), "loopback-network".to_string()],
      other => vec![other.to_string()],
    })
    .collect()
}

/// The origin a grant for `url` applies to, as `new URL(url).origin`.
///
/// # Errors
///
/// An unparseable URL.
pub(crate) fn origin_of(url: &str) -> Result<String> {
  reqwest::Url::parse(url)
    .map(|url| url.origin().ascii_serialization())
    .map_err(|error| FerriError::invalid_argument("origin", format!("{url}: {error}")))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_string()).collect()
  }

  #[test]
  fn each_engine_spells_the_permissions_its_own_way() {
    assert_eq!(
      chromium(&names(&["camera", "clipboard-read", "gyroscope"]), false).unwrap(),
      ["videoCapture", "clipboardReadWrite", "sensors"]
    );
    assert_eq!(
      chromium(&names(&["local-network-access"]), false).unwrap(),
      ["localNetworkAccess", "localNetwork", "loopbackNetwork"]
    );
    assert_eq!(
      chromium(&names(&["local-network-access"]), true).unwrap(),
      ["localNetworkAccess"]
    );
    assert_eq!(
      webkit(&names(&["camera", "geolocation"])).unwrap(),
      ["camera", "geolocation"]
    );
    assert_eq!(
      bidi(&names(&["local-network-access", "geolocation"])),
      ["local-network", "loopback-network", "geolocation"]
    );
  }

  #[test]
  fn an_unknown_permission_is_refused_by_name() {
    assert!(
      chromium(&names(&["teleport"]), false)
        .unwrap_err()
        .to_string()
        .contains("Unknown permission: teleport")
    );
    assert!(webkit(&names(&["midi"])).is_err(), "WebKit grants fewer than Chromium");
  }

  #[test]
  fn an_origin_drops_the_path() {
    assert_eq!(
      origin_of("https://example.com:8443/a?b").unwrap(),
      "https://example.com:8443"
    );
    assert!(origin_of("not a url").is_err());
  }
}
