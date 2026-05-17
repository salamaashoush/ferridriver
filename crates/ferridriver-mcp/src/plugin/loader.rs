//! Plugin discovery and manifest extraction.
//!
//! At server startup each configured plugin path is read and evaluated in
//! a throwaway `QuickJS` runtime (the same `ScriptEngine` that powers
//! `run_script`, just with no live browser bindings). The plugin must
//! assign its manifest(s) to `globalThis.exports`. Three shapes are
//! accepted, in order of recognition:
//!
//! 1. **Multiple tools, with shared metadata** -- `globalThis.exports = {
//!    tools: [ {...}, {...} ] }`. The outer object may carry future
//!    bundle-level fields; only `tools` is consumed today.
//! 2. **Multiple tools, plain array** -- `globalThis.exports = [ {...},
//!    {...} ]`. Equivalent to (1) with no outer metadata.
//! 3. **Single tool** -- `globalThis.exports = { name, description,
//!    inputSchema, allow, exposeAsTool, handler }`. Back-compat with
//!    the original single-tool format. Treated as `tools: [exports]`.
//!
//! The loader strips every `handler` (functions aren't JSON-serialisable)
//! and serialises the rest, which then deserialises into a `Vec<PluginManifest>`.
//! The full source text is retained on [`LoadedPlugin`] so the binding-
//! install path can re-evaluate the file ONCE inside each `run_script`
//! invocation and register every tool the file declares.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ferridriver_script::{InMemoryVars, Outcome, PathSandbox, RunContext, RunOptions, ScriptEngine};

use super::manifest::PluginManifest;

/// A plugin source file that has been discovered, parsed, and validated.
///
/// Carries every tool declared in the file plus the original source so
/// the binding-install path can re-evaluate the file inside the live
/// script context once and bind each tool under `plugins.<name>`.
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
  /// One manifest per tool declared in the source file. At least one.
  pub tools: Vec<PluginManifest>,
  /// Shared (`Arc`) so each session VM that installs this plugin takes a
  /// refcount bump, not a full copy of the source text. Drives the
  /// source-eval fallback when `bytecode` is `None`.
  pub source: Arc<str>,
  /// Pre-compiled wrapper bytecode, filled in by `load_plugins` once the
  /// file's registry index is known. `None` until then (and if compile
  /// fails — the session VM then parses `source` instead).
  pub bytecode: Option<Arc<[u8]>>,
  pub path: PathBuf,
}

/// Failure modes the loader can surface.
#[derive(Debug)]
pub enum PluginLoadError {
  Io { path: PathBuf, error: std::io::Error },
  Eval { path: PathBuf, message: String },
  ManifestMissing { path: PathBuf },
  ManifestInvalid { path: PathBuf, error: serde_json::Error },
  ManifestNoTools { path: PathBuf },
  SandboxInit { error: String },
}

impl std::fmt::Display for PluginLoadError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Io { path, error } => write!(f, "read {}: {error}", path.display()),
      Self::Eval { path, message } => write!(f, "eval {}: {message}", path.display()),
      Self::ManifestMissing { path } => write!(f, "{}: globalThis.exports not set after eval", path.display()),
      Self::ManifestInvalid { path, error } => write!(f, "{}: manifest invalid: {error}", path.display()),
      Self::ManifestNoTools { path } => write!(f, "{}: no tools declared in globalThis.exports", path.display()),
      Self::SandboxInit { error } => write!(f, "sandbox init: {error}"),
    }
  }
}

impl std::error::Error for PluginLoadError {}

