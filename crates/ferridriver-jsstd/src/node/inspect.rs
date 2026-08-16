//! `util.inspect` / `util.format`: the one value renderer this runtime has.
//!
//! Node renders values the same way in three places — `console.log`,
//! `util.inspect` and `util.format`'s `%o` / `%O` / `%j` specifiers — so
//! there is one implementation here, used by the `console` global in
//! `ferridriver-script` and by the `util` module next door.
//!
//! Ferridriver-authored, not vendored from llrt (upstream's `util` has no
//! inspect at all and its console formatter is not reusable).

use std::fmt::Write as _;

use rquickjs::function::This;
use rquickjs::{Function, Object, Value};

/// Strip ANSI escape sequences from a string.
///
/// Every string that comes from JS is run through this as it is written, so
/// page content cannot smuggle terminal control codes into the output while
/// the renderer's own styling survives.
#[must_use]
pub fn strip_ansi(input: &str) -> String {
  let mut out = String::with_capacity(input.len());
  let mut chars = input.chars().peekable();
  while let Some(c) = chars.next() {
    if c == '\x1b' && chars.peek() == Some(&'[') {
      chars.next();
      for nc in chars.by_ref() {
        if ('@'..='~').contains(&nc) {
          break;
        }
      }
    } else {
      out.push(c);
    }
  }
  out
}


/// Nesting depth at which containers render as `[Array]` / `[Object]`, matching
/// `util.inspect`'s `depth: 2` default.
pub const MAX_DEPTH: usize = 2;

/// Node's `maxArrayLength`: elements past this are summarised as
/// `... N more items` rather than printed.
pub const MAX_ARRAY_LENGTH: usize = 100;

/// Ceiling for an explicit `console.dir(value, { depth: null })`, which is
/// otherwise unbounded and would happily walk a cyclic-free but enormous
/// object graph.
pub const MAX_DIR_DEPTH: usize = 8;

const RESET: &str = "\x1b[0m";
/// `util.inspect.styles` mapped to their SGR codes.
const NUMBER: &str = "\x1b[33m";
const STRING: &str = "\x1b[32m";
const BOOLEAN: &str = "\x1b[33m";
const UNDEFINED: &str = "\x1b[90m";
const NULL: &str = "\x1b[1m";
const SYMBOL: &str = "\x1b[32m";
const DATE: &str = "\x1b[35m";
const REGEXP: &str = "\x1b[31m";
const SPECIAL: &str = "\x1b[36m";

/// Renders JS values the way `util.inspect` does, with or without colour.
#[derive(Clone, Copy)]
pub struct Inspector {
  /// Whether rendered values carry SGR colour codes.
  pub styled: bool,
  max_depth: usize,
  /// Whether a string rendered at the top level prints bare. `console.log`
  /// prints its string arguments raw; `util.inspect` (so `dir`, `%o`, `%O`,
  /// table cells) quotes them.
  bare_top_string: bool,
}

impl Inspector {
  pub fn new(styled: bool) -> Self {
    Self {
      styled,
      max_depth: MAX_DEPTH,
      bare_top_string: true,
    }
  }

  pub fn with_depth(self, max_depth: usize) -> Self {
    Self { max_depth, ..self }
  }

  /// Quote strings even at the top level, the way `util.inspect` does.
  pub fn quoted(self) -> Self {
    Self {
      bare_top_string: false,
      ..self
    }
  }

  /// Write `text` (already-formatted, JS-derived) under `code`, sanitising
  /// any escape sequence the value itself carried.
  pub fn paint(self, out: &mut String, code: &str, text: &str) {
    let text = strip_ansi(text);
    // An empty code is "no style" (object keys, top-level strings) — writing
    // a bare reset there would end the enclosing colour, not start one.
    if self.styled && !code.is_empty() {
      out.push_str(code);
      out.push_str(&text);
      out.push_str(RESET);
    } else {
      out.push_str(&text);
    }
  }

  /// Structural punctuation we emit ourselves — never sanitised, never styled.
  pub fn punct(out: &mut String, text: &str) {
    out.push_str(text);
  }

