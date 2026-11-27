//! Single-eval fast paths for `Locator::fill`, `Locator::click`, `Locator::dblclick`,
//! and `Locator::set_checked` that avoid main-world `RemoteObjectId` materialization.

use serde::Deserialize;

use crate::actions;
use crate::error::Result;
use crate::selectors;

use super::{LazyCallLog, Locator, action_log_line, retries};

enum PointerPrepVerdict {
  AlreadyDone,
  NotFound,
  Retry(String),
  Ready {
    point: (f64, f64),
    interceptor_installed: bool,
  },
}

#[derive(Default, Deserialize)]
struct FillReply {
  strict: Option<u64>,
  error: Option<String>,
  result: Option<String>,
}

#[derive(Deserialize)]
struct PrepPoint {
  x: f64,
  y: f64,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PointerPrepReply {
  strict: Option<u64>,
  invalid: Option<String>,
  already_done: Option<bool>,
  error: Option<String>,
  actionable: Option<bool>,
  reason: Option<String>,
  point: Option<PrepPoint>,
  hit: Option<String>,
}

#[derive(Default, Deserialize)]
struct HitIntercept {
  #[serde(rename = "hitTargetDescription")]
  hit_target_description: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum HitReply {
  Intercept(HitIntercept),
  Other(serde::de::IgnoredAny),
}

#[derive(Default, Deserialize)]
struct FinalizeCheckReply {
  hit: Option<HitReply>,
  changed: Option<bool>,
}

/// Cache parsed selector parts across retry iterations when `rsel` is unchanged.
fn cached_parts_json<'a>(cache: &'a mut Option<(String, String)>, rsel: &str) -> Result<&'a str> {
  let needs_rebuild = cache.as_ref().is_none_or(|(prev, _)| prev != rsel);
  if needs_rebuild {
    let parts = selectors::build_parts_json(&selectors::parse(rsel)?);
    *cache = Some((rsel.to_owned(), parts));
  }
  Ok(cache.as_ref().map_or("[]", |(_, parts)| parts.as_str()))
}

impl Locator {
  /// Evaluate `expr` in `rframe` (main frame or child frame).
  pub(super) async fn eval_in_resolved_frame(
    rframe: &crate::frame::Frame,
    expr: &str,
  ) -> Result<Option<serde_json::Value>> {
    let page = rframe.page_arc();
    if rframe.is_main_frame() {
      page.inner().evaluate(expr).await
    } else {
      page.inner().evaluate_in_frame(expr, rframe.id()).await
    }
  }

  /// Single-eval `locator.fill()` directly inside `UTILITY_WORLD` on CDP:
  /// 1 `Runtime.evaluate(contextId = utility)` + 1 `Input.insertText` (2 RTTs),
  /// skipping main-world `RemoteObjectId`, `DOM.describeNode`, `DOM.resolveNode`,
  /// and `Runtime.releaseObject`. Returns `None` when tracing is active or the
  /// backend has no direct utility-world eval (caller falls back to `retry_resolve!`).
  pub(super) async fn fill_fast(&self, value: &str, opts: &crate::options::FillOptions) -> Option<Result<()>> {
    if self.is_tracing_active() {
      return None;
    }
    let page_arc = self.frame.page_arc();
    if !matches!(page_arc.inner(), crate::backend::AnyPage::Cdp(_)) {
      return None;
    }
    let res = Box::pin(self.fill_fast_inner(value, opts)).await;
    page_arc.slow_mo_pause("Locator", "fill").await;
    Some(res)
  }

  /// Sleep for one backoff step (clamped to `deadline`) using `pause_until_change`.
  async fn backoff_pause(&self, idx: usize, deadline: Option<std::time::Instant>) {
    let delay_ms = Self::RETRY_BACKOFFS_MS[idx.min(Self::RETRY_BACKOFFS_MS.len() - 1)];
    if delay_ms > 0 {
      let sleep_ms = deadline.map_or(delay_ms, |d| {
        let left =
          u64::try_from(d.saturating_duration_since(std::time::Instant::now()).as_millis()).unwrap_or(delay_ms);
        delay_ms.min(left)
      });
      if sleep_ms > 0 {
        self
          .frame
          .pause_until_change(std::time::Duration::from_millis(sleep_ms))
          .await;
      }
    }
  }

