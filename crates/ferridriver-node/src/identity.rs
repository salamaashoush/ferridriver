//! One JS object per page and per frame while script holds it, as
//! Playwright's channel owners give: `page.mainFrame() ===
//! page.frames()[0]`, and the `Page` a frame reports is the one whose
//! listeners and mouse state the caller set up. Bindings return
//! [`PageObject`] / [`FrameObject`]; the lookup runs when the value
//! crosses into JS, which is also where an async method's result lands.
//!
//! The cache holds weak references, so an object JS has dropped is made
//! afresh: nothing could compare against the old one.

use std::cell::RefCell;
use std::sync::Arc;

use napi::bindgen_prelude::{JavaScriptClassExt, Reference, ToNapiValue, TypeName, ValueType, WeakReference};
use napi::{Env, sys};

use crate::dialog::Dialog;
use crate::frame::Frame;
use crate::page::Page;

thread_local! {
  static PAGES: RefCell<Vec<(Arc<ferridriver::Page>, WeakReference<Page>)>> = const { RefCell::new(Vec::new()) };
  static FRAMES: RefCell<Vec<(ferridriver::Frame, WeakReference<Frame>)>> = const { RefCell::new(Vec::new()) };
  static DIALOGS: RefCell<Vec<(ferridriver::dialog::Dialog, WeakReference<Dialog>)>> = const { RefCell::new(Vec::new()) };
}

/// A page on its way to JS, as the object that already stands for it.
pub struct PageObject(pub Arc<ferridriver::Page>);

/// A frame on its way to JS, as the object that already stands for it.
pub struct FrameObject(pub ferridriver::Frame);

/// A dialog on its way to JS: the same object for its `dialog` and
/// `dialogclosed` events.
pub struct DialogObject(pub ferridriver::dialog::Dialog);

fn known<K, T: 'static>(
  cache: &RefCell<Vec<(K, WeakReference<T>)>>,
  env: Env,
  is_it: impl Fn(&K) -> bool,
) -> napi::Result<Option<Reference<T>>> {
  let weak = cache
    .borrow()
    .iter()
    .find(|(k, _)| is_it(k))
    .map(|(_, weak)| weak.clone());
  match weak {
    Some(weak) => weak.upgrade(env),
    None => Ok(None),
  }
}

impl ToNapiValue for PageObject {
  unsafe fn to_napi_value(env: sys::napi_env, val: Self) -> napi::Result<sys::napi_value> {
    let js_env = Env::from_raw(env);
    let key = val.0.backend_page_id();
    let found = PAGES.with(|pages| known(pages, js_env, |p| p.backend_page_id() == key))?;
    let reference = match found {
      Some(reference) => reference,
      None => {
        let reference = Page::wrap(Arc::clone(&val.0)).into_reference(js_env)?;
        PAGES.with(|pages| {
          let mut pages = pages.borrow_mut();
          pages.retain(|(page, weak)| !page.is_closed() && weak.get().is_some());
          pages.push((val.0, reference.downgrade()));
        });
        reference
      },
    };
    unsafe { Reference::to_napi_value(env, reference) }
  }
}

impl ToNapiValue for FrameObject {
  unsafe fn to_napi_value(env: sys::napi_env, val: Self) -> napi::Result<sys::napi_value> {
    let js_env = Env::from_raw(env);
    let page_key = val.0.page_arc().backend_page_id();
    let found = FRAMES.with(|frames| {
      known(frames, js_env, |f| {
        f.id() == val.0.id() && f.page_arc().backend_page_id() == page_key
      })
    })?;
    let reference = match found {
      Some(reference) => reference,
      None => {
        let reference = Frame::wrap(val.0.clone()).into_reference(js_env)?;
        FRAMES.with(|frames| {
          let mut frames = frames.borrow_mut();
          frames.retain(|(frame, weak)| !frame.is_detached() && !frame.page_arc().is_closed() && weak.get().is_some());
          frames.push((val.0, reference.downgrade()));
        });
        reference
      },
    };
    unsafe { Reference::to_napi_value(env, reference) }
  }
}

impl ToNapiValue for DialogObject {
  unsafe fn to_napi_value(env: sys::napi_env, val: Self) -> napi::Result<sys::napi_value> {
    let js_env = Env::from_raw(env);
    let found = DIALOGS.with(|dialogs| known(dialogs, js_env, |d| d.is_same(&val.0)))?;
    let reference = match found {
      Some(reference) => reference,
      None => {
        let reference = Dialog::from_core(val.0.clone()).into_reference(js_env)?;
        DIALOGS.with(|dialogs| {
          let mut dialogs = dialogs.borrow_mut();
          dialogs.retain(|(_, weak)| weak.get().is_some());
          dialogs.push((val.0, reference.downgrade()));
        });
        reference
      },
    };
    unsafe { Reference::to_napi_value(env, reference) }
  }
}

impl TypeName for DialogObject {
  fn type_name() -> &'static str {
    "Dialog"
  }

  fn value_type() -> ValueType {
    ValueType::Object
  }
}

impl TypeName for PageObject {
  fn type_name() -> &'static str {
    "Page"
  }

  fn value_type() -> ValueType {
    ValueType::Object
  }
}

impl TypeName for FrameObject {
  fn type_name() -> &'static str {
    "Frame"
  }

  fn value_type() -> ValueType {
    ValueType::Object
  }
}
