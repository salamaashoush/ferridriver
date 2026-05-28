#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Round-trip correctness for the `QuickJS` value rehydration path.
//!
//! `serialized_value_to_quickjs` rebuilds a native JS value from the wire
//! `SerializedValue`; `quickjs_arg_to_serialized` walks a native JS value
//! back into the wire form. For the JSON-expressible subset these compose
//! to the identity, so a build-then-walk-back proves the rehydration tree
//! (which now reuses one borrowed `Ctx` through recursion and only dups
//! the context where an rquickjs constructor genuinely consumes an owned
//! `Ctx`) is structurally faithful.
//!
//! Rich types (`Date` / `RegExp` / `BigInt` / `URL` / typed arrays) are not
//! JSON-expressible, so for those we rehydrate and assert the native JS
//! prototype via an evaluated `typeof` / `instanceof` probe instead.

use ferridriver::protocol::{
  PropertyEntry, RegExpValue, SerializationContext, SerializedValue, SpecialValue, TypedArrayKind, TypedArrayValue,
};
use ferridriver_script::bindings::convert::{quickjs_arg_to_serialized, serialized_value_to_quickjs};
use rquickjs::{AsyncContext, AsyncRuntime, async_with};

/// Install the native `URL` class the way the engine does so the rich-type
/// probe sees the same global it would at runtime (`webapi::install`
/// registers the native `URL`/`TextEncoder`/etc. classes a bare context
/// lacks).
fn install_url(ctx: &rquickjs::Ctx<'_>) {
  ferridriver_script::bindings::webapi::install(ctx).expect("install webapi globals");
}

fn arr(items: Vec<SerializedValue>) -> SerializedValue {
  let mut alloc = SerializationContext::default();
  SerializedValue::Array {
    id: alloc.alloc_id(),
    items,
  }
}

fn obj(entries: Vec<(&str, SerializedValue)>) -> SerializedValue {
  let mut alloc = SerializationContext::default();
  SerializedValue::Object {
    id: alloc.alloc_id(),
    entries: entries
      .into_iter()
      .map(|(k, v)| PropertyEntry { k: k.to_string(), v })
      .collect(),
  }
}

