//! Playwright's utility-script tagged-union wire serializer (isomorphic variant).
//!
//! Exact Rust mirror of
//! `/tmp/playwright/packages/injected/src/utilityScriptSerializers.ts`
//! (which our injected engine ships verbatim under
//! `crates/ferridriver/src/injected/isomorphic/utilityScriptSerializers.ts`).
//! Every variant, tag letter, and wire shape matches Playwright byte-for-byte
//! so the round-trip `Rust serialize → CDP Runtime.callFunctionOn CallArgument
//! → utilityScript.evaluate → parseEvaluationResultValue → user fn(arg)` lines
//! up without a translation step.
//!
//! **Why the "isomorphic" variant and not the "channels" variant.** Playwright
//! has two wire formats: the *channels* format
//! (`playwright-core/src/protocol/serializers.ts`), with every primitive
//! wrapped as `{n: 42}` / `{b: true}` / `{s: "hi"}`, used only on the
//! Playwright client↔server WebSocket RPC; and the *isomorphic* format
//! (utilityScriptSerializers.ts) where primitives pass through raw and only
//! rich types are tagged, used on the server↔page bridge over CDP / `BiDi` /
//! `WebKit`. ferridriver has no client↔server RPC — we *are* the server — so
//! the only format we need is the isomorphic one the page's injected utility
//! script parses. Sending channels-shape primitives (`{n: 42}`) through a
//! CDP `CallArgument.value` would leave the page seeing a literal
//! `{n: 42}` JS object instead of the number 42.
//!
//! ## Wire shape
//!
//! | JS value           | Wire encoding                     |
//! |--------------------|-----------------------------------|
//! | `true` / `false`   | raw `true` / `false`              |
//! | finite `number`    | raw number (`42`, `3.14`)         |
//! | `string`           | raw JSON string                   |
//! | `null`             | `{v: "null"}`                     |
//! | `undefined`        | `{v: "undefined"}`                |
//! | `NaN`              | `{v: "NaN"}`                      |
//! | `Infinity`         | `{v: "Infinity"}`                 |
//! | `-Infinity`        | `{v: "-Infinity"}`                |
//! | `-0`               | `{v: "-0"}`                       |
//! | `Date`             | `{d: "<toJSON()>"}`               |
//! | `URL`              | `{u: "<toJSON()>"}`               |
//! | `BigInt`           | `{bi: "<toString()>"}`            |
//! | `Error`            | `{e: {m, n, s}}`                  |
//! | `RegExp`           | `{r: {p, f}}`                     |
//! | `TypedArray`       | `{ta: {b: <base64>, k: <kind>}}`  |
//! | `ArrayBuffer`      | `{ab: {b: <base64>}}`             |
//! | `Array`            | `{a: [...], id: <n>}`             |
//! | `Object` (plain)   | `{o: [{k,v}, ...], id: <n>}`      |
//! | `JSHandle` ref     | `{h: <index>}`                    |
//! | shared-subgraph    | `{ref: <id>}`                     |
//!
//! `Map` / `Set` are NOT distinct variants — Playwright walks them as plain
//! objects via `Object.keys`, matching browser iteration order.
//!
//! ## Dedup / cycles
//!
//! Arrays and objects carry a unique `id`; a second visit to the same
//! JS object (pointer-equal) is encoded as `{ref: <id>}` instead, which
//! terminates cycles and deduplicates shared subgraphs. For pure-JSON input
//! (no shared structure) every collection still gets a fresh id — the
//! deserializer ignores unreferenced ids harmlessly.

use base64::Engine;
use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// ── SpecialValue ────────────────────────────────────────────────────────────

/// JS values that have no faithful JSON representation but still exist at
/// runtime — `undefined`, `NaN`, `±Infinity`, negative zero distinct from
/// positive zero, and `null` (Playwright wraps `null` the same way so the
/// deserializer can distinguish an intentional `null` from a missing field).
/// Encoded under the `v` tag with the exact string literal Playwright
/// emits (case matters: `NaN`, not `nan`).
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

/// Wire shape of a `RegExp`: pattern + flags, matching
/// `RegExp.prototype.source` / `RegExp.prototype.flags` and the JS
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
  /// stack.
  pub s: String,
}

// ── TypedArrayKind ──────────────────────────────────────────────────────────

/// Enumeration of the `TypedArray` subclasses the serializer supports.
/// Encoded as the short string Playwright uses on the wire (e.g. `"i8"`
/// for `Int8Array`). Matches
/// `/tmp/playwright/packages/injected/src/utilityScriptSerializers.ts:90`.
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
  /// kind names.
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

/// Wire shape of a `TypedArray`: the raw byte buffer plus a kind tag. Bytes
/// are base64-encoded on the JSON wire (matching Playwright's `btoa` /
/// `typedArrayToBase64` helper) and a `Vec<u8>` in Rust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedArrayValue {
  /// Underlying bytes, base64-encoded on the JSON wire.
  pub b: Vec<u8>,
  /// Typed-array constructor tag.
  pub k: TypedArrayKind,
}

#[derive(Serialize, Deserialize)]
struct TypedArrayWire<'a> {
  b: &'a str,
  k: TypedArrayKind,
}

