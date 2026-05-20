//! `WebView` registry — keyed by the `view_id` we hand to the parent in
//! [`Op::CreateView`](ferridriver_webkit_wire::Op::CreateView) replies.

use gtk4::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Per-view state. Holds the `WebView`, its containing `Window` (an
/// offscreen `gtk::Window` so headless works under `xvfb`), the list of
/// parked `WaitNav` request ids, and the user-supplied init scripts
/// stored so we can re-install them after navigation if `WebKit` garbage
/// collected the old `UserContentManager` state.
pub(crate) struct ViewEntry {
  pub web_view: webkit6::WebView,
  /// `Window` keeps the `WebView`'s `GdkSurface` alive — destroying the
  /// window destroys the surface; the view stops loading and
  /// dispatching events without one. We never `present()` it under
  /// xvfb (no compositor) but webkit6 still requires it.
  pub window: gtk4::Window,
  /// `req_id` of every parked `Op::WaitNav` request — the load-changed
  /// signal handler installed in `Op::CreateView` drains this on
  /// `LoadEvent::Finished` and writes `Rep::Ok` to each.
  ///
  /// `Rc<RefCell<…>>` because the GTK signal handler closure and the
  /// dispatch handlers both need mutable access on the same thread.
  pub nav_waiters: Rc<RefCell<Vec<u32>>>,
  /// URI captured at `LoadEvent::Committed`. webkit6's
  /// `WebView::uri()` doesn't track `data:` / `about:srcdoc` loads reliably
  /// — it resets to `about:blank` for them. The load-changed signal
  /// handler stashes the URI here so `Op::GetUrl` can surface the
  /// actually-loaded document URI, matching macOS
  /// `WKWebView.URL.absoluteString`.
  pub committed_uri: Rc<RefCell<Option<String>>>,
  /// User-supplied init scripts (`Op::AddInitScript`). We keep the
  /// source bytes so future Phase 2e work that needs to reinject them
  /// post-navigation has them on hand. Currently the
  /// `UserContentManager` retains them itself.
  #[allow(dead_code)]
  pub init_scripts: Vec<String>,
  /// `UserContentManager` for this view — exposed so `AddInitScript`
  /// can append new `UserScript`s.
  pub ucm: webkit6::UserContentManager,
}

pub(crate) struct ViewRegistry {
  views: HashMap<u64, ViewEntry>,
  next_id: u64,
  /// Shared `NetworkSession` — webkit6 0.6 keeps cookies / cache on
  /// `NetworkSession`. One per host = single context, same envelope as
  /// the macOS host's `WKWebsiteDataStore.nonPersistentDataStore`.
  network_session: Option<webkit6::NetworkSession>,
}

impl ViewRegistry {
  pub fn new() -> Self {
    Self {
      views: HashMap::new(),
      next_id: 1,
      network_session: None,
    }
  }

  /// Lazy `NetworkSession` getter — built on first `CreateView` so we
  /// don't allocate it for hosts that exit before opening a view.
  pub fn network_session(&mut self) -> &webkit6::NetworkSession {
    self
      .network_session
      .get_or_insert_with(webkit6::NetworkSession::new_ephemeral)
  }

  /// Read-only `NetworkSession` accessor for cookie ops that need it
  /// from outside `CreateView`. Returns `None` if no view has ever
  /// been created (no session allocated yet).
  pub fn peek_network_session(&self) -> Option<&webkit6::NetworkSession> {
    self.network_session.as_ref()
  }

  pub fn insert(&mut self, entry: ViewEntry) -> u64 {
    let id = self.next_id;
    self.next_id += 1;
    self.views.insert(id, entry);
    id
  }

  pub fn get(&self, id: u64) -> Option<&ViewEntry> {
    self.views.get(&id)
  }

  /// Held so Phase 2c+ handlers (file-input mutating per-view state,
  /// init-script registration) can mutate `ViewEntry` through the
  /// registry without cloning out the `WebView` first.
  #[allow(dead_code)]
  pub fn get_mut(&mut self, id: u64) -> Option<&mut ViewEntry> {
    self.views.get_mut(&id)
  }

  pub fn remove(&mut self, id: u64) -> Option<ViewEntry> {
    let entry = self.views.remove(&id)?;
    // Destroy the window so the WebView's surface goes away. Without
    // this the GdkSurface lingers until the host exits.
    entry.window.destroy();
    Some(entry)
  }

  pub fn ids(&self) -> Vec<u64> {
    self.views.keys().copied().collect()
  }
}
