//! WHATWG `ReadableStream` — a deliberately small spec subset (the
//! full llrt/stream-web machinery — BYOB, byte controllers, tee — is
//! studied for behaviour only, not ported).
//!
//! Two body sources behind one class:
//! - **Buffered**: `new ReadableStream({ start(controller) })` and
//!   `Blob.stream()` — chunks held in memory.
//! - **Net**: `Response.body` — pulls chunks directly off the live
//!   `reqwest` response ([`ferridriver::http_client::HttpStreamResponse`])
//!   on each `read()`, so a large/streamed body is NOT fully buffered;
//!   the consumer's pull rate is the backpressure.
//!
//! `getReader()` (locks; second getReader -> TypeError), `read()`
//! (`{value:Uint8Array,done}`), `releaseLock()`, `cancel()`, `locked`,
//! async iteration. Reader/controller are not user-constructible
//! (throw, per spec) but the global names + `instanceof` exist.
//! `read()` is async (a Net pull awaits the socket). Subset: no
//! `pull`/`tee`/BYOB underlying-source callbacks.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

use ferridriver::http_client::HttpStreamResponse;
use rquickjs::atom::PredefinedAtom;
use rquickjs::function::{Opt, This};
use rquickjs::{Class, Ctx, Object, TypedArray, Value, class::Trace};
use tokio::sync::Mutex as AsyncMutex;

#[derive(Default)]
struct BufState {
  chunks: VecDeque<Vec<u8>>,
  closed: bool,
  errored: Option<String>,
}

/// The body behind a stream. `Net` is the live response; `Buffered` is
/// an in-memory queue (constructed streams, `Blob.stream()`).
#[derive(Clone)]
enum StreamSource {
  Buffered(Rc<RefCell<BufState>>),
  Net(Arc<AsyncMutex<Option<HttpStreamResponse>>>),
}

/// `locked` is shared between a stream and the reader it hands out so
/// `releaseLock()` is observable on the stream.
type Locked = Rc<Cell<bool>>;

fn chunk_bytes(v: &Value<'_>) -> Vec<u8> {
  if let Some(s) = v.as_string().and_then(|s| s.to_string().ok()) {
    return s.into_bytes();
  }
  if let Ok(ta) = TypedArray::<u8>::from_value(v.clone()) {
    let b: &[u8] = ta.as_ref();
    return b.to_vec();
  }
  if let Some(ab) = rquickjs::ArrayBuffer::from_value(v.clone())
    && let Some(b) = ab.as_bytes()
  {
    return b.to_vec();
  }
  Vec::new()
}

fn result_obj<'js>(ctx: &Ctx<'js>, value: Value<'js>, done: bool) -> rquickjs::Result<Object<'js>> {
  let o = Object::new(ctx.clone())?;
  o.set("value", value)?;
  o.set("done", done)?;
  Ok(o)
}

fn chunk_result<'js>(ctx: &Ctx<'js>, bytes: Vec<u8>) -> rquickjs::Result<Object<'js>> {
  let ta = TypedArray::<u8>::new(ctx.clone(), bytes)?;
  result_obj(ctx, ta.into_value(), false)
}

/// One read step. Buffered is immediate; Net awaits the next socket
/// chunk (the only `.await` for a buffered stream is the readiness
/// yield, so `read()` is uniformly async without a no-op `async`).
async fn pull<'js>(ctx: &Ctx<'js>, source: &StreamSource) -> rquickjs::Result<Object<'js>> {
  match source {
    StreamSource::Buffered(state) => {
      std::future::ready(()).await;
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
        Some(b) => chunk_result(ctx, b),
        None => result_obj(ctx, Value::new_undefined(ctx.clone()), true),
      }
    },
    StreamSource::Net(resp) => {
      let mut guard = resp.lock().await;
      let Some(r) = guard.as_mut() else {
        return result_obj(ctx, Value::new_undefined(ctx.clone()), true);
      };
      match r.chunk().await {
        Ok(Some(bytes)) => chunk_result(ctx, bytes.to_vec()),
        Ok(None) => {
          *guard = None;
          result_obj(ctx, Value::new_undefined(ctx.clone()), true)
        },
        Err(e) => {
          *guard = None;
          Err(rquickjs::Exception::throw_type(ctx, &e.to_string()))
        },
      }
    },
  }
}

fn cancel_source(source: &StreamSource) {
  match source {
    StreamSource::Buffered(state) => {
      let mut s = state.borrow_mut();
      s.chunks.clear();
      s.closed = true;
    },
    // Drop the live response if not mid-read (best-effort; a concurrent
    // read keeps the lock and finishes its chunk first).
    StreamSource::Net(resp) => {
      if let Ok(mut g) = resp.try_lock() {
        *g = None;
      }
    },
  }
}

#[derive(Trace)]
#[rquickjs::class(rename = "ReadableStreamDefaultController")]
pub struct ReadableStreamDefaultControllerJs {
  #[qjs(skip_trace)]
  buf: Rc<RefCell<BufState>>,
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
    self.buf.borrow_mut().chunks.push_back(chunk_bytes(&chunk));
  }

  #[qjs(rename = "close")]
  pub fn close(&self) {
    self.buf.borrow_mut().closed = true;
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
    let mut s = self.buf.borrow_mut();
    s.errored = Some(msg);
    s.closed = true;
  }
}

