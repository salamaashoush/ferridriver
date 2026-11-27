//! Page-event plumbing: name matching, subscription draining, event →
//! live JS object conversion, and the predicate wait loops.

use std::sync::Arc;

use ferridriver::Page;

pub(crate) fn match_event_name(name: &str, ev: &ferridriver::events::PageEvent) -> bool {
  ferridriver::events::event_name_matches(name, ev)
}

/// Receive the next subscribed event matching `event_lc`, skipping
/// non-matching events. `None` once the emitter is dropped.
pub(crate) async fn recv_matching(
  rx: &mut ferridriver::EventSubscription<ferridriver::events::PageEvent>,
  event_lc: &str,
) -> Option<ferridriver::events::PageEvent> {
  loop {
    let ev = rx.recv().await?;
    if match_event_name(event_lc, &ev) {
      return Some(ev);
    }
  }
}

/// Lift a core [`ferridriver::events::PageEvent`] into the live class
/// instance Playwright hands to listeners: `ConsoleMessage`, `Request`,
/// `Response`, `WebSocket`, `Dialog`, `FileChooser`, `Download`, a live
/// `Frame` for the frame events, the `Page` itself for `load` /
/// `domcontentloaded` / `close`, and a native JS `Error` for
/// `pageerror`. Mirrors the NAPI binding's `live_event_arg`; shared by
/// the `page.on` event pump and `waitForEvent`.
pub(crate) fn page_event_to_js<'js>(
  ctx: &rquickjs::Ctx<'js>,
  page: &Arc<Page>,
  ev: ferridriver::events::PageEvent,
) -> rquickjs::Result<rquickjs::Value<'js>> {
  use ferridriver::events::PageEvent;
  use rquickjs::IntoJs;
  use rquickjs::class::Class;
  match ev {
    PageEvent::Console(m) => {
      Class::instance(ctx.clone(), crate::bindings::console_message::ConsoleMessageJs::new(m))?.into_js(ctx)
    },
    PageEvent::Request(r) | PageEvent::RequestFinished(r) | PageEvent::RequestFailed(r) => Class::instance(
      ctx.clone(),
      crate::bindings::network::RequestJs::new_with_page(r, Arc::clone(page)),
    )?
    .into_js(ctx),
    PageEvent::Response(r) => Class::instance(
      ctx.clone(),
      crate::bindings::network::ResponseJs::new_with_page(r, Arc::clone(page)),
    )?
    .into_js(ctx),
    PageEvent::WebSocket(ws) => {
      Class::instance(ctx.clone(), crate::bindings::network::WebSocketJs::new(ws))?.into_js(ctx)
    },
    PageEvent::Dialog(d) | PageEvent::DialogClosed(d) => crate::bindings::identity::dialog_object(ctx, d)?.into_js(ctx),
    PageEvent::FileChooser(fc) => {
      Class::instance(ctx.clone(), crate::bindings::file_chooser::FileChooserJs::new(fc))?.into_js(ctx)
    },
    PageEvent::Download(d) => Class::instance(ctx.clone(), crate::bindings::download::DownloadJs::new(d))?.into_js(ctx),
    PageEvent::PageError(err) => crate::bindings::web_error::build_native_error(ctx, err.error()),
    PageEvent::FrameAttached(info)
    | PageEvent::FrameNavigated(info)
    | PageEvent::FrameNavigatedWithinDocument(info) => {
      crate::bindings::identity::frame_object(ctx, page.frame_for_id(&info.frame_id))?.into_js(ctx)
    },
    PageEvent::FrameDetached { frame_id } => {
      crate::bindings::identity::frame_object(ctx, page.frame_for_id(&frame_id))?.into_js(ctx)
    },
    PageEvent::Load | PageEvent::DomContentLoaded | PageEvent::Close | PageEvent::Crash => {
      crate::bindings::identity::page_object(ctx, Arc::clone(page))?.into_js(ctx)
    },
    PageEvent::Popup(popup) => crate::bindings::identity::page_object(ctx, popup)?.into_js(ctx),
    PageEvent::Worker(worker) => crate::bindings::identity::worker_object(ctx, worker)?.into_js(ctx),
  }
}

/// ECMAScript `ToBoolean` for a predicate's return value.
pub(crate) fn js_truthy(v: &rquickjs::Value<'_>) -> bool {
  if v.is_undefined() || v.is_null() {
    return false;
  }
  if let Some(b) = v.as_bool() {
    return b;
  }
  if let Some(i) = v.as_int() {
    return i != 0;
  }
  if let Some(f) = v.as_float() {
    return f != 0.0 && !f.is_nan();
  }
  if let Some(s) = v.as_string() {
    return !s.to_string().unwrap_or_default().is_empty();
  }
  true
}

