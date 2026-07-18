#![allow(clippy::expect_used, clippy::unwrap_used)]
//! WHATWG streams in the `QuickJS` runtime: `ReadableStream` (default and
//! byte/BYOB), `WritableStream`, `TransformStream`, `pipeTo`/
//! `pipeThrough`, `tee`, and the queuing strategies.
//!
//! Every test asserts an observable effect of the feature under test —
//! a callback that fired, a chunk that arrived on the far side of a
//! pipe, a `desiredSize` that moved — not merely that the call did not
//! throw.

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

fn err(o: &Outcome) -> String {
  match o {
    Outcome::Ok { success } => panic!("expected error, got ok: {:?}", success.value),
    Outcome::Error { error } => format!("{error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn globals_are_the_full_streams_surface() {
  let o = run(
    "return ['ReadableStream','ReadableStreamDefaultReader','ReadableStreamBYOBReader',\
      'ReadableStreamDefaultController','ReadableByteStreamController','ReadableStreamBYOBRequest',\
      'WritableStream','WritableStreamDefaultWriter','WritableStreamDefaultController',\
      'TransformStream','TransformStreamDefaultController',\
      'CountQueuingStrategy','ByteLengthQueuingStrategy'].filter(n => typeof globalThis[n] !== 'function');",
  )
  .await;
  assert_eq!(
    val(&o),
    &serde_json::json!([]),
    "every Streams constructor must be a global"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn underlying_source_pull_and_cancel_fire() {
  let o = run(
    "const log = []; \
     const s = new ReadableStream({ \
       start() { log.push('start'); }, \
       pull(c) { log.push('pull'); c.enqueue(log.length); }, \
       cancel(reason) { log.push('cancel:' + reason); } }); \
     const r = s.getReader(); \
     const a = await r.read(); const b = await r.read(); \
     await r.cancel('bye'); \
     return { a: a.value, b: b.value, log };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["log"][0], serde_json::json!("start"), "start() runs at construction");
  assert!(
    v["log"].as_array().unwrap().iter().any(|e| e == "pull"),
    "pull() is invoked to fill the queue: {v}"
  );
  assert_eq!(
    v["log"].as_array().unwrap().last().unwrap(),
    &serde_json::json!("cancel:bye"),
    "cancel(reason) reaches the underlying source: {v}"
  );
  assert!(v["a"].is_number() && v["b"].is_number(), "both reads produced a chunk");
}

#[tokio::test(flavor = "multi_thread")]
async fn tee_gives_two_independently_readable_branches() {
  let o = run(
    "const s = new ReadableStream({ start(c) { c.enqueue('a'); c.enqueue('b'); c.close(); } }); \
     const [x, y] = s.tee(); \
     const rx = x.getReader(); \
     const first = (await rx.read()).value; \
     const ry = y.getReader(); \
     const drain = async (r) => { const out = []; for (;;) { const v = await r.read(); if (v.done) break; out.push(v.value); } return out; }; \
     const restX = await drain(rx); const allY = await drain(ry); \
     return { locked: s.locked, first, restX, allY };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["locked"], serde_json::json!(true), "tee() locks the source");
  assert_eq!(v["first"], serde_json::json!("a"));
  assert_eq!(v["restX"], serde_json::json!(["b"]));
  assert_eq!(
    v["allY"],
    serde_json::json!(["a", "b"]),
    "the second branch still sees every chunk, at its own pace"
  );
}

/// `tee()` locks the source but — unlike the Rust `tee_readable_stream`
/// helper behind `Response.clone()` — the spec puts no `disturbed` bar on
/// it: a partially-read stream tees, and the branches see only what is
/// left.
#[tokio::test(flavor = "multi_thread")]
async fn tee_of_a_locked_stream_throws_but_a_read_one_tees() {
  let o = run(
    "const s = new ReadableStream({ start(c) { c.enqueue(1); c.enqueue(2); c.close(); } }); \
     const r = s.getReader(); await r.read(); \
     let lockedErr = null; try { s.tee(); } catch (e) { lockedErr = e.message; } \
     r.releaseLock(); \
     const [a] = s.tee(); const ra = a.getReader(); \
     return { lockedErr, rest: (await ra.read()).value };",
  )
  .await;
  let v = val(&o);
  assert!(
    v["lockedErr"].as_str().unwrap_or_default().contains("locked"),
    "teeing a locked stream is a TypeError: {v}"
  );
  assert_eq!(
    v["rest"],
    serde_json::json!(2),
    "after releaseLock the branches carry the remaining chunks"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn writable_stream_sink_receives_chunks_and_close() {
  let o = run(
    "const seen = []; let closed = false; \
     const w = new WritableStream({ write(chunk) { seen.push(chunk); }, close() { closed = true; } }); \
     const wr = w.getWriter(); \
     await wr.write('one'); await wr.write('two'); await wr.close(); \
     return { seen, closed, locked: w.locked };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["seen"], serde_json::json!(["one", "two"]), "sink saw every write");
  assert_eq!(v["closed"], serde_json::json!(true), "close() reached the sink");
  assert_eq!(v["locked"], serde_json::json!(true), "the writer holds the lock");
}

#[tokio::test(flavor = "multi_thread")]
async fn writable_abort_rejects_pending_writes_and_reaches_the_sink() {
  let o = run(
    "let abortReason = null; \
     const w = new WritableStream({ write() {}, abort(r) { abortReason = r; } }); \
     const wr = w.getWriter(); \
     await wr.write('one'); \
     await wr.abort('nope'); \
     let closedErr = null; try { await wr.closed; } catch (e) { closedErr = String(e); } \
     let writeErr = null; try { await wr.write('after'); } catch (e) { writeErr = String(e); } \
     return { abortReason, closedErr, writeErr };",
  )
  .await;
  let v = val(&o);
  assert_eq!(
    v["abortReason"],
    serde_json::json!("nope"),
    "abort(reason) reaches the sink"
  );
  assert!(
    v["closedErr"].as_str().unwrap_or_default().contains("nope"),
    "the writer's closed promise rejects with the reason: {v}"
  );
  assert!(
    v["writeErr"].as_str().unwrap_or_default().contains("nope"),
    "a write after abort rejects with the same reason: {v}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn writer_desired_size_reflects_backpressure() {
  let o = run(
    "const w = new WritableStream({ write() { return new Promise(() => {}); } }, \
       new CountQueuingStrategy({ highWaterMark: 2 })); \
     const wr = w.getWriter(); \
     const start = wr.desiredSize; \
     wr.write('a'); const afterOne = wr.desiredSize; \
     wr.write('b'); const afterTwo = wr.desiredSize; \
     return { start, afterOne, afterTwo };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["start"], serde_json::json!(2), "highWaterMark 2 = 2 slots free");
  assert_eq!(v["afterOne"], serde_json::json!(1));
  assert_eq!(
    v["afterTwo"],
    serde_json::json!(0),
    "queue full: desiredSize drops to 0 (backpressure)"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn byte_length_queuing_strategy_sizes_by_bytes() {
  let o = run(
    "const s = new ByteLengthQueuingStrategy({ highWaterMark: 64 }); \
     return { hwm: s.highWaterMark, size: s.size(new Uint8Array(8)), \
       countSize: new CountQueuingStrategy({ highWaterMark: 3 }).size({}) };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["hwm"], serde_json::json!(64));
  assert_eq!(
    v["size"],
    serde_json::json!(8),
    "byte-length strategy sizes by byteLength"
  );
  assert_eq!(
    v["countSize"],
    serde_json::json!(1),
    "count strategy sizes everything 1"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn pipe_to_moves_every_chunk_and_closes_the_sink() {
  let o = run(
    "const src = new ReadableStream({ start(c) { for (const x of ['a','b','c']) c.enqueue(x); c.close(); } }); \
     const seen = []; let closed = false; \
     const dst = new WritableStream({ write(c) { seen.push(c); }, close() { closed = true; } }); \
     await src.pipeTo(dst); \
     return { seen, closed };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["seen"], serde_json::json!(["a", "b", "c"]));
  assert_eq!(v["closed"], serde_json::json!(true), "pipeTo closes the destination");
}

#[tokio::test(flavor = "multi_thread")]
async fn pipe_to_prevent_close_leaves_the_sink_open() {
  let o = run(
    "const src = new ReadableStream({ start(c) { c.enqueue('x'); c.close(); } }); \
     let closed = false; \
     const dst = new WritableStream({ write() {}, close() { closed = true; } }); \
     await src.pipeTo(dst, { preventClose: true }); \
     return { closed };",
  )
  .await;
  assert_eq!(
    val(&o)["closed"],
    serde_json::json!(false),
    "preventClose: true keeps the destination open"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn pipe_to_signal_aborts_the_pipe() {
  let o = run(
    // The source never produces, so no write is in flight when the
    // signal fires — `shutdownWithAction` waits for pending writes, so a
    // stuck sink would (correctly, per spec) hang instead.
    "const src = new ReadableStream({ pull() { return new Promise(() => {}); } }); \
     const dst = new WritableStream({ write() {} }); \
     const ac = new AbortController(); \
     const p = src.pipeTo(dst, { signal: ac.signal }); \
     ac.abort(); \
     try { await p; return 'resolved'; } catch (e) { return e.name + ':' + e.message; }",
  )
  .await;
  let s = val(&o).as_str().unwrap_or_default().to_string();
  assert!(
    s.starts_with("AbortError"),
    "an aborted pipeTo rejects with a DOMException AbortError, got: {s}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn transform_stream_pipe_through_transforms_every_chunk() {
  let o = run(
    "const upper = new TransformStream({ transform(chunk, c) { c.enqueue(chunk.toUpperCase()); }, \
       flush(c) { c.enqueue('!'); } }); \
     const src = new ReadableStream({ start(c) { c.enqueue('a'); c.enqueue('b'); c.close(); } }); \
     const out = []; \
     const r = src.pipeThrough(upper).getReader(); \
     for (;;) { const v = await r.read(); if (v.done) break; out.push(v.value); } \
     return { out, isStream: upper.readable instanceof ReadableStream, \
       isWritable: upper.writable instanceof WritableStream };",
  )
  .await;
  let v = val(&o);
  assert_eq!(
    v["out"],
    serde_json::json!(["A", "B", "!"]),
    "transform() ran per chunk and flush() appended"
  );
  assert_eq!(v["isStream"], serde_json::json!(true));
  assert_eq!(v["isWritable"], serde_json::json!(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn transform_error_propagates_to_the_readable_side() {
  let o = run(
    "const t = new TransformStream({ transform() { throw new Error('boom'); } }); \
     const w = t.writable.getWriter(); const r = t.readable.getReader(); \
     w.write('x').catch(() => {}); \
     try { await r.read(); return 'no throw'; } catch (e) { return e.message; }",
  )
  .await;
  assert!(
    val(&o).as_str().unwrap_or_default().contains("boom"),
    "a throwing transform errors the readable side: {}",
    val(&o)
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn byte_stream_supports_byob_reads() {
  let o = run(
    "const s = new ReadableStream({ type: 'bytes', \
       start(c) { c.enqueue(new Uint8Array([1,2,3,4])); c.close(); } }); \
     const r = s.getReader({ mode: 'byob' }); \
     const first = await r.read(new Uint8Array(2)); \
     const second = await r.read(new Uint8Array(2)); \
     return { first: Array.from(first.value), second: Array.from(second.value), \
       isByob: r instanceof ReadableStreamBYOBReader };",
  )
  .await;
  let v = val(&o);
  assert_eq!(
    v["first"],
    serde_json::json!([1, 2]),
    "a BYOB read fills the caller's buffer"
  );
  assert_eq!(v["second"], serde_json::json!([3, 4]));
  assert_eq!(v["isByob"], serde_json::json!(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn byte_stream_default_reader_still_works() {
  let o = run(
    "const s = new ReadableStream({ type: 'bytes', \
       start(c) { c.enqueue(new Uint8Array([9,8])); c.close(); } }); \
     const r = s.getReader(); const v = await r.read(); \
     return { bytes: Array.from(v.value), isCtor: true };",
  )
  .await;
  assert_eq!(
    val(&o)["bytes"],
    serde_json::json!([9, 8]),
    "a byte stream also serves default readers"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn second_get_reader_throws_while_locked() {
  let o = run(
    "const s = new ReadableStream({ start(c) { c.close(); } }); \
     s.getReader(); s.getReader();",
  )
  .await;
  let e = err(&o);
  assert!(e.contains("locked"), "a locked stream refuses a second reader: {e}");
}

#[tokio::test(flavor = "multi_thread")]
async fn blob_stream_pipes_through_a_transform() {
  let o = run(
    "const b = new Blob(['hello']); \
     const dec = new TextDecoder(); \
     const t = new TransformStream({ transform(chunk, c) { c.enqueue(dec.decode(chunk).toUpperCase()); } }); \
     const r = b.stream().pipeThrough(t).getReader(); \
     const out = []; for (;;) { const v = await r.read(); if (v.done) break; out.push(v.value); } \
     return out.join('');",
  )
  .await;
  assert_eq!(
    val(&o),
    &serde_json::json!("HELLO"),
    "Blob.stream() is a real ReadableStream that pipes"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn async_iteration_over_a_readable_stream() {
  let o = run(
    "const s = new ReadableStream({ start(c) { c.enqueue(1); c.enqueue(2); c.close(); } }); \
     const out = []; for await (const chunk of s) out.push(chunk); return out;",
  )
  .await;
  assert_eq!(
    val(&o),
    &serde_json::json!([1, 2]),
    "Symbol.asyncIterator yields chunks"
  );
}
