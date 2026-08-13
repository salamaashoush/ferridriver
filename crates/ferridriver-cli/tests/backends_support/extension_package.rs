//! Extension PACKAGES: the `ferridriver` field of a `package.json`.
//!
//! A real extension package has several tool modules plus a shared
//! `lib/`. Before the manifest there was no way to express that: Node's
//! entry fields describe one entry, and pointing the config at the
//! directory made every `lib/` module load as its own extension and fail
//! with "no tools declared". Authors worked around it by listing each
//! entry file in the config by hand, which drifts the moment the package
//! gains a file.
//!
//! `requires` is the other half: the package states the binaries, env
//! names, hosts and sidecars it needs, and an unmet requirement stops the
//! package from loading with a message naming the config key that fixes
//! it — instead of the first tool call failing somewhere in a handler.

use serde_json::json;

use super::client::McpClient;

const LIB_SRC: &str = "export const SHARED_MARKER = 'from-lib';\n";

/// An entry that declares NO tool: it contributes a script-host global.
/// Dropping such a file (which is what "no tools declared" used to do) means
/// its top-level code never runs, so nothing it contributes exists.
const GLOBALS_SRC: &str = r"
globalThis.pkgprobeGlobal = 'installed-by-globals-entry';
";

const LOGIN_SRC: &str = r"
import { SHARED_MARKER } from './lib/shared';

defineTool({
  name: 'pkgprobe.login',
  description: 'Entry one',
  exposeAsTool: true,
  inputSchema: { type: 'object', properties: {} },
  async handler({ settings }) {
    return {
      marker: SHARED_MARKER,
      origin: settings?.origin ?? null,
      // Set by the toolless entry's top-level code.
      fromGlobalsEntry: globalThis.pkgprobeGlobal ?? null,
    };
  },
});
";

const SIGN_SRC: &str = r"
import { SHARED_MARKER } from './lib/shared';

defineTool({
  name: 'pkgprobe.sign',
  description: 'Entry two',
  exposeAsTool: true,
  inputSchema: { type: 'object', properties: {} },
  async handler() {
    return { marker: SHARED_MARKER };
  },
});
";

/// Write the package tree. `manifest_extra` is spliced into the
/// `ferridriver` object so each case can vary `requires` / `settings`.
fn write_package(root: &std::path::Path, manifest_extra: &str) -> std::path::PathBuf {
  let pkg = root.join("pkgprobe");
  std::fs::create_dir_all(pkg.join("src/lib")).expect("mkdir pkg");
  std::fs::write(
    pkg.join("package.json"),
    format!(
      r#"{{
        "name": "@probe/pkgprobe",
        "type": "module",
        "ferridriver": {{
          "entries": ["./src/globals.ts", "./src/login.ts", "./src/sign.ts"]{manifest_extra}
        }}
      }}"#
    ),
  )
  .expect("write package.json");
  std::fs::write(pkg.join("src/lib/shared.ts"), LIB_SRC).expect("write lib");
  std::fs::write(pkg.join("src/globals.ts"), GLOBALS_SRC).expect("write globals");
  std::fs::write(pkg.join("src/login.ts"), LOGIN_SRC).expect("write login");
  std::fs::write(pkg.join("src/sign.ts"), SIGN_SRC).expect("write sign");
  pkg
}

fn write_config(root: &std::path::Path, pkg: &std::path::Path, extra: &str) -> std::path::PathBuf {
  let config = root.join("ferridriver.toml");
  std::fs::write(
    &config,
    format!(
      "[extensions]\npaths = [{}]\n\n[mcp.browser]\nheadless = true\n{extra}",
      serde_json::to_string(&pkg.display().to_string()).expect("json path")
    ),
  )
  .expect("write config");
  config
}

fn extensions_action(c: &mut McpClient, args: serde_json::Value) -> serde_json::Value {
  let res = c.call_tool("ferridriver_extensions", args);
  assert_ne!(res["result"]["isError"], true, "ferridriver_extensions failed: {res}");
  let text = res["result"]["content"][0]["text"].as_str().unwrap_or_default();
  serde_json::from_str(text).unwrap_or_else(|e| panic!("payload {text:?}: {e}"))
}

fn extensions_payload(c: &mut McpClient) -> serde_json::Value {
  extensions_action(c, json!({}))
}

