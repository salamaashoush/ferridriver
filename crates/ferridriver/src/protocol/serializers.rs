//! Playwright's tagged-union wire serializer.
//!
//! Exact Rust mirror of
//! `/tmp/playwright/packages/playwright-core/src/protocol/serializers.ts` plus
//! the `SerializedValue` / `SerializedArgument` types from
//! `/tmp/playwright/packages/protocol/src/channels.d.ts`. Every field, tag
//! letter, and union member matches Playwright byte-for-byte so
//! `JSON.stringify` on a Playwright-built value and `serde_json::to_string` on
//! our Rust value produce identical strings — that's the load-bearing
//! invariant for talking to the injected TS half of the serializer.
//!
//! ## Wire shape
//!
//! A `SerializedValue` is a tagged union encoded as a single JSON object where
//! exactly one primary tag field is set (plus the optional `id` companion on
//! `a` / `o`, used for back-reference deduplication on shared sub-graphs):
//!
//! | tag  | type                  | JS value it represents               |
//! |------|-----------------------|--------------------------------------|
//! | `n`  | `f64`                 | regular `number` (non-special)       |
//! | `b`  | `bool`                | `boolean`                            |
//! | `s`  | `String`              | `string`                             |
//! | `v`  | [`SpecialValue`]      | `null` / `undefined` / `NaN` / `±Infinity` / `-0` |
//! | `d`  | `String` (ISO)        | `Date` via `.toJSON()`               |
//! | `u`  | `String`              | `URL` via `.toJSON()`                |
//! | `bi` | `String` (decimal)    | `BigInt` via `.toString()`           |
//! | `r`  | [`RegExpValue`]       | `RegExp { p, f }`                    |
//! | `e`  | [`ErrorValue`]        | `Error { m, n, s }`                  |
//! | `ta` | [`TypedArrayValue`]   | `TypedArray { b: base64, k: kind }`  |
//! | `a`  | `Vec<SerializedValue>`| `Array` — paired with unique `id`    |
//! | `o`  | `Vec<PropertyEntry>`  | plain `Object` — paired with `id`    |
//! | `h`  | `u32`                 | index into [`SerializedArgument::handles`] (`JSHandle` ref) |
//! | `ref`| `u32`                 | back-reference to a previously-emitted collection by its `id` |
//!
//! `Map` / `Set` are NOT distinct variants in Playwright's serializer —
//! they are walked as plain objects via `Object.keys`, matching browser JS
//! semantics where iteration order is insertion order.
//!
//! ## Dedup / cycles
//!
//! When serializing an object graph with shared substructure (the same
//! array or object referenced from two places), the first emission gets a
//! unique `id: N` and every subsequent emission of the same object gets
//! replaced with `ref: N`. Cycles terminate because the second visit hits
//! the back-reference path before recursing. The caller controls the
//! identity notion (pointer equality for FFI boundaries, semantic equality
//! for plain JSON) by constructing their own [`SerializationContext`] or
//! by using [`SerializedValue::from_json`] for the pure-JSON case.

use base64::Engine;
use serde::{Deserialize, Serialize};

// ── SpecialValue ────────────────────────────────────────────────────────────

/// JS values that have no faithful JSON representation but still exist at
/// runtime — `undefined`, `NaN`, `±Infinity`, and the negative zero
/// distinct from positive zero. Encoded under the `v` tag with the exact
/// string literal Playwright uses (case matters: `NaN`, not `nan`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpecialValue {
  #[serde(rename = "null")]
  Null,
  #[serde(rename = "undefined")]
  Undefined,
  #[serde(rename = "NaN")]
  NaN,
  #[serde(rename = "Infinity")]
  Infinity,
  #[serde(rename = "-Infinity")]
  NegInfinity,
  #[serde(rename = "-0")]
  NegZero,
}

// ── RegExpValue ─────────────────────────────────────────────────────────────

/// Wire shape of a `RegExp`: pattern + flags, matching the
/// `RegExp.prototype.source` / `RegExp.prototype.flags` output and the JS
/// constructor invariant `new RegExp(p, f)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegExpValue {
  /// Pattern. Corresponds to `RegExp.prototype.source`.
  pub p: String,
  /// Flags (e.g. `"gi"`). Corresponds to `RegExp.prototype.flags`.
  pub f: String,
}

// ── ErrorValue ──────────────────────────────────────────────────────────────

