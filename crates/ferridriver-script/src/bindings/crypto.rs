//! Web Crypto subset: `crypto.randomUUID`, `crypto.getRandomValues`,
//! and a `crypto.subtle` with `digest` (SHA-1/256/384/512) plus raw-key
//! HMAC `importKey`/`sign`/`verify`. All native Rust (`getrandom`,
//! `sha1`/`sha2`, `hmac`) — no JS glue.
//!
//! Deliberate subset: asymmetric algorithms (RSA/ECDSA/Ed25519),
//! AES encrypt/decrypt, deriveKey/deriveBits, wrap/unwrapKey and the
//! non-`raw` key formats are not implemented; each rejects with a
//! `NotSupportedError`-named `Error` so callers can feature-detect the
//! same way they would against a browser.

use hmac::Mac;
use rquickjs::function::{Async, Func};
use rquickjs::{ArrayBuffer, Ctx, JsLifetime, Object, Value, class::Trace};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha384, Sha512};

use crate::bindings::convert::throw_named;

/// Web-spec cap for one `getRandomValues` call.
const MAX_RANDOM_BYTES: usize = 65_536;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HashAlgo {
  Sha1,
  Sha256,
  Sha384,
  Sha512,
}

impl HashAlgo {
  fn parse(name: &str) -> Option<Self> {
    match name.to_ascii_uppercase().as_str() {
      "SHA-1" => Some(Self::Sha1),
      "SHA-256" => Some(Self::Sha256),
      "SHA-384" => Some(Self::Sha384),
      "SHA-512" => Some(Self::Sha512),
      _ => None,
    }
  }

  fn name(self) -> &'static str {
    match self {
      Self::Sha1 => "SHA-1",
      Self::Sha256 => "SHA-256",
      Self::Sha384 => "SHA-384",
      Self::Sha512 => "SHA-512",
    }
  }

  fn digest(self, data: &[u8]) -> Vec<u8> {
    match self {
      Self::Sha1 => Sha1::digest(data).to_vec(),
      Self::Sha256 => Sha256::digest(data).to_vec(),
      Self::Sha384 => Sha384::digest(data).to_vec(),
      Self::Sha512 => Sha512::digest(data).to_vec(),
    }
  }

  fn hmac(self, key: &[u8], data: &[u8]) -> Vec<u8> {
    fn mac<M: Mac + hmac::digest::KeyInit>(key: &[u8], data: &[u8]) -> Vec<u8> {
      let mut m = <M as Mac>::new_from_slice(key).unwrap_or_else(|_| unreachable!("HMAC accepts any key length"));
      m.update(data);
      m.finalize().into_bytes().to_vec()
    }
    match self {
      Self::Sha1 => mac::<hmac::Hmac<Sha1>>(key, data),
      Self::Sha256 => mac::<hmac::Hmac<Sha256>>(key, data),
      Self::Sha384 => mac::<hmac::Hmac<Sha384>>(key, data),
      Self::Sha512 => mac::<hmac::Hmac<Sha512>>(key, data),
    }
  }
}

/// `CryptoKey` returned by `subtle.importKey`. Secret (HMAC) keys only.
#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "CryptoKey")]
pub struct CryptoKeyJs {
  #[qjs(skip_trace)]
  key: Vec<u8>,
  #[qjs(skip_trace)]
  hash: HashAlgo,
  extractable: bool,
  #[qjs(skip_trace)]
  usages: Vec<String>,
}

#[rquickjs::methods]
impl CryptoKeyJs {
  #[qjs(get, rename = "type")]
  pub fn key_type(&self) -> &'static str {
    "secret"
  }

  #[qjs(get)]
  pub fn extractable(&self) -> bool {
    self.extractable
  }

  #[qjs(get)]
  pub fn usages(&self) -> Vec<String> {
    self.usages.clone()
  }

  #[qjs(get)]
  pub fn algorithm<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    let o = Object::new(ctx.clone())?;
    o.set("name", "HMAC")?;
    let h = Object::new(ctx)?;
    h.set("name", self.hash.name())?;
    o.set("hash", h)?;
    Ok(o)
  }
}