/// Call a JS predicate and resolve `boolean | Promise<boolean>`.
pub(crate) async fn call_predicate_truthy<'js>(
  pred: &rquickjs::Function<'js>,
  arg: impl rquickjs::IntoJs<'js>,
  ctx: &rquickjs::Ctx<'js>,
) -> rquickjs::Result<bool> {
  let arg = arg.into_js(ctx)?;
  let mp: rquickjs::promise::MaybePromise<'js> = pred.call((arg,))?;
  let v: rquickjs::Value<'js> = mp.into_future().await?;
  Ok(js_truthy(&v))
}

/// Run a URL wait whose condition is `url`: a glob, a `RegExp`, or the
/// caller's `(url: URL) => boolean | Promise<boolean>`. The wait asks a
/// predicate about each URL through a channel, and this future answers on
/// the script's own thread while the wait runs, so the predicate is
/// called like any other callback in the script's turn. What it throws
/// rejects the wait as it was thrown.
pub(crate) async fn wait_for_url_condition<'js, W, F>(
  ctx: &rquickjs::Ctx<'js>,
  url: &rquickjs::Value<'js>,
  wait: W,
) -> rquickjs::Result<()>
where
  W: FnOnce(ferridriver::url_matcher::UrlCondition) -> F,
  F: std::future::Future<Output = ferridriver::error::Result<()>>,
{
  use crate::bindings::convert::FerriResultCtxExt;
  use ferridriver::error::FerriError;
  type Question = (String, tokio::sync::oneshot::Sender<ferridriver::error::Result<bool>>);

  let Some(predicate) = url.as_function().cloned() else {
    let matcher = super::url_value_to_matcher(ctx, url)?;
    return wait(matcher.into()).await.into_js_with(ctx);
  };
  let (asks, mut questions) = tokio::sync::mpsc::unbounded_channel::<Question>();
  let condition = ferridriver::url_matcher::UrlCondition::predicate(move |url| {
    let asks = asks.clone();
    async move {
      let gone = || FerriError::backend("waitForURL: the script stopped answering its predicate");
      let (answer, answered) = tokio::sync::oneshot::channel();
      asks.send((url, answer)).map_err(|_| gone())?;
      answered.await.map_err(|_| gone())?
    }
  });
  let thrown: std::cell::RefCell<Option<rquickjs::Value<'js>>> = std::cell::RefCell::new(None);
  let answering = async {
    while let Some((url, answer)) = questions.recv().await {
      let asked = async {
        let url_ctor: rquickjs::function::Constructor<'js> = ctx.globals().get("URL")?;
        let url_obj: rquickjs::Value<'js> = url_ctor.construct((url,))?;
        call_predicate_truthy(&predicate, url_obj, ctx).await
      };
      let reply = match asked.await {
        Ok(matched) => Ok(matched),
        Err(rquickjs::Error::Exception) => {
          *thrown.borrow_mut() = Some(ctx.catch());
          Err(FerriError::backend("waitForURL: the predicate threw"))
        },
        Err(error) => Err(FerriError::backend(format!("waitForURL: {error}"))),
      };
      let _ = answer.send(reply);
    }
  };
  let waited = {
    let waiting = wait(condition);
    tokio::pin!(answering, waiting);
    tokio::select! {
      biased;
      waited = &mut waiting => waited,
      () = &mut answering => waiting.await,
    }
  };
  match (waited, thrown.into_inner()) {
    (Err(_), Some(exception)) => Err(ctx.throw(exception)),
    (waited, _) => waited.into_js_with(ctx),
  }
}

