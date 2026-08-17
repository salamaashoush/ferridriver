//! Check an extension package's declared preconditions against the host.
//!
//! A package's `package.json` `ferridriver.requires` block states what it
//! needs to work: binaries on `PATH`, `allowEnv` names, host patterns
//! inside the operator's net ceiling, declared sidecars. `ferridriver.settings`
//! states the shape of the `[extensions.settings.<key>]` block it reads.
//!
//! Every one of those was previously a runtime failure with no line
//! pointing at the cause: a missing binary surfaced as a `commands.run`
//! error on the first tool call, an unlisted `allowEnv` name as
//! `process.env.X === undefined`, a host outside the ceiling as a
//! net-policy throw, a mistyped settings key as `settings.origin ===
//! undefined`. Checking them where the package is loaded turns all four
//! into one message naming the package, the requirement, and the config
//! key that satisfies it.
//!
//! A blocking issue means the package cannot work as declared, so its
//! files are not loaded at all — a half-loaded package whose every call
//! fails is strictly worse than an absent one with a reason.

use std::collections::BTreeMap;

use crate::ResolvedExtension;
use ferridriver_config::{ExtensionCommandsCeiling, ExtensionPolicyConfig};

/// What the host provides, for a package's `requires` to be checked
/// against.
pub struct RequirementEnv<'a> {
  /// The operator ceiling (`[extensions.policy]`).
  pub policy: &'a ExtensionPolicyConfig,
  /// Names the operator allow-listed in `[scripting].allowEnv`.
  pub allow_env: &'a [String],
  /// Declared `[[sidecars]]` names.
  pub sidecars: &'a [String],
  /// The operator's `[extensions.settings]` blocks.
  pub settings: &'a BTreeMap<String, serde_json::Value>,
}

impl<'a> RequirementEnv<'a> {
  /// The env a host built from its resolved `[scripting]` caps and its
  /// declared sidecars — the four things every host already has, so no
  /// host has to assemble the gate's inputs itself.
  #[must_use]
  pub fn from_caps(caps: &'a crate::ScriptCaps, sidecars: &'a [String]) -> Self {
    Self {
      policy: &caps.extension_policy,
      allow_env: &caps.allow_env,
      sidecars,
      settings: &caps.extension_settings,
    }
  }
}

/// One unmet or questionable requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequirementIssue {
  /// The package directory (or the spec, when there is no package).
  pub source: String,
  pub message: String,
  /// `true` when the package cannot work as declared and must not load.
  pub blocking: bool,
}

/// Check every resolved package's `requires` + `settings` declarations.
///
/// Returns one issue per unmet requirement. Specs that resolved to loose
/// files or to a package with no manifest declare nothing and produce
/// nothing.
#[must_use]
pub fn check(resolved: &[ResolvedExtension], env: &RequirementEnv<'_>) -> Vec<RequirementIssue> {
  let mut issues = Vec::new();
  for r in resolved {
    let Some(manifest) = r.manifest.as_ref() else {
      continue;
    };
    let source = source_label(r);
    let mut push = |blocking: bool, message: String| {
      issues.push(RequirementIssue {
        source: source.clone(),
        message,
        blocking,
      });
    };

    check_commands(&manifest.requires.commands, env, &mut push);
    check_env(&manifest.requires.env, env, &mut push);
    check_net(&manifest.requires.net, env, &mut push);
    check_sidecars(&manifest.requires.sidecars, env, &mut push);
    check_settings(&manifest.settings, env, &mut push);
  }
  issues
}

/// Specs whose package had a blocking issue, keyed by spec, so the
/// caller can drop their files before loading.
#[must_use]
pub fn blocked_specs(resolved: &[ResolvedExtension], issues: &[RequirementIssue]) -> Vec<String> {
  let blocking: std::collections::BTreeSet<&str> = issues
    .iter()
    .filter(|i| i.blocking)
    .map(|i| i.source.as_str())
    .collect();
  resolved
    .iter()
    .filter(|r| blocking.contains(source_label(r).as_str()))
    .map(|r| r.spec.clone())
    .collect()
}

