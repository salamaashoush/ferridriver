//! Unit tests for wire-level value serialization and deserialization.

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
fn deserializes_numbers_from_wire_text_exactly() {
  let texts = [
    "107.08281326293945",
    "230.86153411865234",
    "119.41610336303711",
    "986.6722412109375",
    "217.92601013183594",
  ];
  let exact = |text: &str| text.parse::<f64>().unwrap().to_bits();
  for text in texts {
    let decoded = serde_json::from_str::<SerializedValue>(text).unwrap();
    assert_eq!(decoded.as_number().map(f64::to_bits), Some(exact(text)), "{text}");
  }
  let wire = format!("{{\"a\":[{}],\"id\":1}}", texts.join(","));
  let SerializedValue::Array { items, .. } = serde_json::from_str::<SerializedValue>(&wire).unwrap() else {
    panic!("{wire} decodes as an array");
  };
  assert_eq!(items.len(), texts.len());
  for (item, text) in items.iter().zip(texts) {
    assert_eq!(
      item.as_number().map(f64::to_bits),
      Some(exact(text)),
      "{text} in an array"
    );
  }
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
    err.to_string().contains("no recognised tag"),
    "unexpected error message: {err}"
  );
}

#[test]
fn deserializes_id_before_collection_and_rejects_missing_id() {
  let from_str = serde_json::from_str::<SerializedValue>(r#"{"id":7,"a":[1,{"id":8,"o":[{"k":"x","v":"ok"}]}]}"#)
    .expect("id before collection");
  assert_eq!(
    from_str,
    SerializedValue::Array {
      id: 7,
      items: vec![
        SerializedValue::Number(1.0),
        SerializedValue::Object {
          id: 8,
          entries: vec![PropertyEntry {
            k: "x".into(),
            v: SerializedValue::Str("ok".into()),
          }],
        },
      ],
    }
  );
  let err = serde_json::from_str::<SerializedValue>(r#"{"a":[1,2]}"#).unwrap_err();
  assert!(err.to_string().contains("must be paired with numeric id"), "{err}");
  let err = serde_json::from_str::<SerializedValue>(r#"{"id":1}"#).unwrap_err();
  assert!(err.to_string().contains("no recognised tag"), "{err}");
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
fn function_crosses_as_its_binding_name() {
  let value = SerializedValue::Function("__pw_fn_1".into());
  assert_eq!(serde_json::to_value(&value).unwrap(), json!({"fn": "__pw_fn_1"}));
  assert_eq!(
    serde_json::from_value::<SerializedValue>(json!({"fn": "__pw_fn_1"})).unwrap(),
    value
  );
}

#[test]
fn js_source_rebuilds_rich_and_cyclic_values() {
  let value = SerializedValue::Object {
    id: 1,
    entries: vec![
      PropertyEntry {
        k: "self".into(),
        v: SerializedValue::Reference(1),
      },
      PropertyEntry {
        k: "when".into(),
        v: SerializedValue::Date("1970-01-01T00:00:00.000Z".into()),
      },
      PropertyEntry {
        k: "cb".into(),
        v: SerializedValue::Function("__pw_fn_2".into()),
      },
      PropertyEntry {
        k: "nz".into(),
        v: SerializedValue::Special(SpecialValue::NegZero),
      },
    ],
  };
  assert_eq!(
    value.to_js_source().unwrap(),
    "(r => (r[1] = {}, r[1][\"self\"] = r[1], r[1][\"when\"] = new Date(\"1970-01-01T00:00:00.000Z\"), \
     r[1][\"cb\"] = ((...args) => globalThis[\"__playwright__binding__controller__\"].callBinding(\"__pw_fn_2\", ...args)), \
     r[1][\"nz\"] = (-0), r[1]))({})"
  );
  assert!(SerializedValue::Handle(0).to_js_source().is_err());
}

#[test]
fn serialized_argument_omits_empty_handles() {
  let arg = SerializedArgument {
    value: SerializedValue::Number(1.0),
    handles: vec![],
    functions: vec![],
  };
  let s = serde_json::to_string(&arg).unwrap();
  assert_eq!(s, r#"{"value":1.0}"#);
}

#[test]
fn serialized_argument_carries_handle_list() {
  let arg = SerializedArgument {
    value: SerializedValue::array(1, vec![SerializedValue::handle(0), SerializedValue::handle(1)]),
    handles: vec![
      HandleId::Cdp {
        object_id: "obj-1".into(),
        session_id: Some("session-sashoush".into()),
      },
      HandleId::Bidi {
        shared_id: "shared-1".into(),
        handle: None,
      },
    ],
    functions: vec![],
  };
  let wire = serde_json::to_value(&arg).unwrap();
  assert_eq!(
    wire,
    json!({
      "value": {"a": [{"h": 0}, {"h": 1}], "id": 1},
      "handles": [
        {"Cdp": {"object_id": "obj-1", "session_id": "session-sashoush"}},
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
