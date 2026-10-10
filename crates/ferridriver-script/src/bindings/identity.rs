//! One JS object per page and per frame, as Playwright's channel owners
//! give: `page.mainFrame() === page.frames()[0]`, and `frame.page()` is
//! the page the frame came from. Every binding that hands a page or a
//! frame to script goes through here.

use std::cell::RefCell;
use std::sync::Arc;

use ferridriver::{Frame, Page};
use rquickjs::{Class, Ctx, JsLifetime, Persistent};

use crate::bindings::frame::FrameJs;
use crate::bindings::page::PageJs;

#[derive(Default)]
struct Wrappers {
  pages: Vec<(Arc<Page>, Persistent<Class<'static, PageJs>>)>,
  frames: Vec<(Frame, Persistent<Class<'static, FrameJs>>)>,
}

impl Wrappers {
  /// Forget closed pages and detached frames, whose objects script can
  /// still hold but no binding will hand out again.
  fn prune(&mut self) {
    self.pages.retain(|(page, _)| !page.is_closed());
    self
      .frames
      .retain(|(frame, _)| !frame.is_detached() && !frame.page_arc().is_closed());
  }
}

struct WrappersUserData(RefCell<Wrappers>);

// SAFETY: holds only `'static` data (`Persistent<…>` handles and owned
// values), so re-stating the unused `'js` lifetime is sound — same
// rationale as `RegistryUserData`.
#[allow(unsafe_code)]
unsafe impl JsLifetime<'_> for WrappersUserData {
  type Changed<'to> = WrappersUserData;
}

fn with_wrappers<R>(ctx: &Ctx<'_>, f: impl FnOnce(&mut Wrappers) -> R) -> R {
  if ctx.userdata::<WrappersUserData>().is_none() {
    let _ = ctx.store_userdata(WrappersUserData(RefCell::new(Wrappers::default())));
  }
  match ctx.userdata::<WrappersUserData>() {
    Some(ud) => f(&mut ud.0.borrow_mut()),
    None => f(&mut Wrappers::default()),
  }
}

/// The page's object, made on first sight.
pub(crate) fn page_object<'js>(ctx: &Ctx<'js>, page: Arc<Page>) -> rquickjs::Result<Class<'js, PageJs>> {
  let key = page.backend_page_id();
  let known = with_wrappers(ctx, |w| {
    w.pages
      .iter()
      .find(|(p, _)| p.backend_page_id() == key)
      .map(|(_, object)| object.clone())
  });
  if let Some(object) = known {
    return object.restore(ctx);
  }
  let object = Class::instance(
    ctx.clone(),
    crate::bindings::page::pagejs_for_ctx(ctx, Arc::clone(&page)),
  )?;
  adopt_page(ctx, &object, page);
  Ok(object)
}

/// Record `object` as the page's object, for a page whose object was made
/// before anything asked for it.
pub(crate) fn adopt_page<'js>(ctx: &Ctx<'js>, object: &Class<'js, PageJs>, page: Arc<Page>) {
  let saved = Persistent::save(ctx, object.clone());
  with_wrappers(ctx, |w| {
    w.prune();
    w.pages.push((page, saved));
  });
}

/// The frame's object, made on first sight.
pub(crate) fn frame_object<'js>(ctx: &Ctx<'js>, frame: Frame) -> rquickjs::Result<Class<'js, FrameJs>> {
  let page_key = frame.page_arc().backend_page_id();
  let known = with_wrappers(ctx, |w| {
    w.frames
      .iter()
      .find(|(f, _)| f.id() == frame.id() && f.page_arc().backend_page_id() == page_key)
      .map(|(_, object)| object.clone())
  });
  if let Some(object) = known {
    return object.restore(ctx);
  }
  let object = Class::instance(ctx.clone(), FrameJs::new(frame.clone()))?;
  let saved = Persistent::save(ctx, object.clone());
  with_wrappers(ctx, |w| {
    w.prune();
    w.frames.push((frame, saved));
  });
  Ok(object)
}
