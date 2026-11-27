//! Live JS value adapter implementing [`ferridriver_expect::LiveValue`].
//!
//! Pure generic JS value inspection (`Object.is`, `instanceof`, `typeof`,
//! iterables, property access) with zero Playwright/browser types.

use ferridriver_expect::{JsType, LiveValue, deep_equal};
use rquickjs::{Atom, Ctx, Function, Object, Value};
use serde_json::Value as JsonValue;

use crate::bindings::convert::{serde_from_js, type_error};

/// A live JS value under assertion — the host half of
/// [`ferridriver_expect::LiveValue`]. Every method here is one JS
/// operation; the matcher decisions all live in core.
pub(super) struct JsLive<'js>(pub(super) Value<'js>);

impl<'js> JsLive<'js> {
  fn ctx(&self) -> &Ctx<'js> {
    self.0.ctx()
  }

  fn well_known_symbol(&self, name: &str) -> rquickjs::Result<Atom<'js>> {
    let symbol: Object<'js> = self.ctx().globals().get("Symbol")?;
    let key: Value<'js> = symbol.get(name)?;
    Atom::from_value(self.ctx().clone(), &key)
  }

  fn json(&self) -> Option<JsonValue> {
    serde_from_js(self.ctx(), self.0.clone()).ok()
  }
}

impl<'js> LiveValue for JsLive<'js> {
  type Error = rquickjs::Error;

  fn js_type(&self) -> JsType {
    use rquickjs::Type;
    match self.0.type_of() {
      Type::Undefined | Type::Uninitialized | Type::Unknown => JsType::Undefined,
      Type::Null => JsType::Null,
      Type::Bool => JsType::Boolean,
      Type::Int | Type::Float => JsType::Number,
      Type::BigInt => JsType::BigInt,
      Type::String => JsType::String,
      Type::Symbol => JsType::Symbol,
      Type::Function | Type::Constructor => JsType::Function,
      Type::Array => JsType::Array,
      Type::Object | Type::Promise | Type::Exception | Type::Proxy | Type::Module => JsType::Object,
    }
  }

