//! Single-owner VM event loop.
//!
//! rquickjs's schedular wake queue holds a SINGLE `AtomicWaker` slot,
//! re-registered by whichever future last polled the schedular. Any
//! short-lived `async_with!` that awaits inside its closure polls the
//! schedular, steals that slot, and dies with it — after which every
//! external completion of a schedular task (a backend response resolving
//! an awaited `page.evaluate`, an mpsc send into a `ctx.spawn` pump)
//! wakes a dead task and the VM never resumes. `AsyncRuntime::drive()`
//! is no fix: it contends for the same single slot, is only woken by
//! new spawns (never by wakes of existing tasks), and crashes under
//! multi-thread load (SIGSEGV in its lock-release path).
//!
//! The fix is the architecture every production QuickJS embedding
//! converges on (LLRT, txiki.js, quickjs-libc, Deno's isolate loop):
//! exactly ONE never-completing future owns the VM and its schedular;
//! everything else sends messages. [`spawn_vm_loop`] starts that future
//! (a single persistent `async_with` per session VM); [`VmHandle::with`]
//! submits a closure as a job which the loop `ctx.spawn`s onto the
//! schedular, so jobs run concurrently with each other and with the
//! loop's own `recv` — an execute parked on a host await never blocks a
//! route/exposeFunction/WS dispatch job arriving behind it. Because the
//! loop future never completes, every wake — schedular queue slot,
//! spawner `listen`, channel recv — always targets a live task.

use std::future::Future;
use std::pin::Pin;

use rquickjs::{AsyncContext, Ctx};

use crate::error::ScriptError;

/// A unit of work executed inside the session's VM event loop. The
/// closure runs under the runtime lock on the loop's execution context;
/// its future is `ctx.spawn`ed so it interleaves with other jobs.
pub type VmJob = Box<dyn for<'js> FnOnce(Ctx<'js>) -> Pin<Box<dyn Future<Output = ()> + Send + 'js>> + Send>;

/// Cloneable submission handle to a session's VM event loop.
#[derive(Clone)]
pub struct VmHandle {
  tx: tokio::sync::mpsc::UnboundedSender<VmJob>,
}

impl VmHandle {
  /// Run `f` inside the VM event loop and await its result.
  ///
  /// Errors only when the loop is gone (session VM discarded): the job
  /// could not be submitted, or the loop dropped it before completion.
  pub async fn with<R, F>(&self, f: F) -> Result<R, ScriptError>
  where
    R: Send + 'static,
    F: for<'js> FnOnce(Ctx<'js>) -> Pin<Box<dyn Future<Output = R> + Send + 'js>> + Send + 'static,
  {
    let (tx, rx) = tokio::sync::oneshot::channel::<R>();
    let job: VmJob = Box::new(move |ctx| {
      Box::pin(async move {
        let r = f(ctx).await;
        let _ = tx.send(r);
      })
    });
    self
      .tx
      .send(job)
      .map_err(|_| ScriptError::internal("session VM loop is gone".to_string()))?;
    rx.await
      .map_err(|_| ScriptError::internal("session VM loop dropped the job".to_string()))
  }
}

/// Signals the VM event loop to finish. Dropping it (with the session)
/// makes the loop break out of `recv`, so the loop future completes
/// normally and releases its `AsyncContext` on its own task — never
/// abort the loop task: tearing the `WithFuture` down mid-flight on a
/// foreign thread leaves live GC objects behind and trips QuickJS's
/// `JS_FreeRuntime` `gc_obj_list` assertion.
pub struct VmShutdown {
  _tx: tokio::sync::oneshot::Sender<()>,
}

/// Spawn the session's single persistent VM driver. The loop runs until
/// the returned [`VmShutdown`] is dropped (or every `VmHandle` clone is
/// gone); until then the task holds a clone of `ctx` (keeping the
/// runtime alive) and is the only future that ever polls the runtime's
/// schedular.
pub fn spawn_vm_loop(ctx: &AsyncContext) -> (VmHandle, VmShutdown) {
  let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<VmJob>();
  let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
  let loop_ctx = ctx.clone();
  tokio::spawn(async move {
    rquickjs::async_with!(loop_ctx => |ctx| {
      loop {
        tokio::select! {
          job = rx.recv() => match job {
            Some(job) => ctx.spawn(job(ctx.clone())),
            None => break,
          },
          _ = &mut shutdown_rx => break,
        }
      }
    })
    .await;
  });
  (VmHandle { tx }, VmShutdown { _tx: shutdown_tx })
}

/// [`VmHandle::with`] with `async_with!` ergonomics: the body runs on
/// the VM event loop with `$ctx: Ctx<'js>` in scope.
#[macro_export]
macro_rules! vm_with {
  ($vm:expr => |$ctx:ident| { $($t:tt)* }) => {
    $vm.with(move |$ctx| {
      // SAFETY: identical argument to rquickjs's own `async_with!`
      // uplift. Everything is moved into the closure (enforced Send),
      // the future is created and driven only under the runtime lock
      // (the loop `ctx.spawn`s it onto the schedular, polled by the
      // single loop future), and nothing borrowed can escape — so
      // recasting the future's lifetime and marking it Send is sound.
      #[allow(unsafe_code)]
      unsafe fn uplift<'a, 'b, R>(
        f: ::core::pin::Pin<::std::boxed::Box<dyn ::core::future::Future<Output = R> + 'a>>,
      ) -> ::core::pin::Pin<::std::boxed::Box<dyn ::core::future::Future<Output = R> + 'b + ::core::marker::Send>>
      {
        unsafe { ::core::mem::transmute(f) }
      }
      let fut = ::std::boxed::Box::pin(async move { $($t)* });
      #[allow(unsafe_code)]
      unsafe {
        uplift(fut)
      }
    })
  };
}
