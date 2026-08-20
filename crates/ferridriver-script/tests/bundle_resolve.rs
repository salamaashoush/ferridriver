#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `[bundler]` module-resolution controls and the `[test].tsconfig`
//! selection: export conditions, main fields, the legacy `browser`
//! field, and a tsconfig auto-discovery would never find.
//!
//! One test fn — the bundler environment lives in a process-global slot,
//! and this integration test binary is its own process.

use std::path::Path;
use std::sync::Arc;

use ferridriver_script::bundle::{BundlerEnv, bundle_source};
use ferridriver_script::{
  InMemoryVars, Outcome, RunContext, RunOptions, ScriptEngineConfig, Session, bundle::set_bundler_env,
  bundle_and_compile,
};

fn ctx(dir: &Path) -> RunContext {
  RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: dir.into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: Vec::new(),
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
  }
}

fn write(path: &Path, contents: &str) {
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent).expect("mkdir");
  }
  std::fs::write(path, contents).expect("write");
}

/// Lay out three `node_modules` packages, each resolvable only through a
/// different mechanism, plus a tsconfig `paths` mapping in a file named
/// so per-module upward discovery can never pick it up.
fn fixture(dir: &Path) {
  // 1. `exports` conditions.
  write(
    &dir.join("node_modules/dual-build/package.json"),
    r#"{"name":"dual-build","type":"module","exports":{".":{"browser":"./browser.js","node":"./node.js","default":"./default.js"}}}"#,
  );
  write(
    &dir.join("node_modules/dual-build/browser.js"),
    "export const build = 'browser';\n",
  );
  write(
    &dir.join("node_modules/dual-build/node.js"),
    "export const build = 'node';\n",
  );
  write(
    &dir.join("node_modules/dual-build/default.js"),
    "export const build = 'default';\n",
  );

  // 2. No `exports` at all: reachable only through a main field.
  write(
    &dir.join("node_modules/main-only/package.json"),
    r#"{"name":"main-only","type":"module","main":"./lib/index.js"}"#,
  );
  write(
    &dir.join("node_modules/main-only/lib/index.js"),
    "export const flavor = 'main';\n",
  );

  // 3. The legacy `browser` FIELD, which remaps paths and is a different
  //    mechanism from a `browser` condition inside `exports`.
  write(
    &dir.join("node_modules/legacy-browser/package.json"),
    r#"{"name":"legacy-browser","type":"module","main":"./node.js","browser":{"./node.js":"./browser.js"}}"#,
  );
  write(
    &dir.join("node_modules/legacy-browser/node.js"),
    "export const legacy = 'node';\n",
  );
  write(
    &dir.join("node_modules/legacy-browser/browser.js"),
    "export const legacy = 'browser';\n",
  );

  // 4. A tsconfig `paths` mapping. Named `tsconfig.test.json` on
  //    purpose: rolldown discovers `tsconfig.json` by walking up from
  //    each module, so this one is reachable ONLY by being selected.
  write(
    &dir.join("tsconfig.test.json"),
    r#"{"compilerOptions":{"baseUrl":".","paths":{"@app/*":["src/*"]}}}"#,
  );
  write(&dir.join("src/mapped.ts"), "export const via = 'paths';\n");
}