  /// Evaluate `resolveAndFill` once in `UTILITY_WORLD`, lazily injecting the
  /// engine into the utility world on first encounter.
  async fn eval_fill_once(
    &self,
    rframe: &crate::frame::Frame,
    parts: &str,
    value_lit: &str,
    force: bool,
  ) -> Option<Result<String>> {
    let frame_id = (!rframe.is_main_frame()).then(|| rframe.id());
    let frame_id_str = frame_id.map(std::ops::Deref::deref);
    let inner = rframe.page_arc().inner();
    let expr = format!(
      "(() => {{ var fd = window.__fd; if (!(fd instanceof Object)) return '{{\"result\":\"inject\"}}'; return fd.resolveAndFill({parts}, {}, {value_lit}, {force}); }})()",
      self.strict
    );
    let raw = match inner.evaluate_in_utility(frame_id_str, &expr).await? {
      Ok(v) => v.and_then(|val| val.as_str().map(str::to_owned)).unwrap_or_default(),
      Err(e) => return Some(Err(e)),
    };
    if !raw.contains("\"inject\"") {
      return Some(Ok(raw));
    }
    let inject_and_fill = format!(
      "(async () => {{ await {}; return window.__fd.resolveAndFill({parts}, {}, {value_lit}, {force}); }})()",
      crate::selectors::lazy_inject_js(),
      self.strict
    );
    match inner.evaluate_in_utility(frame_id_str, &inject_and_fill).await? {
      Ok(v) => Some(Ok(
        v.and_then(|val| val.as_str().map(str::to_owned)).unwrap_or_default(),
      )),
      Err(e) => Some(Err(e)),
    }
  }

  async fn fill_fast_inner(&self, value: &str, opts: &crate::options::FillOptions) -> Result<()> {
    let timeout = opts.timeout.unwrap_or_else(|| self.frame.page_arc().default_timeout());
    let deadline = (timeout != 0).then(|| std::time::Instant::now() + std::time::Duration::from_millis(timeout));
    let expired = || deadline.is_some_and(|d| std::time::Instant::now() >= d);
    let value_lit = serde_json::to_string(value)?;
    let force = opts.is_force();
    let mut call_log = LazyCallLog::new(&self.selector, "fill");
    let budget = crate::operation_budget::OperationBudget::operation(timeout)?;
    budget
      .scope(async {
        if let Some(sig) = crate::abort::current()
          && sig.was_refused()
        {
          return Err(sig.error());
        }
        let outcome = Box::pin(crate::abort::guard(async {
          let mut idx = 0usize;
          let mut cached_parts = None;
          loop {
            if expired() {
              return Err(crate::error::FerriError::timeout("fill", timeout));
            }
            crate::locator_handler::perform_checkpoint(self.frame.page_arc()).await;
            self.backoff_pause(idx, deadline).await;
            idx = idx.saturating_add(1);
            let (rframe, rsel) = match self.resolved().await {
              Ok(resolved) => resolved,
              Err(e @ crate::error::FerriError::InvalidSelector { .. }) => return Err(e),
              Err(_) => continue,
            };
            let parts = cached_parts_json(&mut cached_parts, &rsel)?;
            let raw = match self.eval_fill_once(&rframe, parts, &value_lit, force).await {
              Some(Ok(r)) => r,
              Some(Err(e)) if !expired() && !retries(&e) => return Err(e),
              _ => continue,
            };
            let answer: FillReply = serde_json::from_str(&raw).unwrap_or_default();
            if let Some(count) = answer.strict {
              return Err(crate::error::FerriError::strict(
                self.selector.clone(),
                usize::try_from(count).unwrap_or(2),
              ));
            }
            if let Some(error) = answer.error {
              return Err(crate::error::FerriError::backend(error));
            }
            let inner = rframe.page_arc().inner();
            match answer.result.as_deref() {
              Some("done") => return Ok(()),
              Some("needsinput") if value.is_empty() => return inner.press_key("Delete", 0).await,
              Some("needsinput") => return inner.insert_text(value).await,
              Some("error:notfound") | None => {},
              Some(state) => {
                call_log.record_retry(&action_log_line(state));
              },
            }
          }
        }))
        .await;
        match outcome {
          Err(e @ crate::error::FerriError::Aborted { .. }) => {
            call_log.push_aborted(&e);
            Err(e.with_call_log(call_log.into_vec()))
          },
          Err(e @ crate::error::FerriError::Timeout { .. }) => Err(e.with_call_log(call_log.into_vec())),
          other => other,
        }
      })
      .await
  }

  /// Fast single-eval pointer action (`click` / `dblclick` / `setChecked`)
  /// using `window.__fd.resolveAndClickPrep` or `resolveAndCheckPrep` so no
  /// `RemoteObjectId` is created.
  pub(super) async fn click_fast(&self, opts: &crate::options::ClickOptions, op_name: &'static str) -> Result<()> {
    let page_arc = self.frame.page_arc();
    let res = Box::pin(self.pointer_action_fast(opts, op_name, None)).await;
    page_arc.slow_mo_pause("Locator", op_name).await;
    res
  }

  pub(super) async fn set_checked_fast(&self, checked: bool, opts: &crate::options::CheckOptions) -> Result<()> {
    let page_arc = self.frame.page_arc();
    let click_opts = opts.clone().into_click_options();
    let res = Box::pin(self.pointer_action_fast(&click_opts, "check", Some(checked))).await;
    page_arc.slow_mo_pause("Locator", "check").await;
    res
  }