#[derive(Deserialize)]
struct TypedArrayWireOwned {
  b: String,
  k: TypedArrayKind,
}

impl Serialize for TypedArrayValue {
  fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(&self.b);
    let wire = TypedArrayWire { b: &encoded, k: self.k };
    wire.serialize(s)
  }
}

impl<'de> Deserialize<'de> for TypedArrayValue {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    let wire = TypedArrayWireOwned::deserialize(d)?;
    let b = base64::engine::general_purpose::STANDARD
      .decode(&wire.b)
      .map_err(de::Error::custom)?;
    Ok(Self { b, k: wire.k })
  }
}

// ── ArrayBufferValue ────────────────────────────────────────────────────────

/// Wire shape of a plain `ArrayBuffer` (no kind tag; it's just bytes).
/// Serialized as `{ab: {b: <base64>}}` matching Playwright's isomorphic
/// serializer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrayBufferValue {
  /// Underlying bytes.
  pub b: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct ArrayBufferWire<'a> {
  b: &'a str,
}

#[derive(Deserialize)]
struct ArrayBufferWireOwned {
  b: String,
}

impl Serialize for ArrayBufferValue {
  fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(&self.b);
    let wire = ArrayBufferWire { b: &encoded };
    wire.serialize(s)
  }
}

impl<'de> Deserialize<'de> for ArrayBufferValue {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    let wire = ArrayBufferWireOwned::deserialize(d)?;
    let b = base64::engine::general_purpose::STANDARD
      .decode(&wire.b)
      .map_err(de::Error::custom)?;
    Ok(Self { b })
  }
}

// ── PropertyEntry ───────────────────────────────────────────────────────────

/// A single `{ k, v }` entry inside a serialized `Object` (`o`).
/// Playwright walks `Object.keys(value)` (own enumerable string-keyed
/// properties in insertion order), so entry ordering is significant and
/// we model it as an ordered `Vec`, not a map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropertyEntry {
  /// Property key. Always a string on the wire (Symbol keys are
  /// dropped by the JS serializer, matching `Object.keys`).
  pub k: String,
  /// Property value.
  pub v: SerializedValue,
}

// ── SerializedValue ─────────────────────────────────────────────────────────

/// The wire tagged union. Primitives (`bool` / `f64` / `String`) pass
/// through as raw JSON primitives; rich types ride inside a single-key
/// JSON object matching the [wire-shape](self#wire-shape) table.
#[derive(Debug, Clone, PartialEq)]
pub enum SerializedValue {
  /// `true` / `false` — raw JSON boolean.
  Bool(bool),
  /// Finite non-`±0`-special non-`NaN` non-`±Infinity` number.
  /// IEEE-754 specials go through [`Self::Special`].
  Number(f64),
  /// Regular JS `string` — raw JSON string on the wire.
  Str(String),
  /// `null` / `undefined` / `NaN` / `±Infinity` / `-0` — wire shape
  /// `{v: <tag>}`.
  Special(SpecialValue),
  /// `Date` — wire shape `{d: <iso-string>}`.
  Date(String),
  /// `URL` — wire shape `{u: <url>}`.
  Url(String),
  /// `BigInt` — wire shape `{bi: <decimal>}`.
  BigInt(String),
  /// `Error` — wire shape `{e: {m, n, s}}`.
  Error(ErrorValue),
  /// `RegExp` — wire shape `{r: {p, f}}`.
  RegExp(RegExpValue),
  /// `TypedArray` — wire shape `{ta: {b, k}}`.
  TypedArray(TypedArrayValue),
  /// `ArrayBuffer` — wire shape `{ab: {b}}`.
  ArrayBuffer(ArrayBufferValue),
  /// `Array` — wire shape `{a: [...], id}`. `id` enables back-references
  /// from cycles / shared subgraphs.
  Array { id: u32, items: Vec<SerializedValue> },
  /// Plain `Object` — wire shape `{o: [{k, v}, ...], id}`.
  Object { id: u32, entries: Vec<PropertyEntry> },
  /// `JSHandle` reference — wire shape `{h: <index>}` pointing into
  /// [`SerializedArgument::handles`].
  Handle(u32),
  /// Back-reference to a previously-emitted `a` / `o` by its `id`.
  Reference(u32),
}

impl SerializedValue {
  // ── Primitive builders ────────────────────────────────────────────────────

  #[must_use]
  pub fn boolean(b: bool) -> Self {
    Self::Bool(b)
  }

  #[must_use]
  pub fn number(n: f64) -> Self {
    Self::Number(n)
  }

  #[must_use]
  pub fn string(s: impl Into<String>) -> Self {
    Self::Str(s.into())
  }

  #[must_use]
  pub fn special(v: SpecialValue) -> Self {
    Self::Special(v)
  }

  #[must_use]
  pub fn null() -> Self {
    Self::Special(SpecialValue::Null)
  }

  #[must_use]
  pub fn undefined() -> Self {
    Self::Special(SpecialValue::Undefined)
  }

