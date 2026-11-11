//! The frame cache of a `BiDi` page, moved by the transport's reader in
//! wire order. The page listener emits the same changes as page events,
//! but it can lag the reply to a command that caused them (behind a burst
//! of console messages, or a permission grant it awaits), so a `page.url()`
//! read right after an evaluate that ran `history.pushState` would see the
//! old URL.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use rustc_hash::FxHashMap;

use super::transport::{BidiEvent, EventObserver};
use crate::backend::FrameInfo;
use crate::frame_cache::FrameCache;

pub(super) fn frame_observer(
  context: Arc<str>,
  cache: Arc<Mutex<FrameCache>>,
  closed: Arc<AtomicBool>,
) -> EventObserver {
  // Each child frame of the page, at any depth, and the frame it is in.
  let children: Mutex<FxHashMap<String, String>> = Mutex::new(FxHashMap::default());
  Arc::new(move |event: &BidiEvent| {
    if closed.load(Ordering::Acquire) {
      return false;
    }
    let text = |key: &str| event.params.get(key).and_then(|v| v.as_str()).unwrap_or("");
    let frame = text("context");
    let mut children = children.lock().unwrap_or_else(PoisonError::into_inner);
    // `None` when the event is about another page's frame; otherwise the
    // frame's parent, `None` for the page's own context.
    let parent_of = |frame: &str| -> Option<Option<String>> {
      if frame == &*context {
        Some(None)
      } else {
        children.get(frame).map(|parent| Some(parent.clone()))
      }
    };
    match event.method.as_str() {
      "browsingContext.contextCreated" => {
        let parent = text("parent");
        if !parent.is_empty() && (parent == &*context || children.contains_key(parent)) {
          children.insert(frame.to_owned(), parent.to_owned());
          cache.lock().unwrap_or_else(PoisonError::into_inner).attach(FrameInfo {
            frame_id: frame.to_owned(),
            parent_frame_id: Some(parent.to_owned()),
            name: String::new(),
            url: text("url").to_owned(),
          });
        }
      },
      "browsingContext.contextDestroyed" => {
        if frame == &*context {
          return false;
        }
        if children.remove(frame).is_some() {
          cache.lock().unwrap_or_else(PoisonError::into_inner).detach(frame);
        }
      },
      "browsingContext.navigationCommitted" => {
        if let Some(parent) = parent_of(frame) {
          cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .navigated(FrameInfo {
              frame_id: frame.to_owned(),
              parent_frame_id: parent,
              name: String::new(),
              url: text("url").to_owned(),
            });
        }
      },
      "browsingContext.historyUpdated" | "browsingContext.fragmentNavigated" if parent_of(frame).is_some() => {
        cache
          .lock()
          .unwrap_or_else(PoisonError::into_inner)
          .navigated_within(frame, text("url"));
      },
      _ => {},
    }
    true
  })
}
