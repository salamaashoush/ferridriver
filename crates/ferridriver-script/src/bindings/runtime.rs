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
  match g.get::<_, Object<'js>>("ferridriver") {
    Ok(fd) => Ok(fd),
    Err(_) => {
      let fd = Object::new(ctx.clone())?;
      g.set("ferridriver", fd.clone())?;
      Ok(fd)
    },
  }
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
