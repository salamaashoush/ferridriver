//! Frame, navigation and thread metadata.
//!
//! Everything else keys off this: an insight about "the document
//! request" or "the LCP" means the main frame's, and picking the wrong
//! main frame silently reports an iframe's numbers as the page's.
//!
//! Mirrors devtools-frontend `handlers/MetaHandler.ts`.

use rustc_hash::{FxHashMap, FxHashSet};
use serde::Deserialize;

use crate::event::{Micro, TraceEvent};

#[derive(Debug, Clone, Default)]
pub struct Meta {
  /// The outermost primary frame. Empty when the trace recorded none,
  /// which happens for traces captured without a navigation.
  pub main_frame_id: String,
  pub main_frame_url: String,
  /// Navigations on the main frame, in trace order.
  pub main_frame_navigations: Vec<Navigation>,
  /// Earliest timestamp seen anywhere in the trace.
  pub trace_start: Micro,
  pub trace_end: Micro,
  /// Renderer process id per frame.
  pub frame_to_pid: FxHashMap<String, i64>,
  /// `pid` of the browser process, when the trace named it.
  pub browser_pid: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Navigation {
  pub id: String,
  pub frame: String,
  pub url: String,
  pub ts: Micro,
}

#[derive(Deserialize)]
struct FrameRecord {
  frame: String,
  #[serde(default)]
  url: String,
  #[serde(default)]
  parent: Option<String>,
  #[serde(default)]
  #[serde(rename = "isInPrimaryMainFrame")]
  is_in_primary_main_frame: Option<bool>,
  #[serde(default)]
  #[serde(rename = "isOutermostMainFrame")]
  is_outermost_main_frame: Option<bool>,
}

#[derive(Deserialize)]
struct NavigationData {
  #[serde(default, rename = "navigationId")]
  navigation_id: String,
  #[serde(default, rename = "documentLoaderURL")]
  document_loader_url: String,
  #[serde(default)]
  frame: String,
}

impl Meta {
  /// Walk the trace once and pull out the frame tree, the navigations
  /// and the trace bounds.
  #[must_use]
  pub fn from_events(events: &[TraceEvent]) -> Self {
    let mut meta = Self {
      trace_start: Micro::MAX,
      ..Default::default()
    };
    // A navigationId can repeat (crbug.com/1503982: two identical
    // navigationStart events for a `javascript:` URL); the first wins.
    let mut seen_navigations: FxHashSet<String> = FxHashSet::default();

    for event in events {
      // Metadata records carry no timestamp and would drag the trace
      // start back to zero.
      if event.ts > 0 {
        meta.trace_start = meta.trace_start.min(event.ts);
        meta.trace_end = meta.trace_end.max(event.end());
      }

      match event.name.as_str() {
        "TracingStartedInBrowser" => meta.absorb_frame_tree(event),
        "FrameCommittedInBrowser" => {
          if let Some(frame) = event.data_as::<FrameRecord>()
            && let Some(pid) = event
              .data()
              .and_then(|d| d.get("processId"))
              .and_then(serde_json::Value::as_i64)
          {
            meta.frame_to_pid.insert(frame.frame, pid);
          }
        },
        "navigationStart" => meta.absorb_navigation(event, &mut seen_navigations),
        "process_name" => {
          if matches!(
            event.args.get("name").and_then(serde_json::Value::as_str),
            Some("Browser" | "HeadlessBrowser")
          ) {
            meta.browser_pid = Some(event.pid);
          }
        },
        _ => {},
      }
    }

    if meta.trace_start == Micro::MAX {
      meta.trace_start = 0;
    }
    // `TracingStartedInBrowser` describes the frame tree as it stood when
    // recording began, which for a trace started before the navigation is
    // `about:blank`. Left at that, every request looks third-party and no
    // request matches the main document.
    if let Some(navigation) = meta.main_frame_navigations.last() {
      meta.main_frame_url.clone_from(&navigation.url);
    }
    meta
  }

  /// `TracingStartedInBrowser` carries the frame tree as it stood when
  /// recording began.
  ///
  /// Which field identifies the main frame depends on the Chrome that
  /// wrote the trace, so all three generations are handled: both flags
  /// present is exact, `isOutermostMainFrame` alone is a good guess, and
  /// the oldest traces are left with the historical `DevTools` heuristic
  /// of "has a URL and no parent".
  fn absorb_frame_tree(&mut self, event: &TraceEvent) {
    let Some(frames) = event
      .data()
      .and_then(|d| d.get("frames"))
      .and_then(serde_json::Value::as_array)
    else {
      return;
    };
    for raw in frames {
      let Ok(frame) = serde_json::from_value::<FrameRecord>(raw.clone()) else {
        continue;
      };
      let is_main = match (frame.is_in_primary_main_frame, frame.is_outermost_main_frame) {
        (Some(primary), Some(outermost)) => primary && outermost,
        (None, Some(outermost)) => outermost,
        _ => frame.parent.is_none() && !frame.url.is_empty(),
      };
      if is_main {
        self.main_frame_id = frame.frame;
        self.main_frame_url = frame.url;
      }
    }
  }

  fn absorb_navigation(&mut self, event: &TraceEvent, seen: &mut FxHashSet<String>) {
    let Some(data) = event.data_as::<NavigationData>() else {
      return;
    };
    // A navigationStart with no documentLoaderURL is noise for every
    // consumer downstream, exactly as `DevTools` filters it.
    if data.document_loader_url.is_empty() || !seen.insert(data.navigation_id.clone()) {
      return;
    }
    let frame = if data.frame.is_empty() {
      event
        .args
        .get("frame")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
    } else {
      data.frame
    };
    if frame == self.main_frame_id {
      self.main_frame_navigations.push(Navigation {
        id: data.navigation_id,
        frame,
        url: data.document_loader_url,
        ts: event.ts,
      });
    }
  }

  /// The timestamp insights measure from: the last main-frame
  /// navigation, or the trace start when the trace caught no navigation.
  #[must_use]
  pub fn time_origin(&self) -> Micro {
    self.main_frame_navigations.last().map_or(self.trace_start, |n| n.ts)
  }
}