  fn same_value(&self, other: &Self) -> rquickjs::Result<bool> {
    use rquickjs::Type;
    match (self.0.type_of(), other.0.type_of()) {
      (Type::Undefined | Type::Uninitialized, Type::Undefined | Type::Uninitialized) | (Type::Null, Type::Null) => {
        Ok(true)
      },
      (Type::Bool, Type::Bool) => Ok(self.0.as_bool() == other.0.as_bool()),
      (Type::Int, Type::Int) => Ok(self.0.as_int() == other.0.as_int()),
      (Type::Int | Type::Float, Type::Int | Type::Float) => {
        let (Some(a), Some(b)) = (self.0.as_number(), other.0.as_number()) else {
          return Ok(false);
        };
        Ok((a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits())
      },
      (Type::String, Type::String) => {
        let (Some(a), Some(b)) = (self.0.as_string(), other.0.as_string()) else {
          return Ok(false);
        };
        Ok(a.to_string()? == b.to_string()?)
      },
      (a, b) if a != b => Ok(false),
      _ => {
        let object: Object<'js> = self.ctx().globals().get("Object")?;
        let is: Function<'js> = object.get("is")?;
        is.call((self.0.clone(), other.0.clone()))
      },
    }
  }

  fn structurally_equal(&self, other: &Self) -> bool {
    match (self.json(), other.json()) {
      (Some(a), Some(b)) => deep_equal(&a, &b),
      _ => false,
    }
  }

  fn truthy(&self) -> bool {
    match self.js_type() {
      JsType::Undefined | JsType::Null => false,
      JsType::Boolean => self.0.as_bool().unwrap_or(false),
      JsType::Number => self.0.as_number().is_some_and(|n| n != 0.0 && !n.is_nan()),
      JsType::BigInt => self
        .0
        .as_big_int()
        .cloned()
        .and_then(|b| b.to_i64().ok())
        .is_none_or(|v| v != 0),
      JsType::String => self
        .0
        .as_string()
        .and_then(|s| s.to_string().ok())
        .is_some_and(|s| !s.is_empty()),
      _ => true,
    }
  }

  fn number(&self) -> Option<f64> {
    (self.js_type() == JsType::Number).then(|| self.0.as_number()).flatten()
  }

  fn text(&self) -> Option<String> {
    self.0.as_string().and_then(|s| s.to_string().ok())
  }

  fn length(&self) -> rquickjs::Result<Option<f64>> {
    // A JS string's `.length` counts UTF-16 code units, not characters.
    if let Some(s) = self.text() {
      return Ok(Some(crate::bindings::convert::usize_to_f64(s.encode_utf16().count())));
    }
    let Some(obj) = self.0.as_object() else {
      return Ok(None);
    };
    let len: Value<'js> = obj.get("length")?;
    Ok(len.as_number())
  }

  fn spread(&self) -> rquickjs::Result<Option<Vec<Self>>> {
    let Some(obj) = self.0.as_object() else {
      return Ok(None);
    };
    let iterator = self.well_known_symbol("iterator")?;
    let factory: Value<'js> = obj.get(iterator)?;
    if !factory.is_function() {
      return Ok(None);
    }
    let mut out = Vec::new();
    let iter: rquickjs::JsIterator<'js, Value<'js>> = rquickjs::FromJs::from_js(self.ctx(), self.0.clone())?;
    for item in iter {
      out.push(Self(item?));
    }
    Ok(Some(out))
  }

  fn instance_of(&self, ctor: &Self) -> rquickjs::Result<bool> {
    // `v instanceof C` IS `C[Symbol.hasInstance](v)` — every function
    // inherits the ordinary implementation from `Function.prototype`,
    // and a class defining its own is honoured for free.
    let Some(ctor_obj) = ctor.0.as_object() else {
      return Ok(false);
    };
    let has_instance = self.well_known_symbol("hasInstance")?;
    let check: Function<'js> = ctor_obj.get(has_instance)?;
    check.call((rquickjs::function::This(ctor.0.clone()), self.0.clone()))
  }

  fn describe(&self) -> String {
    let mut out = String::new();
    if ferrijs::std::node::inspect::Inspector::new(false)
      .quoted()
      .value(&mut out, &self.0, 0)
      .is_err()
    {
      return self.0.type_name().to_string();
    }
    out
  }

  fn shape(&self) -> rquickjs::Result<ferridriver_expect::Shape<Self>> {
    use ferridriver_expect::Shape;
    let Some(obj) = self.0.as_object() else {
      return Ok(Shape::Primitive);
    };
    if self.0.is_function() {
      return Ok(Shape::Function);
    }
    // `Object.prototype.toString` is how JS itself tells these apart,
    // and it is not fooled by a subclass or a missing prototype.
    match self.brand()?.as_str() {
      "Date" => {
        let get_time: Function<'js> = obj.get("getTime")?;
        let millis: f64 = get_time.call((rquickjs::function::This(self.0.clone()),))?;
        return Ok(Shape::Date(millis));
      },
      "RegExp" => {
        return Ok(Shape::RegExp {
          source: obj.get::<_, String>("source")?,
          flags: obj.get::<_, String>("flags")?,
        });
      },
      "Error" => {
        return Ok(Shape::Error {
          name: obj.get::<_, Option<String>>("name")?.unwrap_or_default(),
          message: obj.get::<_, Option<String>>("message")?.unwrap_or_default(),
        });
      },
      "Map" => {
        let mut entries = Vec::new();
        let iter: rquickjs::JsIterator<'js, Value<'js>> = rquickjs::FromJs::from_js(self.ctx(), self.0.clone())?;
        for pair in iter {
          let pair = pair?;
          let pair = pair
            .as_array()
            .cloned()
            .ok_or_else(|| type_error(self.ctx(), "expect.toEqual: a Map entry was not a [key, value] pair"))?;
          entries.push((Self(pair.get::<Value<'js>>(0)?), Self(pair.get::<Value<'js>>(1)?)));
        }
        return Ok(Shape::Map(entries));
      },
      "Set" => {
        let mut values = Vec::new();
        let iter: rquickjs::JsIterator<'js, Value<'js>> = rquickjs::FromJs::from_js(self.ctx(), self.0.clone())?;
        for value in iter {
          values.push(Self(value?));
        }
        return Ok(Shape::Set(values));
      },
      _ => {},
    }
    if let Some(bytes) = self.bytes()? {
      return Ok(Shape::Bytes(bytes));
    }
    if let Some(arr) = self.0.as_array() {
      // A hole is an index the array does not own, which is not the
      // same as an index holding `undefined`.
      let has_own: Function<'js> = self.ctx().globals().get::<_, Object<'js>>("Object")?.get("hasOwn")?;
      let mut items = Vec::with_capacity(arr.len());
      for i in 0..arr.len() {
        let present: bool = has_own.call((self.0.clone(), i.to_string()))?;
        items.push(if present {
          Some(Self(arr.get::<Value<'js>>(i)?))
        } else {
          None
        });
      }
      return Ok(Shape::Array(items));
    }
    let mut entries = Vec::new();
    for key in obj.keys::<String>() {
      let key = key?;
      let value: Value<'js> = obj.get(key.as_str())?;
      entries.push((key, Self(value)));
    }
    Ok(Shape::Object(entries))
  }

  fn get_prop(&self, key: &str) -> rquickjs::Result<Option<Self>> {
    // A property read, not a structural walk: `toHaveProperty` should
    // see a getter and an inherited field, which own-enumerable-keys
    // would miss.
    let Some(obj) = self.0.as_object() else {
      return Ok(None);
    };
    if !obj.contains_key(key)? {
      return Ok(None);
    }
    Ok(Some(Self(obj.get(key)?)))
  }

  fn class_name(&self) -> Option<String> {
    let ctor: Value<'js> = self.0.as_object()?.get("constructor").ok()?;
    let name: Value<'js> = ctor.as_object()?.get("name").ok()?;
    name.as_string().and_then(|s| s.to_string().ok())
  }

  fn ref_id(&self) -> Option<u64> {
    // The engine needs a stable identity per object to stop a cyclic
    // comparison. rquickjs hashes a `Value` as its tag plus its raw
    // payload — for an object that payload IS the pointer — so a hasher
    // that records rather than mixes hands back the exact bits, with no
    // `unsafe` and no chance of two objects colliding.
    #[derive(Default)]
    struct RawBits {
      tag: i32,
      bits: u64,
    }
    impl std::hash::Hasher for RawBits {
      fn finish(&self) -> u64 {
        self.bits
      }
      fn write(&mut self, _bytes: &[u8]) {}
      fn write_i32(&mut self, value: i32) {
        self.tag = value;
      }
      fn write_u64(&mut self, value: u64) {
        self.bits = value;
      }
    }
    self.0.as_object()?;
    let mut hasher = RawBits::default();
    std::hash::Hash::hash(&self.0, &mut hasher);
    Some(std::hash::Hasher::finish(&hasher))
  }

  fn as_json(&self) -> Option<JsonValue> {
    self.json()
  }
}

impl<'js> JsLive<'js> {
  /// `Object.prototype.toString.call(v)` reduced to its brand, which is
  /// how JS distinguishes a Date from a Map from a plain object.
  fn brand(&self) -> rquickjs::Result<String> {
    let object: Object<'js> = self.ctx().globals().get("Object")?;
    let proto: Object<'js> = object.get("prototype")?;
    let to_string: Function<'js> = proto.get("toString")?;
    let tag: String = to_string.call((rquickjs::function::This(self.0.clone()),))?;
    Ok(tag.trim_start_matches("[object ").trim_end_matches(']').to_string())
  }

  /// An `ArrayBuffer` or a typed array as its bytes, so two views with
  /// the same contents compare equal.
  fn bytes(&self) -> rquickjs::Result<Option<Vec<u8>>> {
    let Some(obj) = self.0.as_object() else {
      return Ok(None);
    };
    if let Some(buffer) = rquickjs::ArrayBuffer::from_object(obj.clone()) {
      return Ok(crate::bindings::convert::array_buffer_bytes(&buffer, 0, usize::MAX));
    }
    let brand = self.brand()?;
    if !brand.ends_with("Array") || brand == "Array" {
      return Ok(None);
    }
    let Ok(buffer) = obj.get::<_, Value<'js>>("buffer") else {
      return Ok(None);
    };
    let Some(buffer) = buffer.as_object().cloned().and_then(rquickjs::ArrayBuffer::from_object) else {
      return Ok(None);
    };
    let offset = crate::bindings::convert::f64_to_usize(obj.get::<_, Option<f64>>("byteOffset")?.unwrap_or(0.0));
    let length = crate::bindings::convert::f64_to_usize(obj.get::<_, Option<f64>>("byteLength")?.unwrap_or(0.0));
    Ok(crate::bindings::convert::array_buffer_bytes(&buffer, offset, length))
  }
}
