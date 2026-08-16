//! `node:util`.
//!
//! Rendering (`format`, `formatWithOptions`, `inspect`) runs through the
//! one [`Inspector`](super::inspect::Inspector) the `console` global uses,
//! so a value prints the same wherever it is printed.
//!
//! The wrappers (`promisify`, `callbackify`, `deprecate`) hold their target
//! through `Function.prototype.bind` rather than a Rust closure: a native
//! closure that captured a JS value would form a GC cycle the runtime
//! cannot trace, which aborts at teardown.

use rquickjs::function::{Func, Opt, Rest, This};
use rquickjs::{Ctx, Function, Object, Promise, Result, Value};

use super::deep_equal::{Mode, deep_equal};
use super::inspect::{Inspector, MAX_DIR_DEPTH};

/// `Function.prototype.bind`, with `this` left undefined.
fn bind<'js>(ctx: &Ctx<'js>, target: &Function<'js>, args: Vec<Value<'js>>) -> Result<Function<'js>> {
  let bind_fn: Function<'js> = target.get("bind")?;
  let mut call_args = rquickjs::function::Args::new(ctx.clone(), args.len() + 1);
  call_args.this(target.clone())?;
  call_args.push_arg(Value::new_undefined(ctx.clone()))?;
  for arg in args {
    call_args.push_arg(arg)?;
  }
  call_args.apply(&bind_fn)
}

fn options_of<'js>(options: &Opt<Value<'js>>) -> Option<Object<'js>> {
  options.0.as_ref().and_then(|v| v.as_object().cloned())
}

/// `depth` from an options bag, with Node's `null` meaning "as deep as it
/// goes" (bounded, so a huge graph cannot wedge the renderer).
fn depth_of(options: Option<&Object<'_>>) -> usize {
  let Some(options) = options else {
    return 2;
  };
  match options.get::<_, Value<'_>>("depth") {
    Ok(v) if v.is_null() => MAX_DIR_DEPTH,
    Ok(v) => v.as_number().map_or(2, |n| n.max(0.0) as usize),
    Err(_) => 2,
  }
}

fn colors_of(options: Option<&Object<'_>>) -> bool {
  options.and_then(|o| o.get::<_, bool>("colors").ok()).unwrap_or(false)
}

fn format_args(args: &[Value<'_>], colors: bool) -> Result<String> {
  let mut out = String::new();
  Inspector::new(colors).args(&mut out, args)?;
  Ok(out)
}

fn inspect<'js>(value: Value<'js>, options: Opt<Value<'js>>) -> Result<String> {
  let options = options_of(&options);
  let mut out = String::new();
  Inspector::new(colors_of(options.as_ref()))
    .with_depth(depth_of(options.as_ref()))
    .quoted()
    .value(&mut out, &value, 0)?;
  Ok(out)
}

/// The callback half of a promisified call: `(resolve, reject, err, value)`,
/// with the first two bound in.
fn settle_promise<'js>(
  resolve: Function<'js>,
  reject: Function<'js>,
  err: Value<'js>,
  rest: Rest<Value<'js>>,
) -> Result<()> {
  if err.is_null() || err.is_undefined() {
    let value = rest.0.into_iter().next();
    match value {
      Some(v) => resolve.call::<_, ()>((v,)),
      None => resolve.call::<_, ()>(()),
    }
  } else {
    reject.call::<_, ()>((err,))
  }
}

/// The body of a promisified function: `(original, ...args)` with the
/// original bound in.
fn promisified<'js>(
  ctx: Ctx<'js>,
  this: This<Value<'js>>,
  original: Function<'js>,
  args: Rest<Value<'js>>,
) -> Result<Promise<'js>> {
  let (promise, resolve, reject) = ctx.promise()?;
  let settle = Function::new(ctx.clone(), settle_promise)?;
  let callback = bind(&ctx, &settle, vec![resolve.into_value(), reject.into_value()])?;

  let mut call = rquickjs::function::Args::new(ctx.clone(), args.0.len() + 1);
  call.this(this.0)?;
  for arg in args.0 {
    call.push_arg(arg)?;
  }
  call.push_arg(callback)?;
  call.apply::<()>(&original)?;
  Ok(promise)
}

fn promisify<'js>(ctx: Ctx<'js>, original: Function<'js>) -> Result<Function<'js>> {
  // Node honours a `util.promisify.custom` implementation on the target.
  if let Ok(custom) = original.get::<_, Function<'js>>("__promisify__") {
    return Ok(custom);
  }
  let body = Function::new(ctx.clone(), promisified)?;
  bind(&ctx, &body, vec![original.into_value()])
}