pub fn run() {
  every_declared_entry_loads_and_lib_modules_do_not();
  an_unmet_sidecar_requirement_blocks_the_package();
  a_settings_block_that_violates_the_declared_schema_blocks_the_package();
  reload_picks_up_edits_without_a_restart();
}

/// The authoring loop: edit an extension, reload, and both the advertised
/// tool set and the behaviour of a session that is already open change —
/// no restart, and the session keeps its identity.
fn reload_picks_up_edits_without_a_restart() {
  let dir = tempfile::tempdir().expect("tempdir");
  let pkg = write_package(dir.path(), "");
  let config = write_config(dir.path(), &pkg, "");
  let mut c = McpClient::with_config("cdp-pipe", &config);

  // Build a session VM against the ORIGINAL code, so the reload has a
  // live VM to invalidate rather than a cold start.
  let res = c.call_tool("pkgprobe.login", json!({ "session": "default:reload" }));
  assert_ne!(res["result"]["isError"], true, "first call: {res}");
  let before = serde_json::to_string(&res["result"]).expect("json");
  assert!(before.contains("from-lib"), "original marker: {before}");

  let tools = c.list_tools();
  let names: Vec<&str> = tools["result"]["tools"]
    .as_array()
    .expect("tools")
    .iter()
    .filter_map(|t| t["name"].as_str())
    .collect();
  assert!(names.contains(&"pkgprobe.login"), "promoted before reload: {names:?}");
  assert!(
    !names.contains(&"pkgprobe.extra"),
    "the new tool must not exist yet: {names:?}"
  );

  // Edit both a handler body (changing what a live session observes) and
  // the tool set (adding a tool the client must be told about).
  std::fs::write(
    pkg.join("src/lib/shared.ts"),
    "export const SHARED_MARKER = 'edited-lib';\n",
  )
  .expect("edit lib");
  std::fs::write(
    pkg.join("src/sign.ts"),
    format!(
      "{SIGN_SRC}\ndefineTool({{ name: 'pkgprobe.extra', description: 'Added by the edit', \
      exposeAsTool: true, inputSchema: {{ type: 'object', properties: {{}} }}, \
      async handler() {{ return {{ added: true }}; }} }});\n"
    ),
  )
  .expect("edit sign");

  let p = extensions_action(&mut c, json!({ "action": "reload" }));
  assert_eq!(p["count"], 3, "the added tool is registered: {p}");
  assert_eq!(
    p["reloaded"]["added"],
    json!(["pkgprobe.extra"]),
    "the reload reports what changed: {p}"
  );
  assert_eq!(p["reloaded"]["toolListChanged"], true, "{p}");
  assert!(
    p["reloaded"]["droppedSessionVms"].as_u64().unwrap_or(0) >= 1,
    "the live session's VM must be dropped so it reloads: {p}"
  );

  let tools = c.list_tools();
  let names: Vec<&str> = tools["result"]["tools"]
    .as_array()
    .expect("tools")
    .iter()
    .filter_map(|t| t["name"].as_str())
    .collect();
  assert!(
    names.contains(&"pkgprobe.extra"),
    "tools/list reflects the edit: {names:?}"
  );

  // The tool added by the edit is callable, and the SAME session that ran
  // the old code now observes the edited helper.
  let res = c.call_tool("pkgprobe.extra", json!({ "session": "default:reload" }));
  assert_ne!(res["result"]["isError"], true, "added tool call: {res}");
  let text = serde_json::to_string(&res["result"]).expect("json");
  assert!(text.contains("\\\"added\\\": true") || text.contains("added"), "{text}");

  let res = c.call_tool("pkgprobe.login", json!({ "session": "default:reload" }));
  assert_ne!(res["result"]["isError"], true, "post-reload call: {res}");
  let after = serde_json::to_string(&res["result"]).expect("json");
  assert!(
    after.contains("edited-lib"),
    "the live session must run the edited code after a reload: {after}"
  );
}

