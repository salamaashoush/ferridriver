//! Config layer discovery across machine, user, repository ancestor, cwd,
//! local, and explicit `--config` locations.

use std::path::{Path, PathBuf};

use super::paths::normalize_path;
use super::{ConfigLayer, ConfigWarning, LayerKind, LoadOptions, is_script_config};

/// Config file basenames searched for a project/cwd layer, in format
/// precedence order.
const PROJECT_BASENAMES: &[&str] = &[
  "ferridriver.toml",
  "ferridriver.yaml",
  "ferridriver.yml",
  "ferridriver.json",
  "ferridriver.config.ts",
  "ferridriver.config.mts",
  "ferridriver.config.js",
  "ferridriver.config.mjs",
];

/// Personal-override basenames, searched alongside [`PROJECT_BASENAMES`].
const LOCAL_BASENAMES: &[&str] = &[
  "ferridriver.local.toml",
  "ferridriver.local.yaml",
  "ferridriver.local.yml",
  "ferridriver.local.json",
  "ferridriver.local.ts",
  "ferridriver.local.js",
];

/// Basenames searched inside a machine/user config DIRECTORY.
const DIR_BASENAMES: &[&str] = &[
  "config.toml",
  "config.yaml",
  "config.yml",
  "config.json",
  "config.ts",
  "config.js",
];

/// The `--config` file, made absolute against the run's cwd. Relative
/// paths inside a layer anchor to the layer's directory, and a directory
/// taken from a relative `--config` stayed relative to the cwd: `testDir`
/// came out cwd-relative, and the snapshot template, which resolves a
/// relative `{testDir}` against the config's directory, prefixed it again.
fn explicit_layer_path(opts: &LoadOptions) -> Option<PathBuf> {
  let path = opts.explicit.as_ref()?;
  Some(if path.is_absolute() {
    path.clone()
  } else {
    normalize_path(&opts.cwd.join(path))
  })
}

/// Build the ordered list of files to apply, before `extends` expansion.
pub(super) fn discover_layers(opts: &LoadOptions, warnings: &mut Vec<ConfigWarning>) -> Vec<ConfigLayer> {
  if !opts.inherit {
    return match explicit_layer_path(opts) {
      Some(path) => vec![ConfigLayer {
        kind: LayerKind::Explicit,
        path,
      }],
      None => highest_precedence_file(opts, warnings),
    };
  }

  let mut layers = Vec::new();

  if let Some(dir) = &opts.machine_config_dir {
    for path in first_existing_reported(&dir.join("ferridriver"), DIR_BASENAMES, warnings) {
      layers.push(ConfigLayer {
        kind: LayerKind::Machine,
        path,
      });
    }
  }

  if let Some(dir) = &opts.user_config_dir {
    for path in first_existing_reported(&dir.join("ferridriver"), DIR_BASENAMES, warnings) {
      layers.push(ConfigLayer {
        kind: LayerKind::User,
        path,
      });
    }
  }

  // Every ancestor between the repository root and the cwd may carry a
  // config, applied outermost first so the NEAREST one wins. That is
  // what makes a monorepo work: the root sets shared defaults, a
  // package overrides just its own keys.
  let git_root = find_git_root(&opts.cwd);
  for dir in ancestor_chain(git_root.as_deref(), &opts.cwd) {
    for path in first_existing_reported(&dir, PROJECT_BASENAMES, warnings) {
      layers.push(ConfigLayer {
        kind: LayerKind::Project,
        path,
      });
    }
  }

  for path in first_existing_reported(&opts.cwd, PROJECT_BASENAMES, warnings) {
    layers.push(ConfigLayer {
      kind: LayerKind::Cwd,
      path,
    });
  }

  for dir in ancestor_chain(git_root.as_deref(), &opts.cwd) {
    for path in first_existing_reported(&dir, LOCAL_BASENAMES, warnings) {
      layers.push(ConfigLayer {
        kind: LayerKind::Local,
        path,
      });
    }
  }
  for path in first_existing_reported(&opts.cwd, LOCAL_BASENAMES, warnings) {
    layers.push(ConfigLayer {
      kind: LayerKind::Local,
      path,
    });
  }

  if let Some(path) = &explicit_layer_path(opts) {
    if path.exists() {
      layers.push(ConfigLayer {
        kind: LayerKind::Explicit,
        path: path.clone(),
      });
    } else {
      warnings.push(ConfigWarning {
        source: path.display().to_string(),
        message: "config file named by --config does not exist".to_string(),
      });
    }
  }

  layers
}

