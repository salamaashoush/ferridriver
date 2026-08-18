//! Import specifiers packages serve, merged into one table.
//!
//! A package claims a specifier in its manifest (`provides.modules` /
//! `provides.aliases`); everything that imports that specifier — a spec,
//! a step file, another extension — resolves to the package's own
//! module, one instance per VM. That is what lets a suite written
//! against some other package run with no edit to its own source.
//!
//! Claims are process-global and merged from every loaded package, so
//! the rules that decide whether a claim is honoured live here rather
//! than in any one host: a specifier the runtime already serves is not
//! claimable, two packages cannot claim one specifier, an alias may only
//! target a specifier its own package provides, providers may not form a
//! cycle, and the operator's own `moduleAliases` outrank a package.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ferridriver_config::{ExtensionModulesCeiling, ExtensionPolicyConfig};

/// One package's claims, as resolved from its manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageClaims {
  /// Package name for diagnostics — the manifest's `name`, else the
  /// package directory's own name.
  pub package: String,
  /// Canonicalized package directory; two specs resolving to the same
  /// directory are ONE package, however they were spelled.
  pub package_dir: PathBuf,
  /// Position in `extensions = [...]`, which is the tie-break for
  /// otherwise-equal ordering so a run is reproducible.
  pub index: usize,
  /// `specifier -> provider file`, absolute.
  pub modules: BTreeMap<String, PathBuf>,
  /// `specifier -> specifier`, both owned by this package.
  pub aliases: BTreeMap<String, String>,
  /// Claimed specifiers this package's own providers IMPORT, as the
  /// bundler reported them. Filled by the caller from the module graph —
  /// a manifest cannot state it, and it is the only thing that says
  /// which provider must evaluate first.
  pub imports: BTreeSet<String>,
}

/// What one claimed specifier resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvidedModule {
  pub package: String,
  /// The provider file whose module answers this specifier. An alias
  /// carries the file of the specifier it points at.
  pub file: PathBuf,
  /// Set when this entry came from `provides.aliases`.
  pub alias_of: Option<String>,
}

/// The merged claim table plus everything that went wrong building it.
#[derive(Debug, Default)]
pub struct ProvidedModuleTable {
  modules: BTreeMap<String, ProvidedModule>,
  /// Provider files in evaluation order: a provider that imports another
  /// package's provider evaluates after it.
  order: Vec<PathBuf>,
  pub errors: Vec<String>,
  pub warnings: Vec<String>,
}

impl ProvidedModuleTable {
  /// A plain copy of an installed table, for a caller that wants to
  /// report what the process resolved against.
  #[must_use]
  pub fn clone_of(table: &Self) -> Self {
    Self {
      modules: table.modules.clone(),
      order: table.order.clone(),
      errors: table.errors.clone(),
      warnings: table.warnings.clone(),
    }
  }

  #[must_use]
  pub fn get(&self, specifier: &str) -> Option<&ProvidedModule> {
    self.modules.get(specifier)
  }

  #[must_use]
  pub fn specifiers(&self) -> Vec<&str> {
    self.modules.keys().map(String::as_str).collect()
  }

  /// Provider files, in the order they must evaluate.
  #[must_use]
  pub fn provider_order(&self) -> &[PathBuf] {
    &self.order
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.modules.is_empty()
  }

