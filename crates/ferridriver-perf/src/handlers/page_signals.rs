//! Assorted single-purpose signals Blink emits about the document.
//!
//! Each of these is one event kind that exactly one insight reads, so
//! they share a pass rather than each walking the trace again.
//!
//! Draws on devtools-frontend `handlers/UserInteractionsHandler.ts`
//! (viewport) and `handlers/LayoutShiftsHandler.ts` (fonts).

use crate::event::TraceEvent;
use crate::handlers::meta::Meta;
use crate::insights::character_set::MetaCharset;
use crate::insights::font_display::RemoteFont;
use crate::insights::slow_css_selector::SelectorTiming;
use crate::insights::viewport::ViewportState;

#[derive(Debug, Clone, Default)]
pub struct PageSignals {
  pub viewport: ViewportState,
  pub fonts: Vec<RemoteFont>,
  pub meta_charset: MetaCharset,
  pub selector_timings: Vec<SelectorTiming>,
}

impl PageSignals {
  #[must_use]
  pub fn from_events(events: &[TraceEvent<'_>], meta: &Meta) -> Self {
    let mut signals = Self::default();
    let mut viewport_ts = None;

    for event in events {
      match event.name.as_ref() {
        "ParseMetaViewport" => {
          if frame_of(event).is_none_or(|f| meta.main_frame_id.is_empty() || f == meta.main_frame_id) {
            signals.viewport.saw_meta_viewport = true;
            viewport_ts.get_or_insert(event.ts);
          }
        },
        "BeginCommitCompositorFrame" => {
          // Frames committed before the viewport tag was parsed cannot
          // reflect it, so they say nothing about whether the page is
          // mobile optimized.
          if viewport_ts.is_some_and(|ts| event.ts < ts) {
            continue;
          }
          let optimized = event
            .args_get("is_mobile_optimized")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
          // Every committed frame has to be optimized, so one that is
          // not decides the answer.
          signals.viewport.mobile_optimized = Some(signals.viewport.mobile_optimized.unwrap_or(true) && optimized);
        },
        // `url`, `display` and `id` sit directly on `args`, not under
        // the `args.data` every other payload uses. Reading `data` here
        // found no fonts at all, on any trace, silently.
        "BeginRemoteFontLoad" => {
          let string_of = |key: &str| {
            event
              .args_get(key)
              .and_then(serde_json::Value::as_str)
              .unwrap_or_default()
              .to_string()
          };
          let url = string_of("url");
          if !url.is_empty() {
            signals.fonts.push(RemoteFont {
              url,
              display: string_of("display"),
              // Upstream matches the request by `{pid}.{id}` rather
              // than by URL, which is what tells two fetches of the
              // same font apart.
              request_id: event
                .args_get("id")
                .and_then(serde_json::Value::as_i64)
                .map(|id| format!("{}.{id}", event.pid))
                .unwrap_or_default(),
            });
          }
        },
        "MetaCharsetCheck" => {
          if let Some(disposition) = event
            .data()
            .and_then(|d| d.get("disposition"))
            .and_then(serde_json::Value::as_str)
          {
            signals.meta_charset = MetaCharset::from_disposition(disposition);
          }
        },
        // Only present when selector profiling was enabled; the insight
        // reports "not measured" rather than "fast" when it is absent.
        "SelectorStats" => signals.selector_timings.extend(selector_timings(event)),
        _ => {},
      }
    }
    signals
  }
}

/// Blink writes selector statistics as an array of records whose keys
/// carry their units in the name, such as `elapsed (us)`.
fn selector_timings(event: &TraceEvent<'_>) -> Vec<SelectorTiming> {
  let Some(rows) = event
    .args_get("selector_stats")
    .and_then(|s| s.get("selector_timings"))
    .and_then(serde_json::Value::as_array)
  else {
    return Vec::new();
  };
  rows
    .iter()
    .map(|row| {
      let num = |k: &str| row.get(k).and_then(serde_json::Value::as_i64).unwrap_or(0);
      SelectorTiming {
        selector: row
          .get("selector")
          .and_then(serde_json::Value::as_str)
          .unwrap_or_default()
          .to_string(),
        elapsed_us: num("elapsed (us)"),
        match_attempts: num("match_attempts"),
        match_count: num("match_count"),
      }
    })
    .collect()
}

fn frame_of<'a>(event: &'a TraceEvent<'a>) -> Option<&'a str> {
  event
    .data()
    .and_then(|d| d.get("frame"))
    .or_else(|| event.args_get("frame"))
    .and_then(serde_json::Value::as_str)
}

/// Everything the trace saw that can move layout, for `CLSCulprits`.
///
/// The events are all Blink markers whose only consumer is that
/// insight, so they are collected in the same pass as the rest.
#[must_use]
pub fn layout_shift_culprits(events: &[TraceEvent<'_>]) -> Vec<crate::insights::cls_culprits::Culprit> {
  use crate::insights::cls_culprits::{Culprit, CulpritKind};

  let mut culprits = Vec::new();
  for event in events {
    let kind = match event.name.as_ref() {
      "LayoutImageUnsized" => CulpritKind::UnsizedImage,
      "BeginRemoteFontLoad" => CulpritKind::WebFont,
      "RenderFrameImpl::createChildFrame" => CulpritKind::InjectedIframe,
      _ => continue,
    };
    let detail = event
      .data()
      .and_then(|d| {
        d.get("url")
          .or_else(|| d.get("nodeName"))
          .and_then(serde_json::Value::as_str)
      })
      .unwrap_or("(unnamed)")
      .to_string();
    culprits.push(Culprit {
      kind,
      end_ts: event.end(),
      detail,
    });
  }
  culprits
}