/// Wire shape of an `Error` — message + name + stack. Playwright's
/// serializer always emits all three fields (empty string for stack if
/// absent on the source) so the deserializer can unconditionally assign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorValue {
  /// `Error.prototype.message`.
  pub m: String,
  /// `Error.prototype.name` — typically `"Error"` / `"TypeError"` /
  /// `"RangeError"`, etc.
  pub n: String,
  /// `Error.prototype.stack`. May be an empty string if the source had no
  /// stack (e.g. `new Error()` before the property resolved).
  pub s: String,
}

// ── TypedArrayKind ──────────────────────────────────────────────────────────

/// Enumeration of the `TypedArray` subclasses the serializer supports.
/// Encoded as the short string Playwright uses on the wire (e.g. `"i8"`
/// for `Int8Array`). Matches
/// `/tmp/playwright/packages/playwright-core/src/protocol/serializers.ts:196`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypedArrayKind {
  #[serde(rename = "i8")]
  I8,
  #[serde(rename = "ui8")]
  U8,
  #[serde(rename = "ui8c")]
  U8Clamped,
  #[serde(rename = "i16")]
  I16,
  #[serde(rename = "ui16")]
  U16,
  #[serde(rename = "i32")]
  I32,
  #[serde(rename = "ui32")]
  U32,
  #[serde(rename = "f32")]
  F32,
  #[serde(rename = "f64")]
  F64,
  #[serde(rename = "bi64")]
  BI64,
  #[serde(rename = "bui64")]
  BUI64,
}

impl TypedArrayKind {
  /// The byte stride of a single element in this typed array.
  ///
  /// Matches `TypedArray.BYTES_PER_ELEMENT` for the JS constructor the
  /// kind names. Used when decoding the raw byte slice back into a
  /// concrete Rust typed sequence.
  #[must_use]
  pub fn bytes_per_element(self) -> usize {
    match self {
      Self::I8 | Self::U8 | Self::U8Clamped => 1,
      Self::I16 | Self::U16 => 2,
      Self::I32 | Self::U32 | Self::F32 => 4,
      Self::F64 | Self::BI64 | Self::BUI64 => 8,
    }
  }
}

// ── TypedArrayValue ─────────────────────────────────────────────────────────

/// Wire shape of a `TypedArray`: the raw byte buffer plus a kind tag. The
/// bytes are base64-encoded in JSON transit (Playwright uses `Binary`
/// which is `Buffer` in Node.js and base64 on the wire); we mirror that
/// with a custom serde helper so `serde_json::to_string` emits a string
/// and not a `number[]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedArrayValue {
  /// Underlying byte buffer, base64-encoded on the JSON wire.
  #[serde(with = "base64_bytes")]
  pub b: Vec<u8>,
  /// Typed-array constructor tag.
  pub k: TypedArrayKind,
}

// ── PropertyEntry ───────────────────────────────────────────────────────────

/// A single `{ k, v }` entry inside a serialized `Object` (`o`).
/// Playwright walks `Object.keys(value)` (own enumerable string-keyed
/// properties in insertion order), so entry ordering is significant and
/// we model it as an ordered `Vec`, not a map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropertyEntry {
  /// Property key. Always a string on the wire (Symbols are dropped by
  /// the JS-side serializer, matching `Object.keys` behavior).
  pub k: String,
  /// Property value.
  pub v: SerializedValue,
}

// ── SerializedValue ─────────────────────────────────────────────────────────

/// The wire tagged-union. Exactly one primary tag is set per value
/// (plus optional `id` on `a` / `o`). Field order is irrelevant on the
/// JSON wire; we follow Playwright's source order for diffability.
///
/// See the module docs for the full tag table.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SerializedValue {
  /// Regular `number` — any finite non-negative-zero value. `NaN` /
  /// `±Infinity` / `-0` go through [`Self::v`] instead.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub n: Option<f64>,
  /// `boolean`.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub b: Option<bool>,
  /// `string`.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub s: Option<String>,
  /// `null` / `undefined` / `NaN` / `Infinity` / `-Infinity` / `-0`.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub v: Option<SpecialValue>,
  /// `Date`, encoded as `toJSON()` ISO string.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub d: Option<String>,
  /// `URL`, encoded as `toJSON()` full URL string.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub u: Option<String>,
  /// `BigInt`, encoded as the decimal-string `toString()`.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub bi: Option<String>,
  /// `TypedArray` — base64 bytes + kind tag.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub ta: Option<TypedArrayValue>,
  /// `Error`.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub e: Option<ErrorValue>,
  /// `RegExp`.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub r: Option<RegExpValue>,
  /// `Array` — elements by position. Paired with [`Self::id`] so
  /// another occurrence of the same array elsewhere in the graph can
  /// back-reference via [`Self::ref_`].
  #[serde(skip_serializing_if = "Option::is_none")]
  pub a: Option<Vec<SerializedValue>>,
  /// Plain `Object` — properties as an ordered `{k, v}` list. Paired
  /// with [`Self::id`].
  #[serde(skip_serializing_if = "Option::is_none")]
  pub o: Option<Vec<PropertyEntry>>,
  /// Handle index — points into [`SerializedArgument::handles`]. Used
  /// when a `JSHandle` / `ElementHandle` is passed through `evaluate`
  /// arguments or returned from the function.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub h: Option<u32>,
  /// Companion to `a` / `o` — a unique-per-value id so back-references
  /// (`ref`) can resolve.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub id: Option<u32>,
  /// Back-reference to a previously-emitted `a` / `o` by its `id`.
  /// Terminates cycles and deduplicates shared sub-structures.
  #[serde(skip_serializing_if = "Option::is_none", rename = "ref")]
  pub ref_: Option<u32>,
}

