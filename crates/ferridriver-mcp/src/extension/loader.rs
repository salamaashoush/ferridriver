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
  /// One manifest per tool declared in the file. May be empty: an entry
  /// can contribute only BDD steps or script-host globals.
  pub tools: Vec<ToolManifest>,
  /// Precompiled `QuickJS` bytecode of the rolldown-bundled module,
  /// shared (`Arc`) so handing it to a session VM is a refcount bump.
  pub bytecode: Arc<[u8]>,
  pub path: PathBuf,
  /// Maps this file's bundled frames back to the author's source, keyed
  /// by the module name its bytecode carries.
  pub source_map: Option<ferridriver_script::SourceMapper>,
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
}

impl std::fmt::Display for ExtensionLoadError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Io { path, error } => write!(f, "read {}: {error}", path.display()),
      Self::Bundle { path, message } => write!(f, "bundle {}: {message}", path.display()),
      Self::ManifestInvalid { path, error } => write!(f, "{}: manifest invalid: {error}", path.display()),
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
      Self::Io { path, .. } | Self::Bundle { path, .. } | Self::ManifestInvalid { path, .. } => {
        path.display().to_string()
      },
    }
  }
}

/// Bundle + compile + extract every discovered extension file in one batch.
/// Returns the successfully loaded extensions and a per-file error list so
/// the caller can log and skip broken files without aborting startup.
///
/// A file that compiles but declares no tools is returned as loaded with an
/// empty tool list: it may be contributing BDD steps or script-host globals.
///
/// The returned `LoadedExtension`s preserve input file order, which the
/// server keeps when building `ExtensionBinding`s — sessions evaluate the
/// files in the same order the manifests were extracted, so registry
/// tool order matches the manifest order.
pub async fn load_all(
  files: &[PathBuf],
  policy: &ferridriver_config::ExtensionPolicyConfig,
) -> (Vec<LoadedExtension>, Vec<ExtensionLoadError>) {
  let (compiled, bundle_failures) =
    compile_and_extract_extensions(&files.iter().map(|f| vec![f.clone()]).collect::<Vec<_>>(), policy).await;

  let mut loaded = Vec::with_capacity(compiled.len());
  let mut errors: Vec<ExtensionLoadError> = bundle_failures
    .into_iter()
    .map(|(path, e)| ExtensionLoadError::Bundle {
      path,
      message: e.message,
    })
    .collect();

  for cp in compiled {
    let tools: Vec<ToolManifest> = match serde_json::from_str(&cp.manifests_json()) {
      Ok(t) => t,
      Err(error) => {
        errors.push(ExtensionLoadError::ManifestInvalid { path: cp.path, error });
        continue;
      },
    };
    // A file with no tools is NOT a failure: an extension entry may exist
    // only to contribute BDD steps or script-host globals, and dropping it
    // meant those contributions never ran at all. The host warns about the
    // toolless file instead, so a `defineTool` that silently failed to
    // register is still visible.
    let source_map = Some(cp.mapper());
    loaded.push(LoadedExtension {
      tools,
      bytecode: cp.bytecode,
      path: cp.path,
      source_map,
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
///
/// Each spec carries the directory of the config layer that declared
/// it, so a user-level entry resolves against the user config dir
/// rather than the process cwd.
#[must_use]
pub fn discover_specs(specs: &[ferridriver_config::ExtensionSpec]) -> (Vec<PathBuf>, Vec<ExtensionLoadError>) {
  let (resolved, errors) = resolve_specs(specs);
  let mut files = Vec::new();
  for r in resolved {
    for f in r.files {
      if !files.contains(&f) {
        files.push(f);
      }
    }
  }
  (files, errors)
}

/// Like [`discover_specs`], but keeping each spec's package identity and
/// its `ferridriver` package manifest so the host can check the declared
/// requirements (see [`super::requirements`]) before loading.
#[must_use]
pub fn resolve_specs(
  specs: &[ferridriver_config::ExtensionSpec],
) -> (Vec<ferridriver_script::ResolvedExtension>, Vec<ExtensionLoadError>) {
  let (resolved, errors) = ferridriver_script::discover::resolve_extensions(specs);
  let errors = errors
    .into_iter()
    .map(|(spec, e)| ExtensionLoadError::Bundle {
      path: PathBuf::from(spec),
      message: e.message,
    })
    .collect();
  (resolved, errors)
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
    let spec = |s: &str| ferridriver_config::ExtensionSpec {
      spec: s.to_string(),
      base_dir: dir.clone(),
    };
    let (files, errors) = discover_specs(&[spec("./ok.js"), spec("no-such-package-xyz")]);
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
    let (loaded, errors) = load_all(&files, &ferridriver_config::ExtensionPolicyConfig::default()).await;

    assert_eq!(loaded.len(), 2, "the good file AND the toolless one load: {loaded:?}");
    let toolless = loaded
      .iter()
      .find(|l| l.path.ends_with("empty.js"))
      .unwrap_or_else(|| panic!("empty.js must load: {loaded:?}"));
    assert!(
      toolless.tools.is_empty(),
      "a file may contribute steps/globals instead of tools"
    );
    assert!(!toolless.bytecode.is_empty(), "its top-level code must still run");

    let good = loaded
      .iter()
      .find(|l| l.path.ends_with("good.js"))
      .unwrap_or_else(|| panic!("good.js must load: {loaded:?}"));
    let tool = &good.tools[0];
    assert_eq!(tool.name, "good.tool");
    assert_eq!(tool.title.as_deref(), Some("Good"));
    assert!(tool.expose_as_mcp_tool);
    assert!(tool.output_schema.is_some());
    assert_eq!(tool.annotations.as_ref().and_then(|a| a.read_only_hint), Some(true));
    assert!(!good.bytecode.is_empty());

    assert_eq!(errors.len(), 1, "only the unparseable file fails: {errors:?}");
    assert!(errors[0].source_label().contains("broken.js"), "{errors:?}");
    assert!(matches!(errors[0], ExtensionLoadError::Bundle { .. }));
    let _ = std::fs::remove_dir_all(&dir);
  }
}