/// The settle half of a callbackified call: `(callback, is_error, value)`,
/// with the first two bound in.
fn settle_callback<'js>(callback: Function<'js>, is_error: bool, value: Value<'js>) -> Result<()> {
  if is_error {
    callback.call::<_, ()>((value,))
  } else {
    let ctx = callback.ctx().clone();
    callback.call::<_, ()>((Value::new_null(ctx), value))
  }
}

/// The body of a callbackified function: `(original, ...args, callback)`
/// with the original bound in.
fn callbackified<'js>(ctx: Ctx<'js>, this: This<Value<'js>>, original: Function<'js>, args: Rest<Value<'js>>) -> Result<()> {
  let mut args = args.0;
  let callback: Function<'js> = match args.pop().and_then(|v| v.as_function().cloned()) {
    Some(f) => f,
    None => {
      return Err(rquickjs::Exception::throw_type(
        &ctx,
        "The last argument must be of type function",
      ));
    },
  };

  let mut call = rquickjs::function::Args::new(ctx.clone(), args.len());
  call.this(this.0)?;
  for arg in args {
    call.push_arg(arg)?;
  }
  let promise: Promise<'js> = call.apply(&original)?;

  let settle = Function::new(ctx.clone(), settle_callback)?;
  let on_ok = bind(&ctx, &settle, vec![callback.clone().into_value(), Value::new_bool(ctx.clone(), false)])?;
  let on_err = bind(&ctx, &settle, vec![callback.into_value(), Value::new_bool(ctx.clone(), true)])?;
  let then: Function<'js> = promise.get("then")?;
  then.call::<_, ()>((This(promise), on_ok, on_err))
}

fn callbackify<'js>(ctx: Ctx<'js>, original: Function<'js>) -> Result<Function<'js>> {
  let body = Function::new(ctx.clone(), callbackified)?;
  bind(&ctx, &body, vec![original.into_value()])
}

/// The body of a deprecated function: `(original, message, ...args)` with
/// the first two bound in. The warning fires once per wrapped function, as
/// Node's does.
fn deprecated<'js>(
  ctx: Ctx<'js>,
  this: This<Value<'js>>,
  original: Function<'js>,
  message: String,
  args: Rest<Value<'js>>,
) -> Result<Value<'js>> {
  if original.get::<_, bool>("__deprecation_warned__").unwrap_or(false) {
    // already warned
  } else {
    original.set("__deprecation_warned__", true)?;
    if let Ok(console) = ctx.globals().get::<_, Object<'js>>("console") {
      if let Ok(warn) = console.get::<_, Function<'js>>("warn") {
        warn.call::<_, ()>((format!("DeprecationWarning: {message}"),))?;
      }
    }
  }
  let mut call = rquickjs::function::Args::new(ctx.clone(), args.0.len());
  call.this(this.0)?;
  for arg in args.0 {
    call.push_arg(arg)?;
  }
  call.apply(&original)
}

fn deprecate<'js>(ctx: Ctx<'js>, original: Function<'js>, message: String) -> Result<Function<'js>> {
  let body = Function::new(ctx.clone(), deprecated)?;
  bind(
    &ctx,
    &body,
    vec![original.into_value(), rquickjs::String::from_str(ctx.clone(), &message)?.into_value()],
  )
}

fn is_deep_strict_equal<'js>(a: Value<'js>, b: Value<'js>) -> Result<bool> {
  deep_equal(&a, &b, Mode::Strict)
}

/// `util.inherits`: point one constructor's prototype chain at another's.
fn inherits<'js>(ctor: Function<'js>, super_ctor: Function<'js>) -> Result<()> {
  let super_proto: Object<'js> = super_ctor.get("prototype")?;
  let proto: Object<'js> = ctor.get("prototype")?;
  proto.set_prototype(Some(&super_proto))?;
  ctor.set("super_", super_ctor)?;
  Ok(())
}

/// `Object.prototype.toString.call(value)`, the tag every `util.types`
/// predicate is defined in terms of.
fn tag_of(value: &Value<'_>) -> Result<String> {
  let object: Object<'_> = value.ctx().globals().get("Object")?;
  let proto: Object<'_> = object.get("prototype")?;
  let to_string: Function<'_> = proto.get("toString")?;
  to_string.call((This(value.clone()),))
}

const TYPED_ARRAY_TAGS: &[&str] = &[
  "[object Int8Array]",
  "[object Uint8Array]",
  "[object Uint8ClampedArray]",
  "[object Int16Array]",
  "[object Uint16Array]",
  "[object Int32Array]",
  "[object Uint32Array]",
  "[object Float16Array]",
  "[object Float32Array]",
  "[object Float64Array]",
  "[object BigInt64Array]",
  "[object BigUint64Array]",
];

