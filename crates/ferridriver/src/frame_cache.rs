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

/// Page-scoped cache of the frame tree. A page always has a main frame,
/// so the cache is built around one.
#[derive(Debug)]
pub(crate) struct FrameCache {
  /// Ordered list of frame ids (Playwright preserves insertion order
  /// when iterating `_frames`).
  pub(crate) order: Vec<Arc<str>>,
  /// `frame_id -> record`.
  pub(crate) by_id: FxHashMap<Arc<str>, FrameRecord>,
  /// The main frame's current id: the latest top-level frame reported.
  pub(crate) main_id: Arc<str>,
}

impl FrameCache {
  /// A cache holding only the main frame, under the id the backend knows
  /// it by when the page is created.
  pub(crate) fn new(main_frame_id: &str) -> Self {
    let main_id: Arc<str> = Arc::from(main_frame_id);
    let record = FrameRecord {
      info: FrameInfo {
        frame_id: main_frame_id.to_string(),
        parent_frame_id: None,
        name: String::new(),
        url: String::new(),
      },
      detached: false,
    };
    let mut by_id = FxHashMap::default();
    by_id.insert(Arc::clone(&main_id), record);
    Self {
      order: vec![Arc::clone(&main_id)],
      by_id,
      main_id,
    }
  }

  /// The page moved to a new process: every frame but the main one is
  /// gone, and the main frame keeps its record until the new process
  /// reports it under its new id.
  pub(crate) fn new_process(&mut self) {
    let main = Arc::clone(&self.main_id);
    self.by_id.retain(|id, _| *id == main);
    self.order.retain(|id| *id == main);
  }

  /// Merge a fresh `get_frame_tree` response into the cache. Adds any
  /// frame the tree carries that we didn't already have and updates
  /// the name/url for frames whose record was previously attached with
  /// an empty placeholder. Does NOT remove frames present in the cache
  /// but missing from the tree — those may have been added via an
  /// in-flight `FrameAttached` event the seeding caller's underlying
  /// `getTree` round-trip raced. Detach is a separate explicit step
  /// via [`Self::detach`].
  pub(crate) fn seed(&mut self, infos: Vec<FrameInfo>) {
    for info in infos {
      self.attach(info);
    }
  }

  /// Record a new frame's place: the main frame leads `page.frames()` as
  /// it does upstream, even when its children were reported first.
  fn place(&mut self, id: &Arc<str>, is_main: bool) {
    if is_main {
      self.order.insert(0, Arc::clone(id));
    } else {
      self.order.push(Arc::clone(id));
    }
  }

  /// Make `id` the main frame. A top-level frame under a new id is the
  /// main frame after a cross-process navigation, so the old id's record
  /// goes and the main frame keeps its identity, as in Playwright's
  /// `FrameManager.frameAttached`.
  fn adopt_main(&mut self, id: &Arc<str>) {
    if self.main_id == *id {
      return;
    }
    let old = std::mem::replace(&mut self.main_id, Arc::clone(id));
    self.detach_descendants(&old);
    self.by_id.remove(&old);
    self.order.retain(|known| *known != old);
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
    if info.parent_frame_id.is_none() {
      self.adopt_main(&id);
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
      self.place(&id, info.parent_frame_id.is_none());
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
    self.detach_descendants(id);
  }

  /// Apply a `Page.frameNavigated` event — update name/url but preserve
  /// the cached id and `detached` flag. A top-level frame
  /// (`parent_frame_id == None`) becomes the main frame — covers the
  /// bootstrap path where the eager `Page.getFrameTree` RTT was dropped
  /// (`PERF_AUDIT` §L.3.4 / §M.4): the user's first `page.goto` emits
  /// this event for the main frame, populating the cache without an
  /// extra RTT.
  pub(crate) fn navigated(&mut self, info: FrameInfo) {
    let id: Arc<str> = Arc::from(info.frame_id.as_str());
    if info.parent_frame_id.is_none() {
      self.adopt_main(&id);
    }
    let existing = self.by_id.get(&id);
    let detached = existing.is_some_and(|r| r.detached);
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
      self.place(&id, info.parent_frame_id.is_none());
    }
    let merged = FrameInfo {
      frame_id: info.frame_id,
      parent_frame_id: info.parent_frame_id,
      name: merged_name,
      url: merged_url,
    };
    self
      .by_id
      .insert(Arc::clone(&id), FrameRecord { info: merged, detached });
    // A cross-document navigation destroys the frame's subtree (CDP
    // `frameNavigated` is cross-document only). Backends do not
    // reliably emit `frameDetached` for the children, and a stale
    // "live" child wedges every consumer that round-trips into it
    // (trace snapshot captures stalled 2s per dead frame per capture).
    // Mirrors Playwright's frameManager, which drops child frames on
    // the parent's navigation.
    self.detach_descendants(&id);
  }

