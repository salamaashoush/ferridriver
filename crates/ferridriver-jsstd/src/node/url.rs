//! `node:url`.
//!
//! `URL` and `URLSearchParams` are the runtime's own web-platform globals,
//! re-exported rather than re-implemented. What Node adds on top of them —
//! the `file:` path conversions — is here.

use rquickjs::function::Func;
use rquickjs::{Ctx, Exception, Function, Object, Result, Value};

pub const URL_MEMBERS: &[&str] = &["URL", "URLSearchParams", "fileURLToPath", "format", "pathToFileURL"];

/// Percent-decode a `file:` URL's path. Node rejects a URL whose path
/// carries an encoded separator rather than silently producing a path with
/// a literal `/` inside a segment.
fn decode_path(ctx: &Ctx<'_>, path: &str) -> Result<String> {
  let bytes = path.as_bytes();
  let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i] == b'%' && i + 2 < bytes.len() {
      let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
      match u8::from_str_radix(hex, 16) {
        Ok(byte) => {
          if byte == b'/' {
            return Err(Exception::throw_type(
              ctx,
              "File URL path must not include encoded / characters",
            ));
          }
          out.push(byte);
          i += 3;
          continue;
        },
        Err(_) => return Err(Exception::throw_type(ctx, "Invalid percent-encoding in file URL")),
      }
    }
    out.push(bytes[i]);
    i += 1;
  }
  String::from_utf8(out).map_err(|_| Exception::throw_type(ctx, "File URL path is not valid UTF-8"))
}

/// The href of a string or `URL` argument.
fn href_of<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> Result<String> {
  if let Some(text) = value.as_string() {
    return text.to_string();
  }
  if let Some(object) = value.as_object() {
    if let Ok(href) = object.get::<_, String>("href") {
      return Ok(href);
    }
  }
  Err(Exception::throw_type(ctx, "The \"url\" argument must be a string or URL"))
}

fn file_url_to_path<'js>(ctx: Ctx<'js>, url: Value<'js>) -> Result<String> {
  let href = href_of(&ctx, &url)?;
  let rest = href
    .strip_prefix("file://")
    .ok_or_else(|| Exception::throw_type(&ctx, "The URL must be of scheme file"))?;
  // `file:///tmp/x` leaves `/tmp/x`; `file://host/x` is not addressable
  // as a local path on the platforms this runtime supports.
  let path = match rest.find('/') {
    Some(0) => rest,
    _ => return Err(Exception::throw_type(&ctx, "File URL host is not supported")),
  };
  let path = path.split(['?', '#']).next().unwrap_or(path);
  decode_path(&ctx, path)
}

/// Percent-encode the characters a path may carry that a URL may not.
fn encode_path(path: &str) -> String {
  let mut out = String::with_capacity(path.len());
  for ch in path.chars() {
    match ch {
      '%' => out.push_str("%25"),
      '#' => out.push_str("%23"),
      '?' => out.push_str("%3F"),
      '\n' => out.push_str("%0A"),
      '\r' => out.push_str("%0D"),
      '\t' => out.push_str("%09"),
      _ => out.push(ch),
    }
  }
  out
}

fn path_to_file_url<'js>(ctx: Ctx<'js>, path: String) -> Result<Value<'js>> {
  if !path.starts_with('/') {
    return Err(Exception::throw_type(&ctx, "The \"path\" argument must be an absolute path"));
  }
  let url_ctor: rquickjs::function::Constructor<'js> = ctx.globals().get("URL")?;
  let object: Object<'js> = url_ctor.construct((format!("file://{}", encode_path(&path)),))?;
  Ok(object.into_value())
}

/// Node's `url.format(URL, options)` — the serialisation of a `URL`, with
/// the fragment / search / auth trimmings the options ask for.
fn format<'js>(ctx: Ctx<'js>, url: Value<'js>, options: rquickjs::function::Opt<Object<'js>>) -> Result<String> {
  let href = href_of(&ctx, &url)?;
  let Some(options) = options.0 else {
    return Ok(href);
  };
  let mut out = href;
  if options.get::<_, bool>("fragment").is_ok_and(|v| !v) {
    out = out.split('#').next().unwrap_or(&out).to_string();
  }
  if options.get::<_, bool>("search").is_ok_and(|v| !v) {
    let (before, after) = out.split_once('?').unwrap_or((out.as_str(), ""));
    let fragment = after.find('#').map_or("", |i| &after[i..]);
    out = format!("{before}{fragment}");
  }
  Ok(out)
}

/// The `url` module surface.
///
/// # Errors
///
/// Propagates the global reads and property writes.
pub fn url_object<'js>(ctx: &Ctx<'js>) -> Result<Object<'js>> {
  let url = Object::new(ctx.clone())?;
  for name in ["URL", "URLSearchParams"] {
    if let Ok(class) = ctx.globals().get::<_, Value<'js>>(name) {
      if !class.is_undefined() {
        url.set(name, class)?;
      }
    }
  }
  url.set("fileURLToPath", Func::from(file_url_to_path))?;
  url.set("pathToFileURL", Func::from(path_to_file_url))?;
  url.set("format", Func::from(format))?;
  Ok(url)
}

/// Unused import guard: `Function` is part of the public shape this module
/// documents even when the compiler cannot see it used.
type _FunctionInScope<'js> = Function<'js>;
