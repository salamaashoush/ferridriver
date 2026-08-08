//! WHATWG `URLSearchParams` — native class over an ordered
//! `Vec<(String, String)>`. Replaces `rquickjs-extra-url` with proper
//! `application/x-www-form-urlencoded` decode/encode (via the `url`
//! crate's `form_urlencoded`), which the extras version skipped: it
//! neither percent-decoded input nor encoded `toString()`, and an
//! empty query produced a bogus `("", "")` entry.

use rquickjs::atom::PredefinedAtom;
use rquickjs::function::{Opt, This};
use rquickjs::{Array, Class, Coerced, Ctx, Function, JsLifetime, Object, Value, class::Trace};

use crate::bindings::js_iterator::live_iterator;

#[derive(Default, Clone, Trace, JsLifetime)]
#[rquickjs::class(rename = "URLSearchParams")]
pub struct UrlSearchParams {
  #[qjs(skip_trace)]
  data: Vec<(String, String)>,
}

impl UrlSearchParams {
  /// Build directly from a raw query string (no leading `?`). Used by
  /// the `URL.searchParams` getter so it doesn't need to route through
  /// the JS constructor.
  pub(crate) fn from_query(query: &str) -> Self {
    Self {
      data: form_urlencoded_parse(query),
    }
  }

  /// Reset the list to `query`. Used by `URL` when a write replaces the
  /// whole query (`url.search = …`, `url.href = …`) so the live
  /// `searchParams` object the caller already holds reflects it.
  pub(crate) fn replace_from_query(&mut self, query: &str) {
    self.data = form_urlencoded_parse(query);
  }
}

/// Decode `a=1&b=%20x` into ordered pairs, handling `+` and `%XX`.
fn form_urlencoded_parse(query: &str) -> Vec<(String, String)> {
  url::form_urlencoded::parse(query.as_bytes())
    .map(|(k, v)| (k.into_owned(), v.into_owned()))
    .collect()
}

fn project_entry<'js>(
  ctx: &Ctx<'js>,
  parent: &Class<'js, UrlSearchParams>,
  index: usize,
) -> rquickjs::Result<Option<Value<'js>>> {
  let Some((name, value)) = parent.borrow().data.get(index).cloned() else {
    return Ok(None);
  };
  let pair = Array::new(ctx.clone())?;
  pair.set(0, rquickjs::String::from_str(ctx.clone(), &name)?)?;
  pair.set(1, rquickjs::String::from_str(ctx.clone(), &value)?)?;
  Ok(Some(pair.into_value()))
}

fn project_key<'js>(
  ctx: &Ctx<'js>,
  parent: &Class<'js, UrlSearchParams>,
  index: usize,
) -> rquickjs::Result<Option<Value<'js>>> {
  let Some((name, _)) = parent.borrow().data.get(index).cloned() else {
    return Ok(None);
  };
  Ok(Some(rquickjs::String::from_str(ctx.clone(), &name)?.into_value()))
}

fn project_value<'js>(
  ctx: &Ctx<'js>,
  parent: &Class<'js, UrlSearchParams>,
  index: usize,
) -> rquickjs::Result<Option<Value<'js>>> {
  let Some((_, value)) = parent.borrow().data.get(index).cloned() else {
    return Ok(None);
  };
  Ok(Some(rquickjs::String::from_str(ctx.clone(), &value)?.into_value()))
}

