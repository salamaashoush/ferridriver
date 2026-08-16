#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `node:assert`, `node:url`, `node:process` and `node:timers`.

use std::path::Path;
use std::sync::Arc;

use ferridriver_script::{
  InMemoryVars, Outcome, PathSandbox, RunContext, RunOptions, ScriptEngineConfig, Session, bundle_and_compile,
};

fn ctx(dir: &Path) -> RunContext {
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
    session: None,
  }
}

async fn run(source: &str, dir: &Path, session: &Session, context: &RunContext) -> serde_json::Value {
  let entry = dir.join("entry.ts");
  std::fs::write(&entry, source).expect("write entry");
  let bundle = bundle_and_compile(std::slice::from_ref(&entry), dir)
    .await
    .expect("bundle");
  let out = session
    .execute_module(&bundle, &[], RunOptions::default(), context)
    .await;
  match out.result.outcome {
    Outcome::Ok { success, .. } => success.value,
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[tokio::test]
async fn assert_passes_fails_and_carries_node_fields() {
  let dir = tempfile::tempdir().expect("tempdir");
  let context = ctx(dir.path());
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");

  let value = run(
    r"
      import assert from 'node:assert';
      import strict from 'node:assert/strict';

      const caught = (fn) => { try { fn(); return null; } catch (e) { return e; } };

      // Passing assertions must not throw.
      assert(1);
      assert.ok('non-empty');
      assert.equal(1, '1');
      assert.strictEqual('a', 'a');
      assert.notStrictEqual(0, -0);
      assert.deepStrictEqual({ a: [1, { b: new Date(5) }] }, { a: [1, { b: new Date(5) }] });
      assert.notDeepStrictEqual({ a: 1 }, { a: '1' });
      assert.match('hello', /ell/);
      assert.doesNotMatch('hello', /xyz/);
      assert.throws(() => { throw new TypeError('boom'); }, TypeError);
      assert.throws(() => { throw new Error('boom'); }, /boo/);
      assert.doesNotThrow(() => 1);
      assert.ifError(null);
      await assert.rejects(async () => { throw new Error('nope'); }, /nope/);
      await assert.doesNotReject(async () => 1);

      const failure = caught(() => assert.strictEqual(1, 2));
      const looseUnderStrict = caught(() => strict.equal(1, '1'));
      const looseUnderDefault = caught(() => assert.equal(1, '1'));
      const withMessage = caught(() => assert.ok(false, 'custom text'));
      const missingThrow = caught(() => assert.throws(() => 1));
      const wrongError = caught(() => assert.throws(() => { throw new Error('a'); }, /b/));

      let rejectMissing = null;
      try { await assert.rejects(async () => 1); } catch (e) { rejectMissing = e.message; }

      export default {
        name: failure.name,
        code: failure.code,
        operator: failure.operator,
        actual: failure.actual,
        expected: failure.expected,
        generated: failure.generatedMessage,
        messageHasBoth: failure.message.includes('1') && failure.message.includes('2'),
        strictRejectsLoose: looseUnderStrict !== null,
        defaultAllowsLoose: looseUnderDefault === null,
        customMessage: withMessage.message,
        customIsNotGenerated: withMessage.generatedMessage,
        missingThrow: missingThrow.message,
        wrongError: wrongError.operator,
        rejectMissing,
        strictIsStrict: strict.strict === strict,
        assertIsCallable: typeof assert === 'function',
      };
    ",
    dir.path(),
    &session,
    &context,
  )
  .await;

  assert_eq!(value["name"], "AssertionError");
  assert_eq!(value["code"], "ERR_ASSERTION");
  assert_eq!(value["operator"], "strictEqual");
  assert_eq!(value["actual"], 1);
  assert_eq!(value["expected"], 2);
  assert_eq!(value["generated"], serde_json::Value::Bool(true));
  assert_eq!(value["messageHasBoth"], serde_json::Value::Bool(true));
  assert_eq!(
    value["strictRejectsLoose"],
    serde_json::Value::Bool(true),
    "assert/strict's `equal` IS `strictEqual`"
  );
  assert_eq!(value["defaultAllowsLoose"], serde_json::Value::Bool(true));
  assert_eq!(value["customMessage"], "custom text");
  assert_eq!(value["customIsNotGenerated"], serde_json::Value::Bool(false));
  assert_eq!(value["missingThrow"], "Missing expected exception.");
  assert_eq!(value["wrongError"], "throws");
  assert_eq!(value["rejectMissing"], "Missing expected rejection.");
  assert_eq!(value["strictIsStrict"], serde_json::Value::Bool(true));
  assert_eq!(value["assertIsCallable"], serde_json::Value::Bool(true));
}

#[tokio::test]
async fn url_process_and_timers_modules() {
  let dir = tempfile::tempdir().expect("tempdir");
  let context = ctx(dir.path());
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");

  let value = run(
    r"
      import { fileURLToPath, pathToFileURL, URL as UrlFromModule } from 'node:url';
      import process from 'node:process';
      import { setTimeout as setTimeoutCb } from 'node:timers';
      import { setTimeout as delay, setImmediate as soon } from 'node:timers/promises';
      import os from 'node:os';

      const encoded = pathToFileURL('/tmp/a b#c.txt');
      const decoded = fileURLToPath('file:///tmp/a%20b%23c.txt');

      let badScheme = null;
      try { fileURLToPath('https://example.com/x'); } catch (e) { badScheme = e.message; }

      const timerValue = await delay(1, 'late');
      const immediateValue = await soon('now');

      export default {
        decoded,
        roundTrip: fileURLToPath(encoded.href),
        encodedHref: encoded.href,
        badScheme,
        urlIsGlobal: UrlFromModule === URL,
        processIsGlobal: process === globalThis.process,
        platformsAgree: process.platform === os.platform(),
        archesAgree: process.arch === os.arch(),
        platform: process.platform,
        timerIsGlobal: setTimeoutCb === globalThis.setTimeout,
        timerValue,
        immediateValue,
      };
    ",
    dir.path(),
    &session,
    &context,
  )
  .await;

  assert_eq!(value["decoded"], "/tmp/a b#c.txt");
  assert_eq!(value["roundTrip"], "/tmp/a b#c.txt");
  assert_eq!(value["encodedHref"], "file:///tmp/a%20b%23c.txt");
  assert!(
    value["badScheme"].as_str().is_some_and(|m| m.contains("file")),
    "a non-file URL is refused: {value:?}"
  );
  assert_eq!(value["urlIsGlobal"], serde_json::Value::Bool(true));
  assert_eq!(
    value["processIsGlobal"],
    serde_json::Value::Bool(true),
    "the module form is the global object, not a copy"
  );
  assert_eq!(
    value["platformsAgree"],
    serde_json::Value::Bool(true),
    "process.platform and os.platform() read the same constant"
  );
  assert_eq!(value["archesAgree"], serde_json::Value::Bool(true));
  assert_eq!(
    value["platform"],
    if cfg!(target_os = "macos") { "darwin" } else { "linux" },
    "Node's spelling, not Rust's"
  );
  assert_eq!(value["timerIsGlobal"], serde_json::Value::Bool(true));
  assert_eq!(value["timerValue"], "late");
  assert_eq!(value["immediateValue"], "now");
}