  /// Build a number, auto-routing the IEEE-754 specials through the
  /// `v` tag. `-0.0` is detected via bit-equality with the IEEE-754
  /// negative-zero pattern, matching Playwright's `Object.is(value, -0)`.
  #[must_use]
  pub fn from_f64(n: f64) -> Self {
    if n.is_nan() {
      return Self::Special(SpecialValue::NaN);
    }
    if n == f64::INFINITY {
      return Self::Special(SpecialValue::Infinity);
    }
    if n == f64::NEG_INFINITY {
      return Self::Special(SpecialValue::NegInfinity);
    }
    if n == 0.0 && n.to_bits() == (-0.0_f64).to_bits() {
      return Self::Special(SpecialValue::NegZero);
    }
    Self::Number(n)
  }

  // ── Rich-type builders ────────────────────────────────────────────────────

  #[must_use]
  pub fn date(iso: impl Into<String>) -> Self {
    Self::Date(iso.into())
  }

  #[must_use]
  pub fn url(url: impl Into<String>) -> Self {
    Self::Url(url.into())
  }

  #[must_use]
  pub fn bigint(decimal: impl Into<String>) -> Self {
    Self::BigInt(decimal.into())
  }

  #[must_use]
  pub fn regexp(pattern: impl Into<String>, flags: impl Into<String>) -> Self {
    Self::RegExp(RegExpValue {
      p: pattern.into(),
      f: flags.into(),
    })
  }

  #[must_use]
  pub fn error(name: impl Into<String>, message: impl Into<String>, stack: impl Into<String>) -> Self {
    Self::Error(ErrorValue {
      n: name.into(),
      m: message.into(),
      s: stack.into(),
    })
  }

  #[must_use]
  pub fn typed_array(bytes: Vec<u8>, kind: TypedArrayKind) -> Self {
    Self::TypedArray(TypedArrayValue { b: bytes, k: kind })
  }

  #[must_use]
  pub fn array_buffer(bytes: Vec<u8>) -> Self {
    Self::ArrayBuffer(ArrayBufferValue { b: bytes })
  }

  // ── Collection builders ───────────────────────────────────────────────────

  #[must_use]
  pub fn array(id: u32, items: Vec<SerializedValue>) -> Self {
    Self::Array { id, items }
  }

  #[must_use]
  pub fn object(id: u32, entries: Vec<PropertyEntry>) -> Self {
    Self::Object { id, entries }
  }

  #[must_use]
  pub fn reference(id: u32) -> Self {
    Self::Reference(id)
  }

  #[must_use]
  pub fn handle(handle_index: u32) -> Self {
    Self::Handle(handle_index)
  }

  // ── Conversion helpers ────────────────────────────────────────────────────

  /// Convert a [`serde_json::Value`] into a `SerializedValue`, covering
  /// the JSON subset of JS types: `null` → `{v: null}`, `bool`,
  /// finite `number`, `string`, `Array`, `Object`. `ctx` supplies
  /// fresh ids for each collection. Rich JS types (`undefined` /
  /// `NaN` / `Date` / `RegExp` / `BigInt` / handles) aren't
  /// representable in JSON; callers build those via the explicit
  /// constructors.
  #[must_use]
  pub fn from_json(value: &serde_json::Value, ctx: &mut SerializationContext) -> Self {
    match value {
      serde_json::Value::Null => Self::null(),
      serde_json::Value::Bool(b) => Self::Bool(*b),
      serde_json::Value::Number(num) => num.as_f64().map_or_else(
        // Number outside f64-safe range: fall back to BigInt string
        // so no digits are lost. serde_json can hold u64/i64 that f64
        // can't express exactly.
        || Self::BigInt(num.to_string()),
        Self::from_f64,
      ),
      serde_json::Value::String(s) => Self::Str(s.clone()),
      serde_json::Value::Array(items) => {
        let id = ctx.alloc_id();
        let wire_items = items.iter().map(|v| Self::from_json(v, ctx)).collect();
        Self::Array { id, items: wire_items }
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
        Self::Object { id, entries }
      },
    }
  }

  /// Attempt to convert into a [`serde_json::Value`]. Succeeds for the
  /// JSON-expressible subset (`Bool` / `Number` / `Str` / `Special::Null`
  /// / `Array` / `Object`). Returns `None` for rich types (`undefined` /
  /// `NaN` / `Date` / `RegExp` / `Error` / `TypedArray` / `ArrayBuffer` /
  /// `Handle` / `Reference` / `BigInt`).
  #[must_use]
  pub fn to_json_like(&self) -> Option<serde_json::Value> {
    match self {
      Self::Bool(b) => Some(serde_json::Value::Bool(*b)),
      Self::Number(n) => {
        // Preserve integer form when the value is losslessly
        // representable within f64's safe integer range (±2^53).
        const F64_INT_MAX: f64 = 9_007_199_254_740_992.0;
        if n.is_finite() && n.fract() == 0.0 && n.abs() <= F64_INT_MAX {
          #[allow(clippy::cast_possible_truncation)]
          let as_i64 = *n as i64;
          Some(serde_json::Value::Number(as_i64.into()))
        } else {
          serde_json::Number::from_f64(*n).map(serde_json::Value::Number)
        }
      },
      Self::Str(s) => Some(serde_json::Value::String(s.clone())),
      Self::Special(SpecialValue::Null) => Some(serde_json::Value::Null),
      Self::Array { items, .. } => {
        let mut out = Vec::with_capacity(items.len());
        for v in items {
          out.push(v.to_json_like()?);
        }
        Some(serde_json::Value::Array(out))
      },
      Self::Object { entries, .. } => {
        let mut map = serde_json::Map::with_capacity(entries.len());
        for e in entries {
          map.insert(e.k.clone(), e.v.to_json_like()?);
        }
        Some(serde_json::Value::Object(map))
      },
      // Rich types + non-Null specials have no lossless JSON form.
      Self::Special(_)
      | Self::Date(_)
      | Self::Url(_)
      | Self::BigInt(_)
      | Self::Error(_)
      | Self::RegExp(_)
      | Self::TypedArray(_)
      | Self::ArrayBuffer(_)
      | Self::Handle(_)
      | Self::Reference(_) => None,
    }
  }