async fn value_of(entry: &Path, dir: &Path, session: &Session, context: &RunContext) -> serde_json::Value {
  let bundle = bundle_and_compile(std::slice::from_ref(&entry.to_path_buf()), dir)
    .await
    .expect("bundle");
  let run = session
    .execute_module(&bundle, &[], RunOptions::default(), context)
    .await;
  match run.result.outcome {
    Outcome::Ok { success, .. } => success.value,
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

type Config = ferridriver_config::BundlerConfig;

async fn conditions_select_the_exports_branch(root: &Path, session: &Session, context: &RunContext, base: &Config) {
  let entry = root.join("conditions.ts");
  write(&entry, "import { build } from 'dual-build';\nexport default build;\n");

  // Conditions are additive onto the resolver's own base set, so with
  // none configured the `default` branch wins.
  set_bundler_env(BundlerEnv::from_config(base, root));
  assert_eq!(
    value_of(&entry, root, session, context).await,
    serde_json::json!("default"),
    "no configured condition => the exports map's default branch"
  );

  for (condition, expected) in [("browser", "browser"), ("node", "node")] {
    let mut cfg = base.clone();
    cfg.conditions = vec![condition.to_string()];
    set_bundler_env(BundlerEnv::from_config(&cfg, root));
    assert_eq!(
      value_of(&entry, root, session, context).await,
      serde_json::json!(expected),
      "conditions = [\"{condition}\"] selects that build"
    );
  }
}

async fn main_fields_reach_a_package_without_exports(
  root: &Path,
  session: &Session,
  context: &RunContext,
  base: &Config,
) {
  let entry = root.join("main-field.ts");
  write(&entry, "import { flavor } from 'main-only';\nexport default flavor;\n");

  // The shipped default main-fields list is what makes a package with a
  // bare `main` (and no exports map) resolvable at all: rolldown's own
  // default for a neutral platform is empty.
  set_bundler_env(BundlerEnv::from_config(base, root));
  assert_eq!(
    value_of(&entry, root, session, context).await,
    serde_json::json!("main"),
    "the default mainFields resolve a package that has only `main`"
  );

  let mut none = base.clone();
  none.main_fields = Vec::new();
  set_bundler_env(BundlerEnv::from_config(&none, root));
  assert!(
    bundle_and_compile(std::slice::from_ref(&entry), root).await.is_err(),
    "mainFields = [] leaves a main-only package unresolvable"
  );
}

async fn alias_fields_apply_the_legacy_remap(root: &Path, session: &Session, context: &RunContext, base: &Config) {
  let entry = root.join("legacy.ts");
  write(
    &entry,
    "import { legacy } from 'legacy-browser';\nexport default legacy;\n",
  );

  set_bundler_env(BundlerEnv::from_config(base, root));
  assert_eq!(
    value_of(&entry, root, session, context).await,
    serde_json::json!("node"),
    "no aliasFields => the legacy browser field is ignored"
  );

  let mut cfg = base.clone();
  cfg.alias_fields = vec![vec!["browser".to_string()]];
  set_bundler_env(BundlerEnv::from_config(&cfg, root));
  assert_eq!(
    value_of(&entry, root, session, context).await,
    serde_json::json!("browser"),
    "aliasFields = [[\"browser\"]] applies the legacy path remap"
  );
}

async fn a_selected_tsconfig_governs_paths(root: &Path, session: &Session, context: &RunContext, base: &Config) {
  let entry = root.join("paths.ts");
  write(&entry, "import { via } from '@app/mapped';\nexport default via;\n");

  set_bundler_env(BundlerEnv::from_config(base, root));
  assert!(
    bundle_and_compile(std::slice::from_ref(&entry), root).await.is_err(),
    "an undiscoverable tsconfig's paths must not resolve on their own"
  );

  set_bundler_env(BundlerEnv::from_config(base, root).with_tsconfig(Some("tsconfig.test.json"), root));
  assert_eq!(
    value_of(&entry, root, session, context).await,
    serde_json::json!("paths"),
    "the selected tsconfig's paths mapping resolves"
  );

  // That tsconfig is an input of the bundle it governed, so editing a
  // mapping invalidates the cached bytecode built from it.
  let bundled = bundle_source(std::slice::from_ref(&entry), root)
    .await
    .expect("bundle source");
  assert!(
    bundled.config_inputs.iter().any(|p| p.ends_with("tsconfig.test.json")),
    "the governing tsconfig is tracked as a bundle input: {:?}",
    bundled.config_inputs
  );

  // A pinned tsconfig that is not there is an error, not a silent
  // fallback to discovery.
  set_bundler_env(BundlerEnv::from_config(base, root).with_tsconfig(Some("tsconfig.missing.json"), root));
  assert!(
    bundle_and_compile(std::slice::from_ref(&entry), root).await.is_err(),
    "a tsconfig path that names no file fails the bundle"
  );
}

/// Every control salts the bundle cache key, so switching one cannot
/// serve bytecode built under another.
fn every_control_changes_the_cache_key(root: &Path, base: &Config) {
  let with = |mutate: fn(&mut Config)| {
    let mut cfg = base.clone();
    mutate(&mut cfg);
    BundlerEnv::from_config(&cfg, root).fingerprint()
  };
  let fingerprints = [
    BundlerEnv::from_config(base, root).fingerprint(),
    with(|c| c.conditions = vec!["browser".to_string()]),
    with(|c| c.conditions = vec!["node".to_string()]),
    with(|c| c.main_fields = Vec::new()),
    with(|c| c.alias_fields = vec![vec!["browser".to_string()]]),
    BundlerEnv::from_config(base, root)
      .with_tsconfig(Some("tsconfig.test.json"), root)
      .fingerprint(),
  ];
  for (i, a) in fingerprints.iter().enumerate() {
    for b in &fingerprints[i + 1..] {
      assert_ne!(a, b, "each resolution control must change the bundle cache key");
    }
  }
}

#[tokio::test]
async fn resolution_controls_select_the_build_that_gets_bundled() {
  let dir = tempfile::tempdir().expect("tempdir");
  let root = dir.path();
  fixture(root);

  let context = ctx(root);
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");
  let base = Config::default();

  conditions_select_the_exports_branch(root, &session, &context, &base).await;
  main_fields_reach_a_package_without_exports(root, &session, &context, &base).await;
  alias_fields_apply_the_legacy_remap(root, &session, &context, &base).await;
  a_selected_tsconfig_governs_paths(root, &session, &context, &base).await;
  every_control_changes_the_cache_key(root, &base);
}
