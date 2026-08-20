#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `node:util` and `node:events`: the wrappers, the renderer they share
//! with `console`, and the vendored `EventEmitter`.

use std::path::Path;
use std::sync::Arc;

use ferridriver_script::{
  InMemoryVars, Outcome, RunContext, RunOptions, ScriptEngineConfig, Session, bundle_and_compile,
};

fn ctx(dir: &Path) -> RunContext {
  RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: dir.into(),
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
async fn util_formats_wraps_and_classifies() {
  let dir = tempfile::tempdir().expect("tempdir");
  let context = ctx(dir.path());
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");

  let value = run(
    r"
      import util from 'node:util';

      const readish = (value, cb) => cb(null, `read:${value}`);
      const failing = (cb) => cb(new Error('nope'));
      const promised = util.promisify(readish);
      const failed = util.promisify(failing);

      const asyncish = async (n) => n * 2;
      const backToCallback = util.callbackify(asyncish);
      let callbackResult = null;
      backToCallback(21, (err, v) => { callbackResult = err ? `err:${err.message}` : v; });

      let rejected = null;
      try { await failed(); } catch (e) { rejected = e.message; }

      function Base() {}
      Base.prototype.hello = () => 'hi';
      function Derived() {}
      util.inherits(Derived, Base);

      export default {
        format: util.format('%s has %d items: %j', 'cart', 3, { a: 1 }),
        inspectQuotesStrings: util.inspect('hello'),
        inspectNested: util.inspect({ a: { b: { c: { d: 1 } } } }),
        inspectDeep: util.inspect({ a: { b: { c: { d: 1 } } } }, { depth: null }),
        promisified: await promised('file'),
        rejected,
        callbackResult,
        inherited: new Derived().hello(),
        types: {
          date: util.types.isDate(new Date()),
          notDate: util.types.isDate({}),
          regexp: util.types.isRegExp(/x/),
          map: util.types.isMap(new Map()),
          set: util.types.isSet(new Set()),
          promise: util.types.isPromise(Promise.resolve()),
          typedArray: util.types.isTypedArray(new Uint8Array(1)),
          notTypedArray: util.types.isTypedArray([]),
          nativeError: util.types.isNativeError(new Error('x')),
        },
        deepEqual: util.isDeepStrictEqual({ a: [1, { b: 2 }] }, { a: [1, { b: 2 }] }),
        deepUnequal: util.isDeepStrictEqual({ a: 1 }, { a: '1' }),
        hasTextEncoder: typeof util.TextEncoder === 'function',
        inspectCustomIsSymbol: typeof util.inspect.custom === 'symbol',
      };
    ",
    dir.path(),
    &session,
    &context,
  )
  .await;

  assert_eq!(value["format"], "cart has 3 items: {\"a\":1}");
  // `console.log('hello')` prints it bare; `util.inspect` quotes it.
  assert_eq!(value["inspectQuotesStrings"], "'hello'");
  assert!(
    value["inspectNested"].as_str().is_some_and(|s| s.contains("[Object]")),
    "the default depth of 2 elides deeper objects: {value:?}"
  );
  assert!(
    value["inspectDeep"].as_str().is_some_and(|s| s.contains("d: 1")),
    "depth: null walks further: {value:?}"
  );
  assert_eq!(value["promisified"], "read:file");
  assert_eq!(value["rejected"], "nope");
  assert_eq!(value["callbackResult"], 42);
  assert_eq!(value["inherited"], "hi");
  assert_eq!(value["deepEqual"], serde_json::Value::Bool(true));
  assert_eq!(value["deepUnequal"], serde_json::Value::Bool(false));
  assert_eq!(value["hasTextEncoder"], serde_json::Value::Bool(true));
  assert_eq!(value["inspectCustomIsSymbol"], serde_json::Value::Bool(true));

  let types = &value["types"];
  for key in ["date", "regexp", "map", "set", "promise", "typedArray", "nativeError"] {
    assert_eq!(types[key], serde_json::Value::Bool(true), "types.{key}: {types:?}");
  }
  for key in ["notDate", "notTypedArray"] {
    assert_eq!(types[key], serde_json::Value::Bool(false), "types.{key}: {types:?}");
  }
}

#[tokio::test]
async fn events_exposes_the_emitter_over_import_and_require() {
  let dir = tempfile::tempdir().expect("tempdir");
  let context = ctx(dir.path());
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");

  let value = run(
    r"
      import { EventEmitter } from 'node:events';
      const Required = require('events');

      const emitter = new EventEmitter();
      const seen = [];
      emitter.on('tick', (n) => seen.push(n));
      emitter.once('tick', (n) => seen.push(`once:${n}`));
      emitter.emit('tick', 1);
      emitter.emit('tick', 2);

      class Bus extends Required {}
      const bus = new Bus();
      let fromSubclass = null;
      bus.on('msg', (v) => { fromSubclass = v; });
      bus.emit('msg', 'hello');

      export default {
        seen,
        sameClass: Required === EventEmitter,
        instanceOfAcrossPaths: bus instanceof EventEmitter,
        fromSubclass,
        listenerCount: emitter.listenerCount('tick'),
        names: emitter.eventNames(),
      };
    ",
    dir.path(),
    &session,
    &context,
  )
  .await;

  assert_eq!(value["seen"], serde_json::json!([1, "once:1", 2]));
  assert_eq!(value["sameClass"], serde_json::Value::Bool(true));
  assert_eq!(
    value["instanceOfAcrossPaths"],
    serde_json::Value::Bool(true),
    "one constructor per context, whichever path reached it first"
  );
  assert_eq!(value["fromSubclass"], "hello");
  assert_eq!(value["listenerCount"], 1);
  assert_eq!(value["names"], serde_json::json!(["tick"]));
}