  /// Boolean projection for call sites that expect a `Bool` result. Non-bool
  /// values return `None` — callers typically `.unwrap_or(false)`.
  #[must_use]
  pub fn as_bool(&self) -> Option<bool> {
    match self {
      Self::Bool(b) => Some(*b),
      _ => None,
    }
  }

  /// String projection for call sites that expect a `Str` result. Non-string
  /// values return `None`.
  #[must_use]
  pub fn as_str(&self) -> Option<&str> {
    match self {
      Self::Str(s) => Some(s.as_str()),
      _ => None,
    }
  }

  /// Number projection. Returns `None` for non-number tags (including `BigInt`).
  #[must_use]
  pub fn as_number(&self) -> Option<f64> {
    match self {
      Self::Number(n) => Some(*n),
      _ => None,
    }
  }

  /// Array projection — borrows the item slice when this is an `Array`,
  /// `None` otherwise.
  #[must_use]
  pub fn as_array(&self) -> Option<&[SerializedValue]> {
    match self {
      Self::Array { items, .. } => Some(items),
      _ => None,
    }
  }

  /// Lossy text projection: `Str` → its content, numbers / booleans /
  /// `Date` / `Url` / `BigInt` / `RegExp` / `null` → their human-readable
  /// form, everything else (`undefined`, arrays, objects, handles) →
  /// empty string. Intended as a migration shim for call sites that
  /// used to call `Page::evaluate_str(expr)` and expected the raw string
  /// content without the surrounding JSON quoting.
  #[must_use]
  pub fn as_string_lossy(&self) -> String {
    match self {
      Self::Str(s) | Self::Date(s) | Self::Url(s) | Self::BigInt(s) => s.clone(),
      Self::Number(n) => n.to_string(),
      Self::Bool(b) => b.to_string(),
      Self::Special(SpecialValue::Null) => "null".to_string(),
      Self::RegExp(RegExpValue { p, .. }) => p.clone(),
      _ => String::new(),
    }
  }
}

// ── SerializedValue wire serialization ──────────────────────────────────────

impl Serialize for SerializedValue {
  fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
    match self {
      // Primitives pass through as raw JSON primitives.
      Self::Bool(b) => s.serialize_bool(*b),
      Self::Number(n) => s.serialize_f64(*n),
      Self::Str(v) => s.serialize_str(v),
      // Single-key object shapes.
      Self::Special(v) => {
        let mut m = s.serialize_map(Some(1))?;
        m.serialize_entry("v", v)?;
        m.end()
      },
      Self::Date(d) => {
        let mut m = s.serialize_map(Some(1))?;
        m.serialize_entry("d", d)?;
        m.end()
      },
      Self::Url(u) => {
        let mut m = s.serialize_map(Some(1))?;
        m.serialize_entry("u", u)?;
        m.end()
      },
      Self::BigInt(bi) => {
        let mut m = s.serialize_map(Some(1))?;
        m.serialize_entry("bi", bi)?;
        m.end()
      },
      Self::Error(e) => {
        let mut m = s.serialize_map(Some(1))?;
        m.serialize_entry("e", e)?;
        m.end()
      },
      Self::RegExp(r) => {
        let mut m = s.serialize_map(Some(1))?;
        m.serialize_entry("r", r)?;
        m.end()
      },
      Self::TypedArray(ta) => {
        let mut m = s.serialize_map(Some(1))?;
        m.serialize_entry("ta", ta)?;
        m.end()
      },
      Self::ArrayBuffer(ab) => {
        let mut m = s.serialize_map(Some(1))?;
        m.serialize_entry("ab", ab)?;
        m.end()
      },
      Self::Array { id, items } => {
        let mut m = s.serialize_map(Some(2))?;
        m.serialize_entry("a", items)?;
        m.serialize_entry("id", id)?;
        m.end()
      },
      Self::Object { id, entries } => {
        let mut m = s.serialize_map(Some(2))?;
        m.serialize_entry("o", entries)?;
        m.serialize_entry("id", id)?;
        m.end()
      },
      Self::Handle(idx) => {
        let mut m = s.serialize_map(Some(1))?;
        m.serialize_entry("h", idx)?;
        m.end()
      },
      Self::Reference(id) => {
        let mut m = s.serialize_map(Some(1))?;
        m.serialize_entry("ref", id)?;
        m.end()
      },
    }
  }
}

impl<'de> Deserialize<'de> for SerializedValue {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    d.deserialize_any(SerializedValueVisitor)
  }
}