/// Load a single plugin file: read source, eval to capture every tool
/// manifest, return [`LoadedPlugin`] with `tools.len() >= 1`. Handler
/// functions are NOT evaluated here -- only the metadata fields.
///
/// `engine` is the live script engine the rest of the server uses; we
/// reuse its config (timeout, memory limits) so plugin authors get the
/// same behaviour at startup as at runtime.
///
/// # Errors
///
/// Returns [`PluginLoadError`] if the file cannot be read, the manifest
/// extractor script fails to evaluate, `globalThis.exports` is missing
/// after eval, the captured JSON cannot be deserialised, or no tools
/// were declared.
pub async fn load_plugin(path: &Path, engine: &ScriptEngine) -> Result<LoadedPlugin, PluginLoadError> {
  let source = std::fs::read_to_string(path).map_err(|error| PluginLoadError::Io {
    path: path.to_path_buf(),
    error,
  })?;

  // Throwaway sandbox rooted at the plugin's parent directory. Only used
  // so the engine's `fs` global has SOMETHING to bind to -- plugin
  // manifest extraction itself doesn't touch the filesystem.
  let parent = path.parent().unwrap_or_else(|| Path::new("."));
  let sandbox = PathSandbox::new(parent).map_err(|e| PluginLoadError::SandboxInit { error: e.message })?;

  let vars = Arc::new(InMemoryVars::default());
  let ctx = RunContext {
    vars,
    sandbox: Arc::new(sandbox),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    plugins: Vec::new(),
  };

  // Eval the plugin source, then normalise `globalThis.exports` into an
  // array of tool manifests. Three shapes are accepted -- see module
  // docs. Each manifest gets its `handler` stripped before JSON
  // serialisation (functions don't survive JSON).
  let extractor = format!(
    "{source}\n\
     const __exp = globalThis.exports;\n\
     if (typeof __exp !== 'object' || __exp === null) {{ return null; }}\n\
     let __tools;\n\
     if (Array.isArray(__exp)) {{ __tools = __exp; }}\n\
     else if (Array.isArray(__exp.tools)) {{ __tools = __exp.tools; }}\n\
     else {{ __tools = [__exp]; }}\n\
     const __clean = __tools.map((t) => {{\n\
       const m = {{ ...t }};\n\
       delete m.handler;\n\
       return m;\n\
     }});\n\
     return JSON.stringify(__clean);\n"
  );

  let result = engine.run(&extractor, &[], RunOptions::default(), ctx).await;

  let manifest_json = match result.outcome {
    Outcome::Ok { success } => match success.value {
      serde_json::Value::String(s) => s,
      serde_json::Value::Null => {
        return Err(PluginLoadError::ManifestMissing {
          path: path.to_path_buf(),
        });
      },
      other => other.to_string(),
    },
    Outcome::Error { error } => {
      return Err(PluginLoadError::Eval {
        path: path.to_path_buf(),
        message: format!("{:?}: {}", error.kind, error.message),
      });
    },
  };

  let tools: Vec<PluginManifest> =
    serde_json::from_str(&manifest_json).map_err(|error| PluginLoadError::ManifestInvalid {
      path: path.to_path_buf(),
      error,
    })?;

  if tools.is_empty() {
    return Err(PluginLoadError::ManifestNoTools {
      path: path.to_path_buf(),
    });
  }

  Ok(LoadedPlugin {
    tools,
    source: Arc::<str>::from(source),
    // Filled in by `load_plugins` once the registry index is assigned.
    bytecode: None,
    path: path.to_path_buf(),
  })
}

/// Discover plugin files under a path. Directories are scanned shallowly
/// for `*.js` / `*.mjs` files. Single files are returned as-is. Anything
/// else (symlinks, unreadable entries) is reported as an io error.
///
/// # Errors
///
/// Returns [`PluginLoadError::Io`] when the path cannot be stat'd or
/// listed.
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

  let read = std::fs::read_dir(path).map_err(|error| PluginLoadError::Io {
    path: path.to_path_buf(),
    error,
  })?;

  let mut out = Vec::new();
  for entry in read {
    let entry = entry.map_err(|error| PluginLoadError::Io {
      path: path.to_path_buf(),
      error,
    })?;
    let p = entry.path();
    if p.is_file()
      && let Some(ext) = p.extension().and_then(|e| e.to_str())
      && (ext == "js" || ext == "mjs")
    {
      out.push(p);
    }
  }
  out.sort();
  Ok(out)
}
