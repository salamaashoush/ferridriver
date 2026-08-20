#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Web-platform globals that are not `fetch` or streams: `File`,
//! `FormData` iteration, `structuredClone`, `performance`, and the
//! `TextEncoder`/`TextDecoder` options.

use std::sync::Arc;

use ferridriver_script::{Outcome, RunContext, RunOptions, ScriptEngine, ScriptEngineConfig};

async fn run(src: &str) -> Outcome {
  let tmp = tempfile::tempdir().expect("tempdir");
  let ctx = RunContext {
    vars: Arc::new(ferridriver_script::InMemoryVars::new()),
    script_root: tmp.path().into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: Vec::new(),
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
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

#[tokio::test(flavor = "multi_thread")]
async fn file_is_a_blob_with_a_name() {
  let o = run(
    "const f = new File(['hello'], 'note.txt', { type: 'TEXT/Plain', lastModified: 1234 }); \
     return { name: f.name, size: f.size, type: f.type, text: await f.text(), \
       lastModified: f.lastModified, isFile: f instanceof File, isBlob: f instanceof Blob, \
       sliceText: await f.slice(0, 2).text(), sliceIsBlob: f.slice(0, 2) instanceof Blob };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["name"], serde_json::json!("note.txt"));
  assert_eq!(v["size"], serde_json::json!(5));
  assert_eq!(v["type"], serde_json::json!("text/plain"), "type is lowercased");
  assert_eq!(v["text"], serde_json::json!("hello"));
  assert_eq!(v["lastModified"], serde_json::json!(1234));
  assert_eq!(v["isFile"], serde_json::json!(true));
  assert_eq!(
    v["isBlob"],
    serde_json::json!(true),
    "File inherits from Blob (prototype chain)"
  );
  assert_eq!(v["sliceText"], serde_json::json!("he"));
  assert_eq!(v["sliceIsBlob"], serde_json::json!(true), "slicing a File gives a Blob");
}

#[tokio::test(flavor = "multi_thread")]
async fn file_streams_and_defaults_last_modified_to_now() {
  let o = run(
    "const f = new File(['abc'], 'a.bin'); \
     const r = f.stream().getReader(); const chunk = await r.read(); \
     return { bytes: Array.from(chunk.value), recent: f.lastModified > 1600000000000 };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["bytes"], serde_json::json!([97, 98, 99]));
  assert_eq!(
    v["recent"],
    serde_json::json!(true),
    "lastModified defaults to the current time"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn form_data_file_entry_round_trips_as_a_file() {
  let o = run(
    "const fd = new FormData(); \
     fd.append('f', new File(['data'], 'report.csv', { type: 'text/csv' })); \
     fd.append('b', new Blob(['x'])); \
     const f = fd.get('f'); const b = fd.get('b'); \
     return { isFile: f instanceof File, name: f.name, type: f.type, text: await f.text(), \
       blobName: b.name };",
  )
  .await;
  let v = val(&o);
  assert_eq!(
    v["isFile"],
    serde_json::json!(true),
    "a file entry reads back as a File"
  );
  assert_eq!(
    v["name"],
    serde_json::json!("report.csv"),
    "the File supplied its own filename — no explicit third argument"
  );
  assert_eq!(v["type"], serde_json::json!("text/csv"));
  assert_eq!(v["text"], serde_json::json!("data"));
  assert_eq!(
    v["blobName"],
    serde_json::json!("blob"),
    "a bare Blob gets the spec's default filename"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn form_data_appended_file_names_the_multipart_part() {
  let o = run(
    "const fd = new FormData(); \
     fd.append('upload', new File(['payload'], 'invoice.pdf', { type: 'application/pdf' })); \
     const r = new Response(fd); return await r.text();",
  )
  .await;
  let body = val(&o).as_str().unwrap_or_default();
  assert!(
    body.contains("filename=\"invoice.pdf\""),
    "the File's own name reaches the multipart part: {body}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn form_data_iterators_are_real_and_live() {
  let o = run(
    "const fd = new FormData(); fd.append('a', '1'); fd.append('b', '2'); \
     const it = fd.entries(); \
     const isIterator = typeof it.next === 'function' && it[Symbol.iterator]() === it; \
     const first = it.next(); \
     // Live semantics: a delete mid-iteration is observed.\n\
     fd.delete('b'); \
     const second = it.next(); \
     const keys = [...new FormData([].concat()) .keys()]; \
     const fd2 = new FormData(); fd2.append('x', '1'); fd2.append('y', '2'); \
     return { isIterator, firstKey: first.value[0], firstDone: first.done, \
       secondDone: second.done, keys, \
       allKeys: [...fd2.keys()], allValues: [...fd2.values()], \
       spread: [...fd2].map(p => p.join('=')) };",
  )
  .await;
  let v = val(&o);
  assert_eq!(
    v["isIterator"],
    serde_json::json!(true),
    "entries() returns a real iterator, not an array"
  );
  assert_eq!(v["firstKey"], serde_json::json!("a"));
  assert_eq!(v["firstDone"], serde_json::json!(false));
  assert_eq!(
    v["secondDone"],
    serde_json::json!(true),
    "deleting the remaining entry mid-iteration ends the loop (live, not a snapshot)"
  );
  assert_eq!(v["allKeys"], serde_json::json!(["x", "y"]));
  assert_eq!(v["allValues"], serde_json::json!(["1", "2"]));
  assert_eq!(
    v["spread"],
    serde_json::json!(["x=1", "y=2"]),
    "[Symbol.iterator] yields entries"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn structured_clone_deep_copies_and_keeps_shared_references() {
  let o = run(
    "const shared = { n: 1 }; \
     const src = { a: [1, { deep: true }], shared1: shared, shared2: shared, \
       d: new Date(1000), r: /ab+/gi, m: new Map([['k', { v: 1 }]]), s: new Set([1, 2]), \
       buf: new Uint8Array([7, 8]) }; \
     src.self = src; \
     const c = structuredClone(src); \
     c.a[1].deep = false; c.m.get('k').v = 99; \
     return { notSame: c !== src, deepIndependent: src.a[1].deep, \
       mapIndependent: src.m.get('k').v, mapValue: c.m.get('k').v, \
       setHas: c.s.has(2), date: c.d.getTime(), dateIsDate: c.d instanceof Date, \
       regex: c.r.source + ':' + c.r.flags, bytes: Array.from(c.buf), \
       cycle: c.self === c, sharedPreserved: c.shared1 === c.shared2, \
       sharedDetached: c.shared1 !== shared };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["notSame"], serde_json::json!(true));
  assert_eq!(
    v["deepIndependent"],
    serde_json::json!(true),
    "mutating the clone must not touch the original"
  );
  assert_eq!(v["mapIndependent"], serde_json::json!(1));
  assert_eq!(v["mapValue"], serde_json::json!(99));
  assert_eq!(v["setHas"], serde_json::json!(true));
  assert_eq!(v["date"], serde_json::json!(1000));
  assert_eq!(v["dateIsDate"], serde_json::json!(true));
  assert_eq!(v["regex"], serde_json::json!("ab+:gi"));
  assert_eq!(v["bytes"], serde_json::json!([7, 8]));
  assert_eq!(v["cycle"], serde_json::json!(true), "a cycle stays a cycle");
  assert_eq!(
    v["sharedPreserved"],
    serde_json::json!(true),
    "one object reached twice is one object in the clone"
  );
  assert_eq!(v["sharedDetached"], serde_json::json!(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn structured_clone_refuses_uncloneable_values() {
  let o = run(
    "const out = {}; \
     try { structuredClone(() => {}); out.fn = 'no throw'; } \
     catch (e) { out.fn = e.name; out.isDom = e instanceof DOMException; } \
     try { structuredClone({ nested: Symbol('s') }); out.sym = 'no throw'; } \
     catch (e) { out.sym = e.name; } \
     return out;",
  )
  .await;
  let v = val(&o);
  assert_eq!(
    v["fn"],
    serde_json::json!("DataCloneError"),
    "a function is not cloneable"
  );
  assert_eq!(v["isDom"], serde_json::json!(true), "the failure is a DOMException");
  assert_eq!(v["sym"], serde_json::json!("DataCloneError"));
}

#[tokio::test(flavor = "multi_thread")]
async fn performance_now_is_monotonic_with_a_time_origin() {
  let o = run(
    "const a = performance.now(); \
     let sink = 0; for (let i = 0; i < 200000; i++) sink += i; \
     const b = performance.now(); \
     return { ordered: b >= a, fractional: typeof a === 'number', \
       originIsRecent: performance.timeOrigin > 1600000000000, sink: sink > 0 };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["ordered"], serde_json::json!(true), "now() never goes backwards");
  assert_eq!(v["fractional"], serde_json::json!(true));
  assert_eq!(
    v["originIsRecent"],
    serde_json::json!(true),
    "timeOrigin is a wall-clock epoch in ms"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn text_encoder_encode_into_writes_through_and_reports_progress() {
  let o = run(
    "const enc = new TextEncoder(); \
     const buf = new Uint8Array(8); \
     const full = enc.encodeInto('abc', buf); \
     const small = new Uint8Array(3); \
     // 'é' is 2 bytes: only 'ab' plus nothing partial fits after it.\n\
     const partial = enc.encodeInto('abé', small); \
     const tight = new Uint8Array(2); \
     const noSplit = enc.encodeInto('é!', tight); \
     return { full, buf: Array.from(buf.slice(0, 3)), partial, \
       small: Array.from(small), noSplit };",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["full"]["read"], serde_json::json!(3));
  assert_eq!(v["full"]["written"], serde_json::json!(3));
  assert_eq!(
    v["buf"],
    serde_json::json!([97, 98, 99]),
    "encodeInto writes into the caller's buffer, not a copy"
  );
  assert_eq!(v["partial"]["read"], serde_json::json!(2));
  assert_eq!(
    v["partial"]["written"],
    serde_json::json!(2),
    "the 2-byte 'é' does not fit in the remaining byte, so it is not split"
  );
  assert_eq!(v["small"], serde_json::json!([97, 98, 0]));
  assert_eq!(v["noSplit"]["read"], serde_json::json!(1));
  assert_eq!(v["noSplit"]["written"], serde_json::json!(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn text_decoder_honours_fatal_stream_bom_and_labels() {
  let o = run(
    "const out = {}; \
     out.label = new TextDecoder('UTF-8').encoding; \
     try { new TextDecoder('shift_jis'); out.badLabel = 'no throw'; } \
     catch (e) { out.badLabel = e.name; } \
     out.latin1 = new TextDecoder('windows-1252').decode(new Uint8Array([0xe9, 0x41])); \
     out.utf16 = new TextDecoder('utf-16le').decode(new Uint8Array([0x68, 0x00, 0x69, 0x00])); \
     out.lossy = new TextDecoder().decode(new Uint8Array([0xff, 0x41])); \
     try { new TextDecoder('utf-8', { fatal: true }).decode(new Uint8Array([0xff])); \
       out.fatal = 'no throw'; } catch (e) { out.fatal = e.name; } \
     out.fatalFlag = new TextDecoder('utf-8', { fatal: true }).fatal; \
     // 'é' = [0xc3, 0xa9], split across two streaming chunks.\n\
     const d = new TextDecoder(); \
     out.chunk1 = d.decode(new Uint8Array([0x61, 0xc3]), { stream: true }); \
     out.chunk2 = d.decode(new Uint8Array([0xa9]), { stream: true }); \
     out.bomStripped = new TextDecoder().decode(new Uint8Array([0xef, 0xbb, 0xbf, 0x68, 0x69])); \
     out.bomKept = new TextDecoder('utf-8', { ignoreBOM: true }) \
       .decode(new Uint8Array([0xef, 0xbb, 0xbf, 0x68, 0x69])).length; \
     return out;",
  )
  .await;
  let v = val(&o);
  assert_eq!(v["label"], serde_json::json!("utf-8"), "labels are case-insensitive");
  assert_eq!(
    v["badLabel"],
    serde_json::json!("RangeError"),
    "an unimplemented encoding is refused, not silently misdecoded"
  );
  assert_eq!(
    v["latin1"],
    serde_json::json!("\u{e9}A"),
    "windows-1252 is implemented, not merely accepted"
  );
  assert_eq!(v["utf16"], serde_json::json!("hi"), "utf-16le is implemented");
  assert_eq!(v["lossy"], serde_json::json!("\u{fffd}A"), "default is lossy");
  assert_eq!(
    v["fatal"],
    serde_json::json!("TypeError"),
    "fatal: true throws on invalid UTF-8"
  );
  assert_eq!(v["fatalFlag"], serde_json::json!(true));
  assert_eq!(
    v["chunk1"],
    serde_json::json!("a"),
    "the split code point is held back, not emitted as U+FFFD"
  );
  assert_eq!(v["chunk2"], serde_json::json!("é"), "the next chunk completes it");
  assert_eq!(
    v["bomStripped"],
    serde_json::json!("hi"),
    "the BOM is removed by default"
  );
  assert_eq!(
    v["bomKept"],
    serde_json::json!(3),
    "ignoreBOM: true keeps the U+FEFF code point"
  );
}