/// The single file a non-inheriting load would use: the cwd's own
/// config, and nothing else.
///
/// Deliberately does NOT fall back to the user or machine directory —
/// "no inherit" has to mean it, or a reproducible run would still pick
/// up whatever an operator installed under `~/.config`.
fn highest_precedence_file(opts: &LoadOptions, warnings: &mut Vec<ConfigWarning>) -> Vec<ConfigLayer> {
  let local: Vec<ConfigLayer> = first_existing_reported(&opts.cwd, LOCAL_BASENAMES, warnings)
    .into_iter()
    .map(|path| ConfigLayer {
      kind: LayerKind::Local,
      path,
    })
    .collect();
  if !local.is_empty() {
    return local;
  }
  first_existing_reported(&opts.cwd, PROJECT_BASENAMES, warnings)
    .into_iter()
    .map(|path| ConfigLayer {
      kind: LayerKind::Cwd,
      path,
    })
    .collect()
}

/// The configs `dir` contributes: at most one DOCUMENT and at most one
/// MODULE, document first so the module folds on top of it.
///
/// A directory carries both when it has to. `extensions`, `[bundler]`
/// and `[extensions.policy]` are resolved before any module can be
/// compiled — that is what the two-pass load exists for — so a suite
/// whose config is a TypeScript module still needs a document beside it
/// to name the packages that serve the module's own imports. Treating
/// the pair as a shadow made that combination unexpressible: the
/// document won and the module was reported as ignored.
///
/// Within a family the highest-precedence basename still wins alone and
/// a shadowed sibling still warns. Silently taking `ferridriver.toml`
/// while a `ferridriver.yaml` sits beside it makes every edit to the
/// yaml look like it does nothing.
fn first_existing_reported(dir: &Path, basenames: &[&str], warnings: &mut Vec<ConfigWarning>) -> Vec<PathBuf> {
  // One `stat` per candidate, NOT one `read_dir`: the cost has to scale
  // with the number of config formats, which is a fixed handful, and not
  // with the number of files in the directory, which is unbounded.
  // Listing the directory instead measured 29ms slower in a tree with
  // 5000 files beside the config — and a repository root is exactly
  // where a config lives.
  let (documents, modules): (Vec<PathBuf>, Vec<PathBuf>) = basenames
    .iter()
    .map(|name| dir.join(name))
    .filter(|c| c.is_file())
    .partition(|c| !is_script_config(c));

  let mut winners = Vec::new();
  for family in [documents, modules] {
    let mut family = family.into_iter();
    let Some(winner) = family.next() else { continue };
    let shadowed: Vec<String> = family.map(|p| p.display().to_string()).collect();
    if !shadowed.is_empty() {
      warnings.push(ConfigWarning {
        source: winner.display().to_string(),
        message: format!("also present and ignored: {}", shadowed.join(", ")),
      });
    }
    winners.push(winner);
  }
  winners
}

/// Directories from `root` down to (but excluding) `cwd`, outermost
/// first. Empty when there is no repository root, or when `cwd` IS the
/// root — the cwd layer covers that case.
fn ancestor_chain(root: Option<&Path>, cwd: &Path) -> Vec<PathBuf> {
  let Some(root) = root else { return Vec::new() };
  let mut chain = Vec::new();
  let mut current = cwd.parent();
  while let Some(dir) = current {
    if !dir.starts_with(root) {
      break;
    }
    chain.push(dir.to_path_buf());
    if dir == root {
      break;
    }
    current = dir.parent();
  }
  chain.reverse();
  chain
}

/// Nearest ancestor of `from` (inclusive) containing a `.git` entry.
fn find_git_root(from: &Path) -> Option<PathBuf> {
  let mut dir = Some(from);
  while let Some(current) = dir {
    if current.join(".git").exists() {
      return Some(current.to_path_buf());
    }
    dir = current.parent();
  }
  None
}
