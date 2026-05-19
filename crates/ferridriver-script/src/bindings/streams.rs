//! WHATWG `ReadableStream` — a deliberately small spec subset (the
//! full llrt/stream-web machinery — BYOB, byte controllers,
//! backpressure, tee — is studied for behaviour only, not ported).
//!
//! Supported: `new ReadableStream({ start(controller) })` with
//! `controller.enqueue/close/error`; `getReader()` ->
//! `ReadableStreamDefaultReader` with `read()` (`{value:Uint8Array,
//! done}`), `releaseLock()`, `cancel()`, `closed`; stream `cancel()`,
//! `locked`; async iteration (`for await (const chunk of stream)`).
//! `Response.body` is a `ReadableStream` of the body bytes.
//!
//! Subset, documented: the body is buffered (one chunk) rather than
//! incrementally streamed from the socket — `Response.body` is
//! spec-shaped but not yet backpressured/incremental (a follow-up
//! plumbs `reqwest::Response::bytes_stream` through the core). No
//! `pull`/`cancel` underlying-source callbacks, no BYOB, no `tee()`.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use rquickjs::atom::PredefinedAtom;
use rquickjs::function::{Opt, This};
use rquickjs::{Class, Ctx, Object, TypedArray, Value, class::Trace};

#[derive(Default)]
struct StreamState {
  chunks: VecDeque<Vec<u8>>,
  closed: bool,
  errored: Option<String>,
  locked: bool,
}

type Shared = Rc<RefCell<StreamState>>;

/// Bytes from a JS chunk: string (UTF-8), `Uint8Array`, or
/// `ArrayBuffer`. Anything else -> empty.
fn chunk_bytes(v: &Value<'_>) -> Vec<u8> {
  if let Some(s) = v.as_string().and_then(|s| s.to_string().ok()) {
    return s.into_bytes();
  }
  if let Ok(ta) = TypedArray::<u8>::from_value(v.clone()) {
    let bytes: &[u8] = ta.as_ref();
    return bytes.to_vec();
  }
  if let Some(ab) = rquickjs::ArrayBuffer::from_value(v.clone())
    && let Some(bytes) = ab.as_bytes()
  {
    return bytes.to_vec();
  }
  Vec::new()
}

fn result_obj<'js>(ctx: &Ctx<'js>, value: Value<'js>, done: bool) -> rquickjs::Result<Object<'js>> {
  let o = Object::new(ctx.clone())?;
  o.set("value", value)?;
  o.set("done", done)?;
  Ok(o)
}

/// One read step against shared state (buffered model): a queued chunk,
/// else end-of-stream once closed, else (no data, not closed) treated
/// as done — the buffered producer always closes.
fn read_step<'js>(ctx: &Ctx<'js>, state: &Shared) -> rquickjs::Result<Object<'js>> {
  let (chunk, errored) = {
    let mut s = state.borrow_mut();
    if let Some(e) = s.errored.clone() {
      (None, Some(e))
    } else {
      (s.chunks.pop_front(), None)
    }
  };
  if let Some(e) = errored {
    return Err(rquickjs::Exception::throw_type(ctx, &e));
  }
  match chunk {
    Some(bytes) => {
      let ta = TypedArray::<u8>::new(ctx.clone(), bytes)?;
      result_obj(ctx, ta.into_value(), false)
    },
    None => result_obj(ctx, Value::new_undefined(ctx.clone()), true),
  }
}

#[derive(Trace)]
#[rquickjs::class(rename = "ReadableStreamDefaultController")]
pub struct ReadableStreamDefaultControllerJs {
  #[qjs(skip_trace)]
  state: Shared,
}

#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for ReadableStreamDefaultControllerJs {
  type Changed<'to> = ReadableStreamDefaultControllerJs;
}

#[rquickjs::methods(rename_all = "camelCase")]
impl ReadableStreamDefaultControllerJs {
  /// Not user-constructible (only handed to `start`); present so the
  /// global name + `instanceof` exist.
  #[qjs(constructor)]
  pub fn new(ctx: Ctx<'_>) -> rquickjs::Result<Self> {
    Err(rquickjs::Exception::throw_type(&ctx, "Illegal constructor"))
  }

  #[qjs(rename = "enqueue")]
  pub fn enqueue(&self, chunk: Value<'_>) {
    let bytes = chunk_bytes(&chunk);
    self.state.borrow_mut().chunks.push_back(bytes);
  }

  #[qjs(rename = "close")]
  pub fn close(&self) {
    self.state.borrow_mut().closed = true;
  }

  #[qjs(rename = "error")]
  pub fn error(&self, reason: Opt<Value<'_>>) {
    let msg = reason
      .0
      .and_then(|v| {
        v.as_string()
          .and_then(|s| s.to_string().ok())
          .or_else(|| v.as_object().and_then(|o| o.get::<_, String>("message").ok()))
      })
      .unwrap_or_else(|| "stream errored".to_string());
    let mut s = self.state.borrow_mut();
    s.errored = Some(msg);
    s.closed = true;
  }
}

#[derive(Trace)]
#[rquickjs::class(rename = "ReadableStreamDefaultReader")]
pub struct ReadableStreamDefaultReaderJs {
  #[qjs(skip_trace)]
  state: Shared,
  #[qjs(skip_trace)]
  released: bool,
}

