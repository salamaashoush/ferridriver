//! The one path every host takes to turn configured `extensions` into
//! loadable bytecode: resolve the specs, check each package's declared
//! preconditions, drop the packages that cannot work, compile and
//! extract what is left.
//!
//! Before this, four hosts did four different things. The MCP server
//! resolved, gated and compiled; `ferridriver run` compiled without
//! gating; `ferridriver bdd` appended extension SOURCE to the step
//! bundle, so an extension was neither gated nor manifest-extracted and
//! the operator ceiling never reached it; and `ferridriver test` loaded
//! nothing at all. A package that works under one host has to work
//! under the others, which it cannot do while each host decides for
//! itself what loading means.

use std::path::PathBuf;

use ferridriver_config::{ExtensionPolicyConfig, ExtensionSpec};

use crate::error::ScriptError;
use crate::requirements::{RequirementEnv, RequirementIssue};
use crate::{CompiledExtension, ExtensionBinding, ResolvedExtension};

/// What the gate decided, in full: the callers that report
/// (`ferridriver ext check`, `config doctor`) need the packages that
/// were blocked and the issues that did not block, not only the
/// survivors.
pub struct GatedExtensions {
  /// Every spec that resolved, blocked or not.
  pub resolved: Vec<ResolvedExtension>,
  /// Specs that failed to resolve at all.
  pub resolve_errors: Vec<(String, ScriptError)>,
  /// Unmet or questionable requirements, blocking and not.
  pub issues: Vec<RequirementIssue>,
  /// Specs held back by a blocking issue.
  pub blocked: Vec<String>,
  /// Entry files of the packages that survived the gate, in resolution
  /// order, deduplicated keeping the first occurrence — a manifest's
  /// `entries` order is the author's load order.
  pub files: Vec<PathBuf>,
  /// Every resolved entry file, gate ignored. What a report shows as
  /// "declared", against which `files` is what actually loads.
  pub all_files: Vec<PathBuf>,
}

impl GatedExtensions {
  /// Blocking issues only — the ones that held a package back.
  pub fn blocking_issues(&self) -> impl Iterator<Item = &RequirementIssue> {
    self.issues.iter().filter(|i| i.blocking)
  }

  /// Issues worth reporting that did not block anything.
  pub fn warnings(&self) -> impl Iterator<Item = &RequirementIssue> {
    self.issues.iter().filter(|i| !i.blocking)
  }
}

/// Resolve + gate, without compiling anything.
#[must_use]
pub fn gate(specs: &[ExtensionSpec], env: &RequirementEnv<'_>) -> GatedExtensions {
  let (resolved, resolve_errors) = crate::discover::resolve_extensions(specs);
  let issues = crate::requirements::check(&resolved, env);
  let blocked = crate::requirements::blocked_specs(&resolved, &issues);

  let mut files: Vec<PathBuf> = Vec::new();
  let mut all_files: Vec<PathBuf> = Vec::new();
  for r in &resolved {
    for f in &r.files {
      if !all_files.contains(f) {
        all_files.push(f.clone());
      }
      if !blocked.contains(&r.spec) && !files.contains(f) {
        files.push(f.clone());
      }
    }
  }

  GatedExtensions {
    resolved,
    resolve_errors,
    issues,
    blocked,
    files,
    all_files,
  }
}

/// Resolve, gate, compile and extract. The compiled output carries both
/// the bytecode a session loads and the manifests a report reads, so a
/// host takes whichever half it needs from one pass.
///
/// Nothing here fails the caller: a spec that will not resolve, a
/// package the gate blocks and a file that will not compile are all
/// reported and skipped, because one broken extension must not take the
/// host down with it.
pub async fn load(
  specs: &[ExtensionSpec],
  env: &RequirementEnv<'_>,
  policy: &ExtensionPolicyConfig,
) -> (GatedExtensions, Vec<CompiledExtension>, Vec<(PathBuf, ScriptError)>) {
  let gated = gate(specs, env);
  if gated.files.is_empty() {
    return (gated, Vec::new(), Vec::new());
  }
  // rolldown resolves a bundle entry from an absolute id; a relative
  // path (`extensions = ["gateway.ts"]`) would fail with UnresolvedEntry.
  let mut compile_failures = Vec::new();
  let files: Vec<PathBuf> = gated
    .files
    .iter()
    .filter_map(|f| match std::fs::canonicalize(f) {
      Ok(abs) => Some(abs),
      Err(e) => {
        compile_failures.push((f.clone(), ScriptError::internal(format!("{}: {e}", f.display()))));
        None
      },
    })
    .collect();
  let (compiled, failures) = crate::compile_and_extract_extensions(&files, policy).await;
  compile_failures.extend(failures);
  (gated, compiled, compile_failures)
}

/// [`load`], reduced to what a session VM needs, with every diagnostic
/// logged. The shape three of the four hosts want.
pub async fn load_bindings(
  specs: &[ExtensionSpec],
  env: &RequirementEnv<'_>,
  policy: &ExtensionPolicyConfig,
) -> Vec<ExtensionBinding> {
  if specs.is_empty() {
    return Vec::new();
  }
  let (gated, compiled, failures) = load(specs, env, policy).await;
  for (spec, e) in &gated.resolve_errors {
    tracing::warn!(target: "ferridriver::extensions", extension = %spec, error = %e.message, "extension discovery failed; skipping");
  }
  for issue in &gated.issues {
    if issue.blocking {
      tracing::error!(target: "ferridriver::extensions", source = %issue.source, "extension package requirement unmet: {}", issue.message);
    } else {
      tracing::warn!(target: "ferridriver::extensions", source = %issue.source, "{}", issue.message);
    }
  }
  for (path, e) in &failures {
    tracing::warn!(target: "ferridriver::extensions", path = %path.display(), error = %e.message, "extension compile failed; skipping");
  }
  compiled
    .into_iter()
    .map(|cp| ExtensionBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
    })
    .collect()
}
