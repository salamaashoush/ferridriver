//! Module loader for relative ES module imports.
//!
//! Scripts can import other JS files via ES module syntax:
//!
//! ```js
//! import { helper } from './helpers.js';
//! import data from './fixtures/users.js';
//! ```
//!
//! An import path is resolved relative to the importing module's
//! directory, or to `script_root` for an inline script with no base, and
//! read from disk (`.js` / `.mjs`).
//!
//! Bare specifiers (e.g. `import lodash from 'lodash'`) are rejected:
//! there is no node_modules resolution here on purpose. A suite that
//! needs one is bundled by rolldown before it ever reaches this loader.
//! Native module specifiers (`ferridriver`, `@cucumber/cucumber`, the
//! node-compat set) never reach this resolver either: the engine chains
//! [`crate::bindings::native_modules`]'s resolver/loader AHEAD of this
//! pair, so this file handles real files only.

use std::path::{Path, PathBuf};

use rquickjs::{Ctx, Error, Module, Result, loader::Loader, loader::Resolver, module::Declared};

/// Resolves relative ES module specifiers to absolute paths.
#[derive(Debug, Clone)]
pub struct RelativeModuleResolver {
  root: PathBuf,
}

impl RelativeModuleResolver {
  #[must_use]
  pub fn new(root: PathBuf) -> Self {
    Self { root }
  }

  /// Resolve `name` against `base`, falling back to the script root when
  /// the importer has no directory of its own.
  ///
  /// An inline script's base is empty, and a dynamic `import()` from one
  /// carries the eval's NAME (`eval_script`) — whose parent is the empty
  /// path, not a directory. Both mean "no importer directory", so both
  /// resolve from the root.
  fn join_relative(&self, base: &str, name: &str) -> PathBuf {
    let base_dir = Path::new(base)
      .parent()
      .filter(|parent| !parent.as_os_str().is_empty())
      .map_or_else(|| self.root.clone(), Path::to_path_buf);
    base_dir.join(name)
  }
}

impl Resolver for RelativeModuleResolver {
  fn resolve<'js>(
    &mut self,
    _ctx: &Ctx<'js>,
    base: &str,
    name: &str,
    _attributes: Option<rquickjs::loader::ImportAttributes<'js>>,
  ) -> Result<String> {
    // Reject bare specifiers up front — there is no node_modules or
    // package resolution here. Relative and absolute paths both resolve.
    if !(name.starts_with("./") || name.starts_with("../") || name.starts_with('/')) {
      return Err(Error::new_loading_message(
        name,
        "bare module specifiers are not supported; bundle the suite instead",
      ));
    }

    let joined = self.join_relative(base, name);
    let resolved = std::fs::canonicalize(&joined)
      .map_err(|e| Error::new_loading_message(name, format!("cannot resolve {}: {e}", joined.display())))?;

    Ok(resolved.to_string_lossy().into_owned())
  }
}

/// Reads a module the [`RelativeModuleResolver`] resolved.
#[derive(Debug, Clone, Default)]
pub struct RelativeModuleLoader;

impl RelativeModuleLoader {
  #[must_use]
  pub fn new() -> Self {
    Self
  }
}

impl Loader for RelativeModuleLoader {
  fn load<'js>(
    &mut self,
    ctx: &Ctx<'js>,
    name: &str,
    _attributes: Option<rquickjs::loader::ImportAttributes<'js>>,
  ) -> Result<Module<'js, Declared>> {
    let path = Path::new(name);
    let allowed_ext = matches!(path.extension().and_then(|e| e.to_str()), Some("js" | "mjs"));
    if !allowed_ext {
      return Err(Error::new_loading_message(
        name,
        "only .js and .mjs modules are supported",
      ));
    }

    let source = std::fs::read(path).map_err(|e| Error::new_loading_message(name, e.to_string()))?;
    Module::declare(ctx.clone(), name, source)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn mk_root() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonical");
    (tmp, root)
  }

  #[test]
  fn resolver_rejects_bare_specifiers() {
    let (_tmp, root) = mk_root();
    let mut r = RelativeModuleResolver::new(root);
    // The resolver only uses `Ctx` to construct its error.
    let rt = rquickjs::Runtime::new().expect("runtime");
    let cx = rquickjs::Context::full(&rt).expect("context");
    cx.with(|ctx| {
      let err = r.resolve(&ctx, "", "lodash", None).expect_err("bare specifier");
      assert!(err.to_string().contains("bare module"));
    });
  }

  #[test]
  fn resolver_answers_for_a_relative_import() {
    let (tmp, root) = mk_root();
    std::fs::write(tmp.path().join("helper.js"), b"export const x = 1;").expect("write");
    let mut r = RelativeModuleResolver::new(root.clone());
    let rt = rquickjs::Runtime::new().expect("runtime");
    let cx = rquickjs::Context::full(&rt).expect("context");
    cx.with(|ctx| {
      let resolved = r.resolve(&ctx, "", "./helper.js", None).expect("resolve");
      assert_eq!(PathBuf::from(resolved), root.join("helper.js"));
    });
  }

  /// A relative import that climbs out of the root resolves like any
  /// other path. The loader is an anchor, not a jail — a suite whose
  /// helpers live one directory up is ordinary.
  #[test]
  fn resolver_follows_a_parent_import() {
    let (tmp, root) = mk_root();
    let nested = tmp.path().join("specs");
    std::fs::create_dir_all(&nested).expect("mkdir");
    std::fs::write(tmp.path().join("shared.js"), b"export const x = 1;").expect("write");
    let mut r = RelativeModuleResolver::new(nested.clone());
    let rt = rquickjs::Runtime::new().expect("runtime");
    let cx = rquickjs::Context::full(&rt).expect("context");
    cx.with(|ctx| {
      let base = nested.join("a.js").to_string_lossy().into_owned();
      let resolved = r.resolve(&ctx, &base, "../shared.js", None).expect("resolve");
      assert_eq!(PathBuf::from(resolved), root.join("shared.js"));
    });
  }
}
