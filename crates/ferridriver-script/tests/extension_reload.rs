//! Recompiling an extension after an edit must see the edit.
//!
//! The bytecode caches keyed freshness on a source-map-derived input set,
//! and a helper module whose bindings are all inlined leaves no mapping
//! tokens — so it never appeared as an input and an edit to it looked like
//! no change at all. Every reload path (`ferridriver_extensions action:
//! "reload"`, `ferridriver ext dev`) is a second
//! `compile_and_extract_extensions` call in one process, which is exactly
//! where that bites.

use std::path::{Path, PathBuf};

/// Panic with context instead of `unwrap`/`expect`.
fn ok<T, E: std::fmt::Display>(result: Result<T, E>, what: &str) -> T {
  match result {
    Ok(value) => value,
    Err(e) => panic!("{what}: {e}"),
  }
}

fn write(path: &Path, contents: &str) {
  if let Some(parent) = path.parent() {
    ok(std::fs::create_dir_all(parent), "create parent dir");
  }
  ok(std::fs::write(path, contents), "write file");
}

async fn tool_names(entry: &PathBuf) -> Vec<String> {
  let (compiled, failures) = ferridriver_script::compile_and_extract_extensions(std::slice::from_ref(entry)).await;
  assert!(failures.is_empty(), "{failures:?}");
  assert_eq!(compiled.len(), 1, "one entry in, one compile out");
  let manifests: serde_json::Value = ok(serde_json::from_str(&compiled[0].manifests_json), "parse manifests");
  match manifests.as_array() {
    Some(tools) => tools
      .iter()
      .filter_map(|m| m["name"].as_str().map(str::to_string))
      .collect(),
    None => panic!("manifests must be a JSON array, got {manifests}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn editing_an_imported_helper_invalidates_the_entry() {
  let dir = ok(tempfile::tempdir(), "tempdir");
  let entry = dir.path().join("tool.ts");
  let lib = dir.path().join("lib/shared.ts");

  write(&lib, "export const NAME = 'probe.first';\n");
  write(
    &entry,
    "import { NAME } from './lib/shared';\n\
     defineTool({ name: NAME, exposeAsTool: true, handler: async () => ({}) });\n",
  );

  assert_eq!(tool_names(&entry).await, ["probe.first"]);

  // Only the helper changes; the entry file's bytes are identical.
  write(&lib, "export const NAME = 'probe.second';\n");

  assert_eq!(
    tool_names(&entry).await,
    ["probe.second"],
    "a second compile in the same process must pick up the edited helper"
  );
}

/// The layout a real extension package has: entries under `src/`, helpers
/// under `src/lib/`. This is the case that stayed stale when the input set
/// came from the source map.
#[tokio::test(flavor = "multi_thread")]
async fn editing_a_helper_inside_a_package_invalidates_the_entry() {
  let dir = ok(tempfile::tempdir(), "tempdir");
  let entry = dir.path().join("pkg/src/login.ts");
  let lib = dir.path().join("pkg/src/lib/shared.ts");
  write(&dir.path().join("pkg/package.json"), r#"{"name":"p","type":"module"}"#);
  write(&lib, "export const NAME = 'probe.first';\n");
  write(
    &entry,
    "import { NAME } from './lib/shared';\n\
     defineTool({ name: NAME, exposeAsTool: true, handler: async () => ({}) });\n",
  );
  assert_eq!(tool_names(&entry).await, ["probe.first"]);

  write(&lib, "export const NAME = 'probe.second';\n");
  assert_eq!(tool_names(&entry).await, ["probe.second"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unchanged_tree_still_recompiles_to_the_same_manifest() {
  let dir = ok(tempfile::tempdir(), "tempdir");
  let entry = dir.path().join("tool.ts");
  write(
    &entry,
    "defineTool({ name: 'stable.tool', handler: async () => ({}) });\n",
  );

  assert_eq!(tool_names(&entry).await, ["stable.tool"]);
  assert_eq!(tool_names(&entry).await, ["stable.tool"], "cache hit stays correct");
}
