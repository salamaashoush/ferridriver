#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Node-compat native modules (`fs`/`node:fs`, `path`/`node:path`,
//! `buffer`/`node:buffer`) and the native `ferridriver` module — both
//! consumption paths:
//! 1. bundled (rolldown marks them external; bytecode re-links against
//!    the session loader), and
//! 2. dynamic `import()` from a plain script (sandbox loader chain).

use std::sync::Arc;

use ferridriver_script::{
  InMemoryVars, Outcome, PathSandbox, RunContext, RunOptions, ScriptEngineConfig, Session, bundle_and_compile,
};

fn ctx(dir: &std::path::Path) -> RunContext {
  RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(dir).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: Vec::new(),
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
  }
}

#[tokio::test]
async fn bundled_module_uses_native_fs_path_buffer() {
  let dir = tempfile::tempdir().expect("tempdir");
  std::fs::write(dir.path().join("data.txt"), "hello-node-compat").expect("data");

  let entry = dir.path().join("main.ts");
  std::fs::write(
    &entry,
    "import fs from 'node:fs';\n\
     import { readFile } from 'fs';\n\
     import path from 'node:path';\n\
     import { Buffer } from 'node:buffer';\n\
     const dbg: string = JSON.stringify({ t1: typeof fs, g: typeof (globalThis as any).fs });\n\
     const viaDefault: string = await fs.readFile('data.txt');\n\
     const viaNamed: string = await readFile('data.txt');\n\
     const viaPromises: string = await fs.promises.readFile('data.txt');\n\
     const joined: string = path.join('a', '..', 'b', 'c.txt');\n\
     const ext: string = path.extname(joined);\n\
     const b64: string = Buffer.from('hi').toString('base64');\n\
     const round: string = Buffer.from(b64, 'base64').toString('utf8');\n\
     export default { viaDefault, viaNamed, viaPromises, joined, ext, b64, round };\n",
  )
  .expect("entry");

  let bundle = bundle_and_compile(std::slice::from_ref(&entry), dir.path())
    .await
    .expect("bundle");
  let context = ctx(dir.path());
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");
  let run = session
    .execute_module(&bundle, &[], RunOptions::default(), &context)
    .await;
  match run.result.outcome {
    Outcome::Ok { success, .. } => assert_eq!(
      success.value,
      serde_json::json!({
        "viaDefault": "hello-node-compat",
        "viaNamed": "hello-node-compat",
        "viaPromises": "hello-node-compat",
        "joined": "b/c.txt",
        "ext": ".txt",
        "b64": "aGk=",
        "round": "hi"
      })
    ),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[tokio::test]
async fn dynamic_import_resolves_native_modules_in_plain_scripts() {
  let dir = tempfile::tempdir().expect("tempdir");
  let context = ctx(dir.path());
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");
  let run = session
    .execute(
      r"
      const path = (await import('path')).default;
      const { Buffer } = await import('buffer');
      const fd = await import('ferridriver');
      const cucumber = await import('@cucumber/cucumber');
      return {
        dir: path.dirname('/a/b/c.txt'),
        rel: path.relative('/a/b', '/a/d'),
        hex: Buffer.from([0xde, 0xad]).toString('hex'),
        isBuf: Buffer.isBuffer(Buffer.alloc(2)),
        host: fd.host,
        givenIsFn: typeof cucumber.Given === 'function',
      };
      ",
      &[],
      RunOptions::default(),
      &context,
    )
    .await;
  match run.result.outcome {
    Outcome::Ok { success } => assert_eq!(
      success.value,
      serde_json::json!({
        "dir": "/a/b",
        "rel": "../d",
        "hex": "dead",
        "isBuf": true,
        "host": "script",
        "givenIsFn": true
      })
    ),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}
