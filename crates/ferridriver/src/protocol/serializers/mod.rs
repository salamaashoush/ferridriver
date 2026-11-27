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
//! | exposed function   | `{fn: <binding name>}`            |
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
  /// A caller-side function passed with `exposeFunctions` — wire shape
  /// `{fn: <binding name>}`. The page receives a function that calls back
  /// through that binding.
  Function(String),
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

  /// JavaScript source that evaluates to this value. An init script runs
  /// before any utility script exists, so its argument travels inline:
  /// Playwright parses it page-side with `parseInitScriptArg`, and this is
  /// that parse done ahead of time. Handles cannot reach a document that
  /// does not exist yet.
  ///
  /// # Errors
  ///
  /// Returns an error if the value holds a [`Self::Handle`].
  pub fn to_js_source(&self) -> crate::error::Result<String> {
    let mut out = String::from("(r => ");
    self.write_js_source(&mut out).map_err(|e| match e {
      JsSourceError::Handle => {
        crate::error::FerriError::invalid_argument("arg", "a JSHandle cannot be passed to an init script")
      },
      JsSourceError::Format => crate::error::FerriError::invalid_argument("arg", "could not render the argument"),
    })?;
    out.push_str(")({})");
    Ok(out)
  }

  fn write_js_source(&self, out: &mut String) -> Result<(), JsSourceError> {
    use std::fmt::Write as _;
    let quote = |s: &str| serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
    match self {
      Self::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
      Self::Number(n) => write!(out, "({n:?})")?,
      Self::Str(s) => out.push_str(&quote(s)),
      Self::Special(special) => out.push_str(match special {
        SpecialValue::Null => "null",
        SpecialValue::Undefined => "undefined",
        SpecialValue::NaN => "NaN",
        SpecialValue::Infinity => "Infinity",
        SpecialValue::NegInfinity => "(-Infinity)",
        SpecialValue::NegZero => "(-0)",
      }),
      Self::Date(iso) => write!(out, "new Date({})", quote(iso))?,
      Self::Url(url) => write!(out, "new URL({})", quote(url))?,
      Self::BigInt(digits) => write!(out, "BigInt({})", quote(digits))?,
      Self::Error(e) => write!(
        out,
        "Object.assign(new Error({}), {{ name: {}, stack: {} }})",
        quote(&e.m),
        quote(&e.n),
        quote(&e.s)
      )?,
      Self::RegExp(r) => write!(out, "new RegExp({}, {})", quote(&r.p), quote(&r.f))?,
      Self::TypedArray(t) => {
        let constructor = match t.k {
          TypedArrayKind::I8 => "Int8Array",
          TypedArrayKind::U8 => "Uint8Array",
          TypedArrayKind::U8Clamped => "Uint8ClampedArray",
          TypedArrayKind::I16 => "Int16Array",
          TypedArrayKind::U16 => "Uint16Array",
          TypedArrayKind::I32 => "Int32Array",
          TypedArrayKind::U32 => "Uint32Array",
          TypedArrayKind::F32 => "Float32Array",
          TypedArrayKind::F64 => "Float64Array",
          TypedArrayKind::BI64 => "BigInt64Array",
          TypedArrayKind::BUI64 => "BigUint64Array",
        };
        write!(out, "new {constructor}(new Uint8Array({:?}).buffer)", t.b)?;
      },
      Self::ArrayBuffer(a) => write!(out, "new Uint8Array({:?}).buffer", a.b)?,
      // Each collection lands in `r` before its members are built, so a
      // member referring back to it (a cycle) finds it there.
      Self::Array { id, items } => {
        write!(out, "(r[{id}] = [], r[{id}].push(")?;
        for (i, item) in items.iter().enumerate() {
          if i > 0 {
            out.push_str(", ");
          }
          item.write_js_source(out)?;
        }
        write!(out, "), r[{id}])")?;
      },
      Self::Object { id, entries } => {
        write!(out, "(r[{id}] = {{}}")?;
        for entry in entries {
          write!(out, ", r[{id}][{}] = ", quote(&entry.k))?;
          entry.v.write_js_source(out)?;
        }
        write!(out, ", r[{id}])")?;
      },
      Self::Reference(id) => write!(out, "r[{id}]")?,
      Self::Function(name) => write!(
        out,
        "((...args) => globalThis[{}].callBinding({}, ...args))",
        quote(BINDINGS_CONTROLLER),
        quote(name)
      )?,
      Self::Handle(_) => return Err(JsSourceError::Handle),
    }
    Ok(())
  }

  // ── Conversion helpers ────────────────────────────────────────────────────

  /// Owned variant of [`Self::from_json`] that moves strings and object keys
  /// instead of cloning them.
  #[must_use]
  pub fn from_json_owned(value: serde_json::Value, ctx: &mut SerializationContext) -> Self {
    match value {
      serde_json::Value::Null => Self::null(),
      serde_json::Value::Bool(b) => Self::Bool(b),
      serde_json::Value::Number(num) => num
        .as_f64()
        .map_or_else(|| Self::BigInt(num.to_string()), Self::from_f64),
      serde_json::Value::String(s) => Self::Str(s),
      serde_json::Value::Array(items) => {
        let id = ctx.alloc_id();
        let wire_items = items.into_iter().map(|v| Self::from_json_owned(v, ctx)).collect();
        Self::Array { id, items: wire_items }
      },
      serde_json::Value::Object(map) => {
        let id = ctx.alloc_id();
        let entries = map
          .into_iter()
          .map(|(k, v)| PropertyEntry {
            k,
            v: Self::from_json_owned(v, ctx),
          })
          .collect();
        Self::Object { id, entries }
      },
    }
  }

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
          #[expect(
            clippy::cast_possible_truncation,
            reason = "guarded above: finite, integral and within 2^53, so the cast is exact"
          )]
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
      | Self::Reference(_)
      | Self::Function(_) => None,
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
      Self::Function(name) => {
        let mut m = s.serialize_map(Some(1))?;
        m.serialize_entry("fn", name)?;
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

/// `serde_json`'s internal key for an `arbitrary_precision` number when
/// it surfaces through a `deserialize_any` visitor as a map.
const AP_NUMBER_SENTINEL: &str = "$serde_json::private::Number";

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
    #[expect(
      clippy::cast_precision_loss,
      reason = "a JS number is an f64; JSON.parse rounds integers past 2^53 the same way"
    )]
    let as_f64 = v as f64;
    Ok(SerializedValue::Number(as_f64))
  }

  fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
    #[expect(
      clippy::cast_precision_loss,
      reason = "a JS number is an f64; JSON.parse rounds integers past 2^53 the same way"
    )]
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
    // Stream directly into typed fields without materializing an intermediate
    // `serde_json::Map` / `serde_json::Value` tree. Each nested array/object
    // is decoded once in a single pass rather than re-parsed at every depth.
    let mut primary: Option<(u8, SerializedValue)> = None;
    let mut array: Option<Vec<SerializedValue>> = None;
    let mut object: Option<Vec<PropertyEntry>> = None;
    let mut id: Option<u32> = None;
    let mut ap_number: Option<String> = None;
    let mut key_count: usize = 0;
    let mut unknown_keys: Vec<String> = Vec::new();

    let mut set_primary = |prio: u8, val: SerializedValue| {
      if primary.as_ref().is_none_or(|(p, _)| prio < *p) {
        primary = Some((prio, val));
      }
    };

    while let Some(key) = map.next_key::<TagKey>()? {
      key_count += 1;
      match key {
        TagKey::Ref => {
          let raw = map.next_value::<u64>()?;
          let ref_id = u32::try_from(raw).map_err(|e| de::Error::custom(DecodeError::IdRange(e)))?;
          set_primary(0, SerializedValue::Reference(ref_id));
        },
        TagKey::V => {
          let special = map.next_value::<SpecialValue>()?;
          set_primary(1, SerializedValue::Special(special));
        },
        TagKey::D => set_primary(2, SerializedValue::Date(map.next_value::<String>()?)),
        TagKey::U => set_primary(3, SerializedValue::Url(map.next_value::<String>()?)),
        TagKey::Bi => set_primary(4, SerializedValue::BigInt(map.next_value::<String>()?)),
        TagKey::E => set_primary(5, SerializedValue::Error(map.next_value::<ErrorValue>()?)),
        TagKey::R => set_primary(6, SerializedValue::RegExp(map.next_value::<RegExpValue>()?)),
        TagKey::A => {
          array = Some(map.next_value::<Vec<SerializedValue>>()?);
        },
        TagKey::O => {
          object = Some(map.next_value::<Vec<PropertyEntry>>()?);
        },
        TagKey::Id => {
          let raw = map.next_value::<u64>()?;
          id = Some(u32::try_from(raw).map_err(|e| de::Error::custom(DecodeError::IdRange(e)))?);
        },
        TagKey::H => {
          let raw = map.next_value::<u64>()?;
          let idx = u32::try_from(raw).map_err(|e| de::Error::custom(DecodeError::IdRange(e)))?;
          set_primary(9, SerializedValue::Handle(idx));
        },
        TagKey::Fn => set_primary(10, SerializedValue::Function(map.next_value::<String>()?)),
        TagKey::Ta => set_primary(11, SerializedValue::TypedArray(map.next_value::<TypedArrayValue>()?)),
        TagKey::Ab => set_primary(12, SerializedValue::ArrayBuffer(map.next_value::<ArrayBufferValue>()?)),
        TagKey::ApNumber => {
          ap_number = Some(map.next_value::<String>()?);
        },
        TagKey::Unknown(name) => {
          let _ = map.next_value::<de::IgnoredAny>()?;
          unknown_keys.push(name);
        },
      }
    }

    // `serde_json`'s `arbitrary_precision` feature (force-enabled
    // workspace-wide by a transitive dep) makes a bare JSON number
    // reach a `deserialize_any` visitor as a one-key map
    // `{"$serde_json::private::Number": "<literal>"}` instead of
    // `visit_f64`. Decode that back to a number before tag dispatch so
    // the wire stays AP-agnostic.
    if key_count == 1
      && let Some(ref lit) = ap_number
      && let Ok(n) = lit.parse::<f64>()
    {
      return Ok(SerializedValue::Number(n));
    }

    // Tag precedence mirrors Playwright's `parseEvaluationResultValue`:
    // `ref` (0) > `v` (1) > `d`..`r` (2..=6) > `a` (7) > `o` (8) > `h`..`ab` (9..=12).
    if let Some((prio, val)) = primary {
      if prio < 7 {
        return Ok(val);
      }
      if let Some(items) = array {
        let id = id.ok_or_else(|| de::Error::custom(DecodeError::tag("a", "must be paired with numeric id")))?;
        return Ok(SerializedValue::Array { id, items });
      }
      if let Some(entries) = object {
        let id = id.ok_or_else(|| de::Error::custom(DecodeError::tag("o", "must be paired with numeric id")))?;
        return Ok(SerializedValue::Object { id, entries });
      }
      return Ok(val);
    }
    if let Some(items) = array {
      let id = id.ok_or_else(|| de::Error::custom(DecodeError::tag("a", "must be paired with numeric id")))?;
      return Ok(SerializedValue::Array { id, items });
    }
    if let Some(entries) = object {
      let id = id.ok_or_else(|| de::Error::custom(DecodeError::tag("o", "must be paired with numeric id")))?;
      return Ok(SerializedValue::Object { id, entries });
    }
    if id.is_some() {
      unknown_keys.push("id".to_string());
    }
    if ap_number.is_some() {
      unknown_keys.push(AP_NUMBER_SENTINEL.to_string());
    }
    Err(de::Error::custom(DecodeError::UnknownShape(unknown_keys)))
  }
}

