//! Wait-method helpers (`waitForNavigation`, `waitForRequest`,
//! `waitForResponse`, `waitForEvent`) for `PageJs`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rquickjs::class::Class;
use rquickjs::function::Opt;
use rquickjs::promise::Promised;
use rquickjs::{Ctx, Function, IntoJs, Value};

use super::PageJs;
use super::events::{
  call_predicate_truthy, match_event_name, page_event_to_js, recv_matching, wait_request_predicate,
  wait_response_predicate,
};
use super::options::{parse_wait_for_navigation_options, parse_wait_timeout, url_value_to_matcher};
use crate::bindings::CallSite;
use crate::bindings::convert::{FerriResultCtxExt, Null, arm_now, ms_f64_to_u64, throw_named};
use crate::bindings::network::{RequestJs, ResponseJs};

type Pending<T> = Pin<Box<dyn Future<Output = ferridriver::Result<T>> + Send>>;

/// What a `page.waitForEvent` listens with, armed at the call.
enum Armed {
  Events(ferridriver::EventSubscription<ferridriver::events::PageEvent>),
  Dialog(Pending<ferridriver::dialog::Dialog>),
  FileChooser(Pending<ferridriver::file_chooser::FileChooser>),
  Download(Pending<ferridriver::download::Download>),
}

/// A `waitForRequest` / `waitForResponse` armed at the call: a JS
/// predicate over the page's events, or the core wait for a URL.
enum NetworkWait<'js, T> {
  Predicate(
    ferridriver::EventSubscription<ferridriver::events::PageEvent>,
    Function<'js>,
  ),
  Matched(Pending<T>),
}

