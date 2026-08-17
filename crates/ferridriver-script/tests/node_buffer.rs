#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `Buffer`: the vendored llrt implementation, which subclasses
//! `Uint8Array` — the previous hand-written class did not, and every
//! byte-level consumer had to go through an escape hatch.

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
async fn buffer_is_a_uint8array_subclass_with_node_surface() {
  let dir = tempfile::tempdir().expect("tempdir");
  let context = ctx(dir.path());
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");

  let value = run(
    r"
      import { Buffer as FromModule, constants, atob as moduleAtob } from 'node:buffer';

      const buf = Buffer.from('hi');
      const view = new Uint8Array(4);
      view.set(buf, 1);

      const numeric = Buffer.alloc(4);
      numeric.writeUInt32BE(0xdeadbeef, 0);

      export default {
        isUint8Array: buf instanceof Uint8Array,
        indexAccess: buf[0],
        length: buf.length,
        setIntoTypedArray: Array.from(view),
        base64: Buffer.from('hi').toString('base64'),
        roundTrip: Buffer.from('aGk=', 'base64').toString('utf8'),
        hex: Buffer.from([0xde, 0xad]).toString('hex'),
        isBuffer: Buffer.isBuffer(Buffer.alloc(2)),
        concat: Buffer.concat([Buffer.from('a'), Buffer.from('b')]).toString(),
        slice: Buffer.from('abcdef').subarray(1, 3).toString(),
        equals: Buffer.from('x').equals(Buffer.from('x')),
        readBack: numeric.readUInt32BE(0),
        copyInto: (() => { const dst = Buffer.alloc(2); Buffer.from('xy').copy(dst); return dst.toString(); })(),
        written: (() => { const b = Buffer.alloc(3); b.write('ab'); return b.toString('utf8', 0, 2); })(),
        isEncoding: Buffer.isEncoding('base64'),
        json: Buffer.from([1, 2]).toJSON(),
        globalIsModule: FromModule === Buffer,
        maxLength: typeof constants.MAX_LENGTH === 'number',
        atobIsGlobal: moduleAtob === globalThis.atob,
      };
    ",
    dir.path(),
    &session,
    &context,
  )
  .await;

  assert_eq!(
    value["isUint8Array"],
    serde_json::Value::Bool(true),
    "the whole point of the swap: Buffer IS a Uint8Array"
  );
  assert_eq!(value["indexAccess"], 104, "index accessors work (`h`)");
  assert_eq!(value["length"], 2);
  assert_eq!(
    value["setIntoTypedArray"],
    serde_json::json!([0, 104, 105, 0]),
    "a Buffer can be written straight into a typed array"
  );
  assert_eq!(value["base64"], "aGk=");
  assert_eq!(value["roundTrip"], "hi");
  assert_eq!(value["hex"], "dead");
  assert_eq!(value["isBuffer"], serde_json::Value::Bool(true));
  assert_eq!(value["concat"], "ab");
  assert_eq!(value["slice"], "bc");
  assert_eq!(value["equals"], serde_json::Value::Bool(true));
  assert_eq!(value["readBack"], 0xdead_beef_u32);
  assert_eq!(value["copyInto"], "xy");
  assert_eq!(value["written"], "ab");
  assert_eq!(value["isEncoding"], serde_json::Value::Bool(true));
  assert_eq!(value["json"], serde_json::json!({"type": "Buffer", "data": [1, 2]}));
  assert_eq!(value["globalIsModule"], serde_json::Value::Bool(true));
  assert_eq!(value["maxLength"], serde_json::Value::Bool(true));
  assert_eq!(value["atobIsGlobal"], serde_json::Value::Bool(true));
}

/// Node's `latin1` / `binary` is ISO-8859-1 and its `ascii` masks the
/// high bit — neither is UTF-8, and neither is the WHATWG
/// `windows-1252` a `TextDecoder` means by the same label.
#[tokio::test(flavor = "multi_thread")]
async fn buffer_latin1_and_ascii_are_node_encodings() {
  let dir = tempfile::tempdir().expect("tempdir");
  let context = ctx(dir.path());
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");
  let value = run(
    "export default {
       latin1Bytes: Array.from(Buffer.from('\\u00e9A', 'latin1')),
       latin1RoundTrip: Buffer.from([0xe9, 0x41]).toString('latin1'),
       binaryIsLatin1: Buffer.from([0x80]).toString('binary'),
       truncated: Array.from(Buffer.from('\\u20ac', 'latin1')),
       asciiMasked: Array.from(Buffer.from('\\u00e9', 'ascii')),
       asciiDecode: Buffer.from([0xe9]).toString('ascii'),
       utf8Stays: Array.from(Buffer.from('\\u00e9', 'utf8')),
       webLabel: new TextDecoder('windows-1252').decode(new Uint8Array([0x80])),
     };",
    dir.path(),
    &session,
    &context,
  )
  .await;
  assert_eq!(value["latin1Bytes"], serde_json::json!([0xe9, 0x41]), "{value}");
  assert_eq!(value["latin1RoundTrip"], "\u{e9}A");
  assert_eq!(value["binaryIsLatin1"], "\u{80}", "binary is latin1, not windows-1252");
  assert_eq!(
    value["truncated"],
    serde_json::json!([0xac]),
    "Node truncates a code point above U+00FF to its low byte"
  );
  assert_eq!(
    value["asciiMasked"],
    serde_json::json!([0x69]),
    "ascii masks the high bit"
  );
  assert_eq!(value["asciiDecode"], "i");
  assert_eq!(
    value["utf8Stays"],
    serde_json::json!([0xc3, 0xa9]),
    "utf8 is unaffected by the split"
  );
  assert_eq!(
    value["webLabel"], "\u{20ac}",
    "the same label means windows-1252 to a TextDecoder"
  );
}
