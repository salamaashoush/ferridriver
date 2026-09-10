//! The cached BDD step engine for the `run_bdd` tool.
//!
//! One loaded [`JsBddSession`] (step VM + registry, with `BeforeAll`
//! already run) is held on the server and reused across calls and across
//! browser sessions — the world is bound per scenario, so a single engine
//! serves every session. One engine is enough: a server runs one step-set
//! in practice, and a single VM (one global world binding) can't run
//! scenarios concurrently anyway, so the engine's lock serializes build +
//! run.
//!
//! Reuse policy:
//! - **fast path** (no bundling at all) when the step-set key matches and
//!   an mtime check shows sources unchanged — the disk bundle cache would
//!   otherwise re-read + hash every transitive input;
//! - **reload** (rebuild + `AfterAll` on the old engine) when the step-set
//!   changes or the compiled bundle's content hash changes (a source edit);
//! - a failed build is never cached — the slot stays empty and the next
//!   call retries.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use ferridriver_bdd::js::{self, JsBddSession, discover_extension_files, discover_step_files};
use ferridriver_script::CompiledBundle;
use ferridriver_script::CompiledBundleExt as _;

/// Stable hash of the resolved step-set (sorted globs + sorted extensions +
/// world parameters). A change means a different engine must be loaded.
#[must_use]
pub fn logical_key(
  globs: &[String],
  extensions: &[ferridriver_config::ExtensionSpec],
  world_params: &serde_json::Value,
) -> u64 {
  let mut h = std::collections::hash_map::DefaultHasher::new();
  let mut g: Vec<&String> = globs.iter().collect();
  g.sort();
  g.hash(&mut h);
  let mut e: Vec<&ferridriver_config::ExtensionSpec> = extensions.iter().collect();
  e.sort();
  e.hash(&mut h);
  world_params.to_string().hash(&mut h);
  h.finish()
}

/// Content hash of the compiled bundle — changes iff the compiled step
/// graph changes, so an edited source forces a rebuild.
fn content_hash(bytecode: &[u8]) -> u64 {
  let mut h = std::collections::hash_map::DefaultHasher::new();
  bytecode.hash(&mut h);
  h.finish()
}

