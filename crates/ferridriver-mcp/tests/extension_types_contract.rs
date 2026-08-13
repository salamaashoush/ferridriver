//! `@ferridriver/extension` must describe what the runtime actually does.
//!
//! The type package is hand-written (the surface is Rust-side, with no
//! generator that could emit it), so the guard is this test: every field
//! the Rust types serialise and every key the handler context carries has
//! to appear in the declaration. A capability shipped without its type is
//! invisible to every extension author; a type without a capability sends
//! them chasing a binding that does not exist.

use std::collections::BTreeMap;
use std::path::PathBuf;

fn types_source() -> String {
  let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/ferridriver-extension/index.d.ts");
  match std::fs::read_to_string(&path) {
    Ok(s) => s,
    Err(e) => panic!("read {}: {e}", path.display()),
  }
}

/// The body of a top-level `interface <name> {...}` block, so a field
/// found for one interface cannot satisfy another's contract.
fn interface_body(source: &str, name: &str) -> String {
  let header = format!("interface {name}");
  let Some(start) = source.find(&header) else {
    panic!("`{name}` is not declared in @ferridriver/extension");
  };
  let rest = &source[start..];
  let Some(open) = rest.find('{') else {
    panic!("`{name}` has no body");
  };
  let mut depth = 0usize;
  for (i, c) in rest[open..].char_indices() {
    match c {
      '{' => depth += 1,
      '}' => {
        depth -= 1;
        if depth == 0 {
          return rest[open..=open + i].to_string();
        }
      },
      _ => {},
    }
  }
  panic!("`{name}` body is unterminated");
}

fn assert_declares(body: &str, interface: &str, fields: &[String]) {
  for field in fields {
    let declared = body.contains(&format!("{field}?:")) || body.contains(&format!("{field}:"));
    assert!(
      declared,
      "`{interface}` in @ferridriver/extension does not declare `{field}` — the Rust type carries it"
    );
  }
}

/// Every serde field name of a value, as it appears on the wire.
fn wire_fields<T: serde::Serialize>(value: &T) -> Vec<String> {
  let json = match serde_json::to_value(value) {
    Ok(serde_json::Value::Object(map)) => map,
    other => panic!("expected an object, got {other:?}"),
  };
  json.keys().cloned().collect()
}

#[test]
fn tool_definition_declares_every_manifest_field() {
  // Fully populated so no `Option::None` field is skipped by serde.
  let manifest: ferridriver_mcp::extension::ToolManifest = serde_json::from_value(serde_json::json!({
    "name": "ns.tool",
    "title": "T",
    "description": "d",
    "inputSchema": { "type": "object" },
    "outputSchema": { "type": "object" },
    "annotations": { "readOnlyHint": true },
    "allow": { "net": ["example.com"], "commands": { "c": "echo hi" } },
    "exposeAsMcpTool": true,
    "timeoutMs": 1000
  }))
  .expect("manifest fixture");

  let source = types_source();
  let body = interface_body(&source, "ToolDefinition<");
  assert_declares(&body, "ToolDefinition", &wire_fields(&manifest));
  // `handler` is not part of the manifest (it is not serialisable) but is
  // the whole point of the declaration.
  assert!(body.contains("handler:"), "ToolDefinition must declare `handler`");

  let allow_body = interface_body(&source, "ToolAllow");
  assert_declares(&allow_body, "ToolAllow", &wire_fields(&manifest.allow));
}

#[test]
fn tool_context_declares_every_key_the_runtime_installs() {
  let source = types_source();
  let body = interface_body(&source, "ToolContext<");
  let keys: Vec<String> = ferridriver_script::TOOL_CONTEXT_KEYS
    .iter()
    .map(|k| (*k).to_string())
    .collect();
  assert_declares(&body, "ToolContext", &keys);

  // The forwarded set is the subset taken from VM globals; it must not
  // drift out of the full list.
  for key in ferridriver_script::FORWARDED_CONTEXT_KEYS {
    assert!(
      ferridriver_script::TOOL_CONTEXT_KEYS.contains(key),
      "`{key}` is forwarded into the context but missing from TOOL_CONTEXT_KEYS"
    );
  }
}

#[test]
fn package_manifest_types_match_the_config_schema() {
  let manifest = ferridriver_config::ExtensionManifest {
    entries: vec!["./src/a.ts".into()],
    requires: ferridriver_config::ExtensionRequires {
      commands: vec!["c".into()],
      env: vec!["E".into()],
      net: vec!["example.com".into()],
      sidecars: vec!["s".into()],
    },
    settings: BTreeMap::from([("ns".to_string(), serde_json::json!({ "type": "object" }))]),
  };

  let source = types_source();
  assert_declares(
    &interface_body(&source, "ExtensionPackageManifest"),
    "ExtensionPackageManifest",
    &wire_fields(&manifest),
  );
  assert_declares(
    &interface_body(&source, "ExtensionRequires"),
    "ExtensionRequires",
    &wire_fields(&manifest.requires),
  );
}

#[test]
fn command_spec_declares_every_option_the_runtime_reads() {
  let spec: ferridriver_config::CommandSpec = serde_json::from_value(serde_json::json!({
    "run": ["echo", "hi"],
    "timeoutMs": 10,
    "env": ["PATH"],
    "cwd": "/tmp",
    "output": "json",
    "persistent": true
  }))
  .expect("command spec fixture");

  let source = types_source();
  let Some(start) = source.find("export type CommandSpec") else {
    panic!("CommandSpec is not declared in @ferridriver/extension");
  };
  let body = &source[start..start + 600.min(source.len() - start)];
  assert_declares(body, "CommandSpec", &wire_fields(&spec));
}

#[test]
fn the_declaration_has_no_any_escape_hatches() {
  let source = types_source();
  for (i, line) in source.lines().enumerate() {
    let code = line.split("//").next().unwrap_or(line);
    assert!(
      !code.contains(": any") && !code.contains("<any"),
      "line {} uses `any`, which silently disables checking: {line}",
      i + 1
    );
  }
}
