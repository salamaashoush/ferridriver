//! What an editor needs to know about this ferridriver build without
//! linking the browser engine: the TypeScript declarations for the test
//! and extension APIs, and the device registry `use.device` accepts.
//!
//! The CLI writes the declarations out for `ferridriver ext check` and
//! editors; a service that edits scripts and test settings in the browser
//! serves both to its editor, matching the runtime its runners use.

/// The version the declarations describe.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `@ferridriver/extension` — `defineTool`, the handler context, the
/// package manifest.
pub const EXTENSION_DTS: &str = include_str!("../../../packages/ferridriver-extension/index.d.ts");

/// `@ferridriver/test` — the browser bindings (`Page`, `BrowserContext`,
/// `Locator`, ...) the extension declarations build on.
pub const TEST_DTS: &str = include_str!("../../../packages/ferridriver-test/index.d.ts");

/// The Node globals (`process`, `Buffer`, ...) scripts may use, referenced
/// from `TEST_DTS`.
pub const TEST_NODE_DTS: &str = include_str!("../../../packages/ferridriver-test/node.d.ts");

/// Playwright's device registry as JSON, keyed by device name: the same
/// file `ferridriver::devices` compiles in, so a name listed here is one
/// `use.device` resolves.
pub const DEVICES_JSON: &str = include_str!("../../ferridriver/src/devices/deviceDescriptorsSource.json");

/// One declaration package.
pub struct TypesPackage {
  pub name: &'static str,
  pub declaration: &'static str,
  /// Files beside `index.d.ts`, as `(file name, contents)`.
  pub additional: &'static [(&'static str, &'static str)],
}

pub const PACKAGES: &[TypesPackage] = &[
  TypesPackage {
    name: "@ferridriver/extension",
    declaration: EXTENSION_DTS,
    additional: &[],
  },
  TypesPackage {
    name: "@ferridriver/test",
    declaration: TEST_DTS,
    additional: &[("node.d.ts", TEST_NODE_DTS)],
  },
];

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn device_registry_is_keyed_by_name() {
    assert!(DEVICES_JSON.trim_start().starts_with('{'));
    assert!(DEVICES_JSON.contains("\"iPhone 15\": {"));
  }

  #[test]
  fn test_declarations_reference_their_node_globals() {
    assert!(TEST_DTS.starts_with("/// <reference path=\"./node.d.ts\" />"));
    assert!(TEST_NODE_DTS.contains("process"));
  }
}
