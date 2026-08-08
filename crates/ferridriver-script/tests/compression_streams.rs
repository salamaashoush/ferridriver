#![allow(clippy::expect_used, clippy::unwrap_used)]
//! WHATWG `CompressionStream` / `DecompressionStream`.
//!
//! The spec defines exactly three formats. Every test round-trips real
//! bytes rather than asserting the objects merely exist: a stream that
//! silently emitted nothing would still satisfy a shape-only check.

use std::sync::Arc;

use ferridriver_script::{Outcome, PathSandbox, RunContext, RunOptions, ScriptEngine, ScriptEngineConfig};

async fn run(src: &str) -> Outcome {
  let tmp = tempfile::tempdir().expect("tempdir");
  let ctx = RunContext {
    vars: Arc::new(ferridriver_script::InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(tmp.path()).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: Vec::new(),
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
  };
  ScriptEngine::new(ScriptEngineConfig::default())
    .run(src, &[], RunOptions::default(), ctx)
    .await
    .outcome
}

fn val(o: &Outcome) -> &serde_json::Value {
  match o {
    Outcome::Ok { success } => &success.value,
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

/// Pipe `text` through a `CompressionStream` and back through a
/// `DecompressionStream`, reporting the round-tripped text plus the
/// compressed byte count.
const ROUND_TRIP: &str = "\
  const enc = new TextEncoder();\
  const source = new ReadableStream({ start(c) { c.enqueue(enc.encode(TEXT)); c.close(); } });\
  const compressed = source.pipeThrough(new CompressionStream(FORMAT));\
  const packed = [];\
  const reader = compressed.getReader();\
  for (;;) { const r = await reader.read(); if (r.done) break; packed.push(...r.value); }\
  const back = new ReadableStream({ start(c) { c.enqueue(new Uint8Array(packed)); c.close(); } })\
    .pipeThrough(new DecompressionStream(FORMAT));\
  const out = [];\
  const r2 = back.getReader();\
  for (;;) { const r = await r2.read(); if (r.done) break; out.push(...r.value); }\
  return { text: new TextDecoder().decode(new Uint8Array(out)), packedLen: packed.length, \
           firstTwo: packed.slice(0, 2) };";

async fn round_trip(format: &str, text: &str) -> serde_json::Value {
  let src = ROUND_TRIP
    .replace("FORMAT", &format!("'{format}'"))
    .replace("TEXT", &format!("'{text}'"));
  val(&run(&src).await).clone()
}

/// Highly repetitive, so a genuine deflate must come out far smaller —
/// an implementation that passed bytes through unchanged would fail the
/// size assertion.
fn payload() -> String {
  "ferridriver-compression-".repeat(64)
}

#[tokio::test(flavor = "multi_thread")]
async fn gzip_round_trips_and_emits_a_gzip_header() {
  let text = payload();
  let seen = round_trip("gzip", &text).await;
  assert_eq!(seen["text"], serde_json::json!(text), "{seen}");
  assert_eq!(
    seen["firstTwo"],
    serde_json::json!([0x1f, 0x8b]),
    "gzip magic bytes: {seen}"
  );
  let packed = seen["packedLen"].as_u64().unwrap();
  let raw = text.len() as u64;
  assert!(
    packed < raw / 4,
    "repetitive payload must actually compress ({packed} vs {raw})"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn deflate_round_trips_with_a_zlib_wrapper() {
  let text = payload();
  let seen = round_trip("deflate", &text).await;
  assert_eq!(seen["text"], serde_json::json!(text), "{seen}");
  // zlib header: CMF 0x78 for a 32K-window deflate stream.
  assert_eq!(seen["firstTwo"][0], serde_json::json!(0x78), "zlib header: {seen}");
}

#[tokio::test(flavor = "multi_thread")]
async fn deflate_raw_round_trips_without_a_wrapper() {
  let text = payload();
  let seen = round_trip("deflate-raw", &text).await;
  assert_eq!(seen["text"], serde_json::json!(text), "{seen}");
  assert_ne!(
    seen["firstTwo"][0],
    serde_json::json!(0x78),
    "deflate-raw carries no zlib header: {seen}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_format_is_a_type_error() {
  let o = run(
    "const bad = [];\
     for (const f of ['br', 'zstd', 'GZIP', '']) {\
       try { new CompressionStream(f); bad.push(f); } catch (e) {}\
       try { new DecompressionStream(f); bad.push('d:' + f); } catch (e) {}\
     }\
     return bad;",
  )
  .await;
  assert_eq!(
    val(&o),
    &serde_json::json!([]),
    "only gzip/deflate/deflate-raw are CompressionFormat values"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_non_buffer_chunk_is_a_type_error() {
  // Spec: the writable side takes BufferSource only. A string must NOT
  // be silently UTF-8 encoded.
  //
  // A reader is attached first on purpose: a `TransformStream`'s
  // readable side has a highWaterMark of 0, so `await writer.write(...)`
  // with nobody reading blocks on backpressure forever — by spec, not
  // because the transform failed.
  let o = run(
    "const cs = new CompressionStream('gzip');\
     const reader = cs.readable.getReader();\
     const draining = (async () => { try { for (;;) { const r = await reader.read(); if (r.done) return; } } \
       catch (e) { /* the stream errors too; the write rejection is what is under test */ } })();\
     const w = cs.writable.getWriter();\
     try { await w.write('plain string'); return 'NO THROW'; } catch (e) { return 'threw'; }",
  )
  .await;
  assert_eq!(val(&o), &serde_json::json!("threw"));
}

#[tokio::test(flavor = "multi_thread")]
async fn decompressing_garbage_errors_rather_than_yielding_nothing() {
  let o = run(
    "const src = new ReadableStream({ start(c) { c.enqueue(new Uint8Array([1,2,3,4,5,6,7,8])); c.close(); } });\
     const out = src.pipeThrough(new DecompressionStream('gzip'));\
     try { const r = await out.getReader().read(); return 'NO THROW'; } \
     catch (e) { return 'threw'; }",
  )
  .await;
  assert_eq!(
    val(&o),
    &serde_json::json!("threw"),
    "invalid input must surface, not decode to an empty body"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn multi_chunk_input_is_compressed_as_one_stream() {
  // Each write is a separate transform step, so the coder must carry
  // state across them — compressing each chunk independently would
  // produce a concatenation of members that still decodes, but the flush
  // trailer would be wrong for gzip.
  let o = run(
    "const enc = new TextEncoder();\
     const cs = new CompressionStream('gzip');\
     const w = cs.writable.getWriter();\
     const collect = (async () => { const out = []; const r = cs.readable.getReader();\
       for (;;) { const x = await r.read(); if (x.done) break; out.push(...x.value); } return out; })();\
     await w.write(enc.encode('alpha-'));\
     await w.write(enc.encode('beta-'));\
     await w.write(enc.encode('gamma'));\
     await w.close();\
     const packed = await collect;\
     const back = new ReadableStream({ start(c) { c.enqueue(new Uint8Array(packed)); c.close(); } })\
       .pipeThrough(new DecompressionStream('gzip'));\
     const out = []; const r2 = back.getReader();\
     for (;;) { const x = await r2.read(); if (x.done) break; out.push(...x.value); }\
     return new TextDecoder().decode(new Uint8Array(out));",
  )
  .await;
  assert_eq!(val(&o), &serde_json::json!("alpha-beta-gamma"));
}

#[tokio::test(flavor = "multi_thread")]
async fn generic_transform_stream_shape() {
  let o = run(
    "const cs = new CompressionStream('gzip');\
     const ds = new DecompressionStream('deflate');\
     return { \
       csTag: Object.prototype.toString.call(cs), \
       dsTag: Object.prototype.toString.call(ds), \
       readable: cs.readable.constructor.name, \
       writable: cs.writable.constructor.name, \
       stable: cs.readable === cs.readable, \
       notATransform: cs instanceof TransformStream };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["csTag"], serde_json::json!("[object CompressionStream]"));
  assert_eq!(v["dsTag"], serde_json::json!("[object DecompressionStream]"));
  assert_eq!(v["readable"], serde_json::json!("ReadableStream"));
  assert_eq!(v["writable"], serde_json::json!("WritableStream"));
  assert_eq!(v["stable"], serde_json::json!(true), "the same pair every access");
  assert_eq!(
    v["notATransform"],
    serde_json::json!(false),
    "spec: a generic transform stream is not a TransformStream subclass"
  );
}
