//! Reusable NAPI event-listener registry shared across `Page`,
//! `BrowserContext`, `Browser`, `WebSocket`, and `CDPSession`.
//!
//! Tracks each registered JS listener's `FunctionRef` alongside its core
//! listener id so Playwright/Node `off(event, listener)` matches by JS
//! strict equality (`===`) without every binding class reimplementing the
//! same lock + `strict_equals` loops.

use std::sync::{Arc, Mutex};

use napi::Result;
use napi::bindgen_prelude::{Function, FunctionRef, ToNapiValue};

struct ListenerEntry<Arg: ToNapiValue + 'static, Id> {
  event: String,
  id: Id,
  fn_ref: FunctionRef<Arg, ()>,
}

/// Thread-safe registry of JS event listeners keyed by event name and core id.
pub(crate) struct ListenerRegistry<Arg: ToNapiValue + 'static, Id = u64> {
  entries: Arc<Mutex<Vec<ListenerEntry<Arg, Id>>>>,
}

impl<Arg: ToNapiValue + 'static, Id> Default for ListenerRegistry<Arg, Id> {
  fn default() -> Self {
    Self::new()
  }
}

impl<Arg: ToNapiValue + 'static, Id> ListenerRegistry<Arg, Id> {
  pub(crate) fn new() -> Self {
    Self {
      entries: Arc::new(Mutex::new(Vec::new())),
    }
  }

  pub(crate) fn record(&self, event: impl Into<String>, id: Id, fn_ref: FunctionRef<Arg, ()>) {
    self
      .entries
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(ListenerEntry {
        event: event.into(),
        id,
        fn_ref,
      });
  }

  /// Remove entries matching `event` and JS function identity (`===`),
  /// returning their core listener ids so the caller can unsubscribe them
  /// from the core emitter.
  pub(crate) fn take_matching(&self, env: napi::Env, event: &str, listener: &Function<'_, Arg, ()>) -> Result<Vec<Id>> {
    let incoming = listener.create_ref()?;
    let mut regs = self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut removed = Vec::new();
    let mut i = 0;
    while i < regs.len() {
      let hit = regs[i].event == event && {
        let a = incoming.borrow_back(&env)?;
        let b = regs[i].fn_ref.borrow_back(&env)?;
        env.strict_equals(a, b)?
      };
      if hit {
        removed.push(regs.remove(i).id);
      } else {
        i += 1;
      }
    }
    Ok(removed)
  }

  /// Drop all tracked entries for `event`.
  pub(crate) fn forget_event(&self, event: &str) {
    self
      .entries
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .retain(|reg| reg.event != event);
  }

  /// Drop all tracked entries across every event.
  pub(crate) fn clear(&self) {
    self
      .entries
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clear();
  }
}