/// Zero-allocation map key identifier for [`SerializedValueVisitor::visit_map`].
enum TagKey {
  Ref,
  V,
  D,
  U,
  Bi,
  E,
  R,
  A,
  O,
  Id,
  H,
  Fn,
  Ta,
  Ab,
  ApNumber,
  Unknown(String),
}

impl<'de> Deserialize<'de> for TagKey {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    struct TagKeyVisitor;

    impl Visitor<'_> for TagKeyVisitor {
      type Value = TagKey;

      fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a SerializedValue wire tag key")
      }

      fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        Ok(match v {
          "ref" => TagKey::Ref,
          "v" => TagKey::V,
          "d" => TagKey::D,
          "u" => TagKey::U,
          "bi" => TagKey::Bi,
          "e" => TagKey::E,
          "r" => TagKey::R,
          "a" => TagKey::A,
          "o" => TagKey::O,
          "id" => TagKey::Id,
          "h" => TagKey::H,
          "fn" => TagKey::Fn,
          "ta" => TagKey::Ta,
          "ab" => TagKey::Ab,
          AP_NUMBER_SENTINEL => TagKey::ApNumber,
          other => TagKey::Unknown(other.to_string()),
        })
      }

      fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
        match v.as_str() {
          "ref" => Ok(TagKey::Ref),
          "v" => Ok(TagKey::V),
          "d" => Ok(TagKey::D),
          "u" => Ok(TagKey::U),
          "bi" => Ok(TagKey::Bi),
          "e" => Ok(TagKey::E),
          "r" => Ok(TagKey::R),
          "a" => Ok(TagKey::A),
          "o" => Ok(TagKey::O),
          "id" => Ok(TagKey::Id),
          "h" => Ok(TagKey::H),
          "fn" => Ok(TagKey::Fn),
          "ta" => Ok(TagKey::Ta),
          "ab" => Ok(TagKey::Ab),
          AP_NUMBER_SENTINEL => Ok(TagKey::ApNumber),
          _ => Ok(TagKey::Unknown(v)),
        }
      }
    }

    d.deserialize_identifier(TagKeyVisitor)
  }
}