struct SerializedValueVisitor;

impl<'de> Visitor<'de> for SerializedValueVisitor {
  type Value = SerializedValue;

  fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("a Playwright isomorphic SerializedValue: raw bool/number/string, or single-key tagged object")
  }

  // Primitives come in as raw JSON values.

  fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
    Ok(SerializedValue::Bool(v))
  }

  fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
    #[allow(clippy::cast_precision_loss)]
    let as_f64 = v as f64;
    Ok(SerializedValue::Number(as_f64))
  }

  fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
    #[allow(clippy::cast_precision_loss)]
    let as_f64 = v as f64;
    Ok(SerializedValue::Number(as_f64))
  }

  fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
    Ok(SerializedValue::Number(v))
  }

  fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
    Ok(SerializedValue::Str(v.to_string()))
  }

  fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
    Ok(SerializedValue::Str(v))
  }

  fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
    // `undefined` over the JSON wire has no native form; some
    // encoders emit it as absent-field / `null`. Treat as `undefined`.
    Ok(SerializedValue::Special(SpecialValue::Undefined))
  }

  fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
    // Same as above — bare JSON `null` is NOT what Playwright emits
    // (it uses `{v: "null"}`), but be tolerant so callers can pass
    // plain JSON graphs too.
    Ok(SerializedValue::Special(SpecialValue::Null))
  }

  fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
    // Playwright's tagged objects always have either one primary key
    // (plus `id` companion for `a` / `o`). We collect the raw key/value
    // pairs into a `serde_json::Map` first, then dispatch on the
    // primary tag. This keeps the impl compact at the cost of one
    // extra allocation per object — cheap compared to the wire-
    // decoding cost.
    let mut bag = serde_json::Map::new();
    while let Some((k, v)) = map.next_entry::<String, serde_json::Value>()? {
      bag.insert(k, v);
    }
    decode_tagged_object(bag).map_err(de::Error::custom)
  }
}

fn decode_tagged_object(mut bag: serde_json::Map<String, serde_json::Value>) -> Result<SerializedValue, String> {
  // The tag precedence mirrors Playwright's own deserializer
  // (`ref` > `v` > rich types > `a` / `o` > `h` > typed / array
  // buffer). See
  // `/tmp/playwright/packages/injected/src/utilityScriptSerializers.ts:124`.
  if let Some(v) = bag.remove("ref") {
    let id = v.as_u64().ok_or_else(|| format!("ref must be a u64, got {v}"))?;
    return Ok(SerializedValue::Reference(
      u32::try_from(id).map_err(|e| e.to_string())?,
    ));
  }
  if let Some(v) = bag.remove("v") {
    let special: SpecialValue = serde_json::from_value(v).map_err(|e| e.to_string())?;
    return Ok(SerializedValue::Special(special));
  }
  if let Some(v) = bag.remove("d") {
    let iso = v.as_str().ok_or("d must be string")?.to_string();
    return Ok(SerializedValue::Date(iso));
  }
  if let Some(v) = bag.remove("u") {
    let s = v.as_str().ok_or("u must be string")?.to_string();
    return Ok(SerializedValue::Url(s));
  }
  if let Some(v) = bag.remove("bi") {
    let s = v.as_str().ok_or("bi must be string")?.to_string();
    return Ok(SerializedValue::BigInt(s));
  }
  if let Some(v) = bag.remove("e") {
    let e: ErrorValue = serde_json::from_value(v).map_err(|err| err.to_string())?;
    return Ok(SerializedValue::Error(e));
  }
  if let Some(v) = bag.remove("r") {
    let r: RegExpValue = serde_json::from_value(v).map_err(|err| err.to_string())?;
    return Ok(SerializedValue::RegExp(r));
  }
  if let Some(v) = bag.remove("a") {
    let items: Vec<SerializedValue> = serde_json::from_value(v).map_err(|err| err.to_string())?;
    let id = bag
      .remove("id")
      .and_then(|v| v.as_u64())
      .ok_or("a must be paired with numeric id")?;
    return Ok(SerializedValue::Array {
      id: u32::try_from(id).map_err(|e| e.to_string())?,
      items,
    });
  }
  if let Some(v) = bag.remove("o") {
    let entries: Vec<PropertyEntry> = serde_json::from_value(v).map_err(|err| err.to_string())?;
    let id = bag
      .remove("id")
      .and_then(|v| v.as_u64())
      .ok_or("o must be paired with numeric id")?;
    return Ok(SerializedValue::Object {
      id: u32::try_from(id).map_err(|e| e.to_string())?,
      entries,
    });
  }
  if let Some(v) = bag.remove("h") {
    let idx = v.as_u64().ok_or("h must be u64")?;
    return Ok(SerializedValue::Handle(u32::try_from(idx).map_err(|e| e.to_string())?));
  }
  if let Some(v) = bag.remove("ta") {
    let ta: TypedArrayValue = serde_json::from_value(v).map_err(|err| err.to_string())?;
    return Ok(SerializedValue::TypedArray(ta));
  }
  if let Some(v) = bag.remove("ab") {
    let ab: ArrayBufferValue = serde_json::from_value(v).map_err(|err| err.to_string())?;
    return Ok(SerializedValue::ArrayBuffer(ab));
  }
  Err(format!(
    "SerializedValue: no recognized tag in object (keys: {:?})",
    bag.keys().collect::<Vec<_>>()
  ))
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

/// The full `{ value, handles }` envelope ferridriver passes to the
/// injected utility script when a user calls `page.evaluate(fn, arg)`.
/// `value` carries the serialized arg tree (possibly containing
/// `h: N` refs into `handles`); `handles` carries the backend object
/// references those refs resolve to. [`HandleId`] models the cross-
/// backend variant (CDP `RemoteObjectId` string / `BiDi`
/// `{shared_id, handle}` / `WebKit` `__wr[]` index); the backend
/// marshaler layer converts each entry into the protocol's native
/// remote-object reference (CDP `CallArgument.objectId`, etc.).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SerializedArgument {
  pub value: SerializedValue,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub handles: Vec<HandleId>,
}

