//! File watcher for watch mode: detects file changes, classifies them,
//! and sends events through an async channel with debounce.

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};

/// What kind of file changed.
#[derive(Debug, Clone)]
pub enum ChangeKind {
  /// A test file matching `test_match` globs.
  TestFile(PathBuf),
  /// A source file (any .rs/.ts/.js not matching test globs).
  SourceFile(PathBuf),
  /// A BDD `.feature` file.
  FeatureFile(PathBuf),
  /// A BDD step definition file.
  StepFile(PathBuf),
  /// Config file (`ferridriver.config.toml` or `.json`).
  Config,
}

/// Default directories to ignore (checked as path segments, no allocation).
const DEFAULT_IGNORE_SEGMENTS: &[&str] = &[
  "target",
  "node_modules",
  ".git",
  "test-results",
  "dist",
  ".next",
];

/// Check if a path contains any ignored directory segment.
/// Zero-allocation: iterates path components directly.
fn is_ignored(path: &Path, extra_ignore: &[String]) -> bool {
  path.components().any(|c| {
    if let std::path::Component::Normal(s) = c {
      let s = s.to_str().unwrap_or("");
      DEFAULT_IGNORE_SEGMENTS.iter().any(|ign| *ign == s)
        || extra_ignore.iter().any(|ign| ign == s)
    } else {
      false
    }
  })
}

/// File watcher with debounce and change classification.
pub struct FileWatcher {
  rx: async_channel::Receiver<ChangeKind>,
  // Keep the debouncer alive — dropping it stops watching.
  _debouncer: notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
}

impl FileWatcher {
  /// Create a new file watcher on the given root directory.
  ///
  /// Changes are classified using `test_match` globs from the config.
  /// Debounce window is 100ms.
  ///
  /// # Errors
  ///
  /// Returns an error if the watcher cannot be created or the path cannot be watched.
  /// Create a new file watcher.
  ///
  /// * `root` — Directory to watch recursively.
  /// * `test_globs` — Glob patterns that identify test files (from `test_match` config).
  /// * `ignore_patterns` — Extra directory names to ignore (merged with defaults).
  pub fn new(
    root: &Path,
    test_globs: &[String],
    ignore_patterns: &[String],
  ) -> Result<Self, String> {
    let (tx, rx) = async_channel::bounded(256);
    let compiled_globs: Vec<glob::Pattern> = test_globs
      .iter()
      .filter_map(|g| glob::Pattern::new(g).ok())
      .collect();
    let root_owned = root.to_path_buf();
    let extra_ignore: Vec<String> = ignore_patterns.to_vec();

    let mut debouncer = new_debouncer(
      Duration::from_millis(100),
      move |result: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| {
        let Ok(events) = result else { return };
        for event in events {
          if event.kind != DebouncedEventKind::Any {
            continue;
          }
          let path = &event.path;

          // Skip ignored directories — zero allocation for defaults, linear for extras.
          if is_ignored(path, &extra_ignore) {
            continue;
          }

          // Early extension filter — skip files that can never be relevant.
          // Only watch source code, test files, features, and config files.
          let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
          match ext {
            "rs" | "ts" | "tsx" | "js" | "jsx" | "mts" | "mjs" | "feature" => {}
            "toml" | "json" => {
              // Only config files — not arbitrary data files.
              let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
              if !name.starts_with("ferridriver.config")
                && !name.starts_with("tsconfig")
                && !name.starts_with("package")
              {
                continue;
              }
            }
            _ => continue,
          }

          let kind = classify_change(path, &root_owned, &compiled_globs);
          let _ = tx.try_send(kind);
        }
      },
    )
    .map_err(|e| format!("create file watcher: {e}"))?;

    debouncer
      .watcher()
      .watch(root, notify::RecursiveMode::Recursive)
      .map_err(|e| format!("watch {}: {e}", root.display()))?;

    Ok(Self {
      rx,
      _debouncer: debouncer,
    })
  }

  /// Receive the next file change event (async).
  pub async fn recv(&self) -> Option<ChangeKind> {
    self.rx.recv().await.ok()
  }

  /// Non-blocking drain of all pending changes, deduplicated by path.
  pub fn drain_deduped(&self) -> Vec<ChangeKind> {
    let mut seen = rustc_hash::FxHashSet::default();
    let mut changes = Vec::new();
    while let Ok(kind) = self.rx.try_recv() {
      let key = match &kind {
        ChangeKind::TestFile(p)
        | ChangeKind::SourceFile(p)
        | ChangeKind::FeatureFile(p)
        | ChangeKind::StepFile(p) => p.clone(),
        ChangeKind::Config => std::path::PathBuf::from("__config__"),
      };
      if seen.insert(key) {
        changes.push(kind);
      }
    }
    changes
  }
}

/// Classify a changed file path. Zero-allocation for the common case
/// (extension check + component iteration, no string formatting).
fn classify_change(path: &Path, root: &Path, test_globs: &[glob::Pattern]) -> ChangeKind {
  let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

  // Config files — check filename directly, no allocation.
  if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
    if name == "ferridriver.config.toml" || name == "ferridriver.config.json" {
      return ChangeKind::Config;
    }
  }

  // Feature files.
  if ext == "feature" {
    return ChangeKind::FeatureFile(path.to_path_buf());
  }

  // Step definition files — check path components, no string search.
  if (ext == "rs" || ext == "ts" || ext == "js") && path.components().any(|c| {
    matches!(c, std::path::Component::Normal(s) if s == "steps" || s == "step_definitions")
  }) {
    return ChangeKind::StepFile(path.to_path_buf());
  }

  // Test files — match against compiled globs.
  if let Ok(relative) = path.strip_prefix(root) {
    let rel_str = relative.to_string_lossy();
    for glob in test_globs {
      if glob.matches(&rel_str) {
        return ChangeKind::TestFile(path.to_path_buf());
      }
    }
  }

  // Everything else is a source file.
  ChangeKind::SourceFile(path.to_path_buf())
}
