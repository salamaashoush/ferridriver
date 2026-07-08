#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Web Crypto subset (`crypto` global): `randomUUID`,
//! `getRandomValues`, `subtle.digest`, HMAC `importKey`/`sign`/`verify`,
//! and the typed `NotSupportedError` rejections — exercised end-to-end
//! through `ScriptEngine::run` so the whole `QuickJS` dispatch path is
//! covered.

use std::sync::Arc;

use ferridriver_script::{
  InMemoryVars, Outcome, PathSandbox, RunContext, RunOptions, ScriptEngine, ScriptEngineConfig,
};

fn engine() -> (ScriptEngine, tempfile::TempDir, RunContext) {
  let tmp = tempfile::tempdir().expect("tempdir");
  let context = RunContext {
    vars: Arc::new(InMemoryVars::new()),
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
  (ScriptEngine::new(ScriptEngineConfig::default()), tmp, context)
}

async fn run_ok(src: &str) -> serde_json::Value {
  let (eng, _tmp, ctx) = engine();
  let result = eng.run(src, &[], RunOptions::default(), ctx).await;
  match result.outcome {
    Outcome::Ok { success } => success.value,
    Outcome::Error { error } => panic!("script failed: {error:?}"),
  }
}

#[tokio::test]
async fn random_uuid_is_v4_and_unique() {
  let v = run_ok(
    r"
    const a = crypto.randomUUID();
    const b = crypto.randomUUID();
    const re = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
    return { aOk: re.test(a), bOk: re.test(b), distinct: a !== b };
  ",
  )
  .await;
  assert_eq!(v, serde_json::json!({ "aOk": true, "bOk": true, "distinct": true }));
}

#[tokio::test]
async fn get_random_values_fills_in_place_and_validates() {
  let v = run_ok(
    r"
    const buf = new Uint8Array(32);
    const ret = crypto.getRandomValues(buf);
    const filled = buf.some((b) => b !== 0);
    let floatRejected = false;
    try { crypto.getRandomValues(new Float64Array(4)); }
    catch (e) { floatRejected = e.name === 'TypeMismatchError'; }
    let quotaRejected = false;
    try { crypto.getRandomValues(new Uint8Array(65537)); }
    catch (e) { quotaRejected = e.name === 'QuotaExceededError'; }
    return { same: ret === buf, filled, floatRejected, quotaRejected };
  ",
  )
  .await;
  assert_eq!(
    v,
    serde_json::json!({ "same": true, "filled": true, "floatRejected": true, "quotaRejected": true })
  );
}

#[tokio::test]
async fn subtle_digest_matches_known_vectors() {
  // SHA-256("abc") and SHA-1("abc") — FIPS 180-2 test vectors.
  let v = run_ok(
    r"
    const hex = (ab) => Array.from(new Uint8Array(ab)).map((b) => b.toString(16).padStart(2, '0')).join('');
    const data = new TextEncoder().encode('abc');
    const s256 = hex(await crypto.subtle.digest('SHA-256', data));
    const s1 = hex(await crypto.subtle.digest({ name: 'sha-1' }, data));
    return { s256, s1 };
  ",
  )
  .await;
  assert_eq!(
    v,
    serde_json::json!({
      "s256": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
      "s1": "a9993e364706816aba3e25717850c26c9cd0d89d"
    })
  );
}

#[tokio::test]
async fn hmac_sign_and_verify_round_trip() {
  // HMAC-SHA256(key="key", msg="The quick brown fox jumps over the lazy dog")
  let v = run_ok(
    r"
    const enc = new TextEncoder();
    const key = await crypto.subtle.importKey(
      'raw', enc.encode('key'), { name: 'HMAC', hash: 'SHA-256' }, false, ['sign', 'verify']);
    const data = enc.encode('The quick brown fox jumps over the lazy dog');
    const sig = await crypto.subtle.sign('HMAC', key, data);
    const hex = Array.from(new Uint8Array(sig)).map((b) => b.toString(16).padStart(2, '0')).join('');
    const good = await crypto.subtle.verify('HMAC', key, sig, data);
    const tampered = new Uint8Array(sig); tampered[0] ^= 0xff;
    const bad = await crypto.subtle.verify('HMAC', key, tampered, data);
    return { hex, good, bad, type: key.type, algo: key.algorithm.hash.name };
  ",
  )
  .await;
  assert_eq!(
    v,
    serde_json::json!({
      "hex": "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8",
      "good": true,
      "bad": false,
      "type": "secret",
      "algo": "SHA-256"
    })
  );
}

#[tokio::test]
async fn unimplemented_subtle_ops_reject_with_not_supported() {
  let v = run_ok(
    r"
    const out = {};
    for (const op of ['encrypt', 'generateKey', 'deriveKey']) {
      try { await crypto.subtle[op](); out[op] = 'no-throw'; }
      catch (e) { out[op] = e.name; }
    }
    let badAlgo = '';
    try { await crypto.subtle.digest('MD5', new Uint8Array(1)); }
    catch (e) { badAlgo = e.name; }
    return { ...out, badAlgo };
  ",
  )
  .await;
  assert_eq!(
    v,
    serde_json::json!({
      "encrypt": "NotSupportedError",
      "generateKey": "NotSupportedError",
      "deriveKey": "NotSupportedError",
      "badAlgo": "NotSupportedError"
    })
  );
}
