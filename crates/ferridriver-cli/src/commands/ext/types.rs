//! The type declarations an extension is checked against, embedded in the
//! binary.
//!
//! Shipping them inside the binary is what makes `ferridriver ext check`
//! work on a package that has installed nothing: the declarations are the
//! ones for THIS build, so a type error means a real mismatch with the
//! runtime rather than a stale `node_modules` copy. `ferridriver ext
//! types` writes the same bytes out for an editor to resolve.

use std::path::{Path, PathBuf};

/// `@ferridriver/extension` — `defineTool`, the handler context, the
/// package manifest.
pub const EXTENSION_DTS: &str = include_str!("../../../../../packages/ferridriver-extension/index.d.ts");

/// `@ferridriver/test` — the browser bindings (`Page`, `BrowserContext`,
/// `Locator`, ...) the extension declarations build on.
pub const TEST_DTS: &str = include_str!("../../../../../packages/ferridriver-test/index.d.ts");

/// One embedded declaration package.
pub struct TypesPackage {
  pub name: &'static str,
  pub declaration: &'static str,
}

pub const PACKAGES: &[TypesPackage] = &[
  TypesPackage {
    name: "@ferridriver/extension",
    declaration: EXTENSION_DTS,
  },
  TypesPackage {
    name: "@ferridriver/test",
    declaration: TEST_DTS,
  },
];

/// Write every package as `<root>/<name>/index.d.ts` plus a minimal
/// `package.json`, so plain Node resolution (and therefore TypeScript with
/// no `paths` mapping) finds them.
///
/// Returns the written declaration paths, keyed by package name.
pub fn materialize(root: &Path) -> std::io::Result<Vec<(&'static str, PathBuf)>> {
  let version = env!("CARGO_PKG_VERSION");
  let mut written = Vec::new();
  for pkg in PACKAGES {
    let dir = root.join(pkg.name);
    std::fs::create_dir_all(&dir)?;
    let dts = dir.join("index.d.ts");
    std::fs::write(&dts, pkg.declaration)?;
    std::fs::write(
      dir.join("package.json"),
      format!(
        "{{\n  \"name\": \"{}\",\n  \"version\": \"{version}\",\n  \"types\": \"./index.d.ts\",\n  \
         \"exports\": {{ \".\": {{ \"types\": \"./index.d.ts\" }} }}\n}}\n",
        pkg.name
      ),
    )?;
    written.push((pkg.name, dts));
  }
  Ok(written)
}
