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
use crate::provided_modules::{PackageClaims, ProvidedModuleTable, package_label};
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
  /// Import specifiers the surviving packages serve, merged and
  /// conflict-checked. Its own errors and warnings are folded into
  /// [`Self::issues`], because a package whose claim is refused is a
  /// package the operator has to hear about.
  pub provided: ProvidedModuleTable,
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
  // Providers first, in dependency order: every entry that imports a
  // claimed specifier links against a module that must already be
  // there, and a provider importing another package's specifier has to
  // follow it.
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

  // Specifier claims are decided over the packages that SURVIVED the
  // gate: a package held back for an unmet requirement must not also
  // take a specifier with it.
  let claims: Vec<PackageClaims> = resolved
    .iter()
    .enumerate()
    .filter(|(_, r)| !blocked.contains(&r.spec))
    .filter_map(|(index, r)| package_claims(index, r))
    .collect();
  let provided = ProvidedModuleTable::build(
    &claims,
    &crate::module_aliases(),
    env.policy,
    &crate::is_reserved_specifier,
  );
  let mut issues = issues;
  for message in &provided.errors {
    issues.push(RequirementIssue {
      source: "extensions".to_string(),
      message: message.clone(),
      blocking: false,
    });
  }
  for message in &provided.warnings {
    issues.push(RequirementIssue {
      source: "extensions".to_string(),
      message: message.clone(),
      blocking: false,
    });
  }

  let mut ordered: Vec<PathBuf> = provided.provider_order().to_vec();
  for f in files {
    if !ordered.contains(&f) {
      ordered.push(f);
    }
  }
  for f in provided.provider_order() {
    if !all_files.contains(f) {
      all_files.push(f.clone());
    }
  }

  GatedExtensions {
    resolved,
    resolve_errors,
    issues,
    blocked,
    files: ordered,
    all_files,
    provided,
  }
}

/// One package's specifier claims, with every path made absolute
/// against the package directory and the directory canonicalized — two
/// specs that reach the same directory are one package, however they
/// were spelled.
fn package_claims(index: usize, resolved: &ResolvedExtension) -> Option<PackageClaims> {
  let manifest = resolved.manifest.as_ref()?;
  if manifest.provides.is_empty() {
    return None;
  }
  let package_dir = resolved.package_dir.clone()?;
  let package_dir = std::fs::canonicalize(&package_dir).unwrap_or(package_dir);
  Some(PackageClaims {
    package: package_label(manifest.name.as_deref(), &package_dir),
    modules: manifest
      .provides
      .modules
      .iter()
      .map(|(specifier, file)| (specifier.clone(), package_dir.join(file)))
      .collect(),
    aliases: manifest.provides.aliases.clone(),
    imports: std::collections::BTreeSet::new(),
    package_dir,
    index,
  })
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
  let mut gated = gate(specs, env);
  // The claim table has to be installed BEFORE anything bundles: it
  // decides which specifiers stay external, which module name a
  // provider compiles under, and what every later resolver accepts.
  if let Err(e) = crate::provided_modules::set_provided_modules(std::mem::take(&mut gated.provided)) {
    gated.issues.push(RequirementIssue {
      source: "extensions".to_string(),
      message: e,
      blocking: false,
    });
  }
  // Re-read what is installed, so the caller sees the table the process
  // actually resolves against rather than the one it just offered.
  gated.provided = ProvidedModuleTable::clone_of(&crate::provided_modules::provided_modules());
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
      source_map: Some(cp.mapper()),
      provides: crate::provided_modules::provider_module_name(&cp.path),
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
    })
    .collect()
}
