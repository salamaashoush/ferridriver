use std::path::{Path, PathBuf};

use serde_json::Value;

use super::merge::{append_unique, camel_case_segment, nest, parse_env_value};
use super::paths::{anchored, normalize, normalize_path};
use super::{parse_file, suggestion_for};

#[test]
fn camel_case_segment_joins_words() {
  assert_eq!(camel_case_segment("BROWSER"), "browser");
  assert_eq!(camel_case_segment("INSTANCE_ARGS_COMMAND"), "instanceArgsCommand");
  assert_eq!(camel_case_segment("MCP"), "mcp");
}

#[test]
fn nest_builds_nested_objects() {
  let v = nest("a.b.c", Value::Bool(true));
  assert_eq!(v["a"]["b"]["c"], Value::Bool(true));
}

#[test]
fn parse_env_value_prefers_json() {
  assert_eq!(parse_env_value("true"), Value::Bool(true));
  assert_eq!(parse_env_value("7"), Value::from(7));
  assert_eq!(parse_env_value(r#"["--a"]"#), Value::from(vec!["--a"]));
  assert_eq!(parse_env_value("cdp-raw"), Value::String("cdp-raw".into()));
}

#[test]
fn normalize_path_removes_dot_segments() {
  assert_eq!(normalize_path(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
}

#[test]
fn normalize_path_cannot_climb_past_the_root() {
  // A source map's `../`-heavy entry joined onto a shallow cwd
  // produces more `..` than there are components. The OS reads `/..`
  // as `/`; keeping the `..` made the result compare as a prefix of
  // nothing, which is how a multi-project run came to report "no
  // tests matched" for a testDir outside the working directory.
  assert_eq!(
    normalize_path(Path::new("/one/two/../../../../tmp/specs/a.ts")),
    PathBuf::from("/tmp/specs/a.ts")
  );
  // A relative path still keeps the `..` it needs.
  assert_eq!(normalize_path(Path::new("a/../../b")), PathBuf::from("../b"));
}

#[test]
fn anchored_leaves_absolute_tilde_and_templates_alone() {
  let dir = Path::new("/base");
  let expected = normalize_path(&dir.join("x.ts")).to_string_lossy().into_owned();
  assert_eq!(anchored(dir, "./x.ts").as_deref(), Some(expected.as_str()));
  assert_eq!(anchored(dir, "x.ts").as_deref(), Some(expected.as_str()));
  assert!(anchored(dir, "/abs/x.ts").is_none());
  assert!(anchored(dir, "~/x.ts").is_none());
  assert!(anchored(dir, "/tmp/${INSTANCE}/profile").is_none());
  assert!(anchored(dir, "profiles/${INSTANCE}").is_none());
}

#[test]
fn append_unique_concatenates_without_duplicates() {
  let mut base = Value::from(vec!["--a", "--b"]);
  append_unique(&mut base, &Value::from(vec!["--b", "--c"]));
  assert_eq!(base, Value::from(vec!["--a", "--b", "--c"]));
}

#[test]
fn normalize_promotes_extensions_shorthand() {
  let mut v = serde_json::json!({ "extensions": ["./a.ts"] });
  normalize(&mut v);
  assert_eq!(v, serde_json::json!({ "extensions": { "paths": ["./a.ts"] } }));
}

#[test]
fn suggestion_names_the_camel_case_spelling() {
  assert!(suggestion_for("mcp.browser.chrome_args").contains("chromeArgs"));
  assert_eq!(suggestion_for("mcp.browser.nonsense"), "");
}

#[test]
fn yaml_keeps_on_and_yes_as_strings() {
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("ferridriver.yaml");
  std::fs::write(
    &path,
    "test:\n  use:\n    trace: { mode: on, snapshots: false }\n    video: on\n    note: yes\n    headless: true\n",
  )
  .unwrap();
  let value = parse_file(&path).unwrap();
  let used = &value["test"]["use"];
  assert_eq!(used["trace"]["mode"], "on");
  assert_eq!(used["trace"]["snapshots"], false);
  assert_eq!(used["video"], "on");
  assert_eq!(used["note"], "yes");
  assert_eq!(used["headless"], true);
}
