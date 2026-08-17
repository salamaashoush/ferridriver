//! Node modules with no usable upstream in llrt, written here so the
//! runtime has exactly one implementation of each.

use rquickjs::{Ctx, Object, Value};

pub mod assert;
pub mod bytes;
pub mod deep_equal;
pub mod inspect;
pub mod path;
pub mod process;
pub mod timers;
pub mod util;

/// Throw an `Error` carrying a specific `name` (`TypeError`,
/// `RangeError`, ...). QuickJS's own constructors are used so the thrown
/// value is a real `Error` with a stack.
pub fn throw_named<'js>(ctx: &Ctx<'js>, name: &str, message: impl Into<String>) -> rquickjs::Error {
  let message = message.into();
  let built: rquickjs::Result<Value<'js>> = (|| {
    let ctor: rquickjs::function::Constructor<'js> = ctx.globals().get("Error")?;
    let err: Object<'js> = ctor.construct((message.as_str(),))?;
    err.set("name", name)?;
    Ok(err.into_value())
  })();
  match built {
    Ok(v) => ctx.throw(v),
    Err(_) => rquickjs::Exception::throw_message(ctx, &message),
  }
}