/// Both entries register; the shared `lib/` module is bundled through
/// their imports and never loaded as an extension of its own.
fn every_declared_entry_loads_and_lib_modules_do_not() {
  let dir = tempfile::tempdir().expect("tempdir");
  let pkg = write_package(
    dir.path(),
    r#",
          "requires": { "commands": ["sh"] },
          "settings": {
            "pkgprobe": {
              "type": "object",
              "properties": { "origin": { "type": "string" } },
              "required": ["origin"],
              "additionalProperties": false
            }
          }"#,
  );
  let config = write_config(
    dir.path(),
    &pkg,
    "\n[extensions.settings.pkgprobe]\norigin = \"https://probe.test\"\n",
  );
  let mut c = McpClient::with_config("cdp-pipe", &config);

  let p = extensions_payload(&mut c);
  assert_eq!(
    p["errors"].as_array().map_or(0, Vec::len),
    0,
    "a lib/ module loaded as an extension would show up here: {p}"
  );
  assert_eq!(p["count"], 2, "one tool per tool-declaring entry: {p}");
  let files = p["files"].as_array().expect("files");
  assert_eq!(
    files.len(),
    3,
    "exactly the declared entries, toolless one included: {p}"
  );

  // The toolless entry is loaded (so its contributions exist) and reported as
  // a warning (so a defineTool that never ran is still visible).
  let warnings = serde_json::to_string(&p["warnings"]).expect("json");
  assert!(
    warnings.contains("globals.ts") && warnings.contains("declares no tools"),
    "the toolless entry must be reported, not silently dropped: {warnings}"
  );
  for f in files {
    let path = f["path"].as_str().unwrap_or_default();
    assert!(!path.contains("/lib/"), "lib/ must not be an entry: {path}");
  }

  // Both entries dispatch, and each sees the lib helper's value —
  // proof the helper was bundled via the import rather than skipped.
  for tool in ["pkgprobe.login", "pkgprobe.sign"] {
    let res = c.call_tool(tool, json!({}));
    assert_ne!(res["result"]["isError"], true, "{tool}: {res}");
    let text = serde_json::to_string(&res["result"]).expect("json");
    assert!(text.contains("from-lib"), "{tool} must reach the lib helper: {text}");
  }

  // The declared settings schema conforms, so the block reaches the handler.
  let res = c.call_tool("pkgprobe.login", json!({}));
  let text = serde_json::to_string(&res["result"]).expect("json");
  assert!(
    text.contains("https://probe.test"),
    "settings must reach the tool: {text}"
  );
}

/// `sidecars.connect(name)` for an undeclared name throws at call time,
/// so the requirement is checked at load and the package is skipped with
/// the reason on the wire.
fn an_unmet_sidecar_requirement_blocks_the_package() {
  let dir = tempfile::tempdir().expect("tempdir");
  let pkg = write_package(
    dir.path(),
    r#",
          "requires": { "sidecars": ["never-declared-gate"] }"#,
  );
  let config = write_config(dir.path(), &pkg, "");
  let mut c = McpClient::with_config("cdp-pipe", &config);

  let p = extensions_payload(&mut c);
  assert_eq!(p["count"], 0, "a package with an unmet requirement must not load: {p}");
  let errors = serde_json::to_string(&p["errors"]).expect("json");
  assert!(
    errors.contains("never-declared-gate") && errors.contains("[[sidecars]]"),
    "the error must name the requirement and the config key: {errors}"
  );
}

/// A settings block that does not match the package's declared schema is
/// a config bug the operator can fix; reading it as `undefined` inside a
/// handler is not a diagnosis.
fn a_settings_block_that_violates_the_declared_schema_blocks_the_package() {
  let dir = tempfile::tempdir().expect("tempdir");
  let pkg = write_package(
    dir.path(),
    r#",
          "settings": {
            "pkgprobe": {
              "type": "object",
              "properties": { "origin": { "type": "string" } },
              "required": ["origin"],
              "additionalProperties": false
            }
          }"#,
  );
  // `origins` is the classic typo the schema exists to catch.
  let config = write_config(
    dir.path(),
    &pkg,
    "\n[extensions.settings.pkgprobe]\norigins = \"https://probe.test\"\n",
  );
  let mut c = McpClient::with_config("cdp-pipe", &config);

  let p = extensions_payload(&mut c);
  assert_eq!(p["count"], 0, "the package must not load with invalid settings: {p}");
  let errors = serde_json::to_string(&p["errors"]).expect("json");
  assert!(
    errors.contains("extensions.settings.pkgprobe"),
    "the error must name the offending config block: {errors}"
  );
}
