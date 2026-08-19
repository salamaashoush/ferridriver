//! Compiles Playwright's vendored device descriptors into a static table.
//!
//! `src/devices/deviceDescriptorsSource.json` is a verbatim copy of
//! `packages/isomorphic/deviceDescriptorsSource.json`; see
//! `src/devices/VENDOR.md`. Generating the table here rather than
//! parsing at startup means a malformed or drifted descriptor is a
//! build error naming the device, and a run that never asks for one
//! pays nothing.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::Write as _;
use std::{env, fs, path::Path};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Size {
  width: i64,
  height: i64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Descriptor {
  user_agent: String,
  viewport: Size,
  #[serde(default)]
  screen: Option<Size>,
  device_scale_factor: f64,
  is_mobile: bool,
  has_touch: bool,
  default_browser_type: String,
}

fn size(s: &Size) -> String {
  format!("DeviceSize {{ width: {}, height: {} }}", s.width, s.height)
}

fn main() -> Result<(), Box<dyn Error>> {
  let source = Path::new("src/devices/deviceDescriptorsSource.json");
  println!("cargo:rerun-if-changed={}", source.display());

  let raw = fs::read_to_string(source)?;
  // BTreeMap: the emitted slice is sorted by name, so a lookup is a
  // binary search over static data with no map to build at startup.
  let parsed: BTreeMap<String, Descriptor> = serde_json::from_str(&raw)?;

  let mut out = String::with_capacity(parsed.len() * 400);
  out.push_str("/// Every device Playwright ships, sorted by name.\n");
  out.push_str("static DEVICES: &[(&str, DeviceDescriptor)] = &[\n");
  for (name, d) in &parsed {
    let browser = match d.default_browser_type.as_str() {
      "chromium" => "DeviceBrowser::Chromium",
      "firefox" => "DeviceBrowser::Firefox",
      "webkit" => "DeviceBrowser::WebKit",
      other => return Err(format!("device {name:?}: unknown defaultBrowserType {other:?}").into()),
    };
    let screen = d
      .screen
      .as_ref()
      .map_or_else(|| "None".to_owned(), |s| format!("Some({})", size(s)));
    // Writing into a String cannot fail; `write!` still returns a
    // Result, so the error is folded into the build's own.
    writeln!(
      out,
      "  ({:?}, DeviceDescriptor {{ user_agent: {:?}, viewport: {}, screen: {}, device_scale_factor: \
       {:?}, is_mobile: {}, has_touch: {}, default_browser_type: {} }}),",
      name,
      d.user_agent,
      size(&d.viewport),
      screen,
      d.device_scale_factor,
      d.is_mobile,
      d.has_touch,
      browser
    )?;
  }
  out.push_str("];\n");

  fs::write(Path::new(&env::var("OUT_DIR")?).join("devices_table.rs"), out)?;
  Ok(())
}
