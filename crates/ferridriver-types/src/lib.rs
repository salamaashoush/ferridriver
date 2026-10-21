//! The TypeScript declarations for ferridriver's test and extension APIs,
//! embedded so whatever links this crate serves the ones matching the
//! runtime it was built with.
//!
//! The CLI writes them out for `ferridriver ext check` and editors; a
//! service that edits scripts in the browser serves them to its editor
//! without shipping the CLI.

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
  fn test_declarations_reference_their_node_globals() {
    assert!(TEST_DTS.starts_with("/// <reference path=\"./node.d.ts\" />"));
    assert!(TEST_NODE_DTS.contains("process"));
  }
}
