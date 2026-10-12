//! One JS object per page, frame and dialog, as Playwright's channel
//! owners give: `page.mainFrame() === page.frames()[0]`, and the `Page` a
//! frame reports is the one whose listeners and mouse state the caller
//! set up. Bindings return [`PageObject`] / [`FrameObject`] /
//! [`DialogObject`]; the lookup runs when the value crosses into JS,
//! which is also where an async method's result lands.
//!
//! Like Playwright's client, which keeps an object until the server
//! disposes it, the cache holds each object strongly until its page
//! closes or its frame detaches. A weak hold is not an option: upgrading
//! one whose object was collected but not yet finalized fails inside
//! napi, and napi-rs aborts the process on it.

use std::cell::RefCell;
use std::mem::ManuallyDrop;
use std::sync::Arc;

use napi::bindgen_prelude::{JavaScriptClassExt, Reference, ToNapiValue, TypeName, ValueType};
use napi::{Env, sys};

use crate::android::AndroidDevice;
use crate::dialog::Dialog;
use crate::frame::Frame;
use crate::page::Page;

/// Closed dialogs kept so a late `dialogclosed` hands over the same
/// object the `dialog` event did.
const MAX_CLOSED_DIALOGS: usize = 16;

/// Entries are released only through [`Cache::retain`], on the JS thread.
/// `ManuallyDrop` keeps thread teardown from releasing them after the
/// environment is gone, which napi reports as a failure.
struct Cache<K, T: 'static> {
  entries: Vec<(K, ManuallyDrop<Reference<T>>)>,
}

impl<K, T: 'static> Cache<K, T> {
  const fn new() -> Self {
    Self { entries: Vec::new() }
  }

  fn find(&self, env: Env, is_it: impl Fn(&K) -> bool) -> napi::Result<Option<Reference<T>>> {
    match self.entries.iter().find(|(k, _)| is_it(k)) {
      Some((_, reference)) => Reference::clone(reference, env).map(Some),
      None => Ok(None),
    }
  }

  fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) {
    let mut i = 0;
    while i < self.entries.len() {
      if keep(&self.entries[i].0) {
        i += 1;
      } else {
        let (_, mut reference) = self.entries.remove(i);
        // SAFETY: removed from the cache, so dropped exactly once, here.
        unsafe { ManuallyDrop::drop(&mut reference) };
      }
    }
  }

  fn insert(&mut self, env: Env, key: K, reference: &Reference<T>) -> napi::Result<()> {
    self.entries.push((key, ManuallyDrop::new(reference.clone(env)?)));
    Ok(())
  }
}

thread_local! {
  static PAGES: RefCell<Cache<Arc<ferridriver::Page>, Page>> = const { RefCell::new(Cache::new()) };
  static FRAMES: RefCell<Cache<ferridriver::Frame, Frame>> = const { RefCell::new(Cache::new()) };
  static DIALOGS: RefCell<Cache<ferridriver::dialog::Dialog, Dialog>> = const { RefCell::new(Cache::new()) };
  static ANDROID_DEVICES: RefCell<Cache<ferridriver::android::AndroidDevice, AndroidDevice>> =
    const { RefCell::new(Cache::new()) };
}

/// A page on its way to JS, as the object that already stands for it.
pub struct PageObject(pub Arc<ferridriver::Page>);

/// A frame on its way to JS, as the object that already stands for it.
pub struct FrameObject(pub ferridriver::Frame);

/// A dialog on its way to JS: the same object for its `dialog` and
/// `dialogclosed` events.
pub struct DialogObject(pub ferridriver::dialog::Dialog);

impl ToNapiValue for PageObject {
  unsafe fn to_napi_value(env: sys::napi_env, val: Self) -> napi::Result<sys::napi_value> {
    let js_env = Env::from_raw(env);
    let key = val.0.backend_page_id();
    let found = PAGES.with(|pages| pages.borrow().find(js_env, |p| p.backend_page_id() == key))?;
    let reference = match found {
      Some(reference) => reference,
      None => {
        let reference = Page::wrap(Arc::clone(&val.0)).into_reference(js_env)?;
        PAGES.with(|pages| {
          let mut pages = pages.borrow_mut();
          pages.retain(|page| !page.is_closed());
          pages.insert(js_env, val.0, &reference)
        })?;
        reference
      },
    };
    unsafe { Reference::to_napi_value(env, reference) }
  }
}

/// A device on its way to JS: `android.devices()` hands out the same
/// object for a device until it is closed.
pub struct AndroidDeviceObject(pub ferridriver::android::AndroidDevice);

impl ToNapiValue for AndroidDeviceObject {
  unsafe fn to_napi_value(env: sys::napi_env, val: Self) -> napi::Result<sys::napi_value> {
    let js_env = Env::from_raw(env);
    let found = ANDROID_DEVICES.with(|devices| devices.borrow().find(js_env, |device| *device == val.0))?;
    let reference = match found {
      Some(reference) => reference,
      None => {
        let reference = AndroidDevice::from_core(val.0.clone()).into_reference(js_env)?;
        ANDROID_DEVICES.with(|devices| {
          let mut devices = devices.borrow_mut();
          devices.retain(|device| !device.is_closed());
          devices.insert(js_env, val.0, &reference)
        })?;
        reference
      },
    };
    unsafe { Reference::to_napi_value(env, reference) }
  }
}

impl TypeName for AndroidDeviceObject {
  fn type_name() -> &'static str {
    "AndroidDevice"
  }

  fn value_type() -> ValueType {
    ValueType::Object
  }
}

impl ToNapiValue for FrameObject {
  unsafe fn to_napi_value(env: sys::napi_env, val: Self) -> napi::Result<sys::napi_value> {
    let js_env = Env::from_raw(env);
    let page_key = val.0.page_arc().backend_page_id();
    let found = FRAMES.with(|frames| {
      frames.borrow().find(js_env, |f| {
        f.id() == val.0.id() && f.page_arc().backend_page_id() == page_key
      })
    })?;
    let reference = match found {
      Some(reference) => reference,
      None => {
        let reference = Frame::wrap(val.0.clone()).into_reference(js_env)?;
        FRAMES.with(|frames| {
          let mut frames = frames.borrow_mut();
          frames.retain(|frame| !frame.is_detached() && !frame.page_arc().is_closed());
          frames.insert(js_env, val.0, &reference)
        })?;
        reference
      },
    };
    unsafe { Reference::to_napi_value(env, reference) }
  }
}

impl ToNapiValue for DialogObject {
  unsafe fn to_napi_value(env: sys::napi_env, val: Self) -> napi::Result<sys::napi_value> {
    let js_env = Env::from_raw(env);
    let found = DIALOGS.with(|dialogs| dialogs.borrow().find(js_env, |d| d.is_same(&val.0)))?;
    let reference = match found {
      Some(reference) => reference,
      None => {
        let reference = Dialog::from_core(val.0.clone()).into_reference(js_env)?;
        DIALOGS.with(|dialogs| {
          let mut dialogs = dialogs.borrow_mut();
          let mut closed = dialogs.entries.iter().filter(|(d, _)| d.is_closed()).count();
          dialogs.retain(|dialog| {
            if closed > MAX_CLOSED_DIALOGS && dialog.is_closed() {
              closed -= 1;
              return false;
            }
            true
          });
          dialogs.insert(js_env, val.0, &reference)
        })?;
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