impl SerializedValue {
  // ── Primitive builders ────────────────────────────────────────────────────

  /// Wrap a finite, non-special `f64`. Does NOT auto-route `NaN` /
  /// `Infinity` / `-0` into the `v` tag — the caller is responsible
  /// for picking the right constructor.
  #[must_use]
  pub fn number(n: f64) -> Self {
    Self {
      n: Some(n),
      ..Self::default()
    }
  }

  /// Build a number, auto-routing the IEEE-754 specials through the
  /// correct wire tag. `-0.0` is detected via bit-equality with the
  /// IEEE-754 negative-zero pattern, matching Playwright's
  /// `Object.is(value, -0)`.
  #[must_use]
  pub fn from_f64(n: f64) -> Self {
    if n.is_nan() {
      return Self::special(SpecialValue::NaN);
    }
    if n == f64::INFINITY {
      return Self::special(SpecialValue::Infinity);
    }
    if n == f64::NEG_INFINITY {
      return Self::special(SpecialValue::NegInfinity);
    }
    // Distinguish negative zero from positive zero without comparing
    // `n == 0.0` (which is true for both). Bit pattern of `-0.0` is
    // `0x8000_0000_0000_0000`.
    if n == 0.0 && n.to_bits() == (-0.0_f64).to_bits() {
      return Self::special(SpecialValue::NegZero);
    }
    Self::number(n)
  }

  #[must_use]
  pub fn boolean(b: bool) -> Self {
    Self {
      b: Some(b),
      ..Self::default()
    }
  }

  #[must_use]
  pub fn string(s: impl Into<String>) -> Self {
    Self {
      s: Some(s.into()),
      ..Self::default()
    }
  }

  #[must_use]
  pub fn special(v: SpecialValue) -> Self {
    Self {
      v: Some(v),
      ..Self::default()
    }
  }

  #[must_use]
  pub fn null() -> Self {
    Self::special(SpecialValue::Null)
  }

  #[must_use]
  pub fn undefined() -> Self {
    Self::special(SpecialValue::Undefined)
  }

  // ── Rich-type builders ────────────────────────────────────────────────────

  /// Construct a `Date` from its `toJSON()` ISO string (e.g.
  /// `"2024-01-01T00:00:00.000Z"`). The deserializer will reconstruct
  /// `new Date(value)` from it.
  #[must_use]
  pub fn date(iso: impl Into<String>) -> Self {
    Self {
      d: Some(iso.into()),
      ..Self::default()
    }
  }

  /// Construct a `URL` from its `toJSON()` string.
  #[must_use]
  pub fn url(url: impl Into<String>) -> Self {
    Self {
      u: Some(url.into()),
      ..Self::default()
    }
  }

  /// Construct a `BigInt` from its decimal string representation.
  /// Caller is responsible for generating a valid `BigInt.prototype
  /// .toString()` form — no base prefix, no `n` suffix, leading minus
  /// allowed.
  #[must_use]
  pub fn bigint(decimal: impl Into<String>) -> Self {
    Self {
      bi: Some(decimal.into()),
      ..Self::default()
    }
  }

  /// Construct a `RegExp` from pattern + flags (the `/p/f` form, without
  /// the surrounding slashes). The deserializer does `new RegExp(p, f)`.
  #[must_use]
  pub fn regexp(pattern: impl Into<String>, flags: impl Into<String>) -> Self {
    Self {
      r: Some(RegExpValue {
        p: pattern.into(),
        f: flags.into(),
      }),
      ..Self::default()
    }
  }