/// Binding-side wait loop for a `(Request) => boolean` predicate: the
/// predicate needs a live `RequestJs`, so it runs in the JS runtime
/// while the loop drains the page event broadcast.
pub(crate) async fn wait_request_predicate<'js>(
  ctx: rquickjs::Ctx<'js>,
  page: Arc<Page>,
  mut rx: ferridriver::EventSubscription<ferridriver::events::PageEvent>,
  pred: rquickjs::Function<'js>,
  timeout_ms: u64,
) -> rquickjs::Result<crate::bindings::network::RequestJs> {
  use ferridriver::events::PageEvent;
  use rquickjs::class::Class;
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
  loop {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
      return Err(crate::bindings::convert::throw_named(
        &ctx,
        "TimeoutError",
        format!("Timeout {timeout_ms}ms exceeded while waiting for request"),
      ));
    }
    match tokio::time::timeout(remaining, rx.recv()).await {
      Ok(Some(PageEvent::Request(req))) => {
        let probe = crate::bindings::network::RequestJs::new_with_page(req.clone(), Arc::clone(&page));
        let inst = Class::instance(ctx.clone(), probe)?;
        if call_predicate_truthy(&pred, inst, &ctx).await? {
          return Ok(crate::bindings::network::RequestJs::new_with_page(
            req,
            Arc::clone(&page),
          ));
        }
      },
      Ok(Some(_)) => {},
      Ok(None) => {
        return Err(crate::bindings::convert::throw_named(
          &ctx,
          "Error",
          "page closed while waiting for request",
        ));
      },
      Err(_) => {
        return Err(crate::bindings::convert::throw_named(
          &ctx,
          "TimeoutError",
          format!("Timeout {timeout_ms}ms exceeded while waiting for request"),
        ));
      },
    }
  }
}

/// Response-side twin of [`wait_request_predicate`].
pub(crate) async fn wait_response_predicate<'js>(
  ctx: rquickjs::Ctx<'js>,
  page: Arc<Page>,
  mut rx: ferridriver::EventSubscription<ferridriver::events::PageEvent>,
  pred: rquickjs::Function<'js>,
  timeout_ms: u64,
) -> rquickjs::Result<crate::bindings::network::ResponseJs> {
  use ferridriver::events::PageEvent;
  use rquickjs::class::Class;
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
  loop {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
      return Err(crate::bindings::convert::throw_named(
        &ctx,
        "TimeoutError",
        format!("Timeout {timeout_ms}ms exceeded while waiting for response"),
      ));
    }
    match tokio::time::timeout(remaining, rx.recv()).await {
      Ok(Some(PageEvent::Response(resp))) => {
        let probe = crate::bindings::network::ResponseJs::new_with_page(resp.clone(), Arc::clone(&page));
        let inst = Class::instance(ctx.clone(), probe)?;
        if call_predicate_truthy(&pred, inst, &ctx).await? {
          return Ok(crate::bindings::network::ResponseJs::new_with_page(
            resp,
            Arc::clone(&page),
          ));
        }
      },
      Ok(Some(_)) => {},
      Ok(None) => {
        return Err(crate::bindings::convert::throw_named(
          &ctx,
          "Error",
          "page closed while waiting for response",
        ));
      },
      Err(_) => {
        return Err(crate::bindings::convert::throw_named(
          &ctx,
          "TimeoutError",
          format!("Timeout {timeout_ms}ms exceeded while waiting for response"),
        ));
      },
    }
  }
}

