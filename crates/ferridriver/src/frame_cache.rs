//! Frame tree cache owned by [`crate::Page`].
//!
//! Playwright's `Frame` accessors (`name`, `url`, `parentFrame`,
//! `childFrames`, `isDetached`, `mainFrame`, `frames`, `frame`) are
//! **synchronous** — the wire-level backend streams frame lifecycle
//! events to the client (`Page.frameAttached`, `Page.frameDetached`,
//! `Page.frameNavigated` on CDP; equivalent events on BiDi/WebKit), and
//! the client keeps an up-to-date in-memory tree. The user never waits.
//!
//! `FrameCache` is that tree for ferridriver. It is seeded via a one-shot
//! call to [`crate::backend::AnyPage::get_frame_tree`] when the Page is
//! constructed, and kept fresh by a listener task that subscribes to the
//! emitter's `FrameAttached`/`FrameDetached`/`FrameNavigated` events.
//! Sync accessors on `Page` / `Frame` read from the cache directly.
//!
//! Ordering follows Playwright: `frames()` returns the main frame first
//! (insertion order), then child frames in discovery order.

use crate::backend::FrameInfo;
use rustc_hash::FxHashMap;
use std::sync::Arc;

/// One cached frame record.
#[derive(Debug, Clone)]
pub(crate) struct FrameRecord {
  /// Backend-reported frame metadata.
  pub info: FrameInfo,
  /// `true` once `FrameDetached` fires. Detached frames stay in the cache
  /// so `frame.isDetached()` still answers correctly after detachment —
  /// Playwright mirrors this (`Frame._detached = true`).
  pub detached: bool,
}

/// Page-scoped cache of the frame tree.
#[derive(Debug, Default)]
pub(crate) struct FrameCache {
  /// Ordered list of frame ids (Playwright preserves insertion order
  /// when iterating `_frames`).
  pub(crate) order: Vec<Arc<str>>,
  /// `frame_id -> record`.
  pub(crate) by_id: FxHashMap<Arc<str>, FrameRecord>,
  /// Cached main-frame id (first frame whose `parent_frame_id` is `None`).
  pub(crate) main_id: Option<Arc<str>>,
}

impl FrameCache {
  /// Replace the tree from a fresh `get_frame_tree` response.
  pub(crate) fn seed(&mut self, infos: Vec<FrameInfo>) {
    self.order.clear();
    self.by_id.clear();
    self.main_id = None;
    for info in infos {
      let id: Arc<str> = Arc::from(info.frame_id.as_str());
      if info.parent_frame_id.is_none() && self.main_id.is_none() {
        self.main_id = Some(Arc::clone(&id));
      }
      self.order.push(Arc::clone(&id));
      self.by_id.insert(id, FrameRecord { info, detached: false });
    }
  }

  /// Apply a `Page.frameAttached`-equivalent event.
  ///
  /// A `FrameAttached` event carries `name` only when the backend can fill
  /// it from the attach payload itself. `BiDi`'s
  /// `browsingContext.contextCreated` does NOT carry the iframe's `name`
  /// attribute (it lives in the DOM, not in the `BiDi`-level metadata) —
  /// the backend emits an empty `name` and refreshes via a follow-up
  /// `browsingContext.getTree` / `window.name` eval. If a separate code
  /// path (e.g. `Page::sync_frames` on goto-return) has already seeded
  /// the cache with a populated `name`, we must NOT clobber it with the
  /// empty one from the attach event. Same applies to a clobbered `url`:
  /// keep the prior value when the new one is empty.
  pub(crate) fn attach(&mut self, info: FrameInfo) {
    let id: Arc<str> = Arc::from(info.frame_id.as_str());
    if info.parent_frame_id.is_none() && self.main_id.is_none() {
      self.main_id = Some(Arc::clone(&id));
    }
    let existing = self.by_id.get(&id);
    let merged_name = if info.name.is_empty() {
      existing.map(|r| r.info.name.clone()).unwrap_or_default()
    } else {
      info.name.clone()
    };
    let merged_url = if info.url.is_empty() {
      existing.map(|r| r.info.url.clone()).unwrap_or_default()
    } else {
      info.url.clone()
    };
    if existing.is_none() {
      self.order.push(Arc::clone(&id));
    }
    let merged = FrameInfo {
      frame_id: info.frame_id,
      parent_frame_id: info.parent_frame_id,
      name: merged_name,
      url: merged_url,
    };
    self.by_id.insert(
      id,
      FrameRecord {
        info: merged,
        detached: false,
      },
    );
  }

  /// Apply a `Page.frameDetached` event — flip the `detached` flag. Keep
  /// the record so stale Frame handles still resolve a name/url.
  pub(crate) fn detach(&mut self, id: &str) {
    let key: Arc<str> = Arc::from(id);
    if let Some(rec) = self.by_id.get_mut(&key) {
      rec.detached = true;
    }
  }