#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for ReadableStreamDefaultReaderJs {
  type Changed<'to> = ReadableStreamDefaultReaderJs;
}

#[rquickjs::methods(rename_all = "camelCase")]
impl ReadableStreamDefaultReaderJs {
  /// Not user-constructible in this subset (`stream.getReader()` is the
  /// entry point); present so the global name + `instanceof` exist.
  #[qjs(constructor)]
  pub fn new(ctx: Ctx<'_>) -> rquickjs::Result<Self> {
    Err(rquickjs::Exception::throw_type(&ctx, "Illegal constructor"))
  }

  /// `read()` -> `{ value: Uint8Array, done }`. Returned synchronously
  /// (buffered model); `await reader.read()` is transparent on a
  /// non-Promise, matching the rest of this subset.
  #[qjs(rename = "read")]
  pub fn read<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    if self.released {
      return Err(rquickjs::Exception::throw_type(&ctx, "Reader has been released"));
    }
    read_step(&ctx, &self.state)
  }

  #[qjs(rename = "releaseLock")]
  pub fn release_lock(&mut self) {
    self.released = true;
    self.state.borrow_mut().locked = false;
  }

  #[qjs(rename = "cancel")]
  pub fn cancel(&self, _reason: Opt<Value<'_>>) {
    let mut s = self.state.borrow_mut();
    s.chunks.clear();
    s.closed = true;
  }

  /// Spec `reader.closed` is a `Promise`. Buffered model: the producer
  /// has already closed, so resolve eagerly (documented deviation for
  /// the not-yet-closed case).
  #[qjs(get, rename = "closed")]
  pub fn closed<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    result_obj(&ctx, Value::new_undefined(ctx.clone()), true)
  }

  /// Reader is its own async iterator (`for await (const c of
  /// stream.getReader())`).
  #[qjs(rename = PredefinedAtom::SymbolAsyncIterator)]
  pub fn async_iter(this: This<Class<'_, ReadableStreamDefaultReaderJs>>) -> Class<'_, ReadableStreamDefaultReaderJs> {
    this.0
  }

  #[qjs(rename = "next")]
  pub fn next<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    read_step(&ctx, &self.state)
  }
}

#[derive(Trace)]
#[rquickjs::class(rename = "ReadableStream")]
pub struct ReadableStreamJs {
  #[qjs(skip_trace)]
  state: Shared,
}

#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for ReadableStreamJs {
  type Changed<'to> = ReadableStreamJs;
}

impl ReadableStreamJs {
  /// A stream pre-filled with `bytes` as a single chunk and closed —
  /// what `Response.body` returns (buffered subset).
  pub fn from_bytes(bytes: Vec<u8>) -> Self {
    let mut chunks = VecDeque::new();
    if !bytes.is_empty() {
      chunks.push_back(bytes);
    }
    Self {
      state: Rc::new(RefCell::new(StreamState {
        chunks,
        closed: true,
        errored: None,
        locked: false,
      })),
    }
  }
}

#[rquickjs::methods(rename_all = "camelCase")]
impl ReadableStreamJs {
  /// `new ReadableStream(underlyingSource?)` — runs `start(controller)`
  /// synchronously if present (`enqueue`/`close`/`error`). `pull` /
  /// `cancel` / BYOB are not supported (documented subset).
  #[qjs(constructor)]
  pub fn new<'js>(ctx: Ctx<'js>, source: Opt<Object<'js>>) -> rquickjs::Result<Self> {
    let state: Shared = Rc::new(RefCell::new(StreamState::default()));
    if let Some(src) = source.0
      && let Ok(start) = src.get::<_, rquickjs::Function<'js>>("start")
    {
      let controller = Class::instance(ctx.clone(), ReadableStreamDefaultControllerJs { state: state.clone() })?;
      start.call::<_, ()>((controller,))?;
    }
    Ok(Self { state })
  }

  #[qjs(get, rename = "locked")]
  pub fn locked(&self) -> bool {
    self.state.borrow().locked
  }

  #[qjs(rename = "getReader")]
  pub fn get_reader<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Class<'js, ReadableStreamDefaultReaderJs>> {
    {
      let mut s = self.state.borrow_mut();
      if s.locked {
        return Err(rquickjs::Exception::throw_type(
          &ctx,
          "ReadableStream is already locked to a reader",
        ));
      }
      s.locked = true;
    }
    Class::instance(
      ctx,
      ReadableStreamDefaultReaderJs {
        state: self.state.clone(),
        released: false,
      },
    )
  }

  #[qjs(rename = "cancel")]
  pub fn cancel(&self, _reason: Opt<Value<'_>>) {
    let mut s = self.state.borrow_mut();
    s.chunks.clear();
    s.closed = true;
  }

  /// `stream[Symbol.asyncIterator]()` — a fresh reader (async iterator).
  #[qjs(rename = PredefinedAtom::SymbolAsyncIterator)]
  pub fn async_iter<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Class<'js, ReadableStreamDefaultReaderJs>> {
    self.state.borrow_mut().locked = true;
    Class::instance(
      ctx,
      ReadableStreamDefaultReaderJs {
        state: self.state.clone(),
        released: false,
      },
    )
  }
}