  /// Merge every package's claims, applying the rules in module order:
  /// the operator's table wins, the runtime's own specifiers are not
  /// claimable, one specifier has one owner, an alias stays inside its
  /// package, and providers may not cycle.
  ///
  /// Nothing here reads the filesystem or a VM: the whole decision is a
  /// function of the manifests, the operator's aliases and the policy,
  /// which is what lets every host reach the same verdict.
  #[must_use]
  pub fn build(
    claims: &[PackageClaims],
    operator_aliases: &[(String, String)],
    policy: &ExtensionPolicyConfig,
    reserved: &dyn Fn(&str) -> bool,
  ) -> Self {
    let mut table = Self::default();
    // Declaration order first, `extensions = [...]` index as the
    // tie-break, so two packages claiming one specifier always name the
    // same pair in the same order.
    let mut sorted: Vec<&PackageClaims> = claims.iter().collect();
    sorted.sort_by(|a, b| a.index.cmp(&b.index).then_with(|| a.package.cmp(&b.package)));

    let operator: BTreeSet<&str> = operator_aliases.iter().map(|(from, _)| from.as_str()).collect();
    let mut owner: BTreeMap<String, &PackageClaims> = BTreeMap::new();

    for claim in &sorted {
      for (specifier, file) in &claim.modules {
        if !table.admit(specifier, claim, policy, reserved, &operator, &mut owner) {
          continue;
        }
        table.modules.insert(
          specifier.clone(),
          ProvidedModule {
            package: claim.package.clone(),
            file: file.clone(),
            alias_of: None,
          },
        );
      }
    }

    // Aliases resolve after every module claim, because an alias may
    // point at a specifier declared later in the same package.
    for claim in &sorted {
      for (specifier, target) in &claim.aliases {
        if !table.admit(specifier, claim, policy, reserved, &operator, &mut owner) {
          continue;
        }
        let Some(target_file) = claim.modules.get(target) else {
          table.errors.push(format!(
            "extension `{}`: alias `{specifier}` -> `{target}` targets a specifier this package does not provide; \
             an alias may only point at the package's own `provides.modules`",
            claim.package
          ));
          continue;
        };
        table.modules.insert(
          specifier.clone(),
          ProvidedModule {
            package: claim.package.clone(),
            file: target_file.clone(),
            alias_of: Some(target.clone()),
          },
        );
      }
    }

    table.order = provider_order(&sorted, &table.modules, &mut table.errors);
    table
  }

  /// The rules a claim must pass whatever kind it is. Returns false when
  /// the claim was rejected or superseded (with the diagnostic already
  /// recorded).
  fn admit<'a>(
    &mut self,
    specifier: &str,
    claim: &'a PackageClaims,
    policy: &ExtensionPolicyConfig,
    reserved: &dyn Fn(&str) -> bool,
    operator: &BTreeSet<&str>,
    owner: &mut BTreeMap<String, &'a PackageClaims>,
  ) -> bool {
    match policy.modules {
      ExtensionModulesCeiling::None => {
        self.errors.push(format!(
          "extension `{}`: claims import specifier `{specifier}`, but the operator policy \
           (`[extensions.policy] modules = \"none\"`) forbids packages from providing modules",
          claim.package
        ));
        return false;
      },
      ExtensionModulesCeiling::AllowListed if !policy.allow_modules.iter().any(|m| m == specifier) => {
        self.errors.push(format!(
          "extension `{}`: claims import specifier `{specifier}`, which the operator policy \
           (`[extensions.policy] allowModules`) does not list",
          claim.package
        ));
        return false;
      },
      ExtensionModulesCeiling::Any | ExtensionModulesCeiling::AllowListed => {},
    }

    if reserved(specifier) {
      self.errors.push(format!(
        "extension `{}`: claims import specifier `{specifier}`, which the runtime already serves; \
         a package cannot take over a built-in name",
        claim.package
      ));
      return false;
    }

    // The operator's own table is configuration; a package's claim is a
    // default. Configuration wins, and says so.
    if operator.contains(specifier) {
      self.warnings.push(format!(
        "extension `{}`: claim on `{specifier}` is superseded by the operator's `moduleAliases`",
        claim.package
      ));
      return false;
    }

    if let Some(previous) = owner.get(specifier) {
      if previous.package_dir != claim.package_dir {
        self.errors.push(format!(
          "import specifier `{specifier}` is claimed by two packages: `{}` and `{}`; \
           exactly one package may provide a specifier",
          previous.package, claim.package
        ));
        return false;
      }
      // The same package, reached through two specs — already admitted.
      return false;
    }
    owner.insert(specifier.to_string(), claim);
    true
  }
}