impl Default for SerializedValue {
  fn default() -> Self {
    Self::Special(SpecialValue::Undefined)
  }
}

/// Backend-agnostic handle identifier used in
/// [`SerializedArgument::handles`]. Each backend maps this to its own
/// native remote-object reference on marshal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandleId {
  /// CDP `Runtime.RemoteObjectId` — an opaque string.
  Cdp(String),
  /// `WebDriver` `BiDi` shared reference — `{ sharedId, handle? }`.
  Bidi { shared_id: String, handle: Option<String> },
  /// `WebKit` host IPC ref — an index into `window.__wr[]`.
  WebKit(u64),
}

// ── base64 helpers (stand-alone) ────────────────────────────────────────────

/// Emit the same base64 encoding the `ta.b` / `ab.b` serializers use.
/// Handy for backend marshalers that need to produce or consume the raw
/// bytes outside the serde path.
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

  // ── Primitive wire shapes ────────────────────────────────────────────────

  #[test]
  fn serializes_bool_raw() {
    assert_eq!(serde_json::to_value(SerializedValue::Bool(true)).unwrap(), json!(true));
    assert_eq!(
      serde_json::to_value(SerializedValue::Bool(false)).unwrap(),
      json!(false)
    );
  }

  #[test]
  fn serializes_number_raw() {
    // Playwright's isomorphic format emits primitives raw — `42.0`, not
    // `{n: 42.0}`. That's what the page's utilityScript expects.
    assert_eq!(serde_json::to_value(SerializedValue::Number(1.5)).unwrap(), json!(1.5));
  }

  #[test]
  fn serializes_string_raw() {
    assert_eq!(
      serde_json::to_value(SerializedValue::Str("hi".into())).unwrap(),
      json!("hi")
    );
  }

  // ── Special values ───────────────────────────────────────────────────────

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
        serde_json::to_value(SerializedValue::Special(special)).unwrap(),
        expected,
        "variant {special:?}"
      );
    }
  }

  #[test]
  fn from_f64_routes_ieee_specials() {
    assert!(matches!(
      SerializedValue::from_f64(f64::NAN),
      SerializedValue::Special(SpecialValue::NaN)
    ));
    assert!(matches!(
      SerializedValue::from_f64(f64::INFINITY),
      SerializedValue::Special(SpecialValue::Infinity)
    ));
    assert!(matches!(
      SerializedValue::from_f64(f64::NEG_INFINITY),
      SerializedValue::Special(SpecialValue::NegInfinity)
    ));
    assert!(matches!(
      SerializedValue::from_f64(-0.0_f64),
      SerializedValue::Special(SpecialValue::NegZero)
    ));
    assert!(matches!(
      SerializedValue::from_f64(0.0_f64),
      SerializedValue::Number(n) if (n - 0.0_f64).abs() < f64::EPSILON
    ));
    assert!(matches!(
      SerializedValue::from_f64(1.5),
      SerializedValue::Number(n) if (n - 1.5_f64).abs() < f64::EPSILON
    ));
  }

  // ── Rich types ──────────────────────────────────────────────────────────

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
    let wire = serde_json::to_value(SerializedValue::typed_array(vec![1, 2, 3, 4], TypedArrayKind::U8)).unwrap();
    assert_eq!(wire, json!({"ta": {"b": "AQIDBA==", "k": "ui8"}}));
  }

  #[test]
  fn serializes_array_buffer_with_base64() {
    let wire = serde_json::to_value(SerializedValue::array_buffer(vec![0xca, 0xfe, 0xba, 0xbe])).unwrap();
    assert_eq!(wire, json!({"ab": {"b": "yv66vg=="}}));
  }

  #[test]
  fn typed_array_bytes_per_element_matches_js() {
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

  // ── Collections ──────────────────────────────────────────────────────────

  #[test]
  fn serializes_array_with_id() {
    let wire = serde_json::to_value(SerializedValue::array(
      1,
      vec![SerializedValue::Number(1.0), SerializedValue::Number(2.0)],
    ))
    .unwrap();
    assert_eq!(wire, json!({"a": [1.0, 2.0], "id": 1}));
  }

  #[test]
  fn serializes_object_with_id_preserves_order() {
    let wire = serde_json::to_value(SerializedValue::object(
      2,
      vec![
        PropertyEntry {
          k: "first".into(),
          v: SerializedValue::Number(1.0),
        },
        PropertyEntry {
          k: "second".into(),
          v: SerializedValue::Str("two".into()),
        },
      ],
    ))
    .unwrap();
    assert_eq!(
      wire,
      json!({"o": [{"k": "first", "v": 1.0}, {"k": "second", "v": "two"}], "id": 2})
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

  // ── Deserialization ─────────────────────────────────────────────────────

  #[test]
  fn deserializes_primitives_raw() {
    assert_eq!(
      serde_json::from_value::<SerializedValue>(json!(true)).unwrap(),
      SerializedValue::Bool(true)
    );
    assert_eq!(
      serde_json::from_value::<SerializedValue>(json!(42)).unwrap(),
      SerializedValue::Number(42.0)
    );
    assert_eq!(
      serde_json::from_value::<SerializedValue>(json!("hi")).unwrap(),
      SerializedValue::Str("hi".into())
    );
  }

  #[test]
  fn deserializes_special_tag() {
    for (wire, expected) in [
      (json!({"v": "null"}), SpecialValue::Null),
      (json!({"v": "undefined"}), SpecialValue::Undefined),
      (json!({"v": "NaN"}), SpecialValue::NaN),
      (json!({"v": "Infinity"}), SpecialValue::Infinity),
      (json!({"v": "-Infinity"}), SpecialValue::NegInfinity),
      (json!({"v": "-0"}), SpecialValue::NegZero),
    ] {
      let parsed = serde_json::from_value::<SerializedValue>(wire.clone()).unwrap();
      assert_eq!(parsed, SerializedValue::Special(expected), "wire: {wire}");
    }
  }

  #[test]
  fn deserializes_rich_tagged_forms() {
    assert_eq!(
      serde_json::from_value::<SerializedValue>(json!({"d": "2024"})).unwrap(),
      SerializedValue::Date("2024".into())
    );
    assert_eq!(
      serde_json::from_value::<SerializedValue>(json!({"u": "https://a/"})).unwrap(),
      SerializedValue::Url("https://a/".into())
    );
    assert_eq!(
      serde_json::from_value::<SerializedValue>(json!({"bi": "-42"})).unwrap(),
      SerializedValue::BigInt("-42".into())
    );
    assert_eq!(
      serde_json::from_value::<SerializedValue>(json!({"r": {"p": "a", "f": "g"}})).unwrap(),
      SerializedValue::RegExp(RegExpValue {
        p: "a".into(),
        f: "g".into()
      })
    );
    assert_eq!(
      serde_json::from_value::<SerializedValue>(json!({"e": {"n": "E", "m": "m", "s": "s"}})).unwrap(),
      SerializedValue::Error(ErrorValue {
        n: "E".into(),
        m: "m".into(),
        s: "s".into()
      })
    );
  }

  #[test]
  fn deserializes_array_and_object_with_id() {
    assert_eq!(
      serde_json::from_value::<SerializedValue>(json!({"a": [1, 2], "id": 1})).unwrap(),
      SerializedValue::Array {
        id: 1,
        items: vec![SerializedValue::Number(1.0), SerializedValue::Number(2.0)],
      }
    );
    assert_eq!(
      serde_json::from_value::<SerializedValue>(json!({"o": [{"k": "x", "v": true}], "id": 2})).unwrap(),
      SerializedValue::Object {
        id: 2,
        entries: vec![PropertyEntry {
          k: "x".into(),
          v: SerializedValue::Bool(true),
        }],
      }
    );
  }

  #[test]
  fn deserializes_handle_and_reference() {
    assert_eq!(
      serde_json::from_value::<SerializedValue>(json!({"h": 0})).unwrap(),
      SerializedValue::Handle(0)
    );
    assert_eq!(
      serde_json::from_value::<SerializedValue>(json!({"ref": 5})).unwrap(),
      SerializedValue::Reference(5)
    );
  }

  #[test]
  fn deserializes_typed_array_base64() {
    let parsed: SerializedValue = serde_json::from_value(json!({"ta": {"b": "AQIDBA==", "k": "ui8"}})).unwrap();
    assert_eq!(
      parsed,
      SerializedValue::TypedArray(TypedArrayValue {
        b: vec![1, 2, 3, 4],
        k: TypedArrayKind::U8
      })
    );
  }

  #[test]
  fn deserializes_array_buffer_base64() {
    let parsed: SerializedValue = serde_json::from_value(json!({"ab": {"b": "yv66vg=="}})).unwrap();
    assert_eq!(
      parsed,
      SerializedValue::ArrayBuffer(ArrayBufferValue {
        b: vec![0xca, 0xfe, 0xba, 0xbe],
      })
    );
  }

  #[test]
  fn rejects_empty_tagged_object() {
    let err = serde_json::from_value::<SerializedValue>(json!({})).unwrap_err();
    assert!(
      err.to_string().contains("no recognized tag"),
      "unexpected error message: {err}"
    );
  }

  // ── Round-trip ──────────────────────────────────────────────────────────

  #[test]
  fn roundtrips_every_variant_via_serde() {
    let values = vec![
      SerializedValue::Bool(true),
      SerializedValue::Bool(false),
      SerializedValue::Number(1.5),
      SerializedValue::Number(0.0),
      SerializedValue::Str("hello".into()),
      SerializedValue::Special(SpecialValue::Null),
      SerializedValue::Special(SpecialValue::Undefined),
      SerializedValue::Special(SpecialValue::NaN),
      SerializedValue::Special(SpecialValue::Infinity),
      SerializedValue::Special(SpecialValue::NegInfinity),
      SerializedValue::Special(SpecialValue::NegZero),
      SerializedValue::date("2024-06-01T12:00:00.000Z"),
      SerializedValue::url("https://a.test/"),
      SerializedValue::bigint("-12345678901234567890"),
      SerializedValue::regexp("a|b", "i"),
      SerializedValue::error("Error", "boom", ""),
      SerializedValue::typed_array(vec![0xde, 0xad, 0xbe, 0xef], TypedArrayKind::U32),
      SerializedValue::array_buffer(vec![0x01, 0x02]),
      SerializedValue::array(1, vec![SerializedValue::Number(1.0)]),
      SerializedValue::object(
        2,
        vec![PropertyEntry {
          k: "x".into(),
          v: SerializedValue::Bool(true),
        }],
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

  // ── JSON conversion helpers ─────────────────────────────────────────────

  #[test]
  fn from_json_maps_scalars() {
    let mut ctx = SerializationContext::default();
    assert_eq!(
      SerializedValue::from_json(&json!(null), &mut ctx),
      SerializedValue::Special(SpecialValue::Null)
    );
    assert_eq!(
      SerializedValue::from_json(&json!(true), &mut ctx),
      SerializedValue::Bool(true)
    );
    assert_eq!(
      SerializedValue::from_json(&json!("hi"), &mut ctx),
      SerializedValue::Str("hi".into())
    );
    assert!(matches!(
      SerializedValue::from_json(&json!(42), &mut ctx),
      SerializedValue::Number(n) if (n - 42.0).abs() < f64::EPSILON
    ));
  }

  #[test]
  fn from_json_allocates_ids_for_collections() {
    let mut ctx = SerializationContext::default();
    let val = SerializedValue::from_json(&json!({"a": [1, 2], "b": null}), &mut ctx);
    let (outer_id, entries) = match &val {
      SerializedValue::Object { id, entries } => (*id, entries),
      _ => panic!("expected Object"),
    };
    let a_entry = entries.iter().find(|e| e.k == "a").expect("has key `a`");
    let a_id = match &a_entry.v {
      SerializedValue::Array { id, .. } => *id,
      _ => panic!("expected Array"),
    };
    assert_ne!(outer_id, a_id, "ids are distinct");
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
    assert_eq!(ids, vec![1, 2, 3, 4, 5]);
  }

  // ── SerializedArgument ──────────────────────────────────────────────────

  #[test]
  fn serialized_argument_omits_empty_handles() {
    let arg = SerializedArgument {
      value: SerializedValue::Number(1.0),
      handles: vec![],
    };
    let s = serde_json::to_string(&arg).unwrap();
    assert_eq!(s, r#"{"value":1.0}"#);
  }

  #[test]
  fn serialized_argument_carries_handle_list() {
    let arg = SerializedArgument {
      value: SerializedValue::array(1, vec![SerializedValue::handle(0), SerializedValue::handle(1)]),
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
        "handles": [
          {"Cdp": "obj-1"},
          {"Bidi": {"shared_id": "shared-1", "handle": null}}
        ]
      })
    );
  }

  // ── Playwright-wire-exact parity spot checks ────────────────────────────
  //
  // Each case mirrors a value Playwright's own `serializeAsCallArgument` in
  // `/tmp/playwright/packages/injected/src/utilityScriptSerializers.ts`
  // would produce for the corresponding JS value. If we ever drift from
  // these strings the page-side `parseEvaluationResultValue` would
  // misinterpret the CDP arguments.

  #[test]
  fn parity_array_of_mixed_primitives() {
    let v = SerializedValue::array(
      1,
      vec![
        SerializedValue::Number(1.0),
        SerializedValue::Bool(true),
        SerializedValue::Str("x".into()),
        SerializedValue::Special(SpecialValue::Null),
        SerializedValue::Special(SpecialValue::NaN),
      ],
    );
    assert_eq!(
      serde_json::to_value(&v).unwrap(),
      json!({"a": [1.0, true, "x", {"v": "null"}, {"v": "NaN"}], "id": 1})
    );
  }

  #[test]
  fn parity_nested_object_with_handle_refs() {
    let v = SerializedValue::object(
      1,
      vec![
        PropertyEntry {
          k: "el".into(),
          v: SerializedValue::handle(0),
        },
        PropertyEntry {
          k: "meta".into(),
          v: SerializedValue::object(
            2,
            vec![PropertyEntry {
              k: "count".into(),
              v: SerializedValue::Number(3.0),
            }],
          ),
        },
      ],
    );
    assert_eq!(
      serde_json::to_value(&v).unwrap(),
      json!({
        "o": [
          {"k": "el", "v": {"h": 0}},
          {"k": "meta", "v": {"o": [{"k": "count", "v": 3.0}], "id": 2}}
        ],
        "id": 1
      })
    );
  }
}