#[derive(Trace)]
#[rquickjs::class(rename = "ReadableStreamDefaultReader")]
pub struct ReadableStreamDefaultReaderJs {
  #[qjs(skip_trace)]
  source: StreamSource,
  #[qjs(skip_trace)]
  locked: Locked,
  #[qjs(skip_trace)]
  released: bool,
}

#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for ReadableStreamDefaultReaderJs {
  type Changed<'to> = ReadableStreamDefaultReaderJs;
}

#[rquickjs::methods(rename_all = "camelCase")]
impl ReadableStreamDefaultReaderJs {
  #[qjs(constructor)]
  pub fn new(ctx: Ctx<'_>) -> rquickjs::Result<Self> {
    Err(rquickjs::Exception::throw_type(&ctx, "Illegal constructor"))
  }

  /// `read()` -> `Promise<{ value: Uint8Array, done }>` (a Net pull
  /// awaits the socket; buffered resolves immediately).
  #[qjs(rename = "read")]
  pub async fn read<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    if self.released {
      return Err(rquickjs::Exception::throw_type(&ctx, "Reader has been released"));
    }
    pull(&ctx, &self.source).await
  }

  #[qjs(rename = "releaseLock")]
  pub fn release_lock(&mut self) {
    self.released = true;
    self.locked.set(false);
  }

  #[qjs(rename = "cancel")]
  pub fn cancel(&self, _reason: Opt<Value<'_>>) {
    cancel_source(&self.source);
  }

  /// Spec `reader.closed` is a `Promise`; buffered-subset eager-resolve.
  #[qjs(get, rename = "closed")]
  pub fn closed<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    result_obj(&ctx, Value::new_undefined(ctx.clone()), true)
  }

  /// A reader is its own async iterator.
  #[qjs(rename = PredefinedAtom::SymbolAsyncIterator)]
  pub fn async_iter(this: This<Class<'_, ReadableStreamDefaultReaderJs>>) -> Class<'_, ReadableStreamDefaultReaderJs> {
    this.0
  }

  #[qjs(rename = "next")]
  pub async fn next<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    pull(&ctx, &self.source).await
  }
}

#[derive(Trace)]
#[rquickjs::class(rename = "ReadableStream")]
pub struct ReadableStreamJs {
  #[qjs(skip_trace)]
  source: StreamSource,
  #[qjs(skip_trace)]
  locked: Locked,
}

#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for ReadableStreamJs {
  type Changed<'to> = ReadableStreamJs;
}

impl ReadableStreamJs {
  /// A buffered stream pre-filled with `bytes` (one chunk, closed) —
  /// `Blob.stream()` and a buffered `Response.body`.
  pub fn from_bytes(bytes: Vec<u8>) -> Self {
    let mut chunks = VecDeque::new();
    if !bytes.is_empty() {
      chunks.push_back(bytes);
    }
    Self {
      source: StreamSource::Buffered(Rc::new(RefCell::new(BufState {
        chunks,
        closed: true,
        errored: None,
      }))),
      locked: Rc::new(Cell::new(false)),
    }
  }

  /// A live stream over a not-yet-read response — an incremental
  /// `Response.body`.
  pub fn from_net(resp: Arc<AsyncMutex<Option<HttpStreamResponse>>>) -> Self {
    Self {
      source: StreamSource::Net(resp),
      locked: Rc::new(Cell::new(false)),
    }
  }

  fn reader(&self) -> ReadableStreamDefaultReaderJs {
    ReadableStreamDefaultReaderJs {
      source: self.source.clone(),
      locked: self.locked.clone(),
      released: false,
    }
  }
}

#[rquickjs::methods(rename_all = "camelCase")]
impl ReadableStreamJs {
  /// `new ReadableStream(underlyingSource?)` — runs `start(controller)`
  /// synchronously if present. `pull`/`cancel`/BYOB unsupported.
  #[qjs(constructor)]
  pub fn new<'js>(ctx: Ctx<'js>, source: Opt<Object<'js>>) -> rquickjs::Result<Self> {
    let buf = Rc::new(RefCell::new(BufState::default()));
    if let Some(src) = source.0
      && let Ok(start) = src.get::<_, rquickjs::Function<'js>>("start")
    {
      let controller = Class::instance(ctx.clone(), ReadableStreamDefaultControllerJs { buf: buf.clone() })?;
      start.call::<_, ()>((controller,))?;
    }
    Ok(Self {
      source: StreamSource::Buffered(buf),
      locked: Rc::new(Cell::new(false)),
    })
  }

  #[qjs(get, rename = "locked")]
  pub fn locked(&self) -> bool {
    self.locked.get()
  }

  #[qjs(rename = "getReader")]
  pub fn get_reader<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Class<'js, ReadableStreamDefaultReaderJs>> {
    if self.locked.get() {
      return Err(rquickjs::Exception::throw_type(
        &ctx,
        "ReadableStream is already locked to a reader",
      ));
    }
    self.locked.set(true);
    Class::instance(ctx, self.reader())
  }

  #[qjs(rename = "cancel")]
  pub fn cancel(&self, _reason: Opt<Value<'_>>) {
    cancel_source(&self.source);
  }

  /// `stream[Symbol.asyncIterator]()` — a reader (locks the stream).
  #[qjs(rename = PredefinedAtom::SymbolAsyncIterator)]
  pub fn async_iter<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Class<'js, ReadableStreamDefaultReaderJs>> {
    self.locked.set(true);
    Class::instance(ctx, self.reader())
  }
}