/// Depth-first over the provider dependency edges, recording each
/// package's provider files once its dependencies have been recorded.
fn visit<'a>(
  package: &'a str,
  edges: &BTreeMap<&'a str, BTreeSet<&'a str>>,
  by_package: &BTreeMap<&'a str, &'a PackageClaims>,
  state: &mut BTreeMap<&'a str, u8>,
  order: &mut Vec<PathBuf>,
  errors: &mut Vec<String>,
) {
  match state.get(package) {
    Some(2) => return,
    Some(1) => {
      errors.push(format!(
        "extension providers form a cycle through package `{package}`; \
         a provider cannot import a specifier a package that imports it provides"
      ));
      return;
    },
    _ => {},
  }
  state.insert(package, 1);
  for dep in edges.get(package).into_iter().flatten() {
    visit(dep, edges, by_package, state, order, errors);
  }
  state.insert(package, 2);
  if let Some(claim) = by_package.get(package) {
    for file in claim.modules.values() {
      if !order.contains(file) {
        order.push(file.clone());
      }
    }
  }
}

/// Provider files in evaluation order: a provider importing another
/// package's claimed specifier evaluates after that package's provider.
///
/// A cycle is an error rather than an order, because the module that
/// loses the tie would see the other's exports half-initialised.
fn provider_order(
  claims: &[&PackageClaims],
  modules: &BTreeMap<String, ProvidedModule>,
  errors: &mut Vec<String>,
) -> Vec<PathBuf> {
  // Package -> the packages it depends on, via a specifier one of its
  // providers claims from another package.
  let by_package: BTreeMap<&str, &PackageClaims> = claims.iter().map(|c| (c.package.as_str(), *c)).collect();
  let mut edges: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
  for claim in claims {
    let deps = edges.entry(claim.package.as_str()).or_default();
    for specifier in &claim.imports {
      if let Some(provided) = modules.get(specifier.as_str())
        && provided.package != claim.package
      {
        deps.insert(provided.package.as_str());
      }
    }
  }

  let mut order: Vec<PathBuf> = Vec::new();
  let mut state: BTreeMap<&str, u8> = BTreeMap::new();

  for claim in claims {
    visit(
      claim.package.as_str(),
      &edges,
      &by_package,
      &mut state,
      &mut order,
      errors,
    );
  }
  order
}

/// The claim table this process resolves against, installed once by the
/// host from the gate's verdict.
///
/// Process-global for the same reason the alias table is: a bundle is
/// built and a session is created from several call sites across three
/// crates, and all of them must answer "who serves this specifier?" the
/// same way.
static PROVIDED: std::sync::RwLock<Option<std::sync::Arc<ProvidedModuleTable>>> = std::sync::RwLock::new(None);
static PROVIDED_SEALED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Install the merged table. Rejected once anything has resolved
/// against it: a session built earlier would keep a resolver that never
/// heard of the new specifier, and a bundle keyed earlier would have
/// inlined what should have stayed external.
///
/// # Errors
///
/// When the table is already sealed and `table` is not what it holds.
pub fn set_provided_modules(table: ProvidedModuleTable) -> Result<(), String> {
  let mut guard = PROVIDED.write().unwrap_or_else(std::sync::PoisonError::into_inner);
  if PROVIDED_SEALED.load(std::sync::atomic::Ordering::Acquire) {
    let same = guard.as_ref().is_some_and(|current| current.modules == table.modules);
    return if same {
      Ok(())
    } else {
      Err(format!(
        "provided modules are sealed: `{}` arrived after the first module resolved against the table",
        table.specifiers().join("`, `")
      ))
    };
  }
  *guard = Some(std::sync::Arc::new(table));
  Ok(())
}

/// The installed table, sealing it: whatever reads it is about to make a
/// decision that cannot be revisited.
#[must_use]
pub fn provided_modules() -> std::sync::Arc<ProvidedModuleTable> {
  PROVIDED_SEALED.store(true, std::sync::atomic::Ordering::Release);
  PROVIDED
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone()
    .unwrap_or_default()
}

/// The specifier a claimed one resolves to: itself for a provided
/// module, the alias target for an alias, `None` when nothing claims it.
///
/// One module instance per specifier group falls out of this: the
/// provider's bytecode is loaded under the target's name, and every
/// alias normalises to that name before QuickJS looks the module up.
#[must_use]
pub fn canonical_provided_name(specifier: &str) -> Option<String> {
  let table = provided_modules();
  let provided = table.get(specifier)?;
  Some(provided.alias_of.clone().unwrap_or_else(|| specifier.to_string()))
}

