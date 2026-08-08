//! Native underlying sources for the vendored WHATWG `ReadableStream`
//! ([`ferridriver_jsstd::stream_web`]).
//!
//! The stream classes themselves — `tee`, BYOB/byte streams,
//! `WritableStream`, `TransformStream`, `pipeTo`/`pipeThrough`, the
//! queuing strategies — come from the vendored implementation. What
//! lives here are the two sources ferridriver feeds them:
//!
//! - [`from_bytes`] — in-memory payload (`Blob.stream()`, a constructed
//!   `Response`): one pull enqueues the whole buffer and closes.
//! - [`from_net`] — a live [`HttpStreamResponse`]: each pull awaits the
//!   next socket chunk, so a large body is never fully buffered and the
//!   consumer's read rate is the backpressure.

use std::sync::Arc;

use ferridriver::http_client::HttpStreamResponse;
use ferridriver_jsstd::context::CtxExtension;
use ferridriver_jsstd::stream_web::utils::promise::{PromisePrimordials, ResolveablePromise};
use ferridriver_jsstd::stream_web::{
  CancelAlgorithm, PullAlgorithm, ReadableStream, ReadableStreamControllerClass, ReadableStreamDefaultControllerClass,
  readable_stream_default_controller_close_stream, readable_stream_default_controller_enqueue_value,
  readable_stream_default_controller_error_stream,
};
use ferridriver_jsstd::utils::primordials::Primordial;
use rquickjs::{Class, Ctx, TypedArray, Value};
use tokio::sync::Mutex as AsyncMutex;

/// The live response behind a `fetch` body. `None` once the socket has
/// been drained or released, so both the stream and `text()`/`json()`
/// see the same "already consumed" state.
pub type NetBody = Arc<AsyncMutex<Option<HttpStreamResponse>>>;

/// Wall-clock bound on a single `from_net` pull. Mirrors
/// `fetch.rs::FETCH_BODY_DRAIN_TIMEOUT` for the buffered `text()` /
/// `json()` paths: the per-script interrupt handler cannot fire while a
/// native await is pending, so an unbounded `chunk().await` against a
/// stalled (slow-loris) server would pin the session until the
/// execute-level backstop poisons the whole VM.
const NET_CHUNK_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(2);

fn default_controller<'js>(
  ctx: &Ctx<'js>,
  controller: ReadableStreamControllerClass<'js>,
) -> rquickjs::Result<ReadableStreamDefaultControllerClass<'js>> {
  match controller {
    ReadableStreamControllerClass::ReadableStreamDefaultController(c) => Ok(c),
    _ => Err(rquickjs::Exception::throw_type(
      ctx,
      "expected a default ReadableStream controller",
    )),
  }
}

fn resolved_undefined<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Promise<'js>> {
  Ok(PromisePrimordials::get(ctx)?.promise_resolved_with_undefined.clone())
}

/// A `ReadableStream` over an in-memory payload.
pub fn from_bytes<'js>(ctx: &Ctx<'js>, bytes: Vec<u8>) -> rquickjs::Result<Class<'js, ReadableStream<'js>>> {
  let pull = PullAlgorithm::from_fn_once(move |ctx: Ctx<'js>, controller| {
    let ctrl = default_controller(&ctx, controller)?;
    let chunk = TypedArray::<u8>::new(ctx.clone(), bytes)?.into_value();
    readable_stream_default_controller_enqueue_value(ctx.clone(), ctrl.clone(), chunk)?;
    readable_stream_default_controller_close_stream(ctx.clone(), ctrl)?;
    resolved_undefined(&ctx)
  });
  ReadableStream::from_pull_algorithm(ctx.clone(), pull, CancelAlgorithm::ReturnPromiseUndefined)
}

