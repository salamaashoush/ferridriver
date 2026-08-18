#![allow(clippy::expect_used, clippy::unwrap_used)]
//! An extension's frames report the file its author wrote.
//!
//! Each extension is its own rolldown bundle, loaded into the session VM
//! beside whatever module the host executes. The VM therefore holds
//! several source maps, and a frame belongs to whichever module it
//! names — so the maps are registered per module and a stack is remapped
//! frame by frame. Getting this wrong is silent: the line number is
//! still a number, it just points into the wrong file.

use std::path::PathBuf;
use std::sync::Arc;

use ferridriver_script::{
  ExtensionBinding, InMemoryVars, Outcome, PathSandbox, RequirementEnv, RunContext, RunOptions, ScriptCaps,
  ScriptEngineConfig, Session,
};

fn scratch(tag: &str) -> PathBuf {
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_nanos())
    .unwrap_or(0);
  let dir = std::env::temp_dir().join(format!("ferri_ext_maps_{tag}_{nanos}"));
  std::fs::create_dir_all(&dir).expect("mkdir");
  dir
}

async fn bindings_for(specs: &[ferridriver_script::ExtensionSpec]) -> Vec<ExtensionBinding> {
  let caps = ScriptCaps::default();
  let sidecars: Vec<String> = Vec::new();
  let env = RequirementEnv::from_caps(&caps, &sidecars);
  ferridriver_script::load_bindings(specs, &env, &caps.extension_policy).await
}

async fn session_with(dir: &std::path::Path, extensions: Vec<ExtensionBinding>) -> (Session, RunContext) {
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(dir).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions,
    host: ferridriver_script::ExtensionHost::Script,
    caps: ScriptCaps::default(),
    session: None,
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  (session, ctx)
}

async fn error_from(session: &Session, ctx: &RunContext, code: &str) -> ferridriver_script::ScriptError {
  match session
    .execute(code, &[], RunOptions::default(), ctx)
    .await
    .result
    .outcome
  {
    Outcome::Error { error } => error,
    Outcome::Ok { success } => panic!("expected a throw, got {:?}", success.value),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_throw_from_an_extension_names_the_authors_file() {
  let dir = scratch("one");
  std::fs::write(
    dir.join("boom.ts"),
    "const label: string = 'boom';\n\
     defineTool({ name: 'boom', handler: async () => { throw new Error(label); } });\n",
  )
  .expect("write extension");

  let specs = vec![ferridriver_script::ExtensionSpec {
    spec: "./boom.ts".to_string(),
    base_dir: dir.clone(),
  }];
  let bindings = bindings_for(&specs).await;
  assert_eq!(bindings.len(), 1, "the extension must compile");
  assert!(bindings[0].source_map.is_some(), "the binding must carry its map");

  let (session, run_ctx) = session_with(&dir, bindings).await;
  let err = error_from(&session, &run_ctx, "return await tools.boom({});").await;
  let stack = err.stack.clone().unwrap_or_default();
  assert!(
    stack.contains("boom.ts"),
    "the frame must name the author's file, got:\n{stack}"
  );
  assert!(
    !stack.contains("ferri_extension_"),
    "no frame may be left labelled with the bundle module, got:\n{stack}"
  );

  let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn two_extensions_map_through_their_own_maps() {
  // The reason the maps are keyed by module name: with more than one
  // registered, a frame mapped through the wrong map still produces a
  // plausible file:line — the failure is silent.
  let dir = scratch("two");
  std::fs::write(
    dir.join("first.ts"),
    "const pad = 1;\nconst pad2 = 2;\nconst pad3 = 3;\nconst pad4 = 4;\nconst pad5 = 5;\n\
     defineTool({ name: 'first', handler: async () => { throw new Error('first'); } });\n",
  )
  .expect("write first");
  std::fs::write(
    dir.join("second.ts"),
    "defineTool({ name: 'second', handler: async () => { throw new Error('second'); } });\n",
  )
  .expect("write second");

  let specs = vec![
    ferridriver_script::ExtensionSpec {
      spec: "./first.ts".to_string(),
      base_dir: dir.clone(),
    },
    ferridriver_script::ExtensionSpec {
      spec: "./second.ts".to_string(),
      base_dir: dir.clone(),
    },
  ];
  let bindings = bindings_for(&specs).await;
  assert_eq!(bindings.len(), 2, "both extensions must compile");

  let (session, run_ctx) = session_with(&dir, bindings).await;
  let first = error_from(&session, &run_ctx, "return await tools.first({});").await;
  let second = error_from(&session, &run_ctx, "return await tools.second({});").await;

  let first_stack = first.stack.clone().unwrap_or_default();
  let second_stack = second.stack.clone().unwrap_or_default();
  assert!(
    first_stack.contains("first.ts") && !first_stack.contains("second.ts"),
    "first must map through its own map, got:\n{first_stack}"
  );
  assert!(
    second_stack.contains("second.ts") && !second_stack.contains("first.ts"),
    "second must map through its own map, got:\n{second_stack}"
  );

  let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cached_extension_maps_the_same_as_a_freshly_compiled_one() {
  // Both cache tiers carry the map and the module name now. Dropping
  // them on the hit path is what made the FIRST run report the author's
  // file and every run after it report a bundle offset.
  let dir = scratch("cached");
  std::fs::write(
    dir.join("cached.ts"),
    "defineTool({ name: 'cached', handler: async () => { throw new Error('cached'); } });\n",
  )
  .expect("write extension");
  let specs = vec![ferridriver_script::ExtensionSpec {
    spec: "./cached.ts".to_string(),
    base_dir: dir.clone(),
  }];

  let cold = bindings_for(&specs).await;
  let (session, run_ctx) = session_with(&dir, cold).await;
  let cold_stack = error_from(&session, &run_ctx, "return await tools.cached({});")
    .await
    .stack
    .unwrap_or_default();

  // Second load of the same unchanged file: served from the caches.
  let warm = bindings_for(&specs).await;
  let (session2, run_ctx2) = session_with(&dir, warm).await;
  let warm_stack = error_from(&session2, &run_ctx2, "return await tools.cached({});")
    .await
    .stack
    .unwrap_or_default();

  assert!(
    cold_stack.contains("cached.ts"),
    "cold compile must map, got:\n{cold_stack}"
  );
  assert_eq!(cold_stack, warm_stack, "a cache hit must map identically");

  let _ = std::fs::remove_dir_all(&dir);
}