/// Whether any package serves this specifier.
#[must_use]
pub fn is_provided_specifier(specifier: &str) -> bool {
  canonical_provided_name(specifier).is_some()
}

/// The module name a provider file is compiled under: the first
/// specifier it serves, so the loaded module IS the specifier and
/// nothing has to re-export it.
#[must_use]
pub fn provider_module_name(file: &Path) -> Option<String> {
  let table = provided_modules();
  table
    .modules
    .iter()
    .filter(|(_, provided)| provided.alias_of.is_none() && provided.file == file)
    .map(|(specifier, _)| specifier.clone())
    .next()
}

/// Fingerprint of the claim table, folded into every bundle cache key:
/// a claim flips a specifier between "external bare import" and
/// "inlined into the chunk", which changes output for identical inputs.
#[must_use]
pub fn provided_fingerprint() -> u64 {
  use std::hash::{Hash, Hasher};
  let table = provided_modules();
  let mut h = std::collections::hash_map::DefaultHasher::new();
  for (specifier, provided) in &table.modules {
    specifier.hash(&mut h);
    provided.file.hash(&mut h);
    provided.alias_of.hash(&mut h);
  }
  h.finish()
}

/// A package's name for diagnostics: the manifest's own, else the
/// directory's.
#[must_use]
pub fn package_label(manifest_name: Option<&str>, package_dir: &Path) -> String {
  manifest_name.map_or_else(
    || {
      package_dir.file_name().map_or_else(
        || package_dir.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
      )
    },
    str::to_string,
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  fn claim(index: usize, package: &str, modules: &[(&str, &str)]) -> PackageClaims {
    PackageClaims {
      package: package.to_string(),
      package_dir: PathBuf::from(format!("/pkgs/{package}")),
      index,
      modules: modules
        .iter()
        .map(|(spec, file)| ((*spec).to_string(), PathBuf::from(format!("/pkgs/{package}/{file}"))))
        .collect(),
      aliases: BTreeMap::new(),
      imports: BTreeSet::new(),
    }
  }

  fn reserved(specifier: &str) -> bool {
    matches!(specifier, "ferridriver" | "@ferridriver/test" | "fs")
  }

  fn build(
    claims: &[PackageClaims],
    operator: &[(String, String)],
    policy: &ExtensionPolicyConfig,
  ) -> ProvidedModuleTable {
    ProvidedModuleTable::build(claims, operator, policy, &reserved)
  }

  #[test]
  fn a_claim_resolves_to_its_package_file() {
    let table = build(
      &[claim(0, "vendor", &[("fake-vendor", "provide.ts")])],
      &[],
      &ExtensionPolicyConfig::default(),
    );
    assert!(table.errors.is_empty(), "{:?}", table.errors);
    let provided = table.get("fake-vendor").expect("claimed");
    assert_eq!(provided.package, "vendor");
    assert_eq!(provided.file, PathBuf::from("/pkgs/vendor/provide.ts"));
  }

  #[test]
  fn a_specifier_the_runtime_serves_cannot_be_claimed() {
    let table = build(
      &[claim(0, "hijack", &[("@ferridriver/test", "mine.ts")])],
      &[],
      &ExtensionPolicyConfig::default(),
    );
    assert!(table.get("@ferridriver/test").is_none(), "the claim must be refused");
    assert!(
      table
        .errors
        .iter()
        .any(|e| e.contains("hijack") && e.contains("@ferridriver/test")),
      "the error must name package and specifier: {:?}",
      table.errors
    );
  }

  #[test]
  fn two_packages_claiming_one_specifier_is_an_error_naming_both() {
    let table = build(
      &[
        claim(0, "alpha", &[("shared", "a.ts")]),
        claim(1, "beta", &[("shared", "b.ts")]),
      ],
      &[],
      &ExtensionPolicyConfig::default(),
    );
    let message = table.errors.join("\n");
    assert!(message.contains("alpha") && message.contains("beta"), "{message}");
    // The first declaration keeps the specifier, so the outcome does not
    // depend on map iteration order.
    assert_eq!(table.get("shared").expect("first claim wins").package, "alpha");
  }

  #[test]
  fn an_operator_alias_beats_a_package_claim_and_says_so() {
    let table = build(
      &[claim(0, "vendor", &[("fake-vendor", "provide.ts")])],
      &[("fake-vendor".to_string(), "ferridriver".to_string())],
      &ExtensionPolicyConfig::default(),
    );
    assert!(table.get("fake-vendor").is_none(), "configuration outranks a package");
    assert!(
      table
        .warnings
        .iter()
        .any(|w| w.contains("vendor") && w.contains("moduleAliases")),
      "{:?}",
      table.warnings
    );
    assert!(table.errors.is_empty(), "being superseded is not an error");
  }

  #[test]
  fn the_policy_ceiling_refuses_claims_and_names_its_key() {
    let none = ExtensionPolicyConfig {
      modules: ExtensionModulesCeiling::None,
      ..ExtensionPolicyConfig::default()
    };
    let table = build(&[claim(0, "vendor", &[("fake-vendor", "p.ts")])], &[], &none);
    assert!(table.get("fake-vendor").is_none());
    assert!(
      table.errors.iter().any(|e| e.contains("modules = \"none\"")),
      "{:?}",
      table.errors
    );

    let listed = ExtensionPolicyConfig {
      modules: ExtensionModulesCeiling::AllowListed,
      allow_modules: vec!["allowed".to_string()],
      ..ExtensionPolicyConfig::default()
    };
    let table = build(
      &[claim(0, "vendor", &[("allowed", "a.ts"), ("denied", "d.ts")])],
      &[],
      &listed,
    );
    assert!(table.get("allowed").is_some(), "the listed specifier is claimable");
    assert!(table.get("denied").is_none(), "the unlisted one is not");
    assert!(
      table
        .errors
        .iter()
        .any(|e| e.contains("allowModules") && e.contains("denied")),
      "{:?}",
      table.errors
    );
  }

  #[test]
  fn an_alias_may_only_target_its_own_packages_specifier() {
    let mut good = claim(0, "vendor", &[("fake-vendor", "provide.ts")]);
    good
      .aliases
      .insert("fake-vendor/sub".to_string(), "fake-vendor".to_string());
    let mut bad = claim(1, "other", &[("other-thing", "o.ts")]);
    bad.aliases.insert("stolen".to_string(), "fake-vendor".to_string());

    let table = build(&[good, bad], &[], &ExtensionPolicyConfig::default());
    let aliased = table.get("fake-vendor/sub").expect("own alias resolves");
    assert_eq!(aliased.file, PathBuf::from("/pkgs/vendor/provide.ts"));
    assert_eq!(aliased.alias_of.as_deref(), Some("fake-vendor"));
    assert!(table.get("stolen").is_none(), "a foreign target is refused");
    assert!(
      table.errors.iter().any(|e| e.contains("other") && e.contains("stolen")),
      "{:?}",
      table.errors
    );
  }

  #[test]
  fn providers_evaluate_in_dependency_order_and_a_cycle_is_an_error() {
    // `beta` imports what `alpha` provides, so alpha's provider must
    // evaluate first whatever order the packages were configured in.
    let alpha = claim(0, "alpha", &[("a-spec", "a.ts")]);
    let mut beta = claim(1, "beta", &[("b-spec", "b.ts")]);
    beta.imports.insert("a-spec".to_string());
    let table = build(&[beta.clone(), alpha.clone()], &[], &ExtensionPolicyConfig::default());
    assert!(table.errors.is_empty(), "{:?}", table.errors);
    assert_eq!(
      table.provider_order(),
      [PathBuf::from("/pkgs/alpha/a.ts"), PathBuf::from("/pkgs/beta/b.ts")]
    );

    // Mutual imports: neither can be evaluated first.
    let mut alpha_cyclic = alpha;
    alpha_cyclic.imports.insert("b-spec".to_string());
    let table = build(&[alpha_cyclic, beta], &[], &ExtensionPolicyConfig::default());
    assert!(
      table.errors.iter().any(|e| e.contains("cycle")),
      "a provider cycle must be reported: {:?}",
      table.errors
    );
  }
}