  /// Apply a same-document navigation (`history.pushState` /
  /// `replaceState` / fragment change): the document persists, so only
  /// the URL changes — name, parentage, and the child-frame subtree all
  /// survive, unlike [`Self::navigated`], which models a cross-document
  /// swap.
  pub(crate) fn navigated_within(&mut self, frame_id: &str, url: &str) {
    let key: Arc<str> = Arc::from(frame_id);
    if let Some(rec) = self.by_id.get_mut(&key) {
      rec.info.url = url.to_string();
    }
  }

  pub(crate) fn detach_descendants(&mut self, parent: &str) {
    let children: Vec<Arc<str>> = self
      .by_id
      .iter()
      .filter(|(_, rec)| !rec.detached && rec.info.parent_frame_id.as_deref() == Some(parent))
      .map(|(id, _)| Arc::clone(id))
      .collect();
    for child in children {
      if let Some(rec) = self.by_id.get_mut(&child) {
        rec.detached = true;
      }
      self.detach_descendants(&child);
    }
  }

  pub(crate) fn main_frame_id(&self) -> Arc<str> {
    Arc::clone(&self.main_id)
  }

  /// Cached record for `id`, if any (includes detached frames).
  /// Record the name a frame was found to have after it was cached.
  pub(crate) fn rename(&mut self, id: &str, name: &str) {
    if let Some(record) = self.by_id.get_mut(id) {
      name.clone_into(&mut record.info.name);
    }
  }

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
    let mut c = FrameCache::new("initial");
    c.seed(vec![
      mk("root", None, "", "about:blank"),
      mk("child-a", Some("root"), "a", "about:blank"),
      mk("child-b", Some("root"), "b", "about:blank"),
    ]);
    assert_eq!(&*c.main_id, "root");
    assert_eq!(c.order.len(), 3);
    let live: Vec<_> = c.live_ids().map(|id| id.to_string()).collect();
    assert_eq!(live, vec!["root", "child-a", "child-b"]);
  }

  #[test]
  fn navigated_preserves_detached_flag() {
    let mut c = FrameCache::new("initial");
    c.seed(vec![mk("root", None, "", "about:blank")]);
    c.detach("root");
    c.navigated(mk("root", None, "", "https://example.com"));
    assert!(c.by_id.get("root").unwrap().detached);
    assert_eq!(c.by_id.get("root").unwrap().info.url, "https://example.com");
  }

  #[test]
  fn child_ids_filters_detached() {
    let mut c = FrameCache::new("initial");
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
  fn main_frame_leads_when_attached_after_its_children() {
    let mut c = FrameCache::new("initial");
    c.attach(mk("child", Some("root"), "", ""));
    c.attach(mk("root", None, "", ""));
    let ids: Vec<_> = c.live_ids().map(|id| id.to_string()).collect();
    assert_eq!(ids, vec!["root", "child"]);
  }

  #[test]
  fn a_new_top_level_frame_takes_over_the_main_frame() {
    let mut c = FrameCache::new("initial");
    c.seed(vec![
      mk("old", None, "", "https://a.test/"),
      mk("kid", Some("old"), "", ""),
    ]);
    c.navigated(mk("new", None, "", "https://b.test/"));
    assert_eq!(&*c.main_id, "new");
    assert!(c.record("old").is_none());
    assert!(c.by_id.get("kid").unwrap().detached);
    let live: Vec<_> = c.live_ids().map(|id| id.to_string()).collect();
    assert_eq!(live, vec!["new"]);
  }

  #[test]
  fn a_new_process_keeps_only_the_main_frame() {
    let mut c = FrameCache::new("initial");
    c.seed(vec![
      mk("root", None, "", "https://a.test/"),
      mk("kid", Some("root"), "", ""),
    ]);
    c.new_process();
    assert_eq!(&*c.main_id, "root");
    assert_eq!(c.record("root").unwrap().info.url, "https://a.test/");
    assert!(c.record("kid").is_none());
    assert_eq!(c.order.len(), 1);
  }

  #[test]
  fn attach_appends_without_duplicates() {
    let mut c = FrameCache::new("initial");
    c.seed(vec![mk("root", None, "", "")]);
    c.attach(mk("child", Some("root"), "x", ""));
    c.attach(mk("child", Some("root"), "x", "")); // idempotent
    assert_eq!(c.order.len(), 2);
  }
}