impl super::PageJs {
  pub(super) fn off_impl<'js>(
    &self,
    ctx: &rquickjs::Ctx<'js>,
    event: &str,
    listener: Option<&rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    let listener_fn = listener.and_then(|v| v.as_function().cloned());
    let Some(listener_fn) = listener_fn else {
      // Lenient `off(event)` — drop every listener for that event
      // (matches `removeAllListeners(event)` semantics).
      let ids = super::with_page_callbacks(ctx, |r| r.remove_event_listeners_named(event))?;
      for id in ids {
        self.inner.off(ferridriver::events::ListenerId(id));
      }
      return Ok(());
    };
    let target_value: rquickjs::Value<'js> = listener_fn.into_value();
    for (id, saved) in super::with_page_callbacks(ctx, |r| r.event_listeners_named(event))? {
      let restored = saved.restore(ctx)?;
      if restored.into_value() == target_value {
        self.inner.off(ferridriver::events::ListenerId(id));
        super::with_page_callbacks(ctx, |r| r.remove_event_listener(id))?;
      }
    }
    Ok(())
  }

  /// Shared core for `page.on` / `page.once` / `prependListener` / `prependOnceListener`.
  pub(super) fn register_listener<'js>(
    &self,
    ctx: &rquickjs::Ctx<'js>,
    event: &str,
    listener: rquickjs::Function<'js>,
    once: bool,
    front: bool,
  ) -> rquickjs::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};

    let saved = super::SavedCallback::save(ctx, listener);
    let tx = super::ensure_event_pump(ctx);
    let id_slot = Arc::new(AtomicU64::new(0));
    let id_slot_cb = Arc::clone(&id_slot);
    let page_for_cb = Arc::clone(&self.inner);
    let callback: ferridriver::events::EventCallback = Arc::new(move |ev: ferridriver::events::PageEvent| {
      let id = id_slot_cb.load(Ordering::Relaxed);
      let msg = super::callbacks::PageEventMsg::Event {
        id,
        remove_after: once,
        page: Arc::clone(&page_for_cb),
        event: ev,
      };
      if let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) = tx.try_send(msg) {
        tracing::warn!(
          listener_id = id,
          capacity = super::PAGE_EVENT_PUMP_CAPACITY,
          "page event pump full (VM idle between scripts?); dropping event"
        );
      }
    });
    let id = match (once, front) {
      (false, false) => self.inner.on(event, callback),
      (true, false) => self.inner.once(event, callback),
      (false, true) => self.inner.prepend_listener(event, callback),
      (true, true) => self.inner.prepend_once_listener(event, callback),
    };
    id_slot.store(id.0, Ordering::Relaxed);
    let page_key = self.inner.backend_page_id();
    super::with_page_callbacks(ctx, |r| {
      r.insert_event_listener(id.0, event.to_string(), page_key, saved);
    })?;
    Ok(())
  }

  pub(super) fn listeners_impl<'js>(
    &self,
    ctx: &rquickjs::Ctx<'js>,
    event: &str,
  ) -> rquickjs::Result<Vec<rquickjs::Value<'js>>> {
    let saved = super::with_page_callbacks(ctx, |r| r.event_listeners_named(event))?;
    let mut out = Vec::with_capacity(saved.len());
    // Dispatch order lives in core; the registry is a map.
    for id in self.inner.listener_ids_named(event) {
      if let Some((_, cb)) = saved.iter().find(|(saved_id, _)| *saved_id == id.0) {
        out.push(cb.restore(ctx)?.into_value());
      }
    }
    Ok(out)
  }

  pub(super) fn remove_all_listeners_impl<'js>(
    this: rquickjs::function::This<rquickjs::class::Class<'js, Self>>,
    ctx: &rquickjs::Ctx<'js>,
    event: rquickjs::function::Opt<String>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    {
      let page = this.0.borrow();
      if let Some(ev) = event.0 {
        let ids = super::with_page_callbacks(ctx, |r| r.remove_event_listeners_named(&ev))?;
        for id in ids {
          page.inner.off(ferridriver::events::ListenerId(id));
        }
        page.inner.remove_listeners_named(&ev);
      } else {
        page.inner.remove_all_listeners();
        super::with_page_callbacks(ctx, super::PageCallbacks::clear_event_listeners)?;
      }
    }
    let Some(options) = options.0.filter(|v| !v.is_undefined() && !v.is_null()) else {
      return Ok(this.0.into_value());
    };
    let wait = options
      .as_object()
      .and_then(|o| o.get::<_, Option<String>>("behavior").ok().flatten())
      .is_some_and(|behavior| behavior == "wait");
    let tx = super::ensure_event_pump(ctx);
    let promise = rquickjs::promise::Promise::wrap_future(ctx, async move {
      if wait {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        if tx.send(super::callbacks::PageEventMsg::Drain(done_tx)).await.is_ok() {
          let _ = done_rx.await;
        }
      }
      Ok::<(), rquickjs::Error>(())
    })?;
    Ok(promise.into_value())
  }

  pub(super) fn console_messages_impl<'js>(
    &self,
    ctx: &rquickjs::Ctx<'js>,
    options: &rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Vec<rquickjs::Value<'js>>> {
    let filter = ferridriver::observed::ObservedFilter::parse(super::opt_str_field(options, "filter")?.as_deref());
    let msgs = self.inner.console_messages(filter);
    let mut out = Vec::with_capacity(msgs.len());
    for m in msgs {
      let instance =
        rquickjs::class::Class::instance(ctx.clone(), crate::bindings::console_message::ConsoleMessageJs::new(m))?;
      out.push(rquickjs::IntoJs::into_js(instance, ctx)?);
    }
    Ok(out)
  }

  pub(super) fn page_errors_impl<'js>(
    &self,
    ctx: &rquickjs::Ctx<'js>,
    options: &rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Vec<rquickjs::Value<'js>>> {
    let filter = ferridriver::observed::ObservedFilter::parse(super::opt_str_field(options, "filter")?.as_deref());
    let errs = self.inner.page_errors(filter);
    let mut out = Vec::with_capacity(errs.len());
    for e in errs {
      out.push(crate::bindings::web_error::build_native_error(ctx, e.error())?);
    }
    Ok(out)
  }
}