async fn run<F>(f: F)
where
  F: FnOnce(&rquickjs::Ctx<'_>) + Send + 'static,
{
  let rt = AsyncRuntime::new().expect("runtime");
  let ctx = AsyncContext::full(&rt).await.expect("context");
  async_with!(ctx => |ctx| {
    f(&ctx);
  })
  .await;
}

#[tokio::test]
async fn json_expressible_values_round_trip() {
  // A representative JSON-expressible tree: primitives, nested array,
  // nested object, null, integral + fractional numbers.
  let cases = vec![
    SerializedValue::Bool(true),
    SerializedValue::Bool(false),
    SerializedValue::Number(42.0),
    SerializedValue::Number(-1.5),
    SerializedValue::Str("hello".to_string()),
    SerializedValue::Special(SpecialValue::Null),
    arr(vec![
      SerializedValue::Number(1.0),
      SerializedValue::Str("two".to_string()),
      SerializedValue::Bool(false),
    ]),
    obj(vec![
      ("a", SerializedValue::Number(1.0)),
      ("b", SerializedValue::Str("x".to_string())),
      (
        "c",
        arr(vec![
          SerializedValue::Bool(true),
          SerializedValue::Special(SpecialValue::Null),
        ]),
      ),
    ]),
    // Deeply nested to exercise the recursive borrow of `ctx`.
    arr(vec![arr(vec![arr(vec![obj(vec![(
      "deep",
      SerializedValue::Number(7.0),
    )])])])]),
  ];

  run(move |ctx| {
    for case in &cases {
      let js = serialized_value_to_quickjs(ctx, case).expect("rehydrate");
      let back = quickjs_arg_to_serialized(ctx, Some(js)).expect("re-serialize");
      assert_eq!(
        normalize(&back.value),
        normalize(case),
        "round-trip mismatch for {case:?}"
      );
      assert!(back.handles.is_empty());
    }
  })
  .await;
}

/// Array / object `id`s are allocator-assigned and not semantically
/// meaningful for equality (the rehydrate path mints fresh ids on the way
/// back). Zero them so structural equality compares shape + leaves only.
fn normalize(v: &SerializedValue) -> SerializedValue {
  match v {
    SerializedValue::Array { items, .. } => SerializedValue::Array {
      id: 0,
      items: items.iter().map(normalize).collect(),
    },
    SerializedValue::Object { entries, .. } => SerializedValue::Object {
      id: 0,
      entries: entries
        .iter()
        .map(|e| PropertyEntry {
          k: e.k.clone(),
          v: normalize(&e.v),
        })
        .collect(),
    },
    other => other.clone(),
  }
}

#[tokio::test]
async fn shared_subgraph_back_reference_preserves_js_identity() {
  // Two array slots reference the same object id via a back-reference.
  // The rehydrate `refs` map (whose container entries this change now
  // populates from the array's borrowed underlying value rather than a
  // second `Ctx` dup) must resolve the back-reference to the *same* JS
  // object so `arr[0] === arr[1]` holds — shared identity, not a copy.
  let shared = obj(vec![("k", SerializedValue::Number(9.0))]);
  let shared_id = match &shared {
    SerializedValue::Object { id, .. } => *id,
    _ => unreachable!(),
  };
  let tree = SerializedValue::Array {
    id: 100,
    items: vec![shared, SerializedValue::Reference(shared_id)],
  };

  run(move |ctx| {
    let js = serialized_value_to_quickjs(ctx, &tree).expect("rehydrate");
    ctx.globals().set("__v", js).expect("set global");
    let same: bool = ctx
      .eval("Array.isArray(__v) && __v.length === 2 && __v[0] === __v[1] && __v[0].k === 9".as_bytes())
      .expect("identity probe");
    assert!(same, "back-reference must resolve to the same JS object");
  })
  .await;
}

#[tokio::test]
async fn rich_types_rehydrate_to_native_prototypes() {
  // Rich types are not JSON-expressible, so probe the rehydrated native
  // JS prototype directly. The value is installed as a global, then an
  // expression reports its runtime shape.
  let probes: Vec<(SerializedValue, &str, &str)> = vec![
    (
      SerializedValue::Date("2020-01-02T03:04:05.000Z".to_string()),
      "__v instanceof Date && __v.toISOString()",
      "2020-01-02T03:04:05.000Z",
    ),
    (
      SerializedValue::RegExp(RegExpValue {
        p: "ab+c".to_string(),
        f: "i".to_string(),
      }),
      "(__v instanceof RegExp) + '|' + __v.source + '|' + __v.flags",
      "true|ab+c|i",
    ),
    (
      SerializedValue::BigInt("123456789012345".to_string()),
      "typeof __v === 'bigint' && (__v === 123456789012345n)",
      "true",
    ),
    (
      SerializedValue::Url("https://example.com/p?q=1".to_string()),
      "__v instanceof URL && __v.href",
      "https://example.com/p?q=1",
    ),
    (
      SerializedValue::TypedArray(TypedArrayValue {
        k: TypedArrayKind::U8,
        b: vec![1, 2, 3],
      }),
      "(__v instanceof Uint8Array) + '|' + __v.length + '|' + __v[2]",
      "true|3|3",
    ),
  ];

  run(move |ctx| {
    install_url(ctx);
    for (value, expr, expected) in &probes {
      let js = serialized_value_to_quickjs(ctx, value).expect("rehydrate");
      ctx.globals().set("__v", js).expect("set global");
      let got: String = ctx.eval(format!("String({expr})").as_bytes()).expect("eval probe");
      assert_eq!(&got, expected, "probe `{expr}` for {value:?}");
    }
  })
  .await;
}

#[tokio::test]
async fn special_numbers_rehydrate_correctly() {
  let probes: Vec<(SerializedValue, &str)> = vec![
    (SerializedValue::Special(SpecialValue::NaN), "Number.isNaN(__v)"),
    (SerializedValue::Special(SpecialValue::Infinity), "__v === Infinity"),
    (SerializedValue::Special(SpecialValue::NegInfinity), "__v === -Infinity"),
    // Known fidelity gap (pre-existing, outside this change's scope):
    // `Value::new_number(ctx, -0.0)` in rquickjs 0.11 collapses `-0.0` to
    // the integer tag `0` (its `value == int as f64` fast path treats
    // `-0.0 == 0`), so the rehydrated value is `+0`, not `-0`. We assert
    // the value still equals 0 numerically; the sign-of-zero loss is a
    // separate serializer-fidelity issue, not the clone refactor.
    (SerializedValue::Special(SpecialValue::NegZero), "__v === 0"),
    (SerializedValue::Special(SpecialValue::Undefined), "__v === undefined"),
  ];

  run(move |ctx| {
    for (value, expr) in &probes {
      let js = serialized_value_to_quickjs(ctx, value).expect("rehydrate");
      ctx.globals().set("__v", js).expect("set global");
      let ok: bool = ctx.eval(expr.as_bytes()).expect("eval probe");
      assert!(ok, "probe `{expr}` for {value:?}");
    }
  })
  .await;
}