#[rquickjs::methods(rename_all = "camelCase")]
impl UrlSearchParams {
  /// Spec: every platform object carries `Symbol.toStringTag`, so
  /// `Object.prototype.toString.call(x)` reads `[object URLSearchParams]`.
  #[qjs(prop, rename = PredefinedAtom::SymbolToStringTag, configurable)]
  pub fn to_string_tag() -> &'static str {
    "URLSearchParams"
  }

  /// `new URLSearchParams(init?)` — `init` is a query string (optional
  /// leading `?`), an array of `[name, value]` pairs, any iterable of
  /// pairs, or a plain record object.
  #[qjs(constructor)]
  pub fn new(input: Opt<UrlSearchParamsInput<'_>>) -> rquickjs::Result<Self> {
    let Some(input) = input.0 else {
      return Ok(Self::default());
    };
    let data = match input {
      UrlSearchParamsInput::String(s) => {
        let query = s.strip_prefix('?').unwrap_or(&s);
        form_urlencoded_parse(query)
      },
      UrlSearchParamsInput::Array(array) => {
        let mut data = Vec::with_capacity(array.len());
        for it in array.iter::<Array<'_>>() {
          let inner = it?;
          let name = inner.get::<Coerced<String>>(0)?;
          let value = inner.get::<Coerced<String>>(1)?;
          data.push((name.0, value.0));
        }
        data
      },
      UrlSearchParamsInput::Object(obj) => {
        if let Ok(iter_fn) = obj.get::<_, Function<'_>>(PredefinedAtom::SymbolIterator) {
          // Iterable of [name, value] pairs (Map, another
          // URLSearchParams, generators, ...).
          let iterator = iter_fn.call::<_, Object<'_>>((This(obj.clone()),))?;
          let next_fn = iterator.get::<_, Function<'_>>(PredefinedAtom::Next)?;
          let mut data = Vec::new();
          loop {
            let step = next_fn.call::<_, Object<'_>>((This(iterator.clone()),))?;
            if step.get::<_, bool>(PredefinedAtom::Done).unwrap_or(false) {
              break;
            }
            let pair = step.get::<_, Array<'_>>("value")?;
            let name = pair.get::<Coerced<String>>(0)?;
            let value = pair.get::<Coerced<String>>(1)?;
            data.push((name.0, value.0));
          }
          data
        } else {
          // Plain record: own enumerable properties.
          let mut data = Vec::new();
          for it in obj.props::<String, Coerced<String>>() {
            let (name, value) = it?;
            data.push((name, value.0));
          }
          data
        }
      },
    };
    Ok(Self { data })
  }

  #[qjs(rename = PredefinedAtom::SymbolIterator)]
  pub fn iterate<'js>(&self, ctx: Ctx<'js>, this: This<Class<'js, UrlSearchParams>>) -> rquickjs::Result<Object<'js>> {
    live_iterator(&ctx, this.0, project_entry)
  }

  pub fn entries<'js>(&self, ctx: Ctx<'js>, this: This<Class<'js, UrlSearchParams>>) -> rquickjs::Result<Object<'js>> {
    live_iterator(&ctx, this.0, project_entry)
  }

  pub fn keys<'js>(&self, ctx: Ctx<'js>, this: This<Class<'js, UrlSearchParams>>) -> rquickjs::Result<Object<'js>> {
    live_iterator(&ctx, this.0, project_key)
  }

  pub fn values<'js>(&self, ctx: Ctx<'js>, this: This<Class<'js, UrlSearchParams>>) -> rquickjs::Result<Object<'js>> {
    live_iterator(&ctx, this.0, project_value)
  }

  pub fn append(&mut self, name: Coerced<String>, value: Coerced<String>) {
    self.data.push((name.0, value.0));
  }

  /// `delete(name, value?)` — removes matching entries; with `value`
  /// only exact name+value pairs.
  pub fn delete(&mut self, name: Coerced<String>, value: Opt<Coerced<String>>) {
    self.data.retain(|(n, v)| {
      if *n != name.0 {
        return true;
      }
      match &value.0 {
        Some(value) => *v != value.0,
        None => false,
      }
    });
  }

  /// `forEach(cb)` — `cb(value, name, this)` per WHATWG argument
  /// order, iterating the LIVE list by index (mutations from inside
  /// the callback are observed, like Node).
  #[qjs(rename = "forEach")]
  pub fn for_each<'js>(
    &self,
    ctx: Ctx<'js>,
    callback: Function<'js>,
    this: This<Class<'js, UrlSearchParams>>,
  ) -> rquickjs::Result<()> {
    let mut i = 0usize;
    loop {
      let Some((name, value)) = this.0.borrow().data.get(i).cloned() else {
        break;
      };
      callback.call::<_, ()>((
        rquickjs::String::from_str(ctx.clone(), &value)?,
        rquickjs::String::from_str(ctx.clone(), &name)?,
        this.0.clone(),
      ))?;
      i += 1;
    }
    Ok(())
  }

  /// `get(name)` — first value, or `null` (not `undefined`, per spec).
  pub fn get<'js>(&self, ctx: Ctx<'js>, name: Coerced<String>) -> rquickjs::Result<Value<'js>> {
    match self.data.iter().find(|(n, _)| *n == name.0) {
      Some((_, value)) => Ok(rquickjs::String::from_str(ctx, value)?.into_value()),
      None => Ok(Value::new_null(ctx)),
    }
  }

  #[qjs(rename = "getAll")]
  pub fn get_all<'js>(&self, ctx: Ctx<'js>, name: Coerced<String>) -> rquickjs::Result<Vec<rquickjs::String<'js>>> {
    self
      .data
      .iter()
      .filter(|(n, _)| *n == name.0)
      .map(|(_, v)| rquickjs::String::from_str(ctx.clone(), v))
      .collect()
  }

  /// `has(name, value?)`.
  pub fn has(&self, name: Coerced<String>, value: Opt<Coerced<String>>) -> bool {
    self.data.iter().any(|(n, v)| {
      *n == name.0
        && match &value.0 {
          Some(value) => *v == value.0,
          None => true,
        }
    })
  }

  #[qjs(get)]
  pub fn size(&self) -> usize {
    self.data.len()
  }

  /// `set(name, value)` — replace the first match, drop the rest.
  pub fn set(&mut self, name: Coerced<String>, value: Coerced<String>) {
    let mut value = Some(value.0);
    self.data.retain_mut(|(n, v)| {
      if *n != name.0 {
        return true;
      }
      match value.take() {
        Some(new) => {
          *v = new;
          true
        },
        None => false,
      }
    });
    if let Some(new) = value.take() {
      self.data.push((name.0, new));
    }
  }

  /// Stable sort by name, comparing UTF-16 code units (WHATWG order —
  /// differs from Rust's UTF-8 byte order for supplementary-plane vs
  /// U+E000..U+FFFF names).
  pub fn sort(&mut self) {
    self
      .data
      .sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
  }

  /// Serialize as `application/x-www-form-urlencoded`.
  #[qjs(rename = "toString")]
  pub fn to_js_string(&self) -> String {
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in &self.data {
      ser.append_pair(name, value);
    }
    ser.finish()
  }
}

pub enum UrlSearchParamsInput<'js> {
  String(String),
  Array(Array<'js>),
  Object(Object<'js>),
}

impl<'js> rquickjs::FromJs<'js> for UrlSearchParamsInput<'js> {
  fn from_js(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<Self> {
    if let Some(s) = value.as_string() {
      return Ok(Self::String(s.to_string()?));
    }
    if let Some(array) = value.as_array() {
      return Ok(Self::Array(array.clone()));
    }
    if let Some(obj) = value.as_object() {
      return Ok(Self::Object(obj.clone()));
    }
    // WebIDL union fallback: any other value stringifies —
    // `new URLSearchParams(null)` is `"null="` in Node, `(123)` is `"123="`.
    let coerced: Coerced<String> = rquickjs::FromJs::from_js(ctx, value)?;
    Ok(Self::String(coerced.0))
  }
}

pub fn install(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  rquickjs::Class::<UrlSearchParams>::define(&ctx.globals())?;
  Ok(())
}
