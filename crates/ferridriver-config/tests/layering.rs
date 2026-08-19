#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Config layer stack: discovery, merge, path anchoring, provenance.
//!
//! Every case drives [`ferridriver_config::layer::resolve`] with an
//! explicit [`LoadOptions`], so nothing here reads the developer's real
//! `~/.config` or mutates the process cwd — the old tests that leaned
//! on `std::env::current_dir` could not express a multi-layer stack at
//! all.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ferridriver_config::layer::{LayerKind, LoadOptions, Origin, resolve};

/// A scratch tree with a `user/` config dir and a `repo/` git root.
struct Tree {
  dir: tempfile::TempDir,
}

impl Tree {
  fn new() -> Self {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("user/ferridriver")).expect("user dir");
    std::fs::create_dir_all(dir.path().join("repo/.git")).expect("git dir");
    std::fs::create_dir_all(dir.path().join("repo/pkg")).expect("pkg dir");
    Self { dir }
  }

  fn root(&self) -> &Path {
    self.dir.path()
  }

  fn user_dir(&self) -> PathBuf {
    self.dir.path().join("user")
  }

  fn repo(&self) -> PathBuf {
    self.dir.path().join("repo")
  }

  fn write(&self, rel: &str, body: &str) -> PathBuf {
    let path = self.dir.path().join(rel);
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent).expect("parent");
    }
    std::fs::write(&path, body).expect("write");
    path
  }

  /// Options with the user layer wired to this tree and the cwd inside
  /// the repository. No machine layer, no ambient environment.
  fn opts(&self, cwd: PathBuf) -> LoadOptions {
    LoadOptions {
      explicit: None,
      cwd,
      user_config_dir: Some(self.user_dir()),
      machine_config_dir: None,
      env: BTreeMap::new(),
      inherit: true,
      extension_defaults: Vec::new(),
      cache: ferridriver_config::layer::LayerCache::default(),
      module_loader: None,
      documents_only: false,
    }
  }
}

#[test]
fn user_and_project_layers_merge_instead_of_shadowing() {
  let t = Tree::new();
  t.write(
    "user/ferridriver/config.yaml",
    "mcp:\n  server:\n    name: acme-user\n  browser:\n    headless: true\n",
  );
  t.write("repo/ferridriver.toml", "[test]\nworkers = 7\n");

  let r = resolve(&t.opts(t.repo())).expect("resolve");

  // The project file used to replace the whole document; both layers
  // must survive.
  assert_eq!(r.config.mcp.server_name(), "acme-user");
  assert!(r.config.mcp.headless());
  assert_eq!(r.config.test.workers, 7);

  let kinds: Vec<LayerKind> = r.layers.iter().map(|l| l.kind).collect();
  assert_eq!(kinds, [LayerKind::User, LayerKind::Cwd]);
}

#[test]
fn higher_layer_overrides_the_same_scalar() {
  let t = Tree::new();
  t.write("user/ferridriver/config.toml", "[mcp.server]\nname = \"from-user\"\n");
  t.write("repo/ferridriver.toml", "[mcp.server]\nname = \"from-project\"\n");

  let r = resolve(&t.opts(t.repo())).expect("resolve");

  assert_eq!(r.config.mcp.server_name(), "from-project");
  assert_eq!(
    r.provenance.get("mcp.server.name"),
    Some(&Origin::File(t.repo().join("ferridriver.toml")))
  );
}

#[test]
fn additive_keys_concatenate_across_layers() {
  let t = Tree::new();
  t.write(
    "user/ferridriver/config.toml",
    "extensions = [\"/abs/acme.ts\"]\n\n[mcp.browser]\nchromeArgs = [\"--user-flag\"]\n",
  );
  t.write(
    "repo/ferridriver.toml",
    "extensions = [\"/abs/repo.ts\"]\n\n[mcp.browser]\nchromeArgs = [\"--repo-flag\"]\n",
  );

  let r = resolve(&t.opts(t.repo())).expect("resolve");

  assert_eq!(r.config.extensions.paths(), ["/abs/acme.ts", "/abs/repo.ts"]);
  assert_eq!(r.config.mcp.chrome_args(), ["--user-flag", "--repo-flag"]);
}

#[test]
fn additive_keys_do_not_duplicate_a_shared_entry() {
  let t = Tree::new();
  t.write("user/ferridriver/config.toml", "extensions = [\"/abs/acme.ts\"]\n");
  t.write(
    "repo/ferridriver.toml",
    "extensions = [\"/abs/acme.ts\", \"/abs/repo.ts\"]\n",
  );

  let r = resolve(&t.opts(t.repo())).expect("resolve");

  assert_eq!(r.config.extensions.paths(), ["/abs/acme.ts", "/abs/repo.ts"]);
}

