//! The type declarations an extension is checked against, embedded in the
//! binary.
//!
//! Shipping them inside the binary is what makes `ferridriver ext check`
//! work on a package that has installed nothing: the declarations are the
//! ones for THIS build, so a type error means a real mismatch with the
//! runtime rather than a stale `node_modules` copy. `ferridriver ext
//! types` writes the same bytes out for an editor to resolve.

use std::path::{Path, PathBuf};

use ferridriver_types::PACKAGES;

/// Write every package as `<root>/<name>/index.d.ts` plus a minimal
/// `package.json`, so plain Node resolution (and therefore TypeScript with
/// no `paths` mapping) finds them.
///
/// Returns the written declaration paths, keyed by package name.
pub fn materialize(root: &Path) -> std::io::Result<Vec<(&'static str, PathBuf)>> {
  let version = ferridriver_types::VERSION;
  let mut written = Vec::new();
  for pkg in PACKAGES {
    let dir = root.join(pkg.name);
    std::fs::create_dir_all(&dir)?;
    let dts = dir.join("index.d.ts");
    std::fs::write(&dts, pkg.declaration)?;
    for (name, contents) in pkg.additional {
      std::fs::write(dir.join(name), contents)?;
    }
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
