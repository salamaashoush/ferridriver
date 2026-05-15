//! Plugin discovery and manifest extraction.
//!
//! At server startup each configured plugin path is read and evaluated in
//! a throwaway `QuickJS` runtime (the same `ScriptEngine` that powers
//! `run_script`, just with no live browser bindings). The plugin must
//! assign its manifest to `globalThis.exports`. The loader strips the
//! `handler` function and serialises the remainder as JSON, which then
//! deserialises into [`PluginManifest`].
//!
//! The full source text is retained on [`LoadedPlugin`] -- it gets
//! re-evaluated inside each `run_script` invocation so the handler closure
//! captures the live `page`/`context`/`request` globals.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ferridriver_script::{InMemoryVars, Outcome, PathSandbox, RunContext, RunOptions, ScriptEngine};

use super::manifest::PluginManifest;

/// A plugin that has been discovered, parsed, and validated.
///
/// Carries the manifest plus the original source so the binding-install
/// path can re-evaluate the file inside the live script context.
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
  pub manifest: PluginManifest,
  pub source: String,
  pub path: PathBuf,
}

/// Failure modes the loader can surface.
#[derive(Debug)]
pub enum PluginLoadError {
  Io { path: PathBuf, error: std::io::Error },
  Eval { path: PathBuf, message: String },
  ManifestMissing { path: PathBuf },
  ManifestInvalid { path: PathBuf, error: serde_json::Error },
  SandboxInit { error: String },
}

impl std::fmt::Display for PluginLoadError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Io { path, error } => write!(f, "read {}: {error}", path.display()),
      Self::Eval { path, message } => write!(f, "eval {}: {message}", path.display()),
      Self::ManifestMissing { path } => write!(f, "{}: globalThis.exports not set after eval", path.display()),
      Self::ManifestInvalid { path, error } => write!(f, "{}: manifest invalid: {error}", path.display()),
      Self::SandboxInit { error } => write!(f, "sandbox init: {error}"),
    }
  }
}

impl std::error::Error for PluginLoadError {}

/// Load a single plugin file: read source, eval to capture the manifest,
/// return both. The handler is NOT evaluated here -- only the manifest fields.
///
/// `engine` is the live script engine the rest of the server uses; we
/// reuse its config (timeout, memory limits) so plugin authors get the
/// same behaviour at startup as at runtime.
///
/// # Errors
///
/// Returns [`PluginLoadError`] if the file cannot be read, the manifest
/// extractor script fails to evaluate, `globalThis.exports` is missing
/// after eval, or the captured JSON cannot be deserialised into a manifest.
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

  // Eval the plugin source, then synthesise an extractor that strips the
  // handler (functions aren't JSON-serialisable) and returns the rest as
  // a JSON string. The `return` flows back through ScriptResult::value.
  let extractor = format!(
    "{source}\n\
     if (typeof globalThis.exports !== 'object' || globalThis.exports === null) {{ return null; }}\n\
     const m = {{ ...globalThis.exports }};\n\
     delete m.handler;\n\
     return JSON.stringify(m);\n"
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

  let manifest: PluginManifest =
    serde_json::from_str(&manifest_json).map_err(|error| PluginLoadError::ManifestInvalid {
      path: path.to_path_buf(),
      error,
    })?;

  Ok(LoadedPlugin {
    manifest,
    source,
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