#[test]
fn extensions_shorthand_and_policy_table_combine() {
  let t = Tree::new();
  t.write("user/ferridriver/config.toml", "extensions = [\"/abs/acme.ts\"]\n");
  t.write(
    "repo/ferridriver.toml",
    "[extensions]\npaths = [\"/abs/repo.ts\"]\n\n[extensions.policy]\ncommands = \"argvOnly\"\n",
  );

  let r = resolve(&t.opts(t.repo())).expect("resolve");

  assert_eq!(r.config.extensions.paths(), ["/abs/acme.ts", "/abs/repo.ts"]);
  assert_eq!(
    r.config.extensions.policy().commands,
    ferridriver_config::ExtensionCommandsCeiling::ArgvOnly
  );
}

#[test]
fn relative_paths_anchor_to_their_own_layer() {
  let t = Tree::new();
  t.write(
    "user/ferridriver/config.yaml",
    "extensions:\n  - ./plugins/acme.ts\nscriptRoot: ./scripts\n",
  );
  t.write("repo/ferridriver.toml", "[test]\ntestDir = \"./e2e\"\n");

  let r = resolve(&t.opts(t.repo())).expect("resolve");

  // The user layer's relative path must mean "next to the user file",
  // not "next to whatever repository the process is standing in".
  assert_eq!(
    r.config.extensions.paths(),
    [t.user_dir()
      .join("ferridriver/plugins/acme.ts")
      .to_string_lossy()
      .into_owned()]
  );
  assert_eq!(
    r.config.test.test_dir.as_deref(),
    Some(t.repo().join("e2e").to_string_lossy().as_ref())
  );
}

#[test]
fn package_specifiers_keep_their_declaring_directory() {
  let t = Tree::new();
  t.write(
    "user/ferridriver/config.yaml",
    "extensions:\n  - \"@acme/ferridriver-acme\"\n",
  );

  let r = resolve(&t.opts(t.repo())).expect("resolve");

  // Left as a specifier (node_modules resolution), but the base dir is
  // the user layer so the user's packages are found.
  assert_eq!(r.config.extensions.paths(), ["@acme/ferridriver-acme"]);
  let specs = r.config.extension_specs();
  assert_eq!(specs.len(), 1);
  assert_eq!(specs[0].spec, "@acme/ferridriver-acme");
  assert_eq!(specs[0].base_dir, t.user_dir().join("ferridriver"));
}

#[test]
fn tilde_extension_specs_are_expanded_not_treated_as_packages() {
  let t = Tree::new();
  t.write("repo/ferridriver.yaml", "extensions:\n  - ~/plugins/acme/login.ts\n");

  let r = resolve(&t.opts(t.repo())).expect("resolve");

  let resolved = &r.config.extensions.paths()[0];
  assert!(
    !resolved.starts_with('~'),
    "`~` must be expanded here: the extension resolver would otherwise \
     classify it as a package name and report `package not found`, got {resolved}"
  );
  assert!(resolved.ends_with("/plugins/acme/login.ts"), "got {resolved}");
}

#[test]
fn tilde_and_template_paths_are_left_alone() {
  let t = Tree::new();
  t.write(
    "repo/ferridriver.toml",
    "[mcp.browser.instances.staging]\ndiscoverProfile = \"~/.box/profiles/${INSTANCE}\"\n",
  );

  let r = resolve(&t.opts(t.repo())).expect("resolve");

  let staging = r.config.mcp.browser.instances.get("staging").expect("staging instance");
  assert_eq!(staging.discover_profile.as_deref(), Some("~/.box/profiles/${INSTANCE}"));
}

#[test]
fn git_root_layer_applies_from_a_subdirectory() {
  let t = Tree::new();
  t.write("repo/ferridriver.toml", "[test]\nworkers = 3\n");
  std::fs::create_dir_all(t.repo().join("packages/web")).expect("subdir");

  let r = resolve(&t.opts(t.repo().join("packages/web"))).expect("resolve");

  assert_eq!(r.config.test.workers, 3);
  assert_eq!(
    r.layers.iter().map(|l| l.kind).collect::<Vec<_>>(),
    [LayerKind::Project]
  );
}