  async fn dispatch_prepared_pointer(
    rframe: &crate::frame::Frame,
    opts: &crate::options::ClickOptions,
    args: &crate::backend::BackendClickArgs,
    op_name: &'static str,
    target_checked: Option<bool>,
    point: (f64, f64),
    state: (bool, usize),
  ) -> Result<()> {
    let (interceptor_installed, tries) = state;
    let inner = rframe.page_arc().inner();
    let trial = opts.is_trial();
    let (x, y) = point;
    Box::pin(actions::with_pointer_context(
      tries,
      (!rframe.is_main_frame()).then(|| rframe.clone()),
      async {
        let snap = inner.nav_snapshot();
        let restore = actions::hold_modifiers(inner, &opts.modifiers).await?;
        let dispatch = if trial {
          Ok(())
        } else {
          let (dx, dy) = actions::cross_origin_frame_offset().await?;
          inner.click_at_with(x + dx, y + dy, args).await
        };
        let dispatch = dispatch.and(actions::restore_modifiers(inner, restore).await);
        if let Err(e) = dispatch {
          if interceptor_installed {
            let _ = Self::eval_in_resolved_frame(rframe, "JSON.stringify(window.__fd.finalizeHitInterceptor())").await;
          }
          if trial {
            return Err(e);
          }
          return Err(crate::error::FerriError::InputSent {
            action: op_name.into(),
            message: e.to_string(),
          });
        }
        if let Some(checked) = target_checked {
          if trial {
            return Ok(());
          }
          let fin_js = format!("window.__fd.finalizeCheck({checked}, {interceptor_installed})");
          let Ok(fin_val) = Self::eval_in_resolved_frame(rframe, &fin_js).await else {
            return Ok(());
          };
          let fin: FinalizeCheckReply = fin_val
            .as_ref()
            .and_then(serde_json::Value::as_str)
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
          match fin.hit {
            Some(HitReply::Intercept(HitIntercept {
              hit_target_description: Some(desc),
            })) => {
              return Err(crate::error::FerriError::Backend(format!(
                "{desc} intercepts pointer events"
              )));
            },
            Some(HitReply::Intercept(_) | HitReply::Other(_)) | None => {},
          }
          if fin.changed != Some(true) {
            return Err(crate::error::FerriError::backend(
              "clicking the checkbox did not change its state",
            ));
          }
          return Ok(());
        }
        if interceptor_installed
          && let Ok(fin_val) =
            Self::eval_in_resolved_frame(rframe, "JSON.stringify(window.__fd.finalizeHitInterceptor())").await
          && let Some(raw_fin) = fin_val.as_ref().and_then(serde_json::Value::as_str)
          && raw_fin != "\"done\""
          && let Ok(HitIntercept {
            hit_target_description: Some(desc),
          }) = serde_json::from_str::<HitIntercept>(raw_fin)
        {
          return Err(crate::error::FerriError::Backend(format!(
            "{desc} intercepts pointer events"
          )));
        }
        if !trial {
          inner.settle_navigation(snap, 2_000).await;
        }
        Ok(())
      },
    ))
    .await
  }

