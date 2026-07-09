//! Extension discovery and manifest extraction.
//!
//! At server startup every configured extension file is rolldown-bundled
//! (TypeScript, extension-local imports, and `node_modules` resolved +
//! tree-shaken), compiled to `QuickJS` bytecode, and its manifests
//! extracted — all in a single throwaway runtime for the whole batch
//! (`ferridriver_script::compile_and_extract_extensions`), not one engine
//! per file. A extension registers its tools by calling the native
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

use ferridriver_script::{compile_and_extract_extensions, walk_source_files};

use super::manifest::ToolManifest;

/// A extension source file that has been discovered, bundled, compiled, and
/// validated. Carries every tool the file declares plus the precompiled
/// module bytecode each session VM loads.
#[derive(Debug, Clone)]
pub struct LoadedExtension {
  /// One manifest per tool declared in the file. At least one.
  pub tools: Vec<ToolManifest>,
  /// Precompiled `QuickJS` bytecode of the rolldown-bundled module,
  /// shared (`Arc`) so handing it to a session VM is a refcount bump.
  pub bytecode: Arc<[u8]>,
  pub path: PathBuf,
}

/// Failure modes the loader can surface (per file; one bad file never
/// stops the others).
#[derive(Debug)]
pub enum ExtensionLoadError {
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

impl std::fmt::Display for ExtensionLoadError {
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

impl std::error::Error for ExtensionLoadError {}

impl ExtensionLoadError {
  /// The failing source (file path or configured spec), for error
  /// reporting keyed by origin.
  #[must_use]
  pub fn source_label(&self) -> String {
    match self {
      Self::Io { path, .. }
      | Self::Bundle { path, .. }
      | Self::ManifestInvalid { path, .. }
      | Self::ManifestNoTools { path } => path.display().to_string(),
    }
  }
}

/// Bundle + compile + extract every discovered extension file in one batch.
/// Returns the successfully loaded extensions and a per-file error list so
/// the caller can log and skip broken files without aborting startup.
///
/// The returned `LoadedExtension`s preserve input file order, which the
/// server keeps when building `ExtensionBinding`s — sessions evaluate the
/// files in the same order the manifests were extracted, so registry
/// tool order matches the manifest order.
pub async fn load_all(files: &[PathBuf]) -> (Vec<LoadedExtension>, Vec<ExtensionLoadError>) {
  let (compiled, bundle_failures) = compile_and_extract_extensions(files).await;

  let mut loaded = Vec::with_capacity(compiled.len());
  let mut errors: Vec<ExtensionLoadError> = bundle_failures
    .into_iter()
    .map(|(path, e)| ExtensionLoadError::Bundle {
      path,
      message: e.message,
    })
    .collect();

  for cp in compiled {
    let tools: Vec<ToolManifest> = match serde_json::from_str(&cp.manifests_json) {
      Ok(t) => t,
      Err(error) => {
        errors.push(ExtensionLoadError::ManifestInvalid { path: cp.path, error });
        continue;
      },
    };
    if tools.is_empty() {
      errors.push(ExtensionLoadError::ManifestNoTools { path: cp.path });
      continue;
    }
    loaded.push(LoadedExtension {
      tools,
      bytecode: cp.bytecode,
      path: cp.path,
    });
  }

  (loaded, errors)
}

/// Discover extension files under a path. Directories are scanned
/// **recursively** for any [`ferridriver_script::SOURCE_EXTENSIONS`]
/// file (rolldown transpiles TypeScript / JSX). A single file the user
/// named explicitly is returned as-is regardless of extension. This
/// shares the discovery rule with the BDD runner so a `.tsx`/`.cts`
/// extension is visible to both hosts.
///
/// # Errors
///
/// Returns [`ExtensionLoadError::Io`] when the path cannot be stat'd.
pub fn discover(path: &Path) -> Result<Vec<PathBuf>, ExtensionLoadError> {
  let meta = std::fs::metadata(path).map_err(|error| ExtensionLoadError::Io {
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
pub fn discover_specs(specs: &[String], cwd: &Path) -> (Vec<PathBuf>, Vec<ExtensionLoadError>) {
  let (files, errors) = ferridriver_script::discover::resolve_extension_specs(specs, cwd);
  let errors = errors
    .into_iter()
    .map(|(spec, e)| ExtensionLoadError::Bundle {
      path: PathBuf::from(spec),
      message: e.message,
    })
    .collect();
  (files, errors)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn scratch(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("ferri_ext_loader_{label}_{nanos}"));
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
  }

  #[test]
  fn discover_returns_an_explicit_file_as_is() {
    let dir = scratch("file");
    let f = dir.join("tool.weird-ext");
    std::fs::write(&f, "defineTool({ name: 't', handler: () => 1 });").unwrap();
    let found = discover(&f).expect("discover file");
    assert_eq!(found, vec![f]);
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn discover_scans_directories_recursively_for_source_files_only() {
    let dir = scratch("dir");
    std::fs::create_dir_all(dir.join("nested/deep")).unwrap();
    std::fs::write(dir.join("a.ts"), "").unwrap();
    std::fs::write(dir.join("nested/b.tsx"), "").unwrap();
    std::fs::write(dir.join("nested/deep/c.cjs"), "").unwrap();
    std::fs::write(dir.join("nested/readme.md"), "").unwrap();
    std::fs::write(dir.join("data.json"), "{}").unwrap();

    let mut found = discover(&dir).expect("discover dir");
    found.sort();
    let names: Vec<String> = found
      .iter()
      .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
      .collect();
    assert_eq!(names, ["a.ts", "b.tsx", "c.cjs"], "source files only, recursively");
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn discover_missing_path_is_an_io_error() {
    let missing = std::env::temp_dir().join("ferri_ext_loader_definitely_missing/nope.js");
    let err = discover(&missing).expect_err("must fail");
    assert!(matches!(err, ExtensionLoadError::Io { .. }), "got: {err}");
  }

  #[test]
  fn discover_specs_records_unresolvable_specs_as_errors() {
    let dir = scratch("specs");
    std::fs::write(dir.join("ok.js"), "defineTool({ name: 't', handler: () => 1 });").unwrap();
    // A bare name is a package specifier; a path spec needs `./`.
    let (files, errors) = discover_specs(&["./ok.js".to_string(), "no-such-package-xyz".to_string()], &dir);
    assert_eq!(files.len(), 1, "the resolvable spec survives: {files:?}");
    assert_eq!(errors.len(), 1, "the bogus spec is recorded: {errors:?}");
    assert!(errors[0].source_label().contains("no-such-package-xyz"));
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn load_all_extracts_manifests_and_isolates_broken_files() {
    let dir = scratch("load");
    std::fs::write(
      dir.join("good.js"),
      "defineTool({ name: 'good.tool', title: 'Good', exposeAsTool: true, \
       annotations: { readOnlyHint: true }, \
       outputSchema: { type: 'object' }, handler: async () => ({}) });",
    )
    .unwrap();
    std::fs::write(dir.join("broken.js"), "this is not (valid js").unwrap();
    std::fs::write(dir.join("empty.js"), "export const nothing = 1;").unwrap();

    let files = vec![dir.join("good.js"), dir.join("broken.js"), dir.join("empty.js")];
    let (loaded, errors) = load_all(&files).await;

    assert_eq!(loaded.len(), 1, "only the good file loads: {loaded:?}");
    let tool = &loaded[0].tools[0];
    assert_eq!(tool.name, "good.tool");
    assert_eq!(tool.title.as_deref(), Some("Good"));
    assert!(tool.expose_as_mcp_tool);
    assert!(tool.output_schema.is_some());
    assert_eq!(tool.annotations.as_ref().and_then(|a| a.read_only_hint), Some(true));
    assert!(!loaded[0].bytecode.is_empty());

    assert_eq!(errors.len(), 2, "broken + toolless: {errors:?}");
    let by_label = |needle: &str| {
      errors
        .iter()
        .find(|e| e.source_label().contains(needle))
        .unwrap_or_else(|| panic!("no error for {needle}: {errors:?}"))
    };
    assert!(matches!(by_label("broken.js"), ExtensionLoadError::Bundle { .. }));
    assert!(matches!(
      by_label("empty.js"),
      ExtensionLoadError::ManifestNoTools { .. }
    ));
    let _ = std::fs::remove_dir_all(&dir);
  }
}