/// Typed decode-error for the Playwright tagged-object wire shape. Lives
/// private to the serializer because the only consumer is a
/// `serde::Deserialize` Visitor which forwards through
/// `de::Error::custom(...)` and only needs `Display`. Using a dedicated
/// enum (rather than `String`) gives matchable variants if a future
/// caller wants to programmatically distinguish "missing id companion"
/// from "unknown tag".
#[derive(Debug, thiserror::Error)]
enum DecodeError {
  #[error("invalid tag {tag:?}: {reason}")]
  InvalidTag {
    tag: &'static str,
    reason: std::string::String,
  },
  #[error("id companion out of range: {0}")]
  IdRange(#[from] std::num::TryFromIntError),
  #[error("no recognised tag in object (keys: {0:?})")]
  UnknownShape(Vec<std::string::String>),
}

impl DecodeError {
  fn tag(tag: &'static str, reason: impl Into<std::string::String>) -> Self {
    Self::InvalidTag {
      tag,
      reason: reason.into(),
    }
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

/// Build a [`SerializedArgument`] from any `serde::Serialize` value —
/// the typed-`eval` entry path. Handles/rich JS types are not
/// expressible here; use the explicit constructors for those.
///
/// # Errors
///
/// Returns [`crate::error::FerriError::Json`] when the value fails
/// JSON serialization.
pub fn argument_from_serde<A: serde::Serialize + ?Sized>(arg: &A) -> crate::error::Result<SerializedArgument> {
  let json = serde_json::to_value(arg).map_err(|e| crate::error::FerriError::Json(format!("eval argument: {e}")))?;
  let mut ctx = SerializationContext::default();
  Ok(SerializedArgument {
    value: SerializedValue::from_json_owned(json, &mut ctx),
    handles: Vec::new(),
    functions: Vec::new(),
  })
}

/// Decode an evaluate result into any `serde::de::DeserializeOwned`
/// type — the typed-`eval` exit path. `undefined` decodes as JSON
/// `null` (so `()` and `Option<T>` work for value-less scripts).
///
/// # Errors
///
/// Returns [`crate::error::FerriError::Evaluation`] when the result
/// carries a rich JS type (`Date`, `RegExp`, handle, ...) that has no
/// JSON form, and [`crate::error::FerriError::Json`] when it does not
/// deserialize into `T`.
pub fn result_to_serde<T: serde::de::DeserializeOwned>(value: &SerializedValue) -> crate::error::Result<T> {
  let json = if matches!(value, SerializedValue::Special(SpecialValue::Undefined)) {
    serde_json::Value::Null
  } else {
    value.to_json_like().ok_or_else(|| {
      crate::error::FerriError::Evaluation(
        "eval result is not JSON-representable; use evaluate_handle for rich JS values".to_string(),
      )
    })?
  };
  serde_json::from_value(json).map_err(|e| crate::error::FerriError::Json(format!("eval result: {e}")))
}

/// Why [`SerializedValue::to_js_source`] could not render a value.
enum JsSourceError {
  Handle,
  Format,
}

impl From<std::fmt::Error> for JsSourceError {
  fn from(_: std::fmt::Error) -> Self {
    Self::Format
  }
}

// ── SerializedArgument ──────────────────────────────────────────────────────

/// The page global through which a function passed as an argument calls
/// back: Playwright's `kBindingsControllerProperty`, which the vendored
/// `parseEvaluationResultValue` reads.
pub const BINDINGS_CONTROLLER: &str = "__playwright__binding__controller__";

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
  /// The callbacks behind each [`SerializedValue::Function`] in `value`,
  /// registered on the page before the argument reaches it.
  #[serde(skip)]
  pub functions: Vec<ArgFunction>,
}

/// A caller-side function inside an argument, reachable from the page
/// through the binding `name` (Playwright's `exposeFunctions`).
#[derive(Clone)]
pub struct ArgFunction {
  pub name: String,
  pub binding: crate::events::ExposedBinding,
}

impl ArgFunction {
  /// A binding name no other function has had. The `__pw_fn_` prefix is
  /// Playwright's `kFunctionBindingPrefix`.
  #[must_use]
  pub fn fresh_name() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("__pw_fn_{}_{id}", std::process::id())
  }
}

impl std::fmt::Debug for ArgFunction {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ArgFunction")
      .field("name", &self.name)
      .finish_non_exhaustive()
  }
}