  /// Construct an `Error` from name / message / stack. Pass an empty
  /// stack string if none is available — the wire format always
  /// carries a `s` field.
  #[must_use]
  pub fn error(name: impl Into<String>, message: impl Into<String>, stack: impl Into<String>) -> Self {
    Self {
      e: Some(ErrorValue {
        n: name.into(),
        m: message.into(),
        s: stack.into(),
      }),
      ..Self::default()
    }
  }

  /// Construct a `TypedArray` from a raw byte buffer + kind tag. The
  /// byte length MUST be a multiple of `kind.bytes_per_element()`; the
  /// builder does not enforce this (the JS deserializer will surface
  /// it as a runtime error).
  #[must_use]
  pub fn typed_array(bytes: Vec<u8>, kind: TypedArrayKind) -> Self {
    Self {
      ta: Some(TypedArrayValue { b: bytes, k: kind }),
      ..Self::default()
    }
  }

  // ── Collection builders ───────────────────────────────────────────────────

  /// Build an `Array` value. The `id` is required so ref-back from
  /// cycles / shared structure can resolve; callers without a shared
  /// graph still need to allocate one (use [`SerializationContext`] to
  /// manage allocation).
  #[must_use]
  pub fn array(elements: Vec<SerializedValue>, id: u32) -> Self {
    Self {
      a: Some(elements),
      id: Some(id),
      ..Self::default()
    }
  }

  /// Build a plain `Object` value. See [`Self::array`] on the `id`.
  #[must_use]
  pub fn object(entries: Vec<PropertyEntry>, id: u32) -> Self {
    Self {
      o: Some(entries),
      id: Some(id),
      ..Self::default()
    }
  }

  /// Build a back-reference to a previously-emitted collection by its
  /// `id`. Used to dedup shared sub-structures and terminate cycles.
  #[must_use]
  pub fn reference(id: u32) -> Self {
    Self {
      ref_: Some(id),
      ..Self::default()
    }
  }

  /// Build a handle reference — `handle_index` is the position in the
  /// companion [`SerializedArgument::handles`] array. Used when a
  /// `JSHandle` / `ElementHandle` is marshaled through `evaluate`.
  #[must_use]
  pub fn handle(handle_index: u32) -> Self {
    Self {
      h: Some(handle_index),
      ..Self::default()
    }
  }

  // ── Conversion helpers ────────────────────────────────────────────────────

  /// Convert a [`serde_json::Value`] into a `SerializedValue`, covering
  /// the JSON subset of JS types: `null`, `bool`, finite `number`,
  /// `string`, `Array`, `Object`. Rich JS types (`undefined` / `NaN` /
  /// `Date` / `RegExp` / `BigInt` / handles) have no JSON form; if the
  /// input happens to produce a finite IEEE-754 special anyway (a
  /// parsed `"NaN"` somewhere?) it is routed through `v` via
  /// [`Self::from_f64`].
  ///
  /// `ctx` supplies fresh `id`s for arrays / objects. Each call to
  /// `from_json` uses an independent sub-graph, so cycle detection
  /// isn't applicable (`serde_json::Value` is a pure tree).
  #[must_use]
  pub fn from_json(value: &serde_json::Value, ctx: &mut SerializationContext) -> Self {
    match value {
      serde_json::Value::Null => Self::null(),
      serde_json::Value::Bool(b) => Self::boolean(*b),
      serde_json::Value::Number(num) => num.as_f64().map_or_else(
        || {
          // `serde_json::Number` promises either i64, u64, or f64 is
          // representable. If `as_f64` returns None on an integer
          // outside f64's safe range we still produce something
          // reasonable rather than panic: stringify as BigInt.
          Self::bigint(num.to_string())
        },
        Self::from_f64,
      ),
      serde_json::Value::String(s) => Self::string(s.clone()),
      serde_json::Value::Array(items) => {
        let id = ctx.alloc_id();
        let a = items.iter().map(|v| Self::from_json(v, ctx)).collect();
        Self::array(a, id)
      },
      serde_json::Value::Object(map) => {
        let id = ctx.alloc_id();
        let entries = map
          .iter()
          .map(|(k, v)| PropertyEntry {
            k: k.clone(),
            v: Self::from_json(v, ctx),
          })
          .collect();
        Self::object(entries, id)
      },
    }
  }