/// How a resolved spec is named in an issue: its package directory when
/// it resolved to a package, else the spec itself.
///
/// One definition, because every consumer that filters issues by source
/// has to derive it the same way — three hand-rolled copies is three
/// chances for a report to silently show no requirements at all.
#[must_use]
pub fn source_label(resolved: &ResolvedExtension) -> String {
  resolved
    .package_dir
    .as_ref()
    .map_or_else(|| resolved.spec.clone(), |d| d.display().to_string())
}

fn check_commands(required: &[String], env: &RequirementEnv<'_>, push: &mut impl FnMut(bool, String)) {
  if !required.is_empty() && env.policy.commands == ExtensionCommandsCeiling::None {
    push(
      true,
      format!(
        "requires the command(s) {required:?}, but [extensions.policy] sets `commands = \"none\"` — \
         no extension may declare commands under this policy"
      ),
    );
    return;
  }
  for program in required {
    if which::which(program).is_err() {
      push(
        true,
        format!("requires `{program}` on PATH, and it is not installed (or not on this process's PATH)"),
      );
    }
  }
}

fn check_env(required: &[String], env: &RequirementEnv<'_>, push: &mut impl FnMut(bool, String)) {
  for name in required {
    if !env.allow_env.iter().any(|a| a == name) {
      push(
        true,
        format!(
          "requires the environment variable `{name}`, which is not in [scripting].allowEnv — \
           a script cannot read an unlisted variable, so add `allowEnv = [\"{name}\"]`"
        ),
      );
      continue;
    }
    if std::env::var_os(name).is_none() {
      push(
        false,
        format!("requires the environment variable `{name}`: allow-listed, but not set in this process's environment"),
      );
    }
  }
}

fn check_net(required: &[String], env: &RequirementEnv<'_>, push: &mut impl FnMut(bool, String)) {
  let Some(ceiling) = env.policy.net.as_deref() else {
    return;
  };
  let outside: Vec<&str> = required
    .iter()
    .map(String::as_str)
    .filter(|host| !crate::net_entry_subsumed(host, ceiling))
    .collect();
  if !outside.is_empty() {
    push(
      true,
      format!(
        "requires HTTP access to {outside:?}, which the operator ceiling ([extensions.policy] net = \
         {ceiling:?}) does not permit — its tools would be denied at the first request"
      ),
    );
  }
}

fn check_sidecars(required: &[String], env: &RequirementEnv<'_>, push: &mut impl FnMut(bool, String)) {
  for name in required {
    if !env.sidecars.iter().any(|s| s == name) {
      push(
        true,
        format!(
          "requires the sidecar `{name}`, which no [[sidecars]] entry declares — \
           `sidecars.connect(\"{name}\")` would throw"
        ),
      );
    }
  }
}