/// One step of the path to a value inside an argument.
#[derive(Debug, Clone)]
pub enum ArgPathStep {
  Key(String),
  Index(usize),
}

/// Playwright's error for a value an argument cannot carry, a function
/// passed without `exposeFunctions`: `Attempting to serialize unexpected
/// value at position "a.b[0]": <value>`.
#[must_use]
pub fn unexpected_value_error(path: &[ArgPathStep], display: &str) -> crate::error::FerriError {
  let mut chain = String::new();
  for (i, step) in path.iter().enumerate() {
    match step {
      ArgPathStep::Key(key) if i == 0 => chain.push_str(key),
      ArgPathStep::Key(key) => {
        chain.push('.');
        chain.push_str(key);
      },
      ArgPathStep::Index(index) => {
        chain.push('[');
        chain.push_str(&index.to_string());
        chain.push(']');
      },
    }
  }
  let position = if chain.is_empty() {
    String::new()
  } else {
    format!(" at position \"{chain}\"")
  };
  crate::error::FerriError::invalid_argument(
    "arg",
    format!("Attempting to serialize unexpected value{position}: {display}"),
  )
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
  WebDriver {
    id: String,
    frame: String,
    element: bool,
  },
  /// CDP remote object and the renderer session that owns it.
  Cdp {
    object_id: String,
    session_id: Option<String>,
  },
  /// `WebDriver` `BiDi` shared reference — `{ sharedId, handle? }`.
  Bidi {
    shared_id: String,
    handle: Option<String>,
  },
  /// Playwright `WebKit` `Runtime.RemoteObjectId` — an opaque string.
  WebKit(String),
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
mod tests;