/// Copy the bytes of a `BufferSource` (ArrayBuffer or any view over
/// one). Copies because the QuickJS heap may move under a later
/// allocation; every consumer here digests immediately anyway.
pub(crate) fn buffer_source_bytes(ctx: &Ctx<'_>, value: &Value<'_>) -> rquickjs::Result<Vec<u8>> {
  if let Some(ab) = ArrayBuffer::from_value(value.clone()) {
    return ab
      .as_bytes()
      .map(<[u8]>::to_vec)
      .ok_or_else(|| throw_named(ctx, "TypeError", "detached ArrayBuffer"));
  }
  if let Some(obj) = value.as_object() {
    let buffer: rquickjs::Result<ArrayBuffer<'_>> = obj.get("buffer");
    if let Ok(ab) = buffer {
      let offset: usize = obj.get("byteOffset")?;
      let len: usize = obj.get("byteLength")?;
      let bytes = ab
        .as_bytes()
        .ok_or_else(|| throw_named(ctx, "TypeError", "detached ArrayBuffer"))?;
      return bytes
        .get(offset..offset + len)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| throw_named(ctx, "TypeError", "view out of bounds"));
    }
  }
  Err(throw_named(
    ctx,
    "TypeError",
    "expected an ArrayBuffer or ArrayBuffer view",
  ))
}

/// Parse `'SHA-256'` or `{ name: 'SHA-256' }` (and the HMAC import
/// shape's nested `hash`) into a [`HashAlgo`].
fn parse_hash(ctx: &Ctx<'_>, value: &Value<'_>) -> rquickjs::Result<HashAlgo> {
  let name = if let Some(s) = value.as_string() {
    s.to_string()?
  } else if let Some(obj) = value.as_object() {
    obj.get::<_, String>("name")?
  } else {
    return Err(throw_named(ctx, "TypeError", "algorithm must be a string or { name }"));
  };
  HashAlgo::parse(&name).ok_or_else(|| {
    throw_named(
      ctx,
      "NotSupportedError",
      format!("unsupported digest algorithm {name:?}"),
    )
  })
}

fn random_uuid() -> rquickjs::Result<String> {
  let mut b = [0u8; 16];
  getrandom::fill(&mut b).map_err(|e| rquickjs::Error::new_from_js_message("crypto", "randomUUID", e.to_string()))?;
  b[6] = (b[6] & 0x0f) | 0x40; // version 4
  b[8] = (b[8] & 0x3f) | 0x80; // RFC 4122 variant
  Ok(format!(
    "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
    b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
  ))
}

/// `crypto.getRandomValues(view)`: fill an integer typed array in place
/// and return it.
fn get_random_values<'js>(ctx: Ctx<'js>, view: Value<'js>) -> rquickjs::Result<Value<'js>> {
  let Some(obj) = view.as_object() else {
    return Err(throw_named(&ctx, "TypeError", "expected an integer TypedArray"));
  };
  let ctor_name: String = obj
    .get::<_, Object<'js>>("constructor")
    .and_then(|c| c.get::<_, String>("name"))
    .unwrap_or_default();
  // Web spec: integer typed arrays only — Float arrays and DataView
  // reject with TypeMismatchError.
  let allowed = matches!(
    ctor_name.as_str(),
    "Int8Array" | "Uint8Array" | "Uint8ClampedArray" | "Int16Array" | "Uint16Array" | "Int32Array" | "Uint32Array"
  );
  if !allowed {
    return Err(throw_named(
      &ctx,
      "TypeMismatchError",
      format!("getRandomValues does not accept {ctor_name}"),
    ));
  }
  let len: usize = obj.get("byteLength")?;
  if len > MAX_RANDOM_BYTES {
    return Err(throw_named(
      &ctx,
      "QuotaExceededError",
      format!("getRandomValues byte length {len} exceeds {MAX_RANDOM_BYTES}"),
    ));
  }
  let offset: usize = obj.get("byteOffset")?;
  // Generate into an owned buffer FIRST so nothing (allocation,
  // user-visible callback, fallible syscall) happens between fetching
  // the raw backing pointer and writing through it.
  let mut bytes = vec![0u8; len];
  getrandom::fill(&mut bytes)
    .map_err(|e| rquickjs::Error::new_from_js_message("crypto", "getRandomValues", e.to_string()))?;
  let ab: ArrayBuffer<'js> = obj.get("buffer")?;
  let raw = ab
    .as_raw()
    .ok_or_else(|| throw_named(&ctx, "TypeError", "detached ArrayBuffer"))?;
  if offset.checked_add(len).is_none_or(|end| end > raw.len) {
    return Err(throw_named(&ctx, "TypeError", "view out of bounds"));
  }
  debug_assert!(offset + len <= raw.len, "bounds re-checked above");
  // SAFETY: the unsafe window is a single memcpy. INVARIANT a future
  // refactor must preserve: `raw` was fetched on this line group with
  // no QuickJS API call (and thus no heap movement / detach) between
  // `as_raw` and the copy; the VM is single-threaded so no JS runs
  // concurrently; bounds were checked against `raw.len` just above.
  #[allow(unsafe_code)]
  unsafe {
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), raw.ptr.as_ptr().add(offset), len);
  }
  Ok(view)
}