#[test]
fn nested_package_config_wins_over_the_repo_root() {
  let t = Tree::new();
  t.write(
    "repo/ferridriver.toml",
    "[test]\nworkers = 9\nsteps = [\"tests/steps/**/*.ts\"]\n",
  );
  t.write("repo/pkg/ferridriver.toml", "[test]\nsteps = []\n");

  let r = resolve(&t.opts(t.repo().join("pkg"))).expect("resolve");

  // Shared defaults still apply...
  assert_eq!(r.config.test.workers, 9);
  // ...but a nested package can clear a collection the root set. Arrays
  // replace (they are not in APPEND_KEYS), which is what makes opting
  // out of an inherited policy possible at all.
  assert!(r.config.test.steps.is_empty());
  assert_eq!(
    r.layers.iter().map(|l| l.kind).collect::<Vec<_>>(),
    [LayerKind::Project, LayerKind::Cwd]
  );
}

#[test]
fn ancestor_configs_apply_outermost_first() {
  let t = Tree::new();
  std::fs::create_dir_all(t.repo().join("a/b")).expect("dirs");
  t.write("repo/ferridriver.toml", "[test]\nworkers = 1\ntimeout = 1000\n");
  t.write("repo/a/ferridriver.toml", "[test]\nworkers = 2\n");
  t.write("repo/a/b/ferridriver.toml", "[test]\nworkers = 3\n");

  let r = resolve(&t.opts(t.repo().join("a/b"))).expect("resolve");

  assert_eq!(r.config.test.workers, 3, "nearest wins");
  assert_eq!(r.config.test.timeout, 1000, "outer layers still contribute");
  assert_eq!(r.layers.len(), 3);
}

#[test]
fn local_layer_beats_the_committed_file() {
  let t = Tree::new();
  t.write("repo/ferridriver.toml", "[mcp.browser]\nheadless = true\n");
  t.write("repo/ferridriver.local.toml", "[mcp.browser]\nheadless = false\n");

  let r = resolve(&t.opts(t.repo())).expect("resolve");

  assert!(!r.config.mcp.headless());
  assert_eq!(r.layers.last().map(|l| l.kind), Some(LayerKind::Local));
}

#[test]
fn explicit_config_layers_on_top_of_the_user_file() {
  let t = Tree::new();
  t.write(
    "user/ferridriver/config.toml",
    "extensions = [\"/abs/acme.ts\"]\n\n[mcp.server]\nname = \"user\"\n",
  );
  let pinned = t.write("repo/ci.toml", "[mcp.server]\nname = \"ci\"\n");

  let mut opts = t.opts(t.repo());
  opts.explicit = Some(pinned.clone());
  let r = resolve(&opts).expect("resolve");

  // The whole point of the stack: -c pins two settings and still
  // inherits the operator's extensions.
  assert_eq!(r.config.mcp.server_name(), "ci");
  assert_eq!(r.config.extensions.paths(), ["/abs/acme.ts"]);
  assert_eq!(r.layers.last().map(|l| l.kind), Some(LayerKind::Explicit));
}

#[test]
fn no_inherit_uses_only_the_named_file() {
  let t = Tree::new();
  t.write("user/ferridriver/config.toml", "extensions = [\"/abs/acme.ts\"]\n");
  let pinned = t.write("repo/ci.toml", "[mcp.server]\nname = \"ci\"\n");

  let mut opts = t.opts(t.repo());
  opts.explicit = Some(pinned);
  opts.inherit = false;
  let r = resolve(&opts).expect("resolve");

  assert_eq!(r.config.mcp.server_name(), "ci");
  assert!(r.config.extensions.paths().is_empty(), "inheritance was disabled");
}

#[test]
fn extends_is_applied_below_the_extending_file() {
  let t = Tree::new();
  t.write(
    "repo/base.toml",
    "[mcp.server]\nname = \"base\"\n\n[mcp.browser]\nheadless = true\n",
  );
  t.write(
    "repo/ferridriver.toml",
    "extends = [\"./base.toml\"]\n\n[mcp.server]\nname = \"child\"\n",
  );

  let r = resolve(&t.opts(t.repo())).expect("resolve");

  assert_eq!(r.config.mcp.server_name(), "child", "the extending file wins");
  assert!(r.config.mcp.headless(), "the extended file still contributes");
  assert_eq!(
    r.layers.iter().map(|l| l.kind).collect::<Vec<_>>(),
    [LayerKind::Extends, LayerKind::Cwd]
  );
}