  async fn eval_pointer_prep(
    &self,
    rframe: &crate::frame::Frame,
    parts: &str,
    opts: &crate::options::ClickOptions,
    target_checked: Option<bool>,
    tries: usize,
  ) -> Result<PointerPrepVerdict> {
    rframe.page_arc().inner().ensure_engine_injected().await?;
    let pos_lit = match opts.position {
      Some(p) => format!("{{x:{},y:{}}}", p.x, p.y),
      None => "null".to_string(),
    };
    let stable = actions::stable_check().state();
    let force = opts.is_force();
    let trial = opts.is_trial();
    let hit_lit = if !force && !trial { "'mouse'" } else { "null" };
    let scroll_lit = if opts.scrolls() {
      actions::scroll_alignment(tries).unwrap_or("null")
    } else {
      "false"
    };
    let prep_js = match target_checked {
      Some(checked) => format!(
        "window.__fd.resolveAndCheckPrep({parts}, {}, {checked}, {pos_lit}, ['visible','enabled','{stable}'], {scroll_lit}, {hit_lit}, {force}, {trial})",
        self.strict
      ),
      None => format!(
        "window.__fd.resolveAndClickPrep({parts}, {}, {pos_lit}, ['visible','enabled','{stable}'], {scroll_lit}, {hit_lit}, {force}, {trial})",
        self.strict
      ),
    };
    let raw_val = Self::eval_in_resolved_frame(rframe, &prep_js).await?;
    let parsed: PointerPrepReply = raw_val
      .as_ref()
      .and_then(serde_json::Value::as_str)
      .and_then(|s| serde_json::from_str(s).ok())
      .unwrap_or_default();
    if let Some(count) = parsed.strict {
      return Err(crate::error::FerriError::strict(
        self.selector.clone(),
        usize::try_from(count).unwrap_or(2),
      ));
    }
    if let Some(msg) = parsed.invalid {
      return Err(crate::error::FerriError::invalid_argument("element", msg));
    }
    if parsed.already_done == Some(true) {
      return Ok(PointerPrepVerdict::AlreadyDone);
    }
    if let Some(err) = parsed.error {
      return Err(crate::error::FerriError::backend(err));
    }
    if parsed.actionable != Some(true) {
      let reason = parsed.reason.as_deref().unwrap_or("notfound");
      return Ok(if reason == "notfound" {
        PointerPrepVerdict::NotFound
      } else {
        PointerPrepVerdict::Retry(action_log_line(&format!("error:not{reason}")))
      });
    }
    let Some(point) = parsed.point.map(|p| (p.x, p.y)) else {
      return Ok(PointerPrepVerdict::NotFound);
    };
    let interceptor_installed = if force {
      false
    } else {
      match parsed.hit.as_deref() {
        Some("ok") => !trial,
        Some("error:notconnected") => return Ok(PointerPrepVerdict::Retry(action_log_line("error:notconnected"))),
        Some(desc) => return Ok(PointerPrepVerdict::Retry(format!("{desc} intercepts pointer events"))),
        None => false,
      }
    };
    Ok(PointerPrepVerdict::Ready {
      point,
      interceptor_installed,
    })
  }

  async fn pointer_action_fast(
    &self,
    opts: &crate::options::ClickOptions,
    op_name: &'static str,
    target_checked: Option<bool>,
  ) -> Result<()> {
    let timeout = opts.timeout.unwrap_or_else(|| self.frame.page_arc().default_timeout());
    let deadline = (timeout != 0).then(|| std::time::Instant::now() + std::time::Duration::from_millis(timeout));
    let expired = || deadline.is_some_and(|d| std::time::Instant::now() >= d);
    let args = crate::backend::BackendClickArgs::from_options(opts);
    let mut call_log = LazyCallLog::new(&self.selector, op_name);
    let budget = crate::operation_budget::OperationBudget::operation(timeout)?;
    budget
      .scope(async {
        if let Some(sig) = crate::abort::current()
          && sig.was_refused()
        {
          return Err(sig.error());
        }
        let outcome = Box::pin(crate::abort::guard(async {
          let mut idx = 0usize;
          let mut tries = 0usize;
          let mut cached_parts = None;
          loop {
            if expired() {
              return Err(crate::error::FerriError::timeout(op_name, timeout));
            }
            crate::locator_handler::perform_checkpoint(self.frame.page_arc()).await;
            self.backoff_pause(idx, deadline).await;
            idx = idx.saturating_add(1);
            let (rframe, rsel) = match self.resolved().await {
              Ok(resolved) => resolved,
              Err(e @ crate::error::FerriError::InvalidSelector { .. }) => return Err(e),
              Err(_) => continue,
            };
            let parts = cached_parts_json(&mut cached_parts, &rsel)?;
            let (point, interceptor_installed) = match self
              .eval_pointer_prep(&rframe, parts, opts, target_checked, tries)
              .await
            {
              Ok(PointerPrepVerdict::AlreadyDone) => return Ok(()),
              Ok(PointerPrepVerdict::Retry(line)) => {
                tries = tries.saturating_add(1);
                call_log.record_retry(&line);
                continue;
              },
              Ok(PointerPrepVerdict::Ready {
                point,
                interceptor_installed,
              }) => (point, interceptor_installed),
              Err(e) if !expired() && !retries(&e) => return Err(e),
              Ok(PointerPrepVerdict::NotFound) | Err(_) => continue,
            };
            let outcome = Self::dispatch_prepared_pointer(
              &rframe,
              opts,
              &args,
              op_name,
              target_checked,
              point,
              (interceptor_installed, tries),
            )
            .await;
            tries = tries.saturating_add(1);
            match outcome {
              Ok(()) => return Ok(()),
              Err(e) if retries(&e) => {
                call_log.record_retry(&action_log_line(&e.to_string()));
              },
              Err(e) if !expired() => return Err(e),
              Err(_) => {},
            }
          }
        }))
        .await;
        match outcome {
          Err(e @ crate::error::FerriError::Aborted { .. }) => {
            call_log.push_aborted(&e);
            Err(e.with_call_log(call_log.into_vec()))
          },
          Err(e @ crate::error::FerriError::Timeout { .. }) => Err(e.with_call_log(call_log.into_vec())),
          other => other,
        }
      })
      .await
  }
}