async fn subtle_digest<'js>(
  ctx: Ctx<'js>,
  algorithm: Value<'js>,
  data: Value<'js>,
) -> rquickjs::Result<ArrayBuffer<'js>> {
  let algo = parse_hash(&ctx, &algorithm)?;
  let bytes = buffer_source_bytes(&ctx, &data)?;
  ArrayBuffer::new(ctx, algo.digest(&bytes))
}

/// `subtle.importKey('raw', keyData, { name: 'HMAC', hash }, extractable, usages)`.
async fn subtle_import_key<'js>(
  ctx: Ctx<'js>,
  format: String,
  key_data: Value<'js>,
  algorithm: Value<'js>,
  extractable: bool,
  usages: Vec<String>,
) -> rquickjs::Result<rquickjs::Class<'js, CryptoKeyJs>> {
  if format != "raw" {
    return Err(throw_named(
      &ctx,
      "NotSupportedError",
      format!("importKey format {format:?} not supported (only 'raw')"),
    ));
  }
  let Some(obj) = algorithm.as_object() else {
    return Err(throw_named(
      &ctx,
      "TypeError",
      "algorithm must be { name: 'HMAC', hash }",
    ));
  };
  let name: String = obj.get("name")?;
  if !name.eq_ignore_ascii_case("HMAC") {
    return Err(throw_named(
      &ctx,
      "NotSupportedError",
      format!("importKey algorithm {name:?} not supported (only HMAC)"),
    ));
  }
  let hash_val: Value<'js> = obj.get("hash")?;
  let hash = parse_hash(&ctx, &hash_val)?;
  let key = buffer_source_bytes(&ctx, &key_data)?;
  rquickjs::Class::instance(
    ctx,
    CryptoKeyJs {
      key,
      hash,
      extractable,
      usages,
    },
  )
}

async fn subtle_sign<'js>(
  ctx: Ctx<'js>,
  _algorithm: Value<'js>,
  key: rquickjs::Class<'js, CryptoKeyJs>,
  data: Value<'js>,
) -> rquickjs::Result<ArrayBuffer<'js>> {
  let bytes = buffer_source_bytes(&ctx, &data)?;
  let k = key.borrow();
  let out = k.hash.hmac(&k.key, &bytes);
  drop(k);
  ArrayBuffer::new(ctx, out)
}

async fn subtle_verify<'js>(
  ctx: Ctx<'js>,
  _algorithm: Value<'js>,
  key: rquickjs::Class<'js, CryptoKeyJs>,
  signature: Value<'js>,
  data: Value<'js>,
) -> rquickjs::Result<bool> {
  let sig = buffer_source_bytes(&ctx, &signature)?;
  let bytes = buffer_source_bytes(&ctx, &data)?;
  let k = key.borrow();
  let expected = k.hash.hmac(&k.key, &bytes);
  // Constant-time compare (the `subtle` crate ships with `hmac`).
  Ok(subtle::ConstantTimeEq::ct_eq(expected.as_slice(), sig.as_slice()).into())
}

fn reject_unsupported<'js>(
  ctx: Ctx<'js>,
  op: &'static str,
  _args: rquickjs::function::Rest<Value<'js>>,
) -> rquickjs::Result<Value<'js>> {
  Err(throw_named(
    &ctx,
    "NotSupportedError",
    format!("subtle.{op} is not implemented"),
  ))
}

/// Install the `crypto` global. Idempotent per context.
pub fn install(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  rquickjs::Class::<CryptoKeyJs>::define(&ctx.globals())?;

  let crypto = Object::new(ctx.clone())?;
  crypto.set("randomUUID", Func::from(random_uuid))?;
  crypto.set("getRandomValues", Func::from(get_random_values))?;

  let subtle = Object::new(ctx.clone())?;
  subtle.set("digest", Func::from(Async(subtle_digest)))?;
  subtle.set("importKey", Func::from(Async(subtle_import_key)))?;
  subtle.set("sign", Func::from(Async(subtle_sign)))?;
  subtle.set("verify", Func::from(Async(subtle_verify)))?;
  for op in [
    "encrypt",
    "decrypt",
    "deriveBits",
    "deriveKey",
    "exportKey",
    "generateKey",
    "wrapKey",
    "unwrapKey",
  ] {
    let f = rquickjs::Function::new(ctx.clone(), move |fctx, args| reject_unsupported(fctx, op, args))?;
    subtle.set(op, f)?;
  }
  crypto.set("subtle", subtle)?;

  ctx.globals().set("crypto", crypto)?;
  crate::bindings::runtime::mirror_global(ctx, "crypto")?;
  Ok(())
}