#[test]
fn extends_cycle_terminates() {
  let t = Tree::new();
  t.write(
    "repo/ferridriver.toml",
    "extends = [\"./other.toml\"]\n\n[test]\nworkers = 2\n",
  );
  t.write(
    "repo/other.toml",
    "extends = [\"./ferridriver.toml\"]\n\n[test]\nretries = 4\n",
  );

  let r = resolve(&t.opts(t.repo())).expect("resolve");

  assert_eq!(r.config.test.workers, 2);
  assert_eq!(r.config.test.retries, 4);
}

#[test]
fn missing_extends_target_is_an_error() {
  let t = Tree::new();
  t.write("repo/ferridriver.toml", "extends = [\"./nope.toml\"]\n");

  let err = resolve(&t.opts(t.repo())).expect_err("must fail");
  assert!(err.to_string().contains("nope.toml"), "got: {err}");
}

#[test]
fn env_overrides_beat_every_file() {
  let t = Tree::new();
  t.write(
    "repo/ferridriver.toml",
    "[mcp.browser]\nheadless = false\nbackend = \"cdp-pipe\"\n",
  );

  let mut opts = t.opts(t.repo());
  opts
    .env
    .insert("FERRIDRIVER_MCP__BROWSER__HEADLESS".into(), "true".into());
  opts
    .env
    .insert("FERRIDRIVER_MCP__BROWSER__BACKEND".into(), "cdp-raw".into());
  opts.env.insert(
    "FERRIDRIVER_MCP__BROWSER__INSTANCE_ARGS_COMMAND".into(),
    "echo --from-env".into(),
  );
  let r = resolve(&opts).expect("resolve");

  assert!(r.config.mcp.headless());
  assert_eq!(
    r.config.mcp.browser.backend,
    Some(ferridriver_config::mcp::BackendChoice::CdpRaw)
  );
  assert_eq!(
    r.config.mcp.browser.instance_args_command.as_ref().map(|c| &c.run),
    Some(&ferridriver_config::CommandRun::Shell("echo --from-env".into())),
    "single `_` inside a segment is a camelCase boundary"
  );
  assert_eq!(
    r.provenance.get("mcp.browser.headless"),
    Some(&Origin::Env("FERRIDRIVER_MCP__BROWSER__HEADLESS".into()))
  );
}

#[test]
fn legacy_single_segment_env_vars_are_not_config_keys() {
  let t = Tree::new();
  t.write("repo/ferridriver.toml", "[test]\nworkers = 5\n");

  let mut opts = t.opts(t.repo());
  // Owned by the test runner's own override layer; must not be
  // mistaken for a document key here.
  opts.env.insert("FERRIDRIVER_WORKERS".into(), "9".into());
  opts.env.insert("FERRIDRIVER_DEBUG".into(), "1".into());
  let r = resolve(&opts).expect("resolve");

  assert_eq!(r.config.test.workers, 5);
  assert!(r.warnings.is_empty(), "no spurious warnings: {:?}", r.warnings);
}

#[test]
fn unknown_key_is_reported_with_a_suggestion() {
  let t = Tree::new();
  t.write("repo/ferridriver.toml", "[mcp.browser]\nchrome_args = [\"--x\"]\n");

  let r = resolve(&t.opts(t.repo())).expect("resolve");

  // Accepted for back-compat, so no warning for the documented alias...
  assert_eq!(r.config.mcp.chrome_args(), ["--x"]);

  let t2 = Tree::new();
  t2.write("repo/ferridriver.toml", "[mcp.browser]\nchrom_args = [\"--x\"]\n");
  let r2 = resolve(&t2.opts(t2.repo())).expect("resolve");
  let joined = r2
    .warnings
    .iter()
    .map(|w| w.message.clone())
    .collect::<Vec<_>>()
    .join("; ");
  assert!(joined.contains("chrom_args"), "typo must be reported: {joined}");
}

#[test]
fn mcp_section_accepts_camel_case_keys() {
  let t = Tree::new();
  t.write(
    "repo/ferridriver.toml",
    "[mcp.server]\nextraInstructions = \"hello\"\n\n[mcp.browser]\nchromeArgs = [\"--x\"]\nexecutablePath = \"/bin/chrome\"\ninstanceArgsCommand = \"echo hi\"\ncommandCacheTtl = 60\n",
  );

  let r = resolve(&t.opts(t.repo())).expect("resolve");

  assert_eq!(r.config.mcp.chrome_args(), ["--x"]);
  assert_eq!(r.config.mcp.browser.executable_path.as_deref(), Some("/bin/chrome"));
  assert_eq!(
    r.config.mcp.browser.instance_args_command.as_ref().map(|c| &c.run),
    Some(&ferridriver_config::CommandRun::Shell("echo hi".into()))
  );
  assert_eq!(r.config.mcp.browser.command_cache_ttl, Some(60));
  assert!(
    r.config.mcp.server_instructions("base").contains("hello"),
    "extraInstructions must be honoured under its camelCase spelling"
  );
  assert!(r.warnings.is_empty(), "camelCase is canonical: {:?}", r.warnings);
}