fn types_object<'js>(ctx: &Ctx<'js>) -> Result<Object<'js>> {
  let types = Object::new(ctx.clone())?;
  let tagged = |tag: &'static str| {
    Func::from(move |value: Value<'_>| -> Result<bool> { Ok(tag_of(&value)? == tag) })
  };
  types.set("isDate", tagged("[object Date]"))?;
  types.set("isRegExp", tagged("[object RegExp]"))?;
  types.set("isMap", tagged("[object Map]"))?;
  types.set("isSet", tagged("[object Set]"))?;
  types.set("isWeakMap", tagged("[object WeakMap]"))?;
  types.set("isWeakSet", tagged("[object WeakSet]"))?;
  types.set("isPromise", tagged("[object Promise]"))?;
  types.set("isArrayBuffer", tagged("[object ArrayBuffer]"))?;
  types.set("isSharedArrayBuffer", tagged("[object SharedArrayBuffer]"))?;
  types.set("isDataView", tagged("[object DataView]"))?;
  types.set("isNativeError", tagged("[object Error]"))?;
  types.set("isAsyncFunction", tagged("[object AsyncFunction]"))?;
  types.set("isGeneratorFunction", tagged("[object GeneratorFunction]"))?;
  types.set("isGeneratorObject", tagged("[object Generator]"))?;
  types.set("isArgumentsObject", tagged("[object Arguments]"))?;
  types.set(
    "isTypedArray",
    Func::from(|value: Value<'_>| -> Result<bool> { Ok(TYPED_ARRAY_TAGS.contains(&tag_of(&value)?.as_str())) }),
  )?;
  types.set(
    "isBoxedPrimitive",
    Func::from(|value: Value<'_>| -> Result<bool> {
      if !value.is_object() {
        return Ok(false);
      }
      let tag = tag_of(&value)?;
      Ok(matches!(
        tag.as_str(),
        "[object String]" | "[object Number]" | "[object Boolean]" | "[object Symbol]" | "[object BigInt]"
      ))
    }),
  )?;
  Ok(types)
}

/// Every `util` export on one object, for both the ES module and the
/// `require` namespace.
///
/// # Errors
///
/// Propagates the property writes and the global reads it makes.
pub fn util_object<'js>(ctx: &Ctx<'js>) -> Result<Object<'js>> {
  let util = Object::new(ctx.clone())?;

  util.set(
    "format",
    Func::from(|args: Rest<Value<'_>>| -> Result<String> { format_args(&args.0, false) }),
  )?;
  util.set(
    "formatWithOptions",
    Func::from(|options: Opt<Value<'_>>, args: Rest<Value<'_>>| -> Result<String> {
      format_args(&args.0, colors_of(options_of(&options).as_ref()))
    }),
  )?;

  let inspect_fn = Function::new(ctx.clone(), inspect)?.with_name("inspect")?;
  // `util.inspect.custom` — the symbol a class implements to control how it
  // renders. Exposed so third-party code can read it; the renderer does not
  // call it yet.
  let symbol: Object<'js> = ctx.globals().get("Symbol")?;
  let symbol_for: Function<'js> = symbol.get("for")?;
  let custom: Value<'js> = symbol_for.call(("nodejs.util.inspect.custom",))?;
  inspect_fn.set("custom", custom)?;
  util.set("inspect", inspect_fn)?;

  util.set("promisify", Func::from(promisify))?;
  util.set("callbackify", Func::from(callbackify))?;
  util.set("deprecate", Func::from(deprecate))?;
  util.set("inherits", Func::from(inherits))?;
  util.set("types", types_object(ctx)?)?;
  util.set(
    "isDeepStrictEqual",
    Func::from(is_deep_strict_equal),
  )?;

  // The text codecs are web-platform globals this runtime already installs;
  // `util` re-exports the same objects rather than defining its own.
  for name in ["TextEncoder", "TextDecoder"] {
    if let Ok(class) = ctx.globals().get::<_, Value<'js>>(name) {
      if !class.is_undefined() {
        util.set(name, class)?;
      }
    }
  }

  Ok(util)
}

/// The names [`util_object`] sets, for a module's export list.
pub const UTIL_MEMBERS: &[&str] = &[
  "TextDecoder",
  "TextEncoder",
  "callbackify",
  "deprecate",
  "format",
  "formatWithOptions",
  "inherits",
  "inspect",
  "isDeepStrictEqual",
  "promisify",
  "types",
];
