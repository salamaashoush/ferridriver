//! Plugin discovery and manifest extraction.
//!
//! At server startup every configured plugin file is rolldown-bundled
//! (TypeScript, plugin-local imports, and `node_modules` resolved +
//! tree-shaken), compiled to `QuickJS` bytecode, and its manifests
//! extracted — all in a single throwaway runtime for the whole batch
//! (`ferridriver_script::compile_and_extract_plugins`), not one engine
//! per file. A plugin registers its tools by calling the native
//! `defineTool({ name, description, inputSchema, allow,
//! exposeAsMcpTool, handler })` / `tool(...)` contribution points at
//! the module's top level; evaluating the compiled bytecode runs those
//! calls against the Rust `ExtensionRegistry`, and the manifests are
//! read straight off that registry.
//!
//! Each manifest's `handler` is stripped during extraction (functions
//! are not JSON-serialisable and only make sense inside a live VM); the
//! compiled bytecode retains the live handler closures and is loaded
//! into each session VM with no per-session parse.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ferridriver_script::{compile_and_extract_plugins, walk_source_files};

use super::manifest::PluginManifest;

/// A plugin source file that has been discovered, bundled, compiled, and
/// validated. Carries every tool the file declares plus the precompiled
/// module bytecode each session VM loads.
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
  /// One manifest per tool declared in the file. At least one.
  pub tools: Vec<PluginManifest>,
  /// Precompiled `QuickJS` bytecode of the rolldown-bundled module,
  /// shared (`Arc`) so handing it to a session VM is a refcount bump.
  pub bytecode: Arc<[u8]>,
  pub path: PathBuf,
}

/// Failure modes the loader can surface (per file; one bad file never
/// stops the others).
#[derive(Debug)]
pub enum PluginLoadError {
  Io {
    path: PathBuf,
    error: std::io::Error,
  },
  /// Bundle, compile, or manifest extraction failed for this file.
  Bundle {
    path: PathBuf,
    message: String,
  },
  ManifestInvalid {
    path: PathBuf,
    error: serde_json::Error,
  },
  ManifestNoTools {
    path: PathBuf,
  },
}

impl std::fmt::Display for PluginLoadError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Io { path, error } => write!(f, "read {}: {error}", path.display()),
      Self::Bundle { path, message } => write!(f, "bundle {}: {message}", path.display()),
      Self::ManifestInvalid { path, error } => write!(f, "{}: manifest invalid: {error}", path.display()),
      Self::ManifestNoTools { path } => write!(
        f,
        "{}: no tools declared — the file never called defineTool(...)",
        path.display()
      ),
    }
  }
}

impl std::error::Error for PluginLoadError {}

/// Bundle + compile + extract every discovered plugin file in one batch.
/// Returns the successfully loaded plugins and a per-file error list so
/// the caller can log and skip broken files without aborting startup.
///
/// The returned `LoadedPlugin`s preserve input file order, which the
/// server keeps when building `PluginBinding`s — sessions evaluate the
/// files in the same order the manifests were extracted, so registry
/// tool order matches the manifest order.
pub async fn load_all(files: &[PathBuf]) -> (Vec<LoadedPlugin>, Vec<PluginLoadError>) {
  let (compiled, bundle_failures) = compile_and_extract_plugins(files).await;

  let mut loaded = Vec::with_capacity(compiled.len());
  let mut errors: Vec<PluginLoadError> = bundle_failures
    .into_iter()
    .map(|(path, e)| PluginLoadError::Bundle {
      path,
      message: e.message,
    })
    .collect();

  for cp in compiled {
    let tools: Vec<PluginManifest> = match serde_json::from_str(&cp.manifests_json) {
      Ok(t) => t,
      Err(error) => {
        errors.push(PluginLoadError::ManifestInvalid { path: cp.path, error });
        continue;
      },
    };
    if tools.is_empty() {
      errors.push(PluginLoadError::ManifestNoTools { path: cp.path });
      continue;
    }
    loaded.push(LoadedPlugin {
      tools,
      bytecode: cp.bytecode,
      path: cp.path,
    });
  }

  (loaded, errors)
}

/// Discover plugin files under a path. Directories are scanned
/// **recursively** for any [`ferridriver_script::SOURCE_EXTENSIONS`]
/// file (rolldown transpiles TypeScript / JSX). A single file the user
/// named explicitly is returned as-is regardless of extension. This
/// shares the discovery rule with the BDD runner so a `.tsx`/`.cts`
/// extension is visible to both hosts.
///
/// # Errors
///
/// Returns [`PluginLoadError::Io`] when the path cannot be stat'd.
pub fn discover(path: &Path) -> Result<Vec<PathBuf>, PluginLoadError> {
  let meta = std::fs::metadata(path).map_err(|error| PluginLoadError::Io {
    path: path.to_path_buf(),
    error,
  })?;

  if meta.is_file() {
    return Ok(vec![path.to_path_buf()]);
  }

  if !meta.is_dir() {
    return Ok(Vec::new());
  }

  Ok(walk_source_files(path))
}

/// Resolve configured extension specifiers (paths or ESM packages) to
/// concrete entry files.
#[must_use]
pub fn discover_specs(specs: &[String], cwd: &Path) -> (Vec<PathBuf>, Vec<PluginLoadError>) {
  let (files, errors) = ferridriver_script::discover::resolve_extension_specs(specs, cwd);
  let errors = errors
    .into_iter()
    .map(|(spec, e)| PluginLoadError::Bundle {
      path: PathBuf::from(spec),
      message: e.message,
    })
    .collect();
  (files, errors)
}