#[test]
fn snake_case_mcp_keys_still_parse() {
  let t = Tree::new();
  t.write(
    "repo/ferridriver.toml",
    "[mcp.server]\nextra_instructions = \"hello\"\n\n[mcp.browser]\ninstance_discover_command = \"echo ws\"\ncommand_cache_ttl = 30\n",
  );

  let r = resolve(&t.opts(t.repo())).expect("resolve");

  assert!(r.config.mcp.server_instructions("base").contains("hello"));
  assert_eq!(
    r.config.mcp.browser.instance_discover_command.as_ref().map(|c| &c.run),
    Some(&ferridriver_config::CommandRun::Shell("echo ws".into()))
  );
  assert_eq!(r.config.mcp.browser.command_cache_ttl, Some(30));
}

#[test]
fn invalid_backend_is_a_load_error() {
  let t = Tree::new();
  t.write("repo/ferridriver.toml", "[mcp.browser]\nbackend = \"chrom-pipe\"\n");

  let err = resolve(&t.opts(t.repo())).expect_err("a typo must not silently become cdp-pipe");
  let msg = err.to_string();
  assert!(msg.contains("chrom-pipe"), "names the bad value: {msg}");
  assert!(msg.contains("cdp-pipe"), "lists the valid values: {msg}");
}

#[test]
fn malformed_discovered_file_is_an_error() {
  let t = Tree::new();
  t.write("repo/ferridriver.toml", "[mcp.browser\nheadless = true\n");

  let err = resolve(&t.opts(t.repo())).expect_err("must fail");
  assert!(err.to_string().contains("invalid TOML"), "got: {err}");
}

#[test]
fn empty_config_file_participates_without_error() {
  let t = Tree::new();
  t.write("user/ferridriver/config.yaml", "");
  t.write("repo/ferridriver.toml", "[test]\nworkers = 4\n");

  let r = resolve(&t.opts(t.repo())).expect("resolve");
  assert_eq!(r.config.test.workers, 4);
}

#[test]
fn no_files_anywhere_yields_defaults() {
  let t = Tree::new();
  let r = resolve(&t.opts(t.root().to_path_buf())).expect("resolve");
  assert!(r.layers.is_empty());
  assert_eq!(r.config.mcp.server_name(), "ferridriver");
}

/// `defineDefaults` is the LOWEST config layer, and the authoring
/// contract that describes it cannot drift from the schema it feeds.
mod extension_defaults {
  use std::collections::BTreeMap;
  use std::path::{Path, PathBuf};

  use ferridriver_config::layer::{self, LoadOptions};

  fn opts(cwd: PathBuf, defaults: Vec<(String, serde_json::Value)>) -> LoadOptions {
    LoadOptions {
      explicit: None,
      cwd,
      user_config_dir: None,
      machine_config_dir: None,
      env: BTreeMap::new(),
      inherit: true,
      extension_defaults: defaults,
      cache: ferridriver_config::layer::LayerCache::default(),
      module_loader: None,
      documents_only: false,
    }
  }

  fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ferri-defaults-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir.canonicalize().expect("canonical")
  }

  #[test]
  fn a_contribution_applies_when_the_config_is_silent_and_loses_when_it_speaks() {
    let dir = scratch("precedence");
    let defaults = vec![(
      "pkg".to_string(),
      serde_json::json!({ "test": { "testDir": "from-extension", "timeout": 12345 } }),
    )];

    // Nothing on disk: the contribution is what the run sees.
    let resolved = layer::resolve(&opts(dir.clone(), defaults.clone())).expect("resolve");
    assert_eq!(resolved.config.test.test_dir.as_deref(), Some("from-extension"));
    assert_eq!(resolved.config.test.timeout, 12345);
    assert_eq!(
      resolved
        .provenance
        .get("test.timeout")
        .map(ferridriver_config::layer::Origin::describe),
      Some("extension pkg".to_string()),
      "`ferridriver config` can say where it came from",
    );

    // A config file speaks: it wins, key by key.
    std::fs::write(dir.join("ferridriver.toml"), "[test]\ntimeout = 999\n").expect("write config");
    let resolved = layer::resolve(&opts(dir.clone(), defaults)).expect("resolve");
    assert_eq!(resolved.config.test.timeout, 999, "the file overrides the contribution");
    assert_eq!(
      resolved.config.test.test_dir.as_deref(),
      Some("from-extension"),
      "and a key the file is silent about keeps the contributed value",
    );
  }

  #[test]
  fn later_packages_win_over_earlier_ones() {
    let dir = scratch("order");
    let resolved = layer::resolve(&opts(
      dir,
      vec![
        ("first".to_string(), serde_json::json!({ "test": { "timeout": 1 } })),
        ("second".to_string(), serde_json::json!({ "test": { "timeout": 2 } })),
      ],
    ))
    .expect("resolve");
    assert_eq!(resolved.config.test.timeout, 2);
    assert_eq!(
      resolved
        .provenance
        .get("test.timeout")
        .map(ferridriver_config::layer::Origin::describe),
      Some("extension second".to_string()),
    );
  }

  #[test]
  fn a_typo_names_the_key() {
    let dir = scratch("typo");
    let error = layer::resolve(&opts(
      dir,
      vec![(
        "pkg".to_string(),
        serde_json::json!({ "test": { "testIdAttribut": "data-qa" } }),
      )],
    ))
    .expect_err("a contributed typo is a hard failure, not a warning");
    let message = format!("{error}");
    assert!(message.contains("testIdAttribut"), "{message}");
    assert!(message.contains("pkg"), "{message}");
  }

  #[test]
  fn the_loader_configuring_sections_are_refused() {
    let dir = scratch("refused");
    for (payload, needle) in [
      (serde_json::json!({ "extensions": { "policy": {} } }), "extensions"),
      (serde_json::json!({ "bundler": { "conditions": ["node"] } }), "bundler"),
      (
        serde_json::json!({ "scripting": { "allowEnv": ["HOME"] } }),
        "scripting",
      ),
      (
        serde_json::json!({ "test": { "moduleAliases": { "a": "b" } } }),
        "test.moduleAliases",
      ),
    ] {
      let error = layer::resolve(&opts(dir.clone(), vec![("pkg".to_string(), payload)]))
        .expect_err("a package may not configure the loader that read it");
      let message = format!("{error}");
      assert!(message.contains(needle), "expected `{needle}` in: {message}");
    }
  }

  /// The `.d.ts` an extension author type-checks against enumerates the
  /// keys — no index signature — so it can only stay right if something
  /// pins it against the schema it feeds.
  #[test]
  fn the_authoring_contract_lists_exactly_the_schema_s_keys() {
    let packages = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages");
    let contract = std::fs::read_to_string(packages.join("ferridriver-extension/index.d.ts")).expect("contract");
    // The `--config <file.ts>` authoring type is the same enumeration
    // for the same reason, and it is a different file.
    let config_contract = std::fs::read_to_string(packages.join("ferridriver-test/index.d.ts")).expect("contract");

    let declared_in = |source: &str, interface: &str| -> Vec<String> {
      let start = source
        .find(&format!("interface {interface} {{"))
        .unwrap_or_else(|| panic!("{interface} is declared"));
      let body = &source[start..];
      let end = body.find("\n}").map_or_else(
        || body.find("\n  }").expect("interface closes"),
        |at| body.find("\n  }").map_or(at, |inner| inner.min(at)),
      );
      body[..end]
        .lines()
        .skip(1)
        .map(str::trim)
        // A key may carry a JSDoc block; the keys are what is pinned.
        .filter(|line| !(line.starts_with("/*") || line.starts_with('*') || line.starts_with("//")))
        .filter_map(|line| line.split('?').next().map(str::to_string))
        .filter(|key| !key.is_empty())
        .collect()
    };
    let declared = |interface: &str| declared_in(&contract, interface);

    let document = serde_json::to_value(ferridriver_config::FerridriverConfig::default()).expect("serialize");
    let schema_keys = |section: &str, refused: &[&str]| -> Vec<String> {
      document[section]
        .as_object()
        .expect("section")
        .keys()
        .filter(|key| !refused.contains(&key.as_str()))
        .cloned()
        .collect()
    };

    assert_eq!(
      declared("TestConfigDefaults"),
      schema_keys("test", &["moduleAliases"]),
      "TestConfigDefaults must list every `[test]` key an extension may set, and only those",
    );
    assert_eq!(declared("McpConfigDefaults"), schema_keys("mcp", &[]));
    // A config module may also write the document aliases, which are
    // folded into the schema before anything deserializes.
    let mut config_keys = schema_keys("test", &["moduleAliases"]);
    config_keys.extend(
      ferridriver_config::layer::DOCUMENT_ALIASES
        .iter()
        .map(|alias| (*alias).to_string()),
    );
    config_keys.sort();
    assert_eq!(
      declared_in(&config_contract, "FerridriverTestConfig"),
      config_keys,
      "FerridriverTestConfig must list every `[test]` key a config module may set plus the document \
       aliases, and only those",
    );
  }
}

