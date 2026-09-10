use std::path::{Path, PathBuf};

use anyhow::Result;
use ferridriver_script::{
  CollectedAnnotation, ExtensionHost, RunContext, ScriptEngineConfig, Session, bundle_and_compile_named, collect_tests,
  eval_bundle,
};
use serde_json::{Value, json};

fn annotations(items: &[CollectedAnnotation]) -> Vec<Value> {
  items
    .iter()
    .map(|item| {
      json!({
        "kind": item.kind, "value": item.value, "description": item.description,
      })
    })
    .collect()
}

pub async fn collect(root: &Path, context: &RunContext, entries: Vec<PathBuf>, host: ExtensionHost) -> Result<Value> {
  let bundle = bundle_and_compile_named(&entries, root, "ferridriver-tests.js").await?;
  let mut context = context.clone();
  context.host = host;
  let session = Session::create(ScriptEngineConfig::default(), &context).await?;
  eval_bundle(&session.vm_handle(), &bundle).await?;
  let registry = collect_tests(&session.vm_handle()).await?;
  Ok(json!({
    "hasOnly": registry.has_only,
    "tests": registry.tests.iter().map(|test| json!({
      "title": test.title, "suite": test.suite, "annotations": annotations(&test.annotations),
      "timeoutMs": test.timeout_ms, "retries": test.retries, "requested": test.requested,
      "fixtureSet": test.fixture_set, "hasEachArg": test.has_each_arg,
      "line": test.line, "col": test.col, "source": bundle.remap(test.line, test.col),
    })).collect::<Vec<_>>(),
    "suites": registry.suites.iter().map(|suite| json!({
      "name": suite.name, "parent": suite.parent, "mode": suite.mode,
      "annotations": annotations(&suite.annotations), "useOptions": suite.use_options,
      "retries": suite.retries, "timeoutMs": suite.timeout_ms,
    })).collect::<Vec<_>>(),
    "hooks": registry.hooks.iter().map(|hook| json!({
      "kind": hook.kind, "suite": hook.suite, "requested": hook.requested,
    })).collect::<Vec<_>>(),
    "fixtures": registry.fixtures.iter().map(|fixture| json!({
      "name": fixture.name, "deps": fixture.deps, "option": fixture.option,
    })).collect::<Vec<_>>(),
    "fixtureSets": registry.fixture_sets,
    "fileUse": registry.file_use.iter().map(|item| json!({
      "options": item.options, "line": item.line,
    })).collect::<Vec<_>>(),
    "fileAnnotations": registry.file_annotations.iter().map(|item| json!({
      "annotations": annotations(std::slice::from_ref(&item.annotation)),
    })).collect::<Vec<_>>(),
  }))
}
