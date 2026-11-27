//! Context-level event arguments, callback dispatch, and `BindingSource` conversion.

use super::BrowserContext;
use napi::Result;

/// Playwright's `BindingSource` for an `exposeBinding` callback
/// (`packages/playwright-core/types/structs.d.ts`): the live
/// `{ context, page, frame }` the call came from. The page and frame are
/// the objects that already stand for them; a source with no page behind
/// it (the page went away as its call was dispatched) has none of the
/// three.
pub(crate) struct BindingSourceJs(pub ferridriver::events::BindingSource);

#[expect(unsafe_code, reason = "napi-rs declares ToNapiValue::to_napi_value unsafe")]
impl napi::bindgen_prelude::ToNapiValue for BindingSourceJs {
  unsafe fn to_napi_value(env: napi::sys::napi_env, val: Self) -> napi::Result<napi::sys::napi_value> {
    let js_env = napi::Env::from_raw(env);
    let mut object = napi::bindgen_prelude::Object::new(&js_env)?;
    if let Some(context) = val.0.live_context() {
      object.set("context", BrowserContext::wrap(context))?;
    }
    if let Some(page) = val.0.live_page() {
      object.set("page", crate::identity::PageObject(page))?;
    }
    if let Some(frame) = val.0.live_frame() {
      object.set("frame", crate::identity::FrameObject(frame))?;
    }
    // SAFETY: `env` is the live environment napi-rs handed this conversion.
    unsafe { napi::bindgen_prelude::Object::to_napi_value(env, object) }
  }
}

/// Cross-thread dispatch arg for context-level `on`/`once` callbacks.
/// Carries the live core payload across the tokio→napi boundary; the
/// `ToNapiValue` conversion (run on the JS thread) wraps it in the
/// Playwright-shaped class instance per variant. Mirrors the page
/// binding's `live_event_arg` fan-out.
pub enum ContextEventArg {
  WebError(ferridriver::web_error::WebError),
  Download(ferridriver::download::Download),
  Frame {
    page: std::sync::Arc<ferridriver::Page>,
    frame_id: String,
  },
  Page(std::sync::Arc<ferridriver::Page>),
  Dialog(ferridriver::dialog::Dialog),
  Console(ferridriver::console_message::ConsoleMessage),
  Request {
    page: std::sync::Arc<ferridriver::Page>,
    request: ferridriver::network::Request,
  },
  Response {
    page: std::sync::Arc<ferridriver::Page>,
    response: ferridriver::network::Response,
  },
  Context(ferridriver::context::ContextRef),
}

#[expect(unsafe_code, reason = "napi-rs declares ToNapiValue::to_napi_value unsafe")]
impl napi::bindgen_prelude::ToNapiValue for ContextEventArg {
  unsafe fn to_napi_value(env: napi::sys::napi_env, val: Self) -> napi::Result<napi::sys::napi_value> {
    match val {
      // SAFETY: `env` is the live environment napi-rs handed this conversion.
      ContextEventArg::WebError(err) => unsafe {
        crate::web_error::WebErrorArg::to_napi_value(env, crate::web_error::WebErrorArg(err))
      },
      ContextEventArg::Download(d) => {
        let wrapper = crate::download::Download::from_core(d);
        // SAFETY: `env` is the live environment napi-rs handed this conversion.
        unsafe { crate::download::Download::to_napi_value(env, wrapper) }
      },
      // SAFETY: `env` is the live environment napi-rs handed this conversion.
      ContextEventArg::Frame { page, frame_id } => unsafe {
        crate::identity::FrameObject::to_napi_value(env, crate::identity::FrameObject(page.frame_for_id(&frame_id)))
      },
      // SAFETY: `env` is the live environment napi-rs handed this conversion.
      ContextEventArg::Page(page) => unsafe {
        crate::identity::PageObject::to_napi_value(env, crate::identity::PageObject(page))
      },
      // SAFETY: `env` is the live environment napi-rs handed this conversion.
      ContextEventArg::Dialog(dialog) => unsafe {
        crate::identity::DialogObject::to_napi_value(env, crate::identity::DialogObject(dialog))
      },
      // SAFETY: `env` is the live environment napi-rs handed this conversion.
      ContextEventArg::Console(message) => unsafe {
        crate::console_message::ConsoleMessage::to_napi_value(
          env,
          crate::console_message::ConsoleMessage::from_core(message),
        )
      },
      // SAFETY: `env` is the live environment napi-rs handed this conversion.
      ContextEventArg::Request { page, request } => unsafe {
        crate::network::Request::to_napi_value(env, crate::network::Request::from_core_with_page(request, page))
      },
      // SAFETY: `env` is the live environment napi-rs handed this conversion.
      ContextEventArg::Response { page, response } => unsafe {
        crate::network::Response::to_napi_value(env, crate::network::Response::from_core_with_page(response, page))
      },
      // SAFETY: `env` is the live environment napi-rs handed this conversion.
      ContextEventArg::Context(context) => unsafe { BrowserContext::to_napi_value(env, BrowserContext::wrap(context)) },
    }
  }
}

impl ContextEventArg {
  /// Lower a core [`ContextEvent`] into the matching cross-thread arg.
  pub(super) fn from_event(ev: ferridriver::events::ContextEvent) -> Self {
    use ferridriver::events::ContextEvent;
    match ev {
      ContextEvent::WebError(err) => Self::WebError(err),
      ContextEvent::Download(d) => Self::Download(d),
      ContextEvent::FrameAttached { page, frame_id }
      | ContextEvent::FrameDetached { page, frame_id }
      | ContextEvent::FrameNavigated { page, frame_id } => Self::Frame { page, frame_id },
      ContextEvent::Page(page) | ContextEvent::PageClose(page) | ContextEvent::PageLoad(page) => Self::Page(page),
      ContextEvent::Dialog(dialog) | ContextEvent::DialogClosed(dialog) => Self::Dialog(dialog),
      ContextEvent::Console(message) => Self::Console(message),
      ContextEvent::Request { page, request }
      | ContextEvent::RequestFinished { page, request }
      | ContextEvent::RequestFailed { page, request } => Self::Request { page, request },
      ContextEvent::Response { page, response } => Self::Response { page, response },
      ContextEvent::Close(context) => Self::Context(context),
    }
  }
}

/// Lower a JS listener `Function<'_>` (which is `!Send` because it
/// holds a raw NAPI value pointer) into a pure-Send
/// [`ferridriver::events::ContextEventCallback`].
pub(super) fn build_context_event_callback(
  listener: &napi::bindgen_prelude::Function<'_, ContextEventArg, ()>,
) -> Result<ferridriver::events::ContextEventCallback> {
  let tsfn = listener
    .build_threadsafe_function()
    .callee_handled::<false>()
    .weak::<true>()
    .max_queue_size::<0>()
    .build()?;
  Ok(std::sync::Arc::new(move |ev| {
    tsfn.call(
      ContextEventArg::from_event(ev),
      napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
    );
  }))
}