fn mtime(p: &Path) -> Option<SystemTime> {
  std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// The single cached step engine plus the metadata to decide reuse vs
/// reload. Guarded by one async lock on the server; that lock is the
/// build+run guard.
pub struct BddEngine {
  /// Step-set identity the loaded engine was built for.
  key: u64,
  engine: Option<Arc<JsBddSession>>,
  content_hash: u64,
  /// Entry files discovered at the last build — detects added/removed
  /// step/extension files even when no existing file's mtime moved.
  entries: Vec<PathBuf>,
  /// All bundle inputs (entries + transitive imports) with their mtime at
  /// build time — detects edits without re-reading file contents.
  inputs: Vec<(PathBuf, Option<SystemTime>)>,
}

impl Default for BddEngine {
  fn default() -> Self {
    Self::new()
  }
}

impl BddEngine {
  #[must_use]
  pub fn new() -> Self {
    Self {
      key: 0,
      engine: None,
      content_hash: 0,
      entries: Vec::new(),
      inputs: Vec::new(),
    }
  }

  fn discover_entries(globs: &[String], extensions: &[ferridriver_config::ExtensionSpec], cwd: &Path) -> Vec<PathBuf> {
    let mut entries = discover_step_files(globs, cwd);
    entries.extend(discover_extension_files(extensions));
    entries.sort();
    entries.dedup();
    entries
  }

  /// True when the entry set changed or any recorded input's mtime moved.
  /// Stats only (no content reads) — the cheap change signal.
  fn sources_changed(&self, globs: &[String], extensions: &[ferridriver_config::ExtensionSpec], cwd: &Path) -> bool {
    if Self::discover_entries(globs, extensions, cwd) != self.entries {
      return true;
    }
    self.inputs.iter().any(|(p, t)| mtime(p) != *t)
  }

  fn record_inputs(
    &mut self,
    bundle: &CompiledBundle,
    globs: &[String],
    extensions: &[ferridriver_config::ExtensionSpec],
    cwd: &Path,
  ) {
    self.entries = Self::discover_entries(globs, extensions, cwd);
    // Inputs = entries ∪ transitive bundle sources, so edits are caught
    // even when the source map is absent (then `source_files` is empty).
    let mut inputs = bundle.source_files(cwd);
    inputs.extend(self.entries.iter().cloned());
    inputs.sort();
    inputs.dedup();
    self.inputs = inputs.into_iter().map(|p| (p.clone(), mtime(&p))).collect();
  }

  /// Reuse the loaded engine when the step-set + sources are unchanged;
  /// otherwise (re)bundle + (re)load, running `AfterAll` on the old engine
  /// first.
  ///
  /// # Errors
  ///
  /// Returns an error if bundling the step/extension files fails (e.g. a
  /// syntax error or unresolved import) or the step module fails to load.
  ///
  /// Boxed at the definition: it awaits the session build, whose
  /// future carries the whole engine config, and the caller would
  /// otherwise carry it too (`clippy::large_futures`).
  pub fn ensure<'a>(
    &'a mut self,
    key: u64,
    globs: &'a [String],
    extensions: &'a [ferridriver_config::ExtensionSpec],
    world_params: serde_json::Value,
    cwd: &'a Path,
  ) -> impl std::future::Future<Output = anyhow::Result<Arc<JsBddSession>>> + Send + 'a {
    Box::pin(async move {
      // Fast path: same step-set, warm engine, unchanged sources -> no bundle.
      if let Some(engine) = &self.engine
        && self.key == key
        && !self.sources_changed(globs, extensions, cwd)
      {
        return Ok(Arc::clone(engine));
      }

      // Bundle (disk-cached) and decide reuse-vs-rebuild by content hash.
      let bundle = js::bundle_steps_with(globs, extensions, cwd).await?;
      let ch = content_hash(&bundle.bytecode);
      if let Some(engine) = &self.engine
        && self.key == key
        && self.content_hash == ch
      {
        // Same step-set + identical compiled output (e.g. a `touch`):
        // refresh recorded mtimes so the fast path holds next time.
        let engine = Arc::clone(engine);
        self.record_inputs(&bundle, globs, extensions, cwd);
        return Ok(engine);
      }

      // Rebuild. Tear down the old engine's suite (AfterAll) first.
      if let Some(old) = self.engine.take()
        && let Err(e) = old.after_all().await
      {
        tracing::warn!(error = %e, "run_bdd: AfterAll on reload failed");
      }
      // Extensions are installed as compiled bytecode, gated by the loader
      // every host shares — never bundled into the step module. The caps
      // are the ones the tool call just threaded in, so a package's
      // `requires` is answered by the environment its tools will run in.
      let caps = js::bdd_script_caps();
      let sidecar_names: Vec<String> = js::bdd_sidecars().iter().map(|s| s.name.clone()).collect();
      let env = ferridriver_script::RequirementEnv::from_caps(&caps, &sidecar_names);
      let bindings = ferridriver_script::load_bindings(
        extensions,
        &env,
        &caps.extension_policy,
        ferridriver_script::ExtensionHost::Bdd,
      )
      .await;
      // The MCP host runs scenarios against a live browser session, not a
      // `[test]` config layer, so there is no `use` block to decide.
      let session = Arc::new(
        JsBddSession::load(
          Arc::clone(&bundle),
          cwd,
          &ferridriver_bdd::js::BddSessionSetup {
            world_parameters: world_params,
            extensions: Arc::new(bindings),
            ..Default::default()
          },
        )
        .await?,
      );
      self.key = key;
      self.content_hash = ch;
      self.record_inputs(&bundle, globs, extensions, cwd);
      self.engine = Some(Arc::clone(&session));
      Ok(session)
    })
  }
}