impl PageJs {
  pub(super) fn wait_for_navigation_impl<'js>(
    &self,
    call_site: CallSite,
    ctx: Ctx<'js>,
    options: &Opt<Value<'js>>,
  ) -> Promised<impl Future<Output = rquickjs::Result<Null<ResponseJs>>> + use<'js>> {
    // Listening from the call: the navigation is usually started before
    // the promise is first awaited.
    let navigation = self.inner.wait_for_navigation();
    let opts = arm_now(&ctx, || parse_wait_for_navigation_options(&ctx, options));
    let page = Arc::clone(&self.inner);
    Promised::from(call_site.scope(async move {
      let opts = opts.map_err(|deferred| deferred.raise(&ctx))?;
      let response = navigation.options(opts).await.into_js_with(&ctx)?;
      Ok(Null(response.map(|r| ResponseJs::new_with_page(r, page))))
    }))
  }

  pub(super) fn wait_for_request_impl<'js>(
    &self,
    call_site: CallSite,
    ctx: Ctx<'js>,
    url: &Value<'js>,
    options: Opt<Value<'js>>,
  ) -> Promised<impl Future<Output = rquickjs::Result<RequestJs>> + use<'js>> {
    let page = Arc::clone(&self.inner);
    // Listening starts here, before the promise is returned: the caller's
    // next statement may already send the request.
    let wait = arm_now(&ctx, || {
      let timeout = parse_wait_timeout(&ctx, options)?;
      let wait = match url.as_function() {
        Some(pred) => NetworkWait::Predicate(page.events().subscribe(), pred.clone()),
        None => NetworkWait::Matched(Box::pin(
          page.wait_for_request(url_value_to_matcher(&ctx, url)?, timeout),
        )),
      };
      Ok((timeout, wait))
    });
    Promised::from(call_site.scope(async move {
      let (timeout, wait) = wait.map_err(|deferred| deferred.raise(&ctx))?;
      match wait {
        NetworkWait::Predicate(rx, pred) => {
          let t = timeout.unwrap_or_else(|| page.default_timeout());
          wait_request_predicate(ctx.clone(), page, rx, pred, t).await
        },
        NetworkWait::Matched(request) => Ok(RequestJs::new_with_page(request.await.into_js_with(&ctx)?, page)),
      }
    }))
  }

  pub(super) fn wait_for_response_impl<'js>(
    &self,
    call_site: CallSite,
    ctx: Ctx<'js>,
    url: &Value<'js>,
    options: Opt<Value<'js>>,
  ) -> Promised<impl Future<Output = rquickjs::Result<ResponseJs>> + use<'js>> {
    let page = Arc::clone(&self.inner);
    let wait = arm_now(&ctx, || {
      let timeout = parse_wait_timeout(&ctx, options)?;
      let wait = match url.as_function() {
        Some(pred) => NetworkWait::Predicate(page.events().subscribe(), pred.clone()),
        None => NetworkWait::Matched(Box::pin(
          page.wait_for_response(url_value_to_matcher(&ctx, url)?, timeout),
        )),
      };
      Ok((timeout, wait))
    });
    Promised::from(call_site.scope(async move {
      let (timeout, wait) = wait.map_err(|deferred| deferred.raise(&ctx))?;
      match wait {
        NetworkWait::Predicate(rx, pred) => {
          let t = timeout.unwrap_or_else(|| page.default_timeout());
          wait_response_predicate(ctx.clone(), page, rx, pred, t).await
        },
        NetworkWait::Matched(response) => Ok(ResponseJs::new_with_page(response.await.into_js_with(&ctx)?, page)),
      }
    }))
  }

  pub(super) fn wait_for_event_impl<'js>(
    &self,
    call_site: CallSite,
    ctx: Ctx<'js>,
    event: String,
    options_or_predicate: Opt<Value<'js>>,
  ) -> Promised<impl Future<Output = rquickjs::Result<Value<'js>>> + use<'js>> {
    let page = Arc::clone(&self.inner);
    let event_lc = event.to_ascii_lowercase();
    let options = arm_now(&ctx, || {
      let mut timeout_ms: Option<f64> = None;
      let mut predicate: Option<Function<'js>> = None;
      if let Some(v) = options_or_predicate.0 {
        if let Some(n) = v.as_number() {
          timeout_ms = Some(n);
        } else if let Some(f) = v.as_function() {
          predicate = Some(f.clone());
        } else if let Some(obj) = v.as_object() {
          let t: Value<'js> = obj.get("timeout")?;
          timeout_ms = t.as_number();
          let p: Value<'js> = obj.get("predicate")?;
          predicate = p.as_function().cloned();
        }
      }
      Ok((timeout_ms, predicate))
    });
    // Listening starts here, not at the promise's first poll: the caller's
    // next statement can fire the event while nothing has polled it yet (a
    // script that ends without awaiting, a run of synchronous code).
    // `dialog`, `filechooser` and `download` without a predicate claim the
    // live handle through a one-shot handler on the page's manager, which
    // takes it synchronously with the backend event (Playwright's
    // `addDialogHandler` + `dialogDidOpen` flow).
    let armed = options.map(|(timeout_ms, predicate)| {
      let timeout = timeout_ms.map_or_else(|| page.default_timeout(), ms_f64_to_u64);
      let armed = match (predicate.is_none(), event_lc.as_str()) {
        (true, "dialog") => Armed::Dialog(Box::pin(page.wait_for_dialog(timeout))),
        (true, "filechooser") => Armed::FileChooser(Box::pin(page.wait_for_file_chooser(timeout))),
        (true, "download") => Armed::Download(Box::pin(page.wait_for_download(timeout))),
        _ => Armed::Events(page.events().subscribe()),
      };
      (timeout, predicate, armed)
    });

    Promised::from(call_site.scope(async move {
      let (timeout, predicate, armed) = armed.map_err(|deferred| deferred.raise(&ctx))?;
      let mut rx = match armed {
        Armed::Events(rx) => rx,
        Armed::Dialog(dialog) => {
          let dialog = dialog.await.into_js_with(&ctx)?;
          let instance = crate::bindings::identity::dialog_object(&ctx, dialog)?;
          return instance.into_js(&ctx);
        },
        Armed::FileChooser(chooser) => {
          let chooser = chooser.await.into_js_with(&ctx)?;
          let instance = Class::instance(ctx.clone(), crate::bindings::file_chooser::FileChooserJs::new(chooser))?;
          return instance.into_js(&ctx);
        },
        Armed::Download(download) => {
          let download = download.await.into_js_with(&ctx)?;
          let instance = Class::instance(ctx.clone(), crate::bindings::download::DownloadJs::new(download))?;
          return instance.into_js(&ctx);
        },
      };
      let Some(pred) = predicate else {
        let name = event_lc.clone();
        let ev = rx
          .drain_until(move |e| match_event_name(&name, e), timeout)
          .await
          .into_js_with(&ctx)?;
        return page_event_to_js(&ctx, &page, ev);
      };
      // Predicate waits drain the broadcast for every event type — the
      // emitter bridge claims dialog / filechooser / download live
      // handles on behalf of broadcast listeners, so the predicate sees
      // the same live object a `page.on` listener would.
      let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout);
      loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
          return Err(throw_named(
            &ctx,
            "TimeoutError",
            format!("Timeout {timeout}ms exceeded while waiting for event '{event}'"),
          ));
        }
        let recv = tokio::time::timeout(remaining, recv_matching(&mut rx, &event_lc)).await;
        let Ok(Some(ev)) = recv else {
          if recv.is_err() {
            continue; // deadline check at loop top surfaces the timeout
          }
          return Err(throw_named(&ctx, "Error", "page closed while waiting for event"));
        };
        let arg = page_event_to_js(&ctx, &page, ev)?;
        if call_predicate_truthy(&pred, arg.clone(), &ctx).await? {
          return Ok(arg);
        }
      }
    }))
  }
}