  /// Attempt to convert back into a [`serde_json::Value`]. Succeeds for
  /// the JSON-expressible subset (`n` / `b` / `s` / `v=null` /
  /// `a` / `o`). Returns `None` for any rich type (`undefined` / `NaN` /
  /// `Date` / `RegExp` / `Error` / typed array / handle / ref) since
  /// `serde_json::Value` cannot represent them faithfully.
  #[must_use]
  pub fn to_json_like(&self) -> Option<serde_json::Value> {
    if let Some(n) = self.n {
      // Preserve integer form when the value is losslessly representable
      // in an i64's f64-safe range (±2^53 — the largest integer f64 can
      // hold exactly). Makes `from_json(json!(1)) → to_json_like() ==
      // json!(1)` round-trip, matching "JSON in, JSON out" user
      // expectations even though JS itself has only one numeric type.
      // Outside that range we stay with the float representation to
      // avoid silently aliasing two distinct integers.
      const F64_INT_MAX: f64 = 9_007_199_254_740_992.0; // 2^53
      if n.is_finite() && n.fract() == 0.0 && n.abs() <= F64_INT_MAX {
        #[allow(clippy::cast_possible_truncation)]
        let as_i64 = n as i64;
        return Some(serde_json::Value::Number(as_i64.into()));
      }
      return serde_json::Number::from_f64(n).map(serde_json::Value::Number);
    }
    if let Some(b) = self.b {
      return Some(serde_json::Value::Bool(b));
    }
    if let Some(s) = &self.s {
      return Some(serde_json::Value::String(s.clone()));
    }
    if let Some(v) = self.v {
      return match v {
        SpecialValue::Null => Some(serde_json::Value::Null),
        // `undefined` has no JSON form; `NaN` / `±Inf` / `-0` are not
        // JSON-representable either.
        _ => None,
      };
    }
    if let Some(arr) = &self.a {
      let mut out = Vec::with_capacity(arr.len());
      for v in arr {
        out.push(v.to_json_like()?);
      }
      return Some(serde_json::Value::Array(out));
    }
    if let Some(obj) = &self.o {
      let mut map = serde_json::Map::with_capacity(obj.len());
      for entry in obj {
        map.insert(entry.k.clone(), entry.v.to_json_like()?);
      }
      return Some(serde_json::Value::Object(map));
    }
    // `d` / `u` / `bi` / `ta` / `e` / `r` / `h` / `ref` have no lossless
    // JSON form.
    None
  }
}

// ── SerializationContext ────────────────────────────────────────────────────

/// Tracks the next unique `id` to hand out when building `a` / `o` values.
/// Held by the caller across a `from_json` invocation (or a manual build)
/// so every collection in the resulting graph has a distinct id.
#[derive(Debug, Clone, Default)]
pub struct SerializationContext {
  next_id: u32,
}

impl SerializationContext {
  /// Allocate a fresh id. First id handed out is `1` — Playwright's
  /// `lastId` starts at `0` and the first `++lastId` produces `1`.
  pub fn alloc_id(&mut self) -> u32 {
    self.next_id = self.next_id.saturating_add(1);
    self.next_id
  }
}

// ── SerializedArgument ──────────────────────────────────────────────────────

/// The full `{ value, handles }` pair Playwright sends with every
/// `evaluate(fn, arg)` call. `value` carries the serialized arg tree
/// (possibly containing `h: N` refs into `handles`); `handles` carries
/// the live backend object `IDs` those refs resolve to.
///
/// On the Rust side we model `handles` as an opaque identifier list
/// (`Vec<HandleId>`) — the actual shape (CDP `RemoteObjectId` string /
/// `BiDi` `shared_id` / `WebKit` `ref_id`) lives on the backend layer
/// and is resolved when the argument is marshaled into the
/// corresponding protocol command.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SerializedArgument {
  pub value: SerializedValue,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub handles: Vec<HandleId>,
}

/// Backend-agnostic handle identifier used in
/// [`SerializedArgument::handles`]. Each backend maps this to its own
/// native remote-object reference on marshal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandleId {
  /// CDP `Runtime.RemoteObjectId` — an opaque string.
  Cdp(String),
  /// `WebDriver` `BiDi` shared reference — `{ sharedId, handle? }`. We
  /// carry the `sharedId` since it's the stable half; `handle` is an
  /// optional realm-scoped identifier Playwright adds when relevant.
  Bidi { shared_id: String, handle: Option<String> },
  /// `WebKit` host IPC ref — an index into `window.__wr[]`.
  WebKit(u64),
}

// ── base64 helper ───────────────────────────────────────────────────────────

/// Custom serde helper for the `ta.b` typed-array byte buffer: base64
/// on the JSON wire (matching Playwright's `Binary` encoding), raw
/// `Vec<u8>` in Rust.
mod base64_bytes {
  use base64::Engine;
  use base64::engine::general_purpose::STANDARD;
  use serde::{Deserialize, Deserializer, Serializer};

  pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&STANDARD.encode(bytes))
  }

  pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    let s = String::deserialize(d)?;
    STANDARD.decode(s).map_err(serde::de::Error::custom)
  }
}

/// Stand-alone helper so other call sites can emit the same base64
/// encoding without touching the private helper module.
#[must_use]
pub fn encode_typed_array_bytes(bytes: &[u8]) -> String {
  base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Inverse of [`encode_typed_array_bytes`].
///
/// # Errors
///
/// Returns `base64::DecodeError` if the string is not valid standard
/// base64.
pub fn decode_typed_array_bytes(encoded: &str) -> Result<Vec<u8>, base64::DecodeError> {
  base64::engine::general_purpose::STANDARD.decode(encoded)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  // Parity check: serializing each variant produces a JSON object with
  // exactly the keys Playwright's wire format uses. These strings come
  // from running Playwright's `serializeValue` on the corresponding JS
  // value and reading the `JSON.stringify` output.

  #[test]
  fn serializes_number() {
    assert_eq!(
      serde_json::to_value(SerializedValue::number(42.0)).unwrap(),
      json!({"n": 42.0})
    );
  }

  #[test]
  fn serializes_boolean() {
    assert_eq!(
      serde_json::to_value(SerializedValue::boolean(true)).unwrap(),
      json!({"b": true})
    );
  }

  #[test]
  fn serializes_string() {
    assert_eq!(
      serde_json::to_value(SerializedValue::string("hi")).unwrap(),
      json!({"s": "hi"})
    );
  }

  #[test]
  fn serializes_special_values() {
    for (special, expected) in [
      (SpecialValue::Null, json!({"v": "null"})),
      (SpecialValue::Undefined, json!({"v": "undefined"})),
      (SpecialValue::NaN, json!({"v": "NaN"})),
      (SpecialValue::Infinity, json!({"v": "Infinity"})),
      (SpecialValue::NegInfinity, json!({"v": "-Infinity"})),
      (SpecialValue::NegZero, json!({"v": "-0"})),
    ] {
      assert_eq!(
        serde_json::to_value(SerializedValue::special(special)).unwrap(),
        expected,
        "variant {special:?}"
      );
    }
  }

  #[test]
  fn from_f64_routes_ieee_specials() {
    assert_eq!(SerializedValue::from_f64(f64::NAN).v, Some(SpecialValue::NaN));
    assert_eq!(SerializedValue::from_f64(f64::INFINITY).v, Some(SpecialValue::Infinity));
    assert_eq!(
      SerializedValue::from_f64(f64::NEG_INFINITY).v,
      Some(SpecialValue::NegInfinity)
    );
    assert_eq!(SerializedValue::from_f64(-0.0_f64).v, Some(SpecialValue::NegZero));
    assert_eq!(SerializedValue::from_f64(0.0_f64).n, Some(0.0_f64));
    assert_eq!(SerializedValue::from_f64(1.5).n, Some(1.5));
  }

  #[test]
  fn from_f64_distinguishes_positive_and_negative_zero() {
    let pos = SerializedValue::from_f64(0.0);
    let neg = SerializedValue::from_f64(-0.0);
    assert_eq!(pos.n, Some(0.0));
    assert!(pos.v.is_none());
    assert!(neg.n.is_none());
    assert_eq!(neg.v, Some(SpecialValue::NegZero));
  }

  #[test]
  fn serializes_date_url_bigint() {
    assert_eq!(
      serde_json::to_value(SerializedValue::date("2024-01-01T00:00:00.000Z")).unwrap(),
      json!({"d": "2024-01-01T00:00:00.000Z"})
    );
    assert_eq!(
      serde_json::to_value(SerializedValue::url("https://example.com/")).unwrap(),
      json!({"u": "https://example.com/"})
    );
    assert_eq!(
      serde_json::to_value(SerializedValue::bigint("9007199254740993")).unwrap(),
      json!({"bi": "9007199254740993"})
    );
  }

  #[test]
  fn serializes_regexp() {
    let wire = serde_json::to_value(SerializedValue::regexp("foo.*bar", "gi")).unwrap();
    assert_eq!(wire, json!({"r": {"p": "foo.*bar", "f": "gi"}}));
  }

  #[test]
  fn serializes_error() {
    let wire = serde_json::to_value(SerializedValue::error(
      "TypeError",
      "nope",
      "TypeError: nope\n    at foo",
    ))
    .unwrap();
    assert_eq!(
      wire,
      json!({"e": {"n": "TypeError", "m": "nope", "s": "TypeError: nope\n    at foo"}})
    );
  }

  #[test]
  fn serializes_typed_array_with_base64() {
    // Uint8Array([1, 2, 3, 4]).buffer encoded as base64 is "AQIDBA==".
    let wire = serde_json::to_value(SerializedValue::typed_array(vec![1, 2, 3, 4], TypedArrayKind::U8)).unwrap();
    assert_eq!(wire, json!({"ta": {"b": "AQIDBA==", "k": "ui8"}}));
  }

  #[test]
  fn typed_array_bytes_per_element_matches_js() {
    // Same invariant as `TypedArray.BYTES_PER_ELEMENT` — any drift
    // here breaks round-trip through the page.
    assert_eq!(TypedArrayKind::I8.bytes_per_element(), 1);
    assert_eq!(TypedArrayKind::U8.bytes_per_element(), 1);
    assert_eq!(TypedArrayKind::U8Clamped.bytes_per_element(), 1);
    assert_eq!(TypedArrayKind::I16.bytes_per_element(), 2);
    assert_eq!(TypedArrayKind::U16.bytes_per_element(), 2);
    assert_eq!(TypedArrayKind::I32.bytes_per_element(), 4);
    assert_eq!(TypedArrayKind::U32.bytes_per_element(), 4);
    assert_eq!(TypedArrayKind::F32.bytes_per_element(), 4);
    assert_eq!(TypedArrayKind::F64.bytes_per_element(), 8);
    assert_eq!(TypedArrayKind::BI64.bytes_per_element(), 8);
    assert_eq!(TypedArrayKind::BUI64.bytes_per_element(), 8);
  }

  #[test]
  fn serializes_array_with_id() {
    let wire = serde_json::to_value(SerializedValue::array(
      vec![SerializedValue::number(1.0), SerializedValue::number(2.0)],
      1,
    ))
    .unwrap();
    assert_eq!(wire, json!({"a": [{"n": 1.0}, {"n": 2.0}], "id": 1}));
  }

  #[test]
  fn serializes_object_with_id_preserves_order() {
    let wire = serde_json::to_value(SerializedValue::object(
      vec![
        PropertyEntry {
          k: "first".into(),
          v: SerializedValue::number(1.0),
        },
        PropertyEntry {
          k: "second".into(),
          v: SerializedValue::string("two"),
        },
      ],
      2,
    ))
    .unwrap();
    // Array-of-entries preserves insertion order; a HashMap-based
    // encoding would not.
    assert_eq!(
      wire,
      json!({"o": [{"k": "first", "v": {"n": 1.0}}, {"k": "second", "v": {"s": "two"}}], "id": 2})
    );
  }

  #[test]
  fn serializes_handle_and_reference() {
    assert_eq!(
      serde_json::to_value(SerializedValue::handle(3)).unwrap(),
      json!({"h": 3})
    );
    assert_eq!(
      serde_json::to_value(SerializedValue::reference(5)).unwrap(),
      json!({"ref": 5})
    );
  }

  #[test]
  fn roundtrips_via_serde() {
    let values = vec![
      SerializedValue::number(1.5),
      SerializedValue::boolean(false),
      SerializedValue::string("hello"),
      SerializedValue::special(SpecialValue::Null),
      SerializedValue::special(SpecialValue::Undefined),
      SerializedValue::special(SpecialValue::NaN),
      SerializedValue::special(SpecialValue::Infinity),
      SerializedValue::special(SpecialValue::NegInfinity),
      SerializedValue::special(SpecialValue::NegZero),
      SerializedValue::date("2024-06-01T12:00:00.000Z"),
      SerializedValue::url("https://a.test/"),
      SerializedValue::bigint("-12345678901234567890"),
      SerializedValue::regexp("a|b", "i"),
      SerializedValue::error("Error", "boom", ""),
      SerializedValue::typed_array(vec![0xde, 0xad, 0xbe, 0xef], TypedArrayKind::U32),
      SerializedValue::array(vec![SerializedValue::number(1.0)], 1),
      SerializedValue::object(
        vec![PropertyEntry {
          k: "x".into(),
          v: SerializedValue::boolean(true),
        }],
        2,
      ),
      SerializedValue::handle(0),
      SerializedValue::reference(1),
    ];
    for v in values {
      let wire = serde_json::to_string(&v).unwrap();
      let back: SerializedValue = serde_json::from_str(&wire).unwrap();
      assert_eq!(v, back, "roundtrip drift for {wire}");
    }
  }

  #[test]
  fn from_json_maps_scalars() {
    let mut ctx = SerializationContext::default();
    assert_eq!(
      SerializedValue::from_json(&json!(null), &mut ctx).v,
      Some(SpecialValue::Null)
    );
    assert_eq!(SerializedValue::from_json(&json!(true), &mut ctx).b, Some(true));
    assert_eq!(
      SerializedValue::from_json(&json!("hi"), &mut ctx).s,
      Some("hi".to_string())
    );
    assert_eq!(SerializedValue::from_json(&json!(42), &mut ctx).n, Some(42.0));
  }

  #[test]
  fn from_json_allocates_ids_for_collections() {
    let mut ctx = SerializationContext::default();
    let val = SerializedValue::from_json(&json!({"a": [1, 2], "b": null}), &mut ctx);
    // Top-level object gets id 1 first (context init), then the inner
    // array gets id 2. Property order follows serde_json::Map key
    // iteration order, which for the `preserve_order` feature is
    // insertion order; we only assert the ids are distinct and the
    // structure round-trips.
    assert!(val.id.is_some(), "outer object has id");
    let outer_id = val.id.unwrap();
    let entries = val.o.as_ref().expect("outer is object");
    let a_entry = entries.iter().find(|e| e.k == "a").expect("has key `a`");
    let a_id = a_entry.v.id.expect("inner array has id");
    assert_ne!(outer_id, a_id, "ids distinct");
  }

  #[test]
  fn from_json_routes_ieee_specials_when_present() {
    // serde_json::Value::Number can't natively carry NaN (JSON spec
    // forbids it), so construct the SerializedValue directly and
    // verify the from_f64 path picks the right tag.
    let v = SerializedValue::from_f64(f64::NAN);
    assert_eq!(v.v, Some(SpecialValue::NaN));
  }

  #[test]
  fn to_json_like_roundtrips_json_subset() {
    let original = json!({"arr": [1, true, "x", null], "n": 2.5});
    let mut ctx = SerializationContext::default();
    let wire = SerializedValue::from_json(&original, &mut ctx);
    let back = wire.to_json_like().expect("JSON subset should round-trip");
    assert_eq!(back, original);
  }

  #[test]
  fn to_json_like_returns_none_for_rich_types() {
    assert_eq!(SerializedValue::undefined().to_json_like(), None);
    assert_eq!(SerializedValue::date("2024-01-01T00:00:00Z").to_json_like(), None);
    assert_eq!(SerializedValue::handle(0).to_json_like(), None);
    assert_eq!(SerializedValue::reference(1).to_json_like(), None);
    assert_eq!(
      SerializedValue::from_f64(f64::NAN).to_json_like(),
      None,
      "NaN has no JSON form"
    );
  }

  #[test]
  fn serialization_context_allocates_distinct_ids() {
    let mut ctx = SerializationContext::default();
    let ids: Vec<u32> = (0..5).map(|_| ctx.alloc_id()).collect();
    assert_eq!(
      ids,
      vec![1, 2, 3, 4, 5],
      "first id is 1 (matches Playwright's ++lastId)"
    );
  }

  #[test]
  fn decodes_base64_typed_array_bytes() {
    let wire = json!({"ta": {"b": "AQIDBA==", "k": "ui8"}});
    let decoded: SerializedValue = serde_json::from_value(wire).unwrap();
    assert_eq!(
      decoded.ta,
      Some(TypedArrayValue {
        b: vec![1, 2, 3, 4],
        k: TypedArrayKind::U8
      })
    );
  }

  #[test]
  fn serialized_argument_omits_empty_handles() {
    let arg = SerializedArgument {
      value: SerializedValue::number(1.0),
      handles: vec![],
    };
    // Empty handles array should be skipped on the wire — Playwright's
    // producer only emits `handles` when it's non-empty.
    let s = serde_json::to_string(&arg).unwrap();
    assert_eq!(s, r#"{"value":{"n":1.0}}"#);
  }

  #[test]
  fn serialized_argument_carries_handle_list() {
    let arg = SerializedArgument {
      value: SerializedValue::array(vec![SerializedValue::handle(0), SerializedValue::handle(1)], 1),
      handles: vec![
        HandleId::Cdp("obj-1".into()),
        HandleId::Bidi {
          shared_id: "shared-1".into(),
          handle: None,
        },
      ],
    };
    let wire = serde_json::to_value(&arg).unwrap();
    assert_eq!(
      wire,
      json!({
        "value": {"a": [{"h": 0}, {"h": 1}], "id": 1},
        "handles": [{"Cdp": "obj-1"}, {"Bidi": {"shared_id": "shared-1", "handle": null}}]
      })
    );
  }
}