  /// Apply a `Page.frameNavigated` event — update name/url but preserve
  /// the cached id and `detached` flag. Sets `main_id` when the
  /// navigated frame is a top-level frame (`parent_frame_id == None`)
  /// and no main frame is yet recorded — covers the bootstrap path
  /// where the eager `Page.getFrameTree` RTT was dropped (`PERF_AUDIT`
  /// §L.3.4 / §M.4): the user's first `page.goto` emits this event
  /// for the main frame, populating the cache without an extra RTT.
  pub(crate) fn navigated(&mut self, info: FrameInfo) {
    let id: Arc<str> = Arc::from(info.frame_id.as_str());
    let existing = self.by_id.get(&id);
    let detached = existing.is_some_and(|r| r.detached);
    if info.parent_frame_id.is_none() && self.main_id.is_none() {
      self.main_id = Some(Arc::clone(&id));
    }
    // Preserve a previously-resolved `name` when this navigation
    // arrives with an empty one (same reasoning as `attach` — BiDi's
    // navigation events do not carry the iframe's DOM-side name).
    let merged_name = if info.name.is_empty() {
      existing.map(|r| r.info.name.clone()).unwrap_or_default()
    } else {
      info.name.clone()
    };
    let merged_url = if info.url.is_empty() {
      existing.map(|r| r.info.url.clone()).unwrap_or_default()
    } else {
      info.url.clone()
    };
    if existing.is_none() {
      self.order.push(Arc::clone(&id));
    }
    let merged = FrameInfo {
      frame_id: info.frame_id,
      parent_frame_id: info.parent_frame_id,
      name: merged_name,
      url: merged_url,
    };
    self.by_id.insert(id, FrameRecord { info: merged, detached });
  }

  /// Snapshot of the main frame record (`None` only before the first
  /// `seed()` or `attach()` of a root frame).
  pub(crate) fn main_frame_id(&self) -> Option<Arc<str>> {
    self.main_id.clone()
  }

  /// Cached record for `id`, if any (includes detached frames).
  pub(crate) fn record(&self, id: &str) -> Option<&FrameRecord> {
    self.by_id.get(id)
  }

  /// Snapshot every cached frame id — includes detached records so
  /// [`crate::element_handle::ElementHandle::content_frame`] can still
  /// attribute an iframe whose frame has just detached.
  pub(crate) fn all_frame_ids(&self) -> Vec<Arc<str>> {
    self.order.clone()
  }

  /// Iterate non-detached frame ids in insertion order.
  pub(crate) fn live_ids(&self) -> impl Iterator<Item = Arc<str>> + '_ {
    self.order.iter().filter_map(|id| {
      let rec = self.by_id.get(id)?;
      if rec.detached { None } else { Some(Arc::clone(id)) }
    })
  }

  /// Iterate ids of the children of `parent_id` (non-detached only).
  pub(crate) fn child_ids(&self, parent_id: &str) -> Vec<Arc<str>> {
    self
      .order
      .iter()
      .filter_map(|id| {
        let rec = self.by_id.get(id)?;
        if rec.detached {
          return None;
        }
        if rec.info.parent_frame_id.as_deref() == Some(parent_id) {
          Some(Arc::clone(id))
        } else {
          None
        }
      })
      .collect()
  }

  /// Parent id of `child_id`, if any.
  pub(crate) fn parent_id(&self, child_id: &str) -> Option<Arc<str>> {
    self.by_id.get(child_id)?.info.parent_frame_id.as_deref().map(Arc::from)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn mk(id: &str, parent: Option<&str>, name: &str, url: &str) -> FrameInfo {
    FrameInfo {
      frame_id: id.into(),
      parent_frame_id: parent.map(str::to_string),
      name: name.into(),
      url: url.into(),
    }
  }

  #[test]
  fn seed_sets_main_and_order() {
    let mut c = FrameCache::default();
    c.seed(vec![
      mk("root", None, "", "about:blank"),
      mk("child-a", Some("root"), "a", "about:blank"),
      mk("child-b", Some("root"), "b", "about:blank"),
    ]);
    assert_eq!(c.main_id.as_deref(), Some("root"));
    assert_eq!(c.order.len(), 3);
    let live: Vec<_> = c.live_ids().map(|id| id.to_string()).collect();
    assert_eq!(live, vec!["root", "child-a", "child-b"]);
  }

  #[test]
  fn navigated_preserves_detached_flag() {
    let mut c = FrameCache::default();
    c.seed(vec![mk("root", None, "", "about:blank")]);
    c.detach("root");
    c.navigated(mk("root", None, "", "https://example.com"));
    assert!(c.by_id.get("root").unwrap().detached);
    assert_eq!(c.by_id.get("root").unwrap().info.url, "https://example.com");
  }

  #[test]
  fn child_ids_filters_detached() {
    let mut c = FrameCache::default();
    c.seed(vec![
      mk("root", None, "", ""),
      mk("a", Some("root"), "a", ""),
      mk("b", Some("root"), "b", ""),
    ]);
    c.detach("a");
    let kids: Vec<_> = c.child_ids("root").into_iter().map(|id| id.to_string()).collect();
    assert_eq!(kids, vec!["b"]);
  }

  #[test]
  fn attach_appends_without_duplicates() {
    let mut c = FrameCache::default();
    c.seed(vec![mk("root", None, "", "")]);
    c.attach(mk("child", Some("root"), "x", ""));
    c.attach(mk("child", Some("root"), "x", "")); // idempotent
    assert_eq!(c.order.len(), 2);
  }
}