fn check_settings(
  schemas: &BTreeMap<String, serde_json::Value>,
  env: &RequirementEnv<'_>,
  push: &mut impl FnMut(bool, String),
) {
  for (key, schema) in schemas {
    let validator = match jsonschema::validator_for(schema) {
      Ok(v) => v,
      Err(e) => {
        push(true, format!("declares an invalid settings schema for `{key}`: {e}"));
        continue;
      },
    };
    // An absent block is validated as `{}` so a required field is
    // reported rather than read as `undefined` inside the handler.
    let empty = serde_json::json!({});
    let block = env.settings.get(key).unwrap_or(&empty);
    let mut messages: Vec<String> = validator
      .iter_errors(block)
      .map(|e| {
        let path = e.instance_path().to_string();
        if path.is_empty() {
          e.to_string()
        } else {
          format!("{path}: {e}")
        }
      })
      .take(20)
      .collect();
    if messages.is_empty() {
      continue;
    }
    messages.sort();
    messages.dedup();
    push(
      true,
      format!(
        "[extensions.settings.{key}] does not match the schema the package declares: {}",
        messages.join("; ")
      ),
    );
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use ferridriver_config::{ExtensionManifest, ExtensionRequires};
  use std::path::PathBuf;

  fn pkg(manifest: ExtensionManifest) -> ResolvedExtension {
    ResolvedExtension {
      spec: "@acme/ext".to_string(),
      base_dir: PathBuf::from("/base"),
      package_dir: Some(PathBuf::from("/base/node_modules/@acme/ext")),
      manifest: Some(manifest),
      files: vec![PathBuf::from("/base/node_modules/@acme/ext/index.ts")],
    }
  }

  fn env<'a>(
    policy: &'a ExtensionPolicyConfig,
    allow_env: &'a [String],
    sidecars: &'a [String],
    settings: &'a BTreeMap<String, serde_json::Value>,
  ) -> RequirementEnv<'a> {
    RequirementEnv {
      policy,
      allow_env,
      sidecars,
      settings,
    }
  }

  fn defaults() -> (
    ExtensionPolicyConfig,
    Vec<String>,
    Vec<String>,
    BTreeMap<String, serde_json::Value>,
  ) {
    (
      ExtensionPolicyConfig::default(),
      Vec::new(),
      Vec::new(),
      BTreeMap::new(),
    )
  }

  #[test]
  fn no_manifest_declares_nothing() {
    let (p, a, s, st) = defaults();
    let loose = ResolvedExtension {
      spec: "./tool.ts".to_string(),
      base_dir: PathBuf::from("/base"),
      package_dir: None,
      manifest: None,
      files: vec![PathBuf::from("/base/tool.ts")],
    };
    assert!(check(&[loose], &env(&p, &a, &s, &st)).is_empty());
  }

  #[test]
  fn a_missing_binary_blocks_the_package() {
    let (p, a, s, st) = defaults();
    let issues = check(
      &[pkg(ExtensionManifest {
        requires: ExtensionRequires {
          commands: vec!["definitely-not-a-real-binary-xyz".into()],
          ..Default::default()
        },
        ..Default::default()
      })],
      &env(&p, &a, &s, &st),
    );
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(issues[0].blocking);
    assert!(issues[0].message.contains("on PATH"), "{issues:?}");
  }

  #[test]
  fn a_present_binary_passes() {
    let (p, a, s, st) = defaults();
    let issues = check(
      &[pkg(ExtensionManifest {
        requires: ExtensionRequires {
          // `sh` exists on every platform this repo builds for.
          commands: vec!["sh".into()],
          ..Default::default()
        },
        ..Default::default()
      })],
      &env(&p, &a, &s, &st),
    );
    assert!(issues.is_empty(), "{issues:?}");
  }

  #[test]
  fn commands_none_ceiling_blocks_any_command_requirement() {
    let policy = ExtensionPolicyConfig {
      commands: ExtensionCommandsCeiling::None,
      ..Default::default()
    };
    let (_, a, s, st) = defaults();
    let issues = check(
      &[pkg(ExtensionManifest {
        requires: ExtensionRequires {
          commands: vec!["sh".into()],
          ..Default::default()
        },
        ..Default::default()
      })],
      &env(&policy, &a, &s, &st),
    );
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(issues[0].blocking);
    assert!(issues[0].message.contains("commands = \"none\""), "{issues:?}");
  }

  #[test]
  fn an_unlisted_env_name_blocks_and_names_the_config_key() {
    let (p, _, s, st) = defaults();
    let issues = check(
      &[pkg(ExtensionManifest {
        requires: ExtensionRequires {
          env: vec!["ACME_HOME".into()],
          ..Default::default()
        },
        ..Default::default()
      })],
      &env(&p, &[], &s, &st),
    );
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(issues[0].blocking);
    assert!(issues[0].message.contains("allowEnv"), "{issues:?}");
  }

  #[test]
  fn an_allow_listed_but_unset_env_name_is_a_warning_only() {
    let (p, _, s, st) = defaults();
    let allow = vec!["FERRIDRIVER_TEST_UNSET_XYZ".to_string()];
    let issues = check(
      &[pkg(ExtensionManifest {
        requires: ExtensionRequires {
          env: vec!["FERRIDRIVER_TEST_UNSET_XYZ".into()],
          ..Default::default()
        },
        ..Default::default()
      })],
      &env(&p, &allow, &s, &st),
    );
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(!issues[0].blocking, "the operator granted it; absence is not fatal");
  }

  #[test]
  fn a_host_outside_the_ceiling_blocks() {
    let policy = ExtensionPolicyConfig {
      net: Some(vec!["localhost".into()]),
      ..Default::default()
    };
    let (_, a, s, st) = defaults();
    let issues = check(
      &[pkg(ExtensionManifest {
        requires: ExtensionRequires {
          net: vec!["*.acme.com".into()],
          ..Default::default()
        },
        ..Default::default()
      })],
      &env(&policy, &a, &s, &st),
    );
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(issues[0].blocking);
    assert!(issues[0].message.contains("*.acme.com"), "{issues:?}");
  }

  #[test]
  fn a_host_inside_the_ceiling_passes() {
    let policy = ExtensionPolicyConfig {
      net: Some(vec!["*.acme.com".into()]),
      ..Default::default()
    };
    let (_, a, s, st) = defaults();
    let issues = check(
      &[pkg(ExtensionManifest {
        requires: ExtensionRequires {
          net: vec!["api.acme.com".into()],
          ..Default::default()
        },
        ..Default::default()
      })],
      &env(&policy, &a, &s, &st),
    );
    assert!(issues.is_empty(), "{issues:?}");
  }

  #[test]
  fn an_undeclared_sidecar_blocks() {
    let (p, a, _, st) = defaults();
    let issues = check(
      &[pkg(ExtensionManifest {
        requires: ExtensionRequires {
          sidecars: vec!["acme-gate".into()],
          ..Default::default()
        },
        ..Default::default()
      })],
      &env(&p, &a, &[], &st),
    );
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(issues[0].blocking);
    assert!(issues[0].message.contains("[[sidecars]]"), "{issues:?}");

    let declared = vec!["acme-gate".to_string()];
    let issues = check(
      &[pkg(ExtensionManifest {
        requires: ExtensionRequires {
          sidecars: vec!["acme-gate".into()],
          ..Default::default()
        },
        ..Default::default()
      })],
      &env(&p, &a, &declared, &st),
    );
    assert!(issues.is_empty(), "{issues:?}");
  }

  #[test]
  fn settings_are_validated_against_the_declared_schema() {
    let (p, a, s, _) = defaults();
    let manifest = ExtensionManifest {
      settings: BTreeMap::from([(
        "acme".to_string(),
        serde_json::json!({
          "type": "object",
          "properties": { "origin": { "type": "string" } },
          "required": ["origin"],
          "additionalProperties": false
        }),
      )]),
      ..Default::default()
    };

    // Missing block => the required field is reported, not read as undefined.
    let issues = check(&[pkg(manifest.clone())], &env(&p, &a, &s, &BTreeMap::new()));
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(issues[0].blocking);
    assert!(issues[0].message.contains("origin"), "{issues:?}");

    // A mistyped key is an error instead of a silent undefined.
    let typo = BTreeMap::from([("acme".to_string(), serde_json::json!({ "origins": "https://x" }))]);
    let issues = check(&[pkg(manifest.clone())], &env(&p, &a, &s, &typo));
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(
      issues[0].message.contains("origins") || issues[0].message.contains("origin"),
      "{issues:?}"
    );

    // The conforming block passes.
    let good = BTreeMap::from([("acme".to_string(), serde_json::json!({ "origin": "https://x" }))]);
    assert!(check(&[pkg(manifest)], &env(&p, &a, &s, &good)).is_empty());
  }

  #[test]
  fn blocked_specs_names_the_spec_to_skip() {
    let (p, a, s, st) = defaults();
    let resolved = vec![pkg(ExtensionManifest {
      requires: ExtensionRequires {
        sidecars: vec!["missing".into()],
        ..Default::default()
      },
      ..Default::default()
    })];
    let issues = check(&resolved, &env(&p, &a, &s, &st));
    assert_eq!(blocked_specs(&resolved, &issues), vec!["@acme/ext".to_string()]);
  }
}