/// The passes of one startup share the files they read.
#[test]
fn a_shared_cache_reads_each_layer_once_across_passes() {
  use ferridriver_config::layer::LayerCache;

  let dir = std::env::temp_dir().join(format!("ferri-cache-{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).expect("scratch");
  let file = dir.join("ferridriver.toml");
  std::fs::write(&file, "[test]\ntimeout = 1234\n").expect("write");

  let cache = LayerCache::default();
  let first = cache.parse(&file, None).expect("first parse");
  assert_eq!(first["test"]["timeout"], 1234);

  // Change the file underneath. A second pass must answer with what the
  // first pass read, or a startup could merge two different states of
  // the disk into one document.
  std::fs::write(&file, "[test]\ntimeout = 9999\n").expect("rewrite");
  let second = cache.parse(&file, None).expect("second parse");
  assert_eq!(
    second["test"]["timeout"], 1234,
    "the cached document is served, not a fresh read"
  );

  // A cache that never saw the file reads it, so nothing is pinned
  // process-wide.
  let fresh = LayerCache::default().parse(&file, None).expect("fresh parse");
  assert_eq!(fresh["test"]["timeout"], 9999);

  let _ = std::fs::remove_dir_all(&dir);
}

// ── Device descriptors ──────────────────────────────────────────────

#[test]
fn a_device_pre_seeds_every_key_its_descriptor_carries() {
  let t = Tree::new();
  t.write("repo/ferridriver.toml", "[test.browser.use]\ndevice = \"iPhone 15\"\n");

  let r = resolve(&t.opts(t.repo())).expect("resolve");
  let browser = &r.config.test.browser;
  let used = &browser.use_options;

  assert!(used.user_agent.as_deref().expect("userAgent").contains("iPhone"));
  assert!(used.is_mobile);
  assert!(used.has_touch);
  assert_eq!(used.device_scale_factor, Some(3.0));
  assert_eq!(used.default_browser_type.as_deref(), Some("webkit"));
  let viewport = used
    .viewport
    .as_ref()
    .expect("viewport is set")
    .size()
    .expect("not null");
  assert_eq!((viewport.width, viewport.height), (393, 659));
  let screen = used.screen.clone().expect("screen");
  assert_eq!((screen.width, screen.height), (393, 852));
}

#[test]
fn a_key_written_beside_the_device_wins_over_it() {
  let t = Tree::new();
  t.write(
    "repo/ferridriver.toml",
    "[test.browser.use]\ndevice = \"iPhone 15\"\nhasTouch = false\nuserAgent = \"mine\"\n",
  );

  let r = resolve(&t.opts(t.repo())).expect("resolve");
  let used = &r.config.test.browser.use_options;

  assert!(!used.has_touch, "an explicit hasTouch is not overwritten by the device");
  assert_eq!(used.user_agent.as_deref(), Some("mine"));
  // Untouched keys still come from the descriptor.
  assert!(used.is_mobile);
}

#[test]
fn a_higher_layer_wins_over_a_lower_layers_device() {
  let t = Tree::new();
  t.write(
    "user/ferridriver/config.toml",
    "[test.browser.use]\ndevice = \"iPhone 15\"\n",
  );
  t.write(
    "repo/ferridriver.toml",
    "[test.browser.use]\nuserAgent = \"repo-agent\"\n",
  );

  let r = resolve(&t.opts(t.repo())).expect("resolve");
  let used = &r.config.test.browser.use_options;

  // Expansion happens per layer, before the fold, so the device's
  // userAgent is an ordinary lower-layer value the repo overrides.
  assert_eq!(used.user_agent.as_deref(), Some("repo-agent"));
  assert!(used.is_mobile, "the rest of the descriptor still applies");
}

#[test]
fn an_unknown_device_name_is_reported_not_expanded() {
  let t = Tree::new();
  t.write("repo/ferridriver.toml", "[test.browser.use]\ndevice = \"Nokia 3310\"\n");

  let r = resolve(&t.opts(t.repo())).expect("resolve");
  let used = &r.config.test.browser.use_options;

  assert_eq!(used.device.as_deref(), Some("Nokia 3310"));
  assert!(used.user_agent.is_none(), "nothing is invented for a name nobody ships");
  assert!(used.viewport.is_none());
}

#[test]
fn the_top_level_use_block_is_the_browsers_use_block() {
  let t = Tree::new();
  t.write(
    "repo/ferridriver.toml",
    "[test.use]\nlocale = \"fr-FR\"\ndevice = \"Pixel 5\"\n",
  );

  let r = resolve(&t.opts(t.repo())).expect("resolve");
  let used = &r.config.test.browser.use_options;

  assert_eq!(used.locale.as_deref(), Some("fr-FR"));
  assert_eq!(used.device_scale_factor, Some(2.75));
  assert!(
    r.warnings.iter().all(|w| !w.message.contains("use")),
    "the alias is folded into the schema, not reported as unknown: {:?}",
    r.warnings
  );
}

#[test]
fn the_browsers_own_use_key_wins_over_the_top_level_one() {
  let t = Tree::new();
  t.write(
    "repo/ferridriver.toml",
    "[test.use]\nlocale = \"fr-FR\"\n\n[test.browser.use]\nlocale = \"de-DE\"\n",
  );

  let r = resolve(&t.opts(t.repo())).expect("resolve");
  assert_eq!(r.config.test.browser.use_options.locale.as_deref(), Some("de-DE"));
}

#[test]
fn a_projects_use_block_reaches_that_projects_context() {
  let t = Tree::new();
  t.write(
    "repo/ferridriver.toml",
    "[test.browser]\nheadless = true\n\n[[test.projects]]\nname = \"phone\"\n\n[test.projects.use]\ndevice = \"iPhone 15\"\n",
  );

  let r = resolve(&t.opts(t.repo())).expect("resolve");
  let project = r.config.test.projects.first().expect("project");
  let effective = r.config.test.merge_project(project);

  assert!(effective.browser.use_options.is_mobile);
  assert_eq!(effective.browser.browser, "webkit", "the descriptor names the engine");
  assert_eq!(effective.browser.backend, "webkit");
  assert!(
    effective.browser.headless,
    "a project `use` block must not materialise a browser block and turn headless back off",
  );
}

#[test]
fn an_explicit_browser_name_beats_the_devices_engine() {
  let t = Tree::new();
  t.write(
    "repo/ferridriver.toml",
    "[test.use]\ndevice = \"iPhone 15\"\nbrowserName = \"chromium\"\n",
  );

  let r = resolve(&t.opts(t.repo())).expect("resolve");
  let mut browser = r.config.test.browser.clone();
  browser.apply_use_engine();
  browser.normalize();

  assert_eq!(browser.browser, "chromium");
}

#[test]
fn a_device_does_not_move_an_engine_someone_else_chose() {
  let t = Tree::new();
  t.write(
    "repo/ferridriver.toml",
    "[test.browser]\nbrowser = \"firefox\"\nbackend = \"bidi\"\n\n[test.use]\ndevice = \"iPhone 15\"\n",
  );

  let r = resolve(&t.opts(t.repo())).expect("resolve");
  let mut browser = r.config.test.browser.clone();
  browser.apply_use_engine();
  browser.normalize();

  assert_eq!(
    browser.browser, "firefox",
    "defaultBrowserType only fills an unset engine"
  );
  assert!(
    browser.use_options.is_mobile,
    "the rest of the descriptor still applies"
  );
}

#[test]
fn a_null_viewport_is_not_an_absent_one() {
  let t = Tree::new();
  t.write("repo/ferridriver.toml", "[test.use]\nviewport = { }\n");
  let err = resolve(&t.opts(t.repo())).expect_err("an empty table is not a size");
  assert!(err.to_string().contains("width"), "{err}");

  let t2 = Tree::new();
  t2.write("repo/ferridriver.yaml", "test:\n  use:\n    viewport: null\n");
  let r2 = resolve(&t2.opts(t2.repo())).expect("resolve");
  assert!(
    matches!(
      r2.config.test.browser.use_options.viewport,
      Some(ferridriver_config::test::ViewportOverride::Disabled)
    ),
    "`viewport: null` says NO fixed viewport, which absent does not",
  );
}
