//! The scripting environment a `ferridriver` process hands to the engine.
//!
//! `ferridriver run` and a `ferridriver session host` resolve the same things
//! from the same config document — the `fs` and `artifacts` sandboxes, the
//! sandbox relaxations, the loaded extensions, the engine limits and declared
//! sidecars — so they resolve them here, once. A script must not behave
//! differently depending on whether it runs locally or against a bound
//! session.

use std::path::Path;
use std::sync::Arc;

use ferridriver_config::FerridriverConfig;

/// The resolved scripting environment.
pub struct ScriptSetup {
  /// Root for `fs.*`.
  pub sandbox: Arc<ferridriver_script::PathSandbox>,
  /// Root for `artifacts.*`, or `None` when it could not be prepared.
  pub artifacts: Option<Arc<ferridriver_script::PathSandbox>>,
  pub caps: ferridriver_script::ScriptCaps,
  pub extensions: Vec<ferridriver_script::ExtensionBinding>,
  pub engine: ferridriver_script::ScriptEngineConfig,
  /// Values redacted from everything handed back. Also on `engine`, which
  /// covers the run itself; this copy is for what the host renders around it
  /// (the response sections and the echoed code).
  pub secrets: ferridriver::response::Secrets,
  /// Ceiling on the artifacts root, when the operator set one.
  pub artifacts_budget: Option<ferridriver::response::OutputBudget>,
}

/// Resolve the scripting environment for `cwd` from `config`, adding any
/// `--extension` specs the caller typed (which resolve relative to `cwd`,
/// while config entries keep their declaring layer's directory).
///
/// # Errors
///
/// Returns an error only when the `fs` sandbox root itself cannot be prepared;
/// a missing artifacts root degrades to `None` (scripts can still write
/// through `fs`), and an unloadable extension is warned about and skipped.
pub async fn resolve(
  config: &FerridriverConfig,
  cwd: &Path,
  extra_extensions: &[String],
) -> anyhow::Result<ScriptSetup> {
  let sandbox = Arc::new(
    ferridriver_script::PathSandbox::new(cwd)
      .map_err(|e| anyhow::anyhow!("sandbox init ({}): {}", cwd.display(), e.message))?,
  );

  let artifacts_root = config.artifacts_root();
  let artifacts = match std::fs::create_dir_all(&artifacts_root)
    .map_err(|e| e.to_string())
    .and_then(|()| ferridriver_script::PathSandbox::new(&artifacts_root).map_err(|e| e.message.clone()))
  {
    Ok(sandbox) => Some(Arc::new(sandbox)),
    Err(e) => {
      tracing::warn!(
        artifacts_root = %artifacts_root.display(),
        error = %e,
        "artifacts binding disabled: could not prepare artifacts_root; scripts can still write via fs"
      );
      None
    },
  };

  let caps = ferridriver_script::ScriptCaps::resolve_with_commands(
    &config.scripting.allow_env,
    config.scripting.allow.commands.clone(),
  )
  .with_extension_policy(config.extensions.policy())
  .with_extension_settings(config.extensions.settings());

  let mut roots = config.extension_specs();
  roots.extend(extra_extensions.iter().map(|spec| ferridriver_script::ExtensionSpec {
    spec: spec.clone(),
    base_dir: cwd.to_path_buf(),
  }));
  let extensions = load_extensions(&roots).await;

  // A misconfigured secrets source fails the run: silently continuing would
  // mean an operator who asked for redaction gets none and is not told.
  let secrets = ferridriver::response::Secrets::new(config.secrets.resolve()?);

  let artifacts_budget = config.artifacts_max_bytes.map(ferridriver::response::OutputBudget::new);

  let engine = ferridriver_script::ScriptEngineConfig {
    sidecars: sidecar_specs(config),
    secrets: secrets.clone(),
    artifacts_budget,
    ..Default::default()
  };

  Ok(ScriptSetup {
    sandbox,
    artifacts,
    caps,
    extensions,
    engine,
    secrets,
    artifacts_budget,
  })
}

/// Resolve, compile and extract every configured extension. A spec that fails
/// to resolve or compile is warned about and skipped: one broken extension
/// must not take down the run.
async fn load_extensions(roots: &[ferridriver_script::ExtensionSpec]) -> Vec<ferridriver_script::ExtensionBinding> {
  if roots.is_empty() {
    return Vec::new();
  }
  let (mut files, errors) = ferridriver_script::discover::resolve_extension_specs_with_bases(roots);
  for (spec, e) in errors {
    tracing::warn!(extension = %spec, error = %e.message, "extension discovery failed; skipping");
  }
  if files.is_empty() {
    return Vec::new();
  }
  // rolldown resolves the bundle entry from an absolute id; a relative
  // path (e.g. `extensions = ["gateway.ts"]` in ferridriver.toml) would
  // fail with UnresolvedEntry. Canonicalize, dropping any that vanished.
  files = files
    .into_iter()
    .filter_map(|f| match std::fs::canonicalize(&f) {
      Ok(abs) => Some(abs),
      Err(e) => {
        tracing::warn!(path = %f.display(), error = %e, "extension path not found; skipping");
        None
      },
    })
    .collect();
  let (compiled, failures) = ferridriver_script::compile_and_extract_extensions(&files).await;
  for (path, err) in failures {
    tracing::warn!(path = %path.display(), error = %err.message, "extension compile failed; skipping");
  }
  compiled
    .into_iter()
    .map(|cp| ferridriver_script::ExtensionBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
    })
    .collect()
}

/// Lower the declared `[[sidecars]]` config entries into the scripting
/// engine's `SidecarSpec`s. Shared by the `run`, `mcp`, `bdd` and session-host
/// paths so the same config table drives every host.
#[must_use]
pub fn sidecar_specs(config: &FerridriverConfig) -> Vec<ferridriver_script::sidecar::SidecarSpec> {
  config
    .sidecars
    .iter()
    .map(|s| ferridriver_script::sidecar::SidecarSpec {
      name: s.name.clone(),
      command: s.command.clone(),
      env: s
        .env
        .as_ref()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default(),
      cwd: s.cwd.clone(),
    })
    .collect()
}
