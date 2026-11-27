//! Cookie, page-array, and exposed-binding helpers for `BrowserContextJs`.

use std::sync::Arc;

use rquickjs::{Ctx, Value};

use super::BrowserContextJs;
use crate::bindings::convert::{serde_from_js, throw_named, type_error};

/// Pages as the objects that already stand for them.
pub(super) fn page_array<'js>(
  ctx: &Ctx<'js>,
  pages: Vec<Arc<ferridriver::Page>>,
) -> rquickjs::Result<rquickjs::Array<'js>> {
  let array = rquickjs::Array::new(ctx.clone())?;
  for (index, page) in pages.into_iter().enumerate() {
    array.set(index, crate::bindings::identity::page_object(ctx, page)?)?;
  }
  Ok(array)
}

/// The `urls?: string | string[]` of `cookies()`.
pub(crate) fn cookie_urls<'js>(ctx: &Ctx<'js>, urls: Option<Value<'js>>) -> rquickjs::Result<Vec<String>> {
  match urls {
    None => Ok(Vec::new()),
    Some(value) if value.is_undefined() || value.is_null() => Ok(Vec::new()),
    Some(value) => match value.as_string() {
      Some(one) => Ok(vec![one.to_string()?]),
      None => serde_from_js(ctx, value),
    },
  }
}

/// The `options?: { name?, domain?, path? }` of `clearCookies()`, each a
/// `string | RegExp`. `None` when the caller passed no filter at all.
pub(crate) fn clear_cookie_options<'js>(
  ctx: &Ctx<'js>,
  options: Option<Value<'js>>,
  method: &str,
) -> rquickjs::Result<Option<ferridriver::backend::ClearCookieOptions>> {
  let Some(value) = options.filter(|value| !value.is_undefined() && !value.is_null()) else {
    return Ok(None);
  };
  let obj = value
    .as_object()
    .ok_or_else(|| type_error(ctx, format!("{method}: expected an options object")))?;
  let field = |key: &str| -> rquickjs::Result<Option<ferridriver::options::StringOrRegex>> {
    let value: Value<'_> = obj.get(key)?;
    if value.is_undefined() || value.is_null() {
      return Ok(None);
    }
    crate::bindings::page::options::string_or_regex_from_js(&value).map(Some)
  };
  Ok(Some(ferridriver::backend::ClearCookieOptions {
    name: field("name")?,
    domain: field("domain")?,
    path: field("path")?,
  }))
}

impl BrowserContextJs {
  /// Stash `callback` in the shared exposed-callback registry and build an
  /// [`ferridriver::ExposedBinding`] backed by the shared cross-task helper.
  pub(super) fn make_binding<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    callback: rquickjs::Function<'js>,
    with_source: bool,
  ) -> rquickjs::Result<ferridriver::ExposedBinding> {
    let Some(vm) = ferrijs::vm_handle(ctx) else {
      return Err(throw_named(
        ctx,
        "Error",
        "BrowserContext.exposeBinding requires the script engine's VM handle",
      ));
    };
    crate::bindings::page::routing::prepare_exposed_binding(
      ctx,
      vm,
      None,
      "BrowserContext.exposeBinding",
      name,
      callback,
      with_source,
    )
  }

  /// Build the `{ dispose() }` Disposable returned from
  /// `exposeBinding` / `exposeFunction`.
  pub(super) fn make_disposable<'js>(&self, ctx: &Ctx<'js>, name: String) -> rquickjs::Result<Value<'js>> {
    let obj = rquickjs::Object::new(ctx.clone())?;
    let inner = Arc::clone(&self.inner);
    let dispose = rquickjs::Function::new(
      ctx.clone(),
      rquickjs::prelude::Async(move |ctx: Ctx<'_>| {
        crate::bindings::page::remove_exposed_callback(&ctx, &name);
        let inner = Arc::clone(&inner);
        let name = name.clone();
        async move {
          let _ = inner.remove_exposed_binding(&name).await;
        }
      }),
    )?;
    obj.set("dispose", dispose)?;
    rquickjs::IntoJs::into_js(obj, ctx)
  }
}
