//! Machine-independent test identity.
//!
//! A test's file arrives relative to the process working directory, which
//! is a fact about one invocation, not about the suite: two CI machines, or
//! one machine started from a subdirectory, would hash the same test to two
//! different ids and shard it differently. Identity is therefore computed
//! from the file relative to the config root (Playwright's `configDir`),
//! which is the same wherever the suite is checked out.

use std::path::{Component, Path, PathBuf};

use crate::config::TestConfig;

/// The directory ids are relative to: the directory of the config file the
/// run resolved from, or the working directory when there is none.
#[must_use]
pub fn root(config: &TestConfig) -> PathBuf {
  config
    .config_dir
    .clone()
    .or_else(|| std::env::current_dir().ok())
    .unwrap_or_else(|| PathBuf::from("."))
}

/// `path` in the form a prefix comparison can trust.
///
/// Canonical when the path exists, so a symlinked temp directory (`/var` vs
/// `/private/var`) resolves the same way on both sides; otherwise absolute
/// and lexically normalized, since a `..` kept verbatim is under no prefix.
#[must_use]
pub fn comparable_path(path: &Path) -> PathBuf {
  if let Ok(real) = std::fs::canonicalize(path) {
    return real;
  }
  let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
  ferridriver_config::layer::normalize_path(&absolute)
}

/// `file` relative to `root` (already a [`comparable_path`]), with `/`
/// separators on every platform. A file outside the root climbs out of it
/// with `..`, which is still the same string on every machine that checks
/// the two out side by side.
#[must_use]
pub fn relative_to(root: &Path, file: &str) -> String {
  let file = comparable_path(Path::new(file));
  let root: Vec<Component<'_>> = root.components().collect();
  let target: Vec<Component<'_>> = file.components().collect();
  let common = root.iter().zip(&target).take_while(|(a, b)| a == b).count();
  if common == 0 {
    return slashes(&file);
  }
  let mut parts: Vec<String> = std::iter::repeat_n("..".to_string(), root.len() - common).collect();
  parts.extend(
    target[common..]
      .iter()
      .map(|component| component.as_os_str().to_string_lossy().into_owned()),
  );
  parts.join("/")
}

fn slashes(path: &Path) -> String {
  path
    .components()
    .map(|component| component.as_os_str().to_string_lossy().into_owned())
    .collect::<Vec<_>>()
    .join("/")
    .replacen("//", "/", 1)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{TestId, TestPlan, TestSuite};

  fn plan_with(file: &str) -> TestPlan {
    TestPlan {
      suites: vec![TestSuite {
        name: "suite".into(),
        file: file.into(),
        tests: vec![crate::model::TestCase {
          id: TestId {
            file: file.into(),
            suite: Some(format!("{file}::suite")),
            name: "works".into(),
            line: Some(3),
            ..TestId::default()
          },
          test_fn: std::sync::Arc::new(|_| Box::pin(async { Ok(()) })),
          fixture_requests: Vec::new(),
          annotations: Vec::new(),
          timeout: None,
          retries: None,
          expected_status: crate::model::ExpectedStatus::Pass,
          use_options: None,
          metadata: None,
        }],
        hooks: crate::model::Hooks::default(),
        annotations: Vec::new(),
        mode: crate::model::SuiteMode::Parallel,
        serial_scope: None,
      }],
      total_tests: 1,
      ..TestPlan::default()
    }
  }

  fn id_of(plan: &TestPlan) -> String {
    plan.suites[0].tests[0].id.stable_id("chromium")
  }

  #[test]
  fn a_file_under_the_root_is_relative_with_forward_slashes() {
    let root = Path::new("/repo");
    assert_eq!(relative_to(root, "/repo/tests/a.spec.ts"), "tests/a.spec.ts");
  }

  #[test]
  fn a_file_beside_the_root_climbs_out_of_it() {
    let root = Path::new("/repo/config");
    assert_eq!(relative_to(root, "/repo/tests/a.spec.ts"), "../tests/a.spec.ts");
  }

  #[test]
  fn the_id_is_the_same_from_any_working_directory() {
    // The same spec seen from the repository root and from a subdirectory:
    // the working-directory paths differ, the anchored ids must not.
    let root = std::env::temp_dir().join(format!("ferri-identity-{}", std::process::id()));
    std::fs::create_dir_all(root.join("tests")).expect("dir");
    std::fs::write(root.join("tests/a.spec.ts"), "").expect("spec");
    let root = comparable_path(&root);

    let mut from_root = plan_with(&root.join("tests/a.spec.ts").display().to_string());
    from_root.anchor(&root);
    let other_cwd = root.join("tests").join("..").join("tests").join("a.spec.ts");
    let mut from_sub = plan_with(&other_cwd.display().to_string());
    from_sub.anchor(&root);

    assert_eq!(
      from_root.suites[0].tests[0].id.root_path.as_deref(),
      Some("tests/a.spec.ts")
    );
    assert_eq!(id_of(&from_root), id_of(&from_sub));

    // A checkout somewhere else entirely gives the same id too.
    let elsewhere = std::env::temp_dir().join(format!("ferri-identity-b-{}", std::process::id()));
    std::fs::create_dir_all(elsewhere.join("tests")).expect("dir");
    std::fs::write(elsewhere.join("tests/a.spec.ts"), "").expect("spec");
    let elsewhere = comparable_path(&elsewhere);
    let mut moved = plan_with(&elsewhere.join("tests/a.spec.ts").display().to_string());
    moved.anchor(&elsewhere);
    assert_eq!(id_of(&from_root), id_of(&moved));

    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&elsewhere).ok();
  }

  #[test]
  fn an_unanchored_id_still_hashes_its_file() {
    let plan = plan_with("tests/a.spec.ts");
    let mut anchored = plan.clone();
    anchored.suites[0].tests[0].id.root_path = Some("tests/a.spec.ts".into());
    assert_eq!(id_of(&plan), id_of(&anchored));
  }
}
