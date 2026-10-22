//! Canonical `ferridriver` host object.
//!
//! Existing globals (`page`, `Given`, `tools`, ...) stay for
//! compatibility, but new API surface should also hang off this object
//! so scripts have one stable namespace and the virtual `"ferridriver"`
//! module can simply re-export it.

use rquickjs::{Ctx, Object, Value};

/// Get or create `globalThis.ferridriver`.
pub fn ensure_ferridriver<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
  let g = ctx.globals();
  if let Ok(fd) = g.get::<_, Object<'js>>("ferridriver") {
    return Ok(fd);
  }
  let fd = Object::new(ctx.clone())?;
  g.set("ferridriver", fd.clone())?;
  Ok(fd)
}

/// Install/update `ferridriver.host`.
pub fn install_host(ctx: &Ctx<'_>, host: &str) -> rquickjs::Result<()> {
  ensure_ferridriver(ctx)?.set("host", host)
}

/// Copy a global binding onto `ferridriver.<name>` if the global exists.
pub fn mirror_global(ctx: &Ctx<'_>, name: &str) -> rquickjs::Result<()> {
  let g = ctx.globals();
  if let Ok(v) = g.get::<_, Value<'_>>(name) {
    ensure_ferridriver(ctx)?.set(name, v)?;
  }
  Ok(())
}

/// Set `ferridriver.<name> = value`.
pub fn set<'js, V>(ctx: &Ctx<'js>, name: &str, value: V) -> rquickjs::Result<()>
where
  V: rquickjs::IntoJs<'js>,
{
  ensure_ferridriver(ctx)?.set(name, value)
}

#[derive(serde::Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct TotpOptions {
  digits: Option<u32>,
  period: Option<u64>,
  algorithm: Option<String>,
  /// Unix seconds; now when absent.
  timestamp: Option<u64>,
}

/// `ferridriver.totp(secret, options?)`: the current code for a base32
/// secret or an `otpauth://totp/` link.
fn totp<'js>(ctx: Ctx<'js>, secret: String, options: rquickjs::function::Opt<Value<'js>>) -> rquickjs::Result<String> {
  let options: TotpOptions = match options.0 {
    Some(value) if !value.is_undefined() && !value.is_null() => crate::bindings::convert::serde_from_js(&ctx, value)?,
    _ => TotpOptions::default(),
  };
  let throw = |e: ferridriver::error::FerriError| crate::bindings::convert::ferri_throw(&ctx, &e);
  let mut totp = ferridriver::totp::Totp::parse(&secret).map_err(throw)?;
  if let Some(digits) = options.digits {
    totp.digits = digits;
  }
  if let Some(period) = options.period {
    totp.period = period;
  }
  if let Some(algorithm) = &options.algorithm {
    totp.algorithm = ferridriver::totp::Algorithm::parse(algorithm).map_err(throw)?;
  }
  match options.timestamp {
    Some(at) => totp.code_at(at),
    None => totp.code(),
  }
  .map_err(throw)
}

pub fn install_totp(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  set(ctx, "totp", rquickjs::Function::new(ctx.clone(), totp)?)
}
