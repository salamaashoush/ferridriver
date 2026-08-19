#![allow(clippy::expect_used, clippy::unwrap_used)]
//! The documentation has to describe what the runtime actually does.
//!
//! Two-way, because either direction failing is its own kind of wrong: a
//! contribution point nobody can find in the docs may as well not exist,
//! and a documented one nothing installs sends an author chasing a
//! binding that is not there. So `CONTRIBUTION_POINTS` is checked
//! against a LIVE session's globals in one direction and against
//! `docs/extensions.md` in the other.
//!
//! Plus the cheap one that keeps a doc tree honest as it is edited: no
//! internal link may point at a file that does not exist.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ferridriver_script::{
  CONTRIBUTION_POINTS, ExtensionHost, InMemoryVars, PathSandbox, RunContext, ScriptCaps, ScriptEngineConfig, Session,
};

fn repo_root() -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(relative: &str) -> String {
  let path = repo_root().join(relative);
  std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[tokio::test(flavor = "multi_thread")]
async fn every_contribution_point_is_a_real_global() {
  let dir = tempfile::tempdir().expect("tempdir");
  let context = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(dir.path()).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: Vec::new(),
    // The host an extension's top level runs under during extraction —
    // every contribution point has to exist there, whatever the host
    // goes on to consume.
    host: ExtensionHost::Script,
    caps: ScriptCaps::default(),
    session: None,
  };
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");

  let missing = ferridriver_script::vm_with!(session.vm_handle() => |ctx| {
    let globals = ctx.globals();
    let mut missing: Vec<&'static str> = Vec::new();
    for name in CONTRIBUTION_POINTS {
      let found: rquickjs::Value<'_> = globals.get(*name).unwrap_or_else(|_| rquickjs::Value::new_undefined(ctx.clone()));
      if !found.is_function() {
        missing.push(name);
      }
    }
    Ok::<Vec<&'static str>, ferridriver_script::ScriptError>(missing)
  })
  .await
  .expect("vm")
  .expect("probe");

  assert!(
    missing.is_empty(),
    "CONTRIBUTION_POINTS names globals the runtime does not install: {missing:?}"
  );
}

#[test]
fn every_contribution_point_is_documented() {
  let docs = read("docs/extensions.md");
  let undocumented: Vec<&&str> = CONTRIBUTION_POINTS
    .iter()
    .filter(|name| !docs.contains(&format!("`{name}`")) && !docs.contains(&format!("{name}(")))
    .collect();
  assert!(
    undocumented.is_empty(),
    "docs/extensions.md documents no contribution point named {undocumented:?} — \
     a capability shipped without its documentation is invisible to an author"
  );
}

#[test]
fn every_host_is_documented() {
  let docs = read("docs/extensions.md");
  for host in ExtensionHost::ALL {
    let quoted = format!("\"{}\"", host.as_str());
    assert!(
      docs.contains(&quoted),
      "docs/extensions.md never mentions the {} host as {quoted} — \
       `ferridriver.host` answers it, so an author has to be able to look it up",
      host.as_str()
    );
  }
}

/// Globals the three installers set from a string LITERAL.
///
/// The looped installs (`STEP_REGISTRARS`, `CUCUMBER_HOOKS`) are pinned
/// structurally by a unit test in `bindings::extensions`; this catches
/// the other direction for the one-off `g.set("name", …)` forms, where
/// nothing but a scan can notice a new contribution point that nobody
/// added to the list.
fn installed_literals() -> BTreeSet<String> {
  let mut out = BTreeSet::new();
  let pattern = regex::Regex::new(r#"(?:globals\(\)|\bg)\.set\("([A-Za-z_$][A-Za-z0-9_$]*)""#).expect("regex");
  for file in [
    "crates/ferridriver-script/src/bindings/registry.rs",
    "crates/ferridriver-script/src/bindings/bdd.rs",
    "crates/ferridriver-script/src/bindings/test.rs",
  ] {
    for capture in pattern.captures_iter(&read(file)) {
      out.insert(capture[1].to_string());
    }
  }
  out
}

#[test]
fn no_contribution_point_is_installed_without_being_listed() {
  let unlisted: Vec<String> = installed_literals()
    .into_iter()
    .filter(|name| !CONTRIBUTION_POINTS.contains(&name.as_str()))
    .collect();
  assert!(
    unlisted.is_empty(),
    "installed as a global by an extension surface but absent from CONTRIBUTION_POINTS, \
     so undocumented and unchecked: {unlisted:?}"
  );
}

/// Every markdown file under the two doc trees.
fn doc_files() -> Vec<PathBuf> {
  let mut out = Vec::new();
  for tree in ["docs", "site/docs"] {
    collect_markdown(&repo_root().join(tree), &mut out);
  }
  out.sort();
  out
}

fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) {
  let Ok(entries) = std::fs::read_dir(dir) else {
    return;
  };
  for entry in entries.flatten() {
    let path = entry.path();
    if path.is_dir() {
      collect_markdown(&path, out);
    } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
      out.push(path);
    }
  }
}

/// Link targets that name a repository file, with the anchor and any
/// title stripped.
fn internal_links(source: &str) -> BTreeSet<String> {
  let mut out = BTreeSet::new();
  let bytes: Vec<char> = source.chars().collect();
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i] != '(' {
      i += 1;
      continue;
    }
    // Only a `](` opens a markdown destination.
    if i == 0 || bytes[i - 1] != ']' {
      i += 1;
      continue;
    }
    let Some(end) = bytes[i..].iter().position(|c| *c == ')') else {
      break;
    };
    let target: String = bytes[i + 1..i + end].iter().collect();
    i += end;
    let target = target.split_whitespace().next().unwrap_or_default();
    let target = target.split('#').next().unwrap_or_default();
    if target.is_empty() || target.starts_with("http") || target.starts_with("mailto:") || target.starts_with('/') {
      continue;
    }
    out.insert(target.to_string());
  }
  out
}

#[test]
fn no_internal_doc_link_dangles() {
  let mut dangling: Vec<String> = Vec::new();
  for file in doc_files() {
    let source = std::fs::read_to_string(&file).unwrap_or_default();
    let dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
    for target in internal_links(&source) {
      // A site link may name a route rather than a file; only check the
      // ones that look like a path to something on disk.
      if !target.contains('.') {
        continue;
      }
      if dir.join(&target).exists() || repo_root().join(&target).exists() {
        continue;
      }
      dangling.push(format!("{} -> {target}", file.display()));
    }
  }
  assert!(
    dangling.is_empty(),
    "documentation links at nothing:\n{}",
    dangling.join("\n")
  );
}