/// Hand a JS `ReadableStream` to the HTTP engine as a request body.
///
/// The stream is owned by the QuickJS VM and only its own thread may
/// pull it, but the engine needs a `Send` stream it can poll from the
/// request future. The bridge is a pump: a task spawned ON the
/// interpreter thread (`spawn_exit_simple`, the same mechanism
/// [`from_net`] uses) reads through the stream's public reader and
/// forwards each chunk down a channel; the engine sees only the
/// receiving end.
///
/// The channel holds ONE chunk, so the pump advances no faster than the
/// socket drains it — the reader is the backpressure, exactly as it is
/// for a response body.
pub fn to_byte_stream<'js>(
  ctx: &Ctx<'js>,
  stream: Class<'js, ReadableStream<'js>>,
) -> rquickjs::Result<ferridriver::fetch::ByteStream> {
  let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, String>>(1);
  let reader = {
    let obj = stream
      .clone()
      .into_value()
      .into_object()
      .ok_or_else(|| rquickjs::Error::new_from_js_message("fetch", "TypeError", "body is not a ReadableStream"))?;
    obj
      .get::<_, rquickjs::Function<'js>>("getReader")?
      .call::<_, rquickjs::Object<'js>>((rquickjs::function::This(obj),))?
  };

  ctx.spawn_exit_simple(async move {
    let read: rquickjs::Function<'js> = reader.get("read")?;
    loop {
      let step: rquickjs::Promise<'js> = read.call((rquickjs::function::This(reader.clone()),))?;
      let outcome: rquickjs::Result<rquickjs::Object<'js>> = step.into_future().await;
      let message = match outcome {
        Ok(res) => {
          if res.get::<_, bool>("done").unwrap_or(false) {
            return Ok(());
          }
          Ok(chunk_bytes(&res.get::<_, Value<'js>>("value")?))
        },
        // A source that errors must fail the request rather than end the
        // body early, which the server would read as a complete payload.
        Err(e) => Err(e.to_string()),
      };
      let failed = message.is_err();
      // A closed receiver means the request is already over (aborted,
      // or the engine failed); stop pulling rather than spinning.
      if tx.send(message).await.is_err() || failed {
        return Ok(());
      }
    }
  });

  Ok(ferridriver::fetch::channel_stream(rx))
}

/// Bytes behind a stream chunk (`Uint8Array` / `ArrayBuffer` / string).
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

/// A `ReadableStream` that pulls chunks off a live response.
///
/// `cancel()` releases the socket so a partially-read body does not pin
/// the connection.
pub fn from_net<'js>(ctx: &Ctx<'js>, net: NetBody) -> rquickjs::Result<Class<'js, ReadableStream<'js>>> {
  let pull_net = net.clone();
  let pull = PullAlgorithm::from_fn(move |ctx: Ctx<'js>, controller| {
    let ctrl = default_controller(&ctx, controller)?;
    let net = pull_net.clone();
    let resolveable = ResolveablePromise::new(&ctx)?;
    let promise = resolveable.promise.clone();
    let ctx2 = ctx.clone();
    ctx.spawn_exit_simple(async move {
      let mut guard = net.lock().await;
      match guard.as_mut() {
        None => readable_stream_default_controller_close_stream(ctx2, ctrl)?,
        Some(resp) => match tokio::time::timeout(NET_CHUNK_TIMEOUT, resp.chunk()).await {
          Ok(Ok(Some(bytes))) => {
            let chunk = TypedArray::<u8>::new(ctx2.clone(), bytes.to_vec())?.into_value();
            readable_stream_default_controller_enqueue_value(ctx2, ctrl, chunk)?;
          },
          Ok(Ok(None)) => {
            *guard = None;
            readable_stream_default_controller_close_stream(ctx2, ctrl)?;
          },
          Ok(Err(e)) => {
            *guard = None;
            let err = rquickjs::String::from_str(ctx2, &e.to_string())?.into_value();
            readable_stream_default_controller_error_stream(ctrl, err)?;
          },
          Err(_) => {
            *guard = None;
            let err = rquickjs::String::from_str(ctx2, "body read timed out: no chunk within 120s")?.into_value();
            readable_stream_default_controller_error_stream(ctrl, err)?;
          },
        },
      }
      resolveable.resolve_undefined()?;
      Ok(())
    });
    Ok(promise)
  });

  let cancel = CancelAlgorithm::from_fn(move |reason: Value<'js>| {
    // Best-effort: a pull holding the lock finishes its chunk first, and
    // the `None` it then sees closes the stream anyway.
    if let Ok(mut g) = net.try_lock() {
      *g = None;
    }
    resolved_undefined(reason.ctx())
  });

  ReadableStream::from_pull_algorithm(ctx.clone(), pull, cancel)
}
