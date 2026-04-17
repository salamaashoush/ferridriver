//! Module loader rooted at the `script_root` sandbox.
//!
//! Scripts can import other JS files via ES module syntax:
//!
//! ```js
//! import { helper } from './helpers.js';
//! import data from './fixtures/users.js';
//! ```
//!
//! All import paths are:
//! 1. Resolved relative to the importing module's directory (or the sandbox
//!    root for inline scripts with no base).
//! 2. Validated against the [`PathSandbox`] just like `fs.readFile` — absolute
//!    paths, `..` components, and symlink escapes are rejected.
//! 3. Loaded from disk via rquickjs's built-in [`ScriptLoader`] (`.js` by
//!    default; `.mjs` also accepted).
//!
//! Bare specifiers (e.g. `import lodash from 'lodash'`) are rejected — there
//! is no node_modules resolution on purpose, the sandbox is self-contained.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rquickjs::{Ctx, Error, Module, Result, loader::Loader, loader::Resolver, module::Declared};

use crate::fs::PathSandbox;

/// Path-sanitising resolver that maps ES module specifiers to absolute paths
/// inside the sandbox root.
#[derive(Debug, Clone)]
pub struct SandboxResolver {
  sandbox: Arc<PathSandbox>,
}

impl SandboxResolver {
  #[must_use]
  pub fn new(sandbox: Arc<PathSandbox>) -> Self {
    Self { sandbox }
  }

  /// Resolve `name` against `base` and return the path relative to the
  /// sandbox root (without canonicalising — that happens in [`PathSandbox`]).
  fn join_relative(&self, base: &str, name: &str) -> PathBuf {
    // `base` is an absolute path inside the sandbox root (what a previous
    // resolve returned) or empty for inline scripts. Take its parent dir and
    // append `name`; if base is empty we're at the sandbox root.
    let base_dir: PathBuf = if base.is_empty() {
      self.sandbox.root().to_path_buf()
    } else {
      Path::new(base)
        .parent()
        .map_or_else(|| self.sandbox.root().to_path_buf(), PathBuf::from)
    };
    base_dir.join(name)
  }
}

impl Resolver for SandboxResolver {
  fn resolve(&mut self, _ctx: &Ctx<'_>, base: &str, name: &str) -> Result<String> {
    // Reject bare specifiers up front — we don't support node_modules or
    // package resolution. Only relative (`./x`, `../x`) and explicit absolute
    // paths inside the sandbox are allowed; the latter is still rejected
    // syntactically by PathSandbox::resolve_read.
    if !(name.starts_with("./") || name.starts_with("../") || name.starts_with('/')) {
      return Err(Error::new_loading_message(
        name,
        "bare module specifiers are not supported inside the sandbox",
      ));
    }

    let joined = self.join_relative(base, name);

    // Re-express as a path relative to the sandbox root so PathSandbox's
    // sanitizer runs — it canonicalises and verifies no escape.
    let rel = joined
      .strip_prefix(self.sandbox.root())
      .unwrap_or(&joined)
      .to_string_lossy()
      .into_owned();

    let resolved = self
      .sandbox
      .resolve_read(&rel)
      .map_err(|e| Error::new_loading_message(name, e.message.clone()))?;

    Ok(resolved.to_string_lossy().into_owned())
  }
}

/// Loader wrapper that accepts only paths the [`SandboxResolver`] produced.
///
/// rquickjs splits resolution from loading; we re-check path containment here
/// so a future resolver bug cannot smuggle a path outside the sandbox into
/// the loader. Reading the file itself goes through `tokio::fs` synchronously
/// via `std::fs::read` — same as rquickjs's built-in `ScriptLoader`.
#[derive(Debug, Clone)]
pub struct SandboxLoader {
  sandbox: Arc<PathSandbox>,
}

impl SandboxLoader {
  #[must_use]
  pub fn new(sandbox: Arc<PathSandbox>) -> Self {
    Self { sandbox }
  }
}

impl Loader for SandboxLoader {
  fn load<'js>(&mut self, ctx: &Ctx<'js>, name: &str) -> Result<Module<'js, Declared>> {
    // Defensive check: the resolver should have returned a path inside the
    // sandbox, but verify before reading from disk.
    let path = Path::new(name);
    if !path.starts_with(self.sandbox.root()) {
      return Err(Error::new_loading_message(name, "path escapes script_root"));
    }

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

  fn mk_sandbox() -> (tempfile::TempDir, Arc<PathSandbox>) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sb = Arc::new(PathSandbox::new(tmp.path()).expect("sandbox"));
    (tmp, sb)
  }

  #[test]
  fn resolver_rejects_bare_specifiers() {
    let (_tmp, sb) = mk_sandbox();
    let mut r = SandboxResolver::new(sb);
    // We need a Ctx but the resolver only uses it in its trait contract for
    // error construction — construct a fresh runtime to get one.
    let rt = rquickjs::Runtime::new().expect("runtime");
    let cx = rquickjs::Context::full(&rt).expect("context");
    cx.with(|ctx| {
      let err = r.resolve(&ctx, "", "lodash").unwrap_err();
      assert!(err.to_string().contains("bare module"));
    });
  }

  #[test]
  fn resolver_rejects_traversal() {
    let (_tmp, sb) = mk_sandbox();
    let mut r = SandboxResolver::new(sb);
    let rt = rquickjs::Runtime::new().expect("runtime");
    let cx = rquickjs::Context::full(&rt).expect("context");
    cx.with(|ctx| {
      let err = r.resolve(&ctx, "", "../escape.js").unwrap_err();
      assert!(err.is_loading());
    });
  }

  #[test]
  fn resolver_accepts_valid_relative() {
    let (tmp, sb) = mk_sandbox();
    std::fs::write(tmp.path().join("helper.js"), b"export const x = 1;").unwrap();
    let mut r = SandboxResolver::new(sb);
    let rt = rquickjs::Runtime::new().expect("runtime");
    let cx = rquickjs::Context::full(&rt).expect("context");
    cx.with(|ctx| {
      let resolved = r.resolve(&ctx, "", "./helper.js").expect("resolve");
      assert!(resolved.ends_with("helper.js"));
    });
  }
}