  /// Node's `util.format` core: when the first argument is a string,
  /// `%s` / `%d` / `%i` / `%f` / `%j` / `%o` / `%O` / `%c` / `%%`
  /// consume the following arguments; leftovers are appended
  /// space-separated. Returns how many arguments were consumed
  /// (including the format string itself).
  pub fn printf(self, out: &mut String, fmt: &str, args: &[Value<'_>]) -> rquickjs::Result<usize> {
    let mut consumed = 0usize;
    let mut chars = fmt.chars().peekable();
    let mut literal = String::new();
    while let Some(c) = chars.next() {
      if c != '%' {
        literal.push(c);
        continue;
      }
      let Some(&spec) = chars.peek() else {
        literal.push('%');
        break;
      };
      if spec == '%' {
        chars.next();
        literal.push('%');
        continue;
      }
      if !matches!(spec, 's' | 'd' | 'i' | 'f' | 'j' | 'o' | 'O' | 'c') {
        literal.push('%');
        continue;
      }
      let Some(arg) = args.get(consumed) else {
        // More specifiers than arguments — Node leaves them literal.
        literal.push('%');
        continue;
      };
      chars.next();
      consumed += 1;
      // The format string is JS-supplied too: flush it through the
      // sanitiser before the substitution lands.
      out.push_str(&strip_ansi(&std::mem::take(&mut literal)));
      match spec {
        's' => {
          if let Some(s) = arg.as_string() {
            self.paint(out, "", &s.to_string()?);
          } else {
            // Node inspects a `%s` object at depth 0, without colour.
            Inspector::new(false).with_depth(0).value(out, arg, 0)?;
          }
        },
        // Node coerces through `Number` / `parseInt` / `parseFloat`, so a
        // numeric string converts and `'42px'` yields 42 under `%i`.
        'd' | 'i' | 'f' => self.paint(out, NUMBER, &coerce_number(arg, spec)?),
        'j' => {
          // A circular structure makes `JSON.stringify` throw; Node prints
          // `[Circular]` rather than letting the console call fail.
          let json = arg
            .ctx()
            .json_stringify(arg.clone())
            .ok()
            .flatten()
            .and_then(|s| s.to_string().ok());
          match json {
            Some(text) => self.paint(out, "", &text),
            None if arg.is_undefined() => self.paint(out, "", "undefined"),
            None => self.paint(out, "", "[Circular]"),
          }
        },
        // `%o` inspects deeper (Node uses depth 4), `%O` uses the default.
        'o' => self.quoted().with_depth(4).value(out, arg, 0)?,
        'O' => self.quoted().with_depth(MAX_DEPTH).value(out, arg, 0)?,
        // %c consumes a CSS argument and renders nothing in a terminal;
        // the guard above filters everything else out.
        _ => {},
      }
    }
    out.push_str(&strip_ansi(&literal));
    Ok(consumed + 1)
  }

  /// Render a whole `console.*` argument list: a leading format string
  /// consumes what it needs, the rest is appended space-separated.
  pub fn args(self, out: &mut String, args: &[Value<'_>]) -> rquickjs::Result<()> {
    let mut start = 0usize;
    if let Some(fmt) = args.first().and_then(rquickjs::Value::as_string) {
      let fmt = fmt.to_string()?;
      if fmt.contains('%') {
        start = self.printf(out, &fmt, &args[1..])?;
      }
    }
    for (i, v) in args.iter().enumerate().skip(start) {
      if i > 0 || start > 0 {
        out.push(' ');
      }
      self.value(out, v, 0)?;
    }
    Ok(())
  }

  /// Node-ish console value renderer: top-level strings unquoted (quoted
  /// with `'` inside containers, like `util.inspect`), arrays as
  /// `[ 1, 2 ]`, objects as `{ a: 1, b: 2 }`, `Map(n) { k => v }`,
  /// `Set(n) { v }`, Dates as ISO strings, RegExp as `/src/flags`,
  /// `[Function: name]`, `Symbol(desc)`, `123n` bigints, `name: message`
  /// (+ stack) for Error values, and `[Array]` / `[Object]` past
  /// `max_depth` nesting.
  #[allow(clippy::too_many_lines)]
  pub fn value(self, out: &mut String, value: &Value<'_>, depth: usize) -> rquickjs::Result<()> {
    use rquickjs::Type;

    match value.type_of() {
      Type::String => {
        if let Some(s) = value.as_string() {
          let s = s.to_string()?;
          if depth == 0 && self.bare_top_string {
            self.paint(out, "", &s);
          } else {
            // Inside containers Node quotes strings, escaping control
            // characters and picking a quote the body does not contain.
            self.paint(out, STRING, &quote_js_string(&s));
          }
        }
      },
      Type::Int => self.paint(out, NUMBER, &value.as_int().unwrap_or_default().to_string()),
      Type::Bool => self.paint(out, BOOLEAN, &value.as_bool().unwrap_or_default().to_string()),
      Type::Float => self.paint(out, NUMBER, &value.as_float().unwrap_or_default().to_string()),
      Type::BigInt => {
        if let Some(b) = value.clone().into_big_int() {
          self.paint(out, NUMBER, &format!("{}n", b.clone().to_i64()?));
        }
      },
      Type::Array => {
        let Some(array) = value.as_array() else { return Ok(()) };
        if depth > self.max_depth {
          self.paint(out, SPECIAL, "[Array]");
          return Ok(());
        }
        if array.is_empty() {
          Self::punct(out, "[]");
          return Ok(());
        }
        Self::punct(out, "[ ");
        let len = array.len();
        for (i, element) in array.iter::<Value<'_>>().take(MAX_ARRAY_LENGTH).enumerate() {
          if i > 0 {
            Self::punct(out, ", ");
          }
          self.value(out, &element?, depth + 1)?;
        }
        // Node's `maxArrayLength`: the tail is summarised, never printed.
        if len > MAX_ARRAY_LENGTH {
          let more = len - MAX_ARRAY_LENGTH;
          let plural = if more == 1 { "item" } else { "items" };
          Self::punct(out, &format!(", ... {more} more {plural}"));
        }
        Self::punct(out, " ]");
      },
      Type::Exception => {
        if let Some(ex) = value.as_exception() {
          let name = ex.get::<_, String>("name").unwrap_or_else(|_| "Error".to_string());
          let mut rendered = name;
          if let Some(message) = ex.message() {
            rendered.push_str(": ");
            rendered.push_str(&message);
          }
          // Node prints the stack under the message; keep it at top level
          // only so nested Errors don't explode container output.
          if depth == 0 {
            if let Some(stack) = ex.stack().filter(|s| !s.is_empty()) {
              rendered.push('\n');
              rendered.push_str(&stack);
            }
          }
          self.paint(out, REGEXP, &rendered);
        }
      },
      Type::Object => {
        if depth > self.max_depth {
          self.paint(out, SPECIAL, "[Object]");
          return Ok(());
        }
        let Some(object) = value.as_object() else { return Ok(()) };
        if self.special_object(out, object, depth)? {
          return Ok(());
        }
        // `Foo { a: 1 }` for a class instance, `[Object: null prototype] {}`
        // for one made with `Object.create(null)` — both are how Node warns
        // that this is not a plain object literal.
        match constructor_name(object) {
          Some(name) if name != "Object" => {
            self.paint(out, "", &name);
            Self::punct(out, " ");
          },
          None => {
            self.paint(out, SPECIAL, "[Object: null prototype]");
            Self::punct(out, " ");
          },
          Some(_) => {},
        }
        let mut wrote_any = false;
        for (i, prop) in object.props::<String, Value<'_>>().enumerate() {
          let (key, val) = prop?;
          if i == 0 {
            Self::punct(out, "{ ");
            wrote_any = true;
          } else {
            Self::punct(out, ", ");
          }
          self.paint(out, "", &key);
          Self::punct(out, ": ");
          self.value(out, &val, depth + 1)?;
        }
        Self::punct(out, if wrote_any { " }" } else { "{}" });
      },
      Type::Symbol => {
        if let Some(symbol) = value.as_symbol() {
          let description = symbol
            .description()?
            .as_string()
            .map(rquickjs::String::to_string)
            .transpose()?
            .unwrap_or_default();
          self.paint(out, SYMBOL, &format!("Symbol({description})"));
        }
      },
      Type::Function | Type::Constructor => {
        let name = value
          .as_object()
          .and_then(|f| f.get::<_, String>("name").ok())
          .filter(|n| !n.is_empty());
        match name {
          Some(name) => self.paint(out, SPECIAL, &format!("[Function: {name}]")),
          None => self.paint(out, SPECIAL, "[Function (anonymous)]"),
        }
      },
      // A promise is its own `Type`, so it never reached the object arm and
      // used to render as an empty string — hiding the most common console
      // mistake there is, logging a promise instead of awaiting it.
      Type::Promise => {
        let Some(promise) = value.as_promise() else {
          return Ok(());
        };
        Self::punct(out, "Promise { ");
        match promise.state() {
          rquickjs::promise::PromiseState::Pending => self.paint(out, SPECIAL, "<pending>"),
          rquickjs::promise::PromiseState::Resolved => match promise.result::<Value<'_>>() {
            Some(Ok(inner)) => self.value(out, &inner, depth + 1)?,
            _ => self.paint(out, SPECIAL, "<pending>"),
          },
          rquickjs::promise::PromiseState::Rejected => {
            self.paint(out, REGEXP, "<rejected>");
            Self::punct(out, " ");
            // Reading the result of a rejected promise SETS the pending
            // exception; `catch` takes it back off the context, so inspecting
            // a rejection leaves no residue for the caller to trip over.
            if let Some(Err(_)) = promise.result::<Value<'_>>() {
              let reason = value.ctx().catch();
              self.value(out, &reason, depth + 1)?;
            }
          },
        }
        Self::punct(out, " }");
      },
      Type::Null => self.paint(out, NULL, "null"),
      Type::Undefined | Type::Uninitialized => self.paint(out, UNDEFINED, "undefined"),
      _ => {},
    }
    Ok(())
  }

  /// Render Date / RegExp / Map / Set the way Node's `util.inspect` does
  /// (`2026-01-01T00:00:00.000Z`, `/ab+c/i`, `Map(1) { 'a' => 1 }`,
  /// `Set(2) { 1, 2 }`). Returns `false` when `object` is none of those
  /// so the caller falls through to plain-object rendering. Detection is
  /// by constructor name — cheap, and correct for anything built from
  /// the real globals.
  pub fn special_object(self, out: &mut String, object: &Object<'_>, depth: usize) -> rquickjs::Result<bool> {
    let ctor_name: String = object
      .get::<_, Object<'_>>("constructor")
      .and_then(|c| c.get::<_, String>("name"))
      .unwrap_or_default();
    match ctor_name.as_str() {
      "Date" => {
        // toISOString throws on Invalid Date — match Node's rendering.
        let iso = object
          .get::<_, Function<'_>>("toISOString")
          .and_then(|f| f.call::<_, String>((This(object.clone()),)));
        match iso {
          Ok(s) => self.paint(out, DATE, &s),
          Err(_) => self.paint(out, DATE, "Invalid Date"),
        }
        Ok(true)
      },
      "RegExp" => {
        let source: String = object.get("source").unwrap_or_default();
        let flags: String = object.get("flags").unwrap_or_default();
        self.paint(out, REGEXP, &format!("/{source}/{flags}"));
        Ok(true)
      },
      kind @ ("WeakMap" | "WeakSet") => {
        Self::punct(out, kind);
        Self::punct(out, " { ");
        self.paint(out, SPECIAL, "<items unknown>");
        Self::punct(out, " }");
        Ok(true)
      },
      kind @ ("Int8Array" | "Uint8Array" | "Uint8ClampedArray" | "Int16Array" | "Uint16Array" | "Int32Array"
      | "Uint32Array" | "Float32Array" | "Float64Array" | "BigInt64Array" | "BigUint64Array") => {
        let len: usize = object.get("length").unwrap_or_default();
        Self::punct(out, &format!("{kind}({len})"));
        if len == 0 {
          Self::punct(out, " []");
          return Ok(true);
        }
        Self::punct(out, " [ ");
        for i in 0..len.min(MAX_ARRAY_LENGTH) {
          if i > 0 {
            Self::punct(out, ", ");
          }
          let element: Value<'_> = object.get(i as u32)?;
          self.value(out, &element, depth + 1)?;
        }
        if len > MAX_ARRAY_LENGTH {
          let more = len - MAX_ARRAY_LENGTH;
          let plural = if more == 1 { "item" } else { "items" };
          Self::punct(out, &format!(", ... {more} more {plural}"));
        }
        Self::punct(out, " ]");
        Ok(true)
      },
      "ArrayBuffer" | "SharedArrayBuffer" => {
        let len: usize = object.get("byteLength").unwrap_or_default();
        Self::punct(out, &format!("{ctor_name} {{ byteLength: "));
        self.paint(out, NUMBER, &len.to_string());
        Self::punct(out, " }");
        Ok(true)
      },
      kind @ ("Map" | "Set") => {
        let size: usize = object.get("size").unwrap_or_default();
        Self::punct(out, &format!("{kind}({size})"));
        if size == 0 {
          Self::punct(out, " {}");
          return Ok(true);
        }
        if depth > self.max_depth {
          return Ok(true);
        }
        // Drive the JS iterator so insertion order is preserved.
        let entries: rquickjs::Result<Function<'_>> = object.get("entries");
        let values: rquickjs::Result<Function<'_>> = object.get("values");
        let iter_fn = if kind == "Map" { entries } else { values };
        let Ok(iter_fn) = iter_fn else { return Ok(true) };
        let iterator: Object<'_> = iter_fn.call((This(object.clone()),))?;
        let next_fn: Function<'_> = iterator.get("next")?;
        Self::punct(out, " { ");
        let mut first = true;
        loop {
          let step: Object<'_> = next_fn.call((This(iterator.clone()),))?;
          if step.get::<_, bool>("done").unwrap_or(true) {
            break;
          }
          if !first {
            Self::punct(out, ", ");
          }
          first = false;
          let entry: Value<'_> = step.get("value")?;
          if kind == "Map" {
            let Some(pair) = entry.as_array() else { continue };
            self.value(out, &pair.get::<Value<'_>>(0)?, depth + 1)?;
            Self::punct(out, " => ");
            self.value(out, &pair.get::<Value<'_>>(1)?, depth + 1)?;
          } else {
            self.value(out, &entry, depth + 1)?;
          }
        }
        Self::punct(out, " }");
        Ok(true)
      },
      _ => Ok(false),
    }
  }
}

/// The object's constructor name, or `None` for a null-prototype object.
/// Reading `constructor` off a null-prototype object yields nothing, which is
/// exactly the case Node marks as `[Object: null prototype]`.
fn constructor_name(object: &Object<'_>) -> Option<String> {
  let prototype = object.get::<_, Value<'_>>("__proto__").ok()?;
  if prototype.is_null() || prototype.is_undefined() {
    return None;
  }
  object
    .get::<_, Object<'_>>("constructor")
    .and_then(|c| c.get::<_, String>("name"))
    .ok()
    .filter(|n| !n.is_empty())
}

/// Quote a string the way `util.inspect` does: prefer single quotes, fall back
/// to double then backtick when the body contains the previous choice, and
/// escape backslashes and control characters so one value cannot break the
/// surrounding rendering across lines.
fn quote_js_string(text: &str) -> String {
  let quote = if !text.contains('\'') {
    '\''
  } else if !text.contains('"') {
    '"'
  } else {
    '`'
  };
  let mut out = String::with_capacity(text.len() + 2);
  out.push(quote);
  for c in text.chars() {
    match c {
      '\\' => out.push_str("\\\\"),
      '\n' => out.push_str("\\n"),
      '\r' => out.push_str("\\r"),
      '\t' => out.push_str("\\t"),
      c if c == quote => {
        out.push('\\');
        out.push(c);
      },
      c if (c as u32) < 0x20 => {
        // `write!` into the buffer instead of allocating a throwaway
        // String per control character.
        let _ = write!(out, "\\x{:02x}", c as u32);
      },
      c => out.push(c),
    }
  }
  out.push(quote);
  out
}

/// Coerce a `%d` / `%i` / `%f` argument the way Node does: `%d` through
/// `Number`, `%i` through `parseInt`, `%f` through `parseFloat` — so a numeric
/// string converts and `'42px'` yields 42 under `%i`. BigInt keeps its `n`
/// suffix and Symbol is `NaN`, neither of which the global functions accept.
fn coerce_number(arg: &Value<'_>, spec: char) -> rquickjs::Result<String> {
  use rquickjs::Type;

  match arg.type_of() {
    Type::BigInt => {
      if let Some(b) = arg.clone().into_big_int() {
        return Ok(format!("{}n", b.to_i64()?));
      }
      return Ok("NaN".to_string());
    },
    Type::Symbol => return Ok("NaN".to_string()),
    _ => {},
  }
  let global = match spec {
    'i' => "parseInt",
    'f' => "parseFloat",
    _ => "Number",
  };
  let Ok(convert) = arg.ctx().globals().get::<_, Function<'_>>(global) else {
    return Ok("NaN".to_string());
  };
  let converted: f64 = convert.call((arg.clone(),)).unwrap_or(f64::NAN);
  if converted.is_nan() {
    return Ok("NaN".to_string());
  }
  Ok(converted.to_string())
}
