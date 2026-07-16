//! `Clock` — fake-time control (`context.clock` / `page.clock`).
//!
//! Playwright: `Clock` lives on the `BrowserContext` (`page.clock` is the
//! same object, `client/page.ts:137`) with seven methods —
//! `install`, `fastForward`, `pauseAt`, `resume`, `runFor`,
//! `setFixedTime`, `setSystemTime` (`types.d.ts:20004`). Every method
//! auto-installs the engine on first use
//! (`server/clock.ts::_installIfNeeded`).
//!
//! The in-page engine is Playwright's `injected/src/clock.ts`, vendored
//! verbatim and bundled to `injected/dist/clock.min.js`. Install flow
//! mirrors `server/clock.ts`: (1) a context init script delivers the
//! engine + `__pwClock` bootstrap to every future document, and the
//! engine is evaluated into every live frame immediately; (2) each API
//! call appends a permanent `controller.log(type, wallTime, param)`
//! init script so a freshly navigated document replays the clock's
//! history (`clock.ts::_replayLogOnce`); (3) the controller method is
//! evaluated into every live frame for immediate effect.

use crate::context::ContextRef;
use crate::error::{FerriError, Result};

/// The compiled fake-clock engine (Playwright `injected/src/clock.ts`).
const CLOCK_JS: &str = include_str!("injected/dist/clock.min.js");

/// A time instant accepted by `install` / `pauseAt` / `setFixedTime` /
/// `setSystemTime`. Playwright: `number | string | Date` — the `Date`
/// form lowers to epoch milliseconds at the binding layer.
#[derive(Debug, Clone)]
pub enum ClockTime {
  /// Epoch milliseconds.
  Millis(f64),
  /// A date string (`"2024-02-02"`, `"2024-02-02T10:00:00Z"`, ...).
  Text(String),
}

/// A duration accepted by `fastForward` / `runFor`. Playwright:
/// `number | string` where the string is `"ss"`, `"mm:ss"`, or
/// `"hh:mm:ss"`.
#[derive(Debug, Clone)]
pub enum ClockTicks {
  /// Milliseconds.
  Millis(f64),
  /// Humanized `"hh:mm:ss"` form.
  Text(String),
}

/// `context.clock` handle. Cheap to construct (wraps a [`ContextRef`]).
pub struct Clock {
  ctx: ContextRef,
}

impl Clock {
  #[must_use]
  pub(crate) fn new(ctx: ContextRef) -> Self {
    Self { ctx }
  }

  /// Playwright: `clock.install(options?: { time? })`. Installs the
  /// fake clock (all timers + `Date` + `performance` + `Intl` +
  /// `AbortSignal.timeout` are faked, auto-advancing from `time`,
  /// default: current system time).
  ///
  /// # Errors
  ///
  /// Errors on an invalid `time` or if delivery to the page fails.
  pub async fn install(&self, time: Option<ClockTime>) -> Result<()> {
    let time_ms = match time {
      Some(t) => parse_time(&t)?,
      None => now_epoch_ms(),
    };
    self.dispatch("install", Some(time_ms)).await
  }

  /// Playwright: `clock.fastForward(ticks)`. Jumps forward, firing each
  /// due timer at most once ("laptop lid closed" semantics).
  ///
  /// # Errors
  ///
  /// Errors on an invalid `ticks` grammar or a page-side engine error.
  pub async fn fast_forward(&self, ticks: ClockTicks) -> Result<()> {
    let ticks_ms = parse_ticks(&ticks)?;
    self.dispatch("fastForward", Some(ticks_ms)).await
  }

  /// Playwright: `clock.pauseAt(time)`. Advances to `time` firing due
  /// timers, then stops the clock.
  ///
  /// # Errors
  ///
  /// Errors on an invalid `time` or when `time` is in the past.
  pub async fn pause_at(&self, time: ClockTime) -> Result<()> {
    let time_ms = parse_time(&time)?;
    self.dispatch("pauseAt", Some(time_ms)).await
  }

  /// Playwright: `clock.resume()`. Resumes auto-advancing time.
  ///
  /// # Errors
  ///
  /// Errors if delivery to the page fails.
  pub async fn resume(&self) -> Result<()> {
    self.dispatch("resume", None).await
  }

  /// Playwright: `clock.runFor(ticks)`. Runs the clock forward firing
  /// every due timer (intervals repeatedly).
  ///
  /// # Errors
  ///
  /// Errors on an invalid `ticks` grammar, negative ticks, or a timer
  /// callback exception (rethrown by the engine).
  pub async fn run_for(&self, ticks: ClockTicks) -> Result<()> {
    let ticks_ms = parse_ticks(&ticks)?;
    self.dispatch("runFor", Some(ticks_ms)).await
  }

  /// Playwright: `clock.setFixedTime(time)`. `Date.now()` returns
  /// `time` while timers keep running.
  ///
  /// # Errors
  ///
  /// Errors on an invalid `time`.
  pub async fn set_fixed_time(&self, time: ClockTime) -> Result<()> {
    let time_ms = parse_time(&time)?;
    self.dispatch("setFixedTime", Some(time_ms)).await
  }

  /// Playwright: `clock.setSystemTime(time)`. Sets the wall clock
  /// without firing timers.
  ///
  /// # Errors
  ///
  /// Errors on an invalid `time`.
  pub async fn set_system_time(&self, time: ClockTime) -> Result<()> {
    let time_ms = parse_time(&time)?;
    self.dispatch("setSystemTime", Some(time_ms)).await
  }

  /// The `server/clock.ts` three-step delivery: install-if-needed, log
  /// init script for future documents, immediate evaluate in live
  /// frames.
  async fn dispatch(&self, method: &str, param_ms: Option<f64>) -> Result<()> {
    self.install_engine_if_needed().await?;

    let wall_ms = now_epoch_ms();
    let param = param_ms.map_or(String::new(), |v| format!(", {v}"));
    let log_script =
      format!("globalThis.__pwClock && globalThis.__pwClock.controller.log('{method}', {wall_ms}{param});");
    self.ctx.add_init_script_source(log_script).await?;

    let call = format!(
      "globalThis.__pwClock.controller.{method}({})",
      param_ms.map_or(String::new(), |v| v.to_string())
    );
    self.evaluate_in_live_frames(&call).await
  }

  /// Deliver the engine bundle: once per context, register the
  /// context init script AND evaluate into every live frame.
  async fn install_engine_if_needed(&self) -> Result<()> {
    let composite = self.ctx.composite();
    {
      let state = self.ctx.state().read().await;
      let mut installed = state
        .clock_installed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      if !installed.insert(composite) {
        return Ok(());
      }
    }
    let browser_name = {
      let state = self.ctx.state().read().await;
      match state.backend_kind() {
        crate::backend::BackendKind::CdpPipe | crate::backend::BackendKind::CdpRaw => "chromium",
        crate::backend::BackendKind::WebKit => "webkit",
        crate::backend::BackendKind::Bidi => "firefox",
      }
    };
    let engine = format!("{CLOCK_JS}\nglobalThis.__ferriClockInstall({browser_name:?});");
    self.ctx.add_init_script_source(engine.clone()).await?;
    self.evaluate_in_live_frames(&engine).await
  }

  /// Evaluate a bare expression in the main world of every frame of
  /// every open page (mirrors
  /// `browserContext.safeNonStallingEvaluateInAllFrames`). Frames whose
  /// execution context is mid-navigation are tolerated; page-side
  /// engine errors ("Cannot fast-forward to the past", timer callback
  /// exceptions, ...) propagate.
  async fn evaluate_in_live_frames(&self, expression: &str) -> Result<()> {
    let pages = {
      let state = self.ctx.state().read().await;
      state
        .context(self.ctx.name())
        .map(|c| c.pages.clone())
        .unwrap_or_default()
    };
    for page in pages {
      // Main frame first (frame-id-less evaluate), then every subframe
      // from the live tree.
      let mut frame_ids: Vec<Option<String>> = vec![None];
      if let Ok(tree) = page.get_frame_tree().await {
        frame_ids.extend(
          tree
            .into_iter()
            .filter(|f| f.parent_frame_id.is_some())
            .map(|f| Some(f.frame_id)),
        );
      }
      for frame_id in frame_ids {
        let result = match frame_id.as_deref() {
          None => page.evaluate(expression).await.map(|_| ()),
          Some(fid) => page.evaluate_in_frame(expression, fid).await.map(|_| ()),
        };
        if let Err(err) = result {
          if is_navigation_churn(&err) {
            continue;
          }
          return Err(err);
        }
      }
    }
    Ok(())
  }
}

/// Whether an evaluate failure is navigation churn (frame/context torn
/// down mid-call) rather than a page-side engine error. Mirrors the
/// tolerance in `safeNonStallingEvaluateInAllFrames`, which swallows
/// everything except real in-page JS errors.
fn is_navigation_churn(err: &FerriError) -> bool {
  let msg = err.to_string().to_ascii_lowercase();
  msg.contains("context") || msg.contains("destroyed") || msg.contains("detached") || msg.contains("navigat")
}

fn now_epoch_ms() -> f64 {
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default();
  // f64 keeps millisecond precision for dates well past year 275760.
  now.as_secs_f64() * 1000.0
}

/// `server/clock.ts::parseTicks` — number passes through (ms); a string
/// must match `^(\d\d:){0,2}\d\d?$`, each `:`-separated segment < 60,
/// value = seconds-style positional sum × 1000.
///
/// # Errors
///
/// Playwright's exact messages: "Clock only understands numbers, 'mm:ss'
/// and 'hh:mm:ss'" and "Invalid time <str>".
pub fn parse_ticks(ticks: &ClockTicks) -> Result<f64> {
  match ticks {
    ClockTicks::Millis(ms) => Ok(*ms),
    ClockTicks::Text(str_value) => {
      let grammar_error = || {
        FerriError::invalid_argument(
          "ticks",
          "Clock only understands numbers, 'mm:ss' and 'hh:mm:ss'".to_string(),
        )
      };
      let segments: Vec<&str> = str_value.split(':').collect();
      if segments.len() > 3 {
        return Err(grammar_error());
      }
      for (i, segment) in segments.iter().enumerate() {
        let is_last = i == segments.len() - 1;
        let len_ok = if is_last {
          segment.len() == 1 || segment.len() == 2
        } else {
          segment.len() == 2
        };
        if !len_ok || !segment.bytes().all(|b| b.is_ascii_digit()) {
          return Err(grammar_error());
        }
      }
      let mut total_seconds: f64 = 0.0;
      for segment in &segments {
        let value: u32 = segment
          .parse()
          .map_err(|_| FerriError::invalid_argument("ticks", format!("Invalid time {str_value}")))?;
        if value >= 60 {
          return Err(FerriError::invalid_argument(
            "ticks",
            format!("Invalid time {str_value}"),
          ));
        }
        total_seconds = total_seconds * 60.0 + f64::from(value);
      }
      Ok(total_seconds * 1000.0)
    },
  }
}

/// `server/clock.ts::parseTime` — number passes through (epoch ms); a
/// string parses as a date. Playwright feeds the string to JS
/// `new Date(...)`, so beyond ISO-8601 (`YYYY-MM-DD`,
/// `YYYY-MM-DD[T ]HH:MM[:SS[.mmm]][Z|±HH:MM]`) the common V8 shapes are
/// accepted: month-name forms ("Feb 1 2024", "1 February 2024",
/// "Mon Feb 01 2024 10:30:15", "Thu, 01 Feb 2024 10:30:00 GMT"),
/// slash dates ("2024/02/01", "02/01/2024" US order), each with an
/// optional `HH:MM[:SS[.mmm]]` time and `GMT`/`UTC`/`Z`/`±HH[:]MM`
/// designator.
///
/// # Errors
///
/// "Invalid date: <str>" on anything unparsable, like Playwright.
pub fn parse_time(time: &ClockTime) -> Result<f64> {
  match time {
    ClockTime::Millis(ms) => Ok(*ms),
    ClockTime::Text(text) => parse_iso_date_ms(text)
      .or_else(|| parse_human_date_ms(text))
      .ok_or_else(|| FerriError::invalid_argument("time", format!("Invalid date: {text}"))),
  }
}

/// Minimal ISO-8601 parser: date, optional time, optional fraction,
/// optional `Z`/`±HH:MM` offset. Bare dates and date-times are UTC
/// (matches JS `new Date("2024-02-02")` semantics for the date-only
/// form; JS treats bare date-TIMEs as local, but a wall-clock fake
/// driven from tests is expected to be TZ-stable, so UTC is used for
/// both — pass an explicit offset for anything else).
fn parse_iso_date_ms(text: &str) -> Option<f64> {
  let s = text.trim();
  let bytes = s.as_bytes();
  if bytes.len() < 10 || bytes[4] != b'-' || bytes[7] != b'-' {
    return None;
  }
  let year: i64 = s.get(0..4)?.parse().ok()?;
  let month: u32 = s.get(5..7)?.parse().ok()?;
  let day: u32 = s.get(8..10)?.parse().ok()?;
  if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
    return None;
  }
  let mut rest = &s[10..];
  let (mut hour, mut minute, mut second, mut millis): (u32, u32, u32, f64) = (0, 0, 0, 0.0);
  let mut offset_minutes: i64 = 0;
  if !rest.is_empty() {
    let sep = rest.chars().next()?;
    if sep != 'T' && sep != 't' && sep != ' ' {
      return None;
    }
    rest = &rest[1..];
    // Split the trailing timezone designator first.
    let (time_part, tz_part) = match rest.find(['Z', 'z', '+']) {
      Some(idx) => rest.split_at(idx),
      None => match rest.rfind('-') {
        // A '-' inside the time section can only be an offset sign.
        Some(idx) if idx >= 5 => rest.split_at(idx),
        _ => (rest, ""),
      },
    };
    (hour, minute, second, millis) = parse_hms(time_part)?;
    match tz_part.chars().next() {
      None => {},
      Some('Z' | 'z') if tz_part.len() == 1 => {},
      Some('+' | '-') => offset_minutes = parse_tz_offset(tz_part)?,
      _ => return None,
    }
  }
  Some(civil_to_utc_ms(
    year,
    month,
    day,
    (hour, minute, second, millis),
    offset_minutes,
  ))
}

/// `HH:MM[:SS[.fraction]]` → (hour, minute, second, millis).
fn parse_hms(time_part: &str) -> Option<(u32, u32, u32, f64)> {
  let (hms, frac) = match time_part.find('.') {
    Some(idx) => time_part.split_at(idx),
    None => (time_part, ""),
  };
  let hms_parts: Vec<&str> = hms.split(':').collect();
  if hms_parts.len() < 2 || hms_parts.len() > 3 {
    return None;
  }
  let hour: u32 = hms_parts[0].parse().ok()?;
  let minute: u32 = hms_parts[1].parse().ok()?;
  let second: u32 = if hms_parts.len() == 3 {
    hms_parts[2].parse().ok()?
  } else {
    0
  };
  if hour > 23 || minute > 59 || second > 60 {
    return None;
  }
  let mut millis = 0.0;
  if !frac.is_empty() {
    let digits = &frac[1..];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
      return None;
    }
    let f: f64 = format!("0.{digits}").parse().ok()?;
    millis = f * 1000.0;
  }
  Some((hour, minute, second, millis))
}

/// `±HH:MM` / `±HHMM` / `±HH` → signed minutes east of UTC.
fn parse_tz_offset(tz_part: &str) -> Option<i64> {
  let sign = tz_part.chars().next()?;
  if sign != '+' && sign != '-' {
    return None;
  }
  let tz = &tz_part[1..];
  let (oh, om) = match tz.find(':') {
    Some(idx) => (tz[..idx].parse::<i64>().ok()?, tz[idx + 1..].parse::<i64>().ok()?),
    None if tz.len() == 4 => (tz[..2].parse::<i64>().ok()?, tz[2..].parse::<i64>().ok()?),
    None if tz.len() == 2 => (tz.parse::<i64>().ok()?, 0),
    _ => return None,
  };
  let mut offset_minutes = oh * 60 + om;
  if sign == '-' {
    offset_minutes = -offset_minutes;
  }
  Some(offset_minutes)
}

fn civil_to_utc_ms(year: i64, month: u32, day: u32, hms: (u32, u32, u32, f64), offset_minutes: i64) -> f64 {
  let (hour, minute, second, millis) = hms;
  let days = days_from_civil(year, month, day);
  let secs = days * 86_400 + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second);
  // Civil-date seconds stay far below 2^53 — exact in f64.
  #[allow(clippy::cast_precision_loss)]
  let utc_ms = secs as f64 * 1000.0 + millis - (offset_minutes as f64) * 60_000.0;
  utc_ms
}

/// The common non-ISO shapes V8's `new Date(string)` accepts — month
/// names ("Feb 1 2024", "1 February 2024"), an optional weekday prefix
/// ("Mon Feb 01 2024 ...", "Thu, 01 Feb 2024 10:30:00 GMT"), and slash
/// dates ("2024/02/01", "02/01/2024" in US month/day/year order) —
/// each with an optional `HH:MM[:SS[.mmm]]` time and a
/// `GMT`/`UTC`(`±HH[:]MM`) / `Z` / bare `±HH[:]MM` designator.
/// Designator-less strings are UTC (same TZ-stability policy as the
/// ISO parser above).
fn parse_human_date_ms(text: &str) -> Option<f64> {
  let cleaned = text.replace(',', " ");
  let mut tokens: Vec<&str> = cleaned.split_whitespace().collect();
  if tokens.first().is_some_and(|t| weekday_from_name(t)) {
    tokens.remove(0);
  }
  if tokens.is_empty() {
    return None;
  }

  let (year, month, day): (i64, u32, u32);
  let rest: &[&str];
  if tokens[0].contains('/') {
    let parts: Vec<&str> = tokens[0].split('/').collect();
    if parts.len() != 3 {
      return None;
    }
    if parts[0].len() == 4 {
      year = parts[0].parse().ok()?;
      month = parts[1].parse().ok()?;
      day = parts[2].parse().ok()?;
    } else {
      // JS reads all-numeric slash dates as US month/day/year.
      month = parts[0].parse().ok()?;
      day = parts[1].parse().ok()?;
      year = parts[2].parse().ok()?;
    }
    rest = &tokens[1..];
  } else if let Some(m) = month_from_name(tokens[0]) {
    month = m;
    day = tokens.get(1)?.parse().ok()?;
    year = tokens.get(2)?.parse().ok()?;
    rest = tokens.get(3..).unwrap_or(&[]);
  } else if let Some(m) = tokens.get(1).and_then(|t| month_from_name(t)) {
    day = tokens[0].parse().ok()?;
    month = m;
    year = tokens.get(2)?.parse().ok()?;
    rest = tokens.get(3..).unwrap_or(&[]);
  } else {
    return None;
  }
  if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
    return None;
  }

  let (mut hour, mut minute, mut second, mut millis): (u32, u32, u32, f64) = (0, 0, 0, 0.0);
  let mut offset_minutes: i64 = 0;
  let mut saw_time = false;
  for tok in rest {
    if !saw_time && tok.contains(':') && tok.as_bytes().first().is_some_and(u8::is_ascii_digit) {
      (hour, minute, second, millis) = parse_hms(tok)?;
      saw_time = true;
      continue;
    }
    let upper = tok.to_ascii_uppercase();
    if upper == "Z" {
      continue;
    }
    let after_name = upper.strip_prefix("GMT").or_else(|| upper.strip_prefix("UTC"));
    match after_name {
      Some("") => {},
      Some(off) => offset_minutes = parse_tz_offset(off)?,
      None if upper.starts_with('+') || upper.starts_with('-') => offset_minutes = parse_tz_offset(&upper)?,
      None => return None,
    }
  }
  Some(civil_to_utc_ms(
    year,
    month,
    day,
    (hour, minute, second, millis),
    offset_minutes,
  ))
}

fn month_from_name(token: &str) -> Option<u32> {
  const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
  ];
  let t = token.to_ascii_lowercase();
  if t.len() < 3 {
    return None;
  }
  let idx = MONTHS.iter().position(|m| m.starts_with(&t))?;
  Some(u32::try_from(idx).ok()? + 1)
}

fn weekday_from_name(token: &str) -> bool {
  const DAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
  ];
  let t = token.to_ascii_lowercase();
  t.len() >= 3 && DAYS.iter().any(|d| d.starts_with(&t))
}

/// Howard Hinnant's `days_from_civil`: Gregorian date → days since the
/// Unix epoch (inverse of the `civil_from_days` in `tracing.rs`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
  let y = if m <= 2 { y - 1 } else { y };
  let era = y.div_euclid(400);
  let yoe = y.rem_euclid(400);
  let mp = i64::from((m + 9) % 12);
  let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
  let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
  era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn ticks_number_passes_through() {
    assert!((parse_ticks(&ClockTicks::Millis(1500.0)).unwrap() - 1500.0).abs() < f64::EPSILON);
  }

  #[test]
  fn ticks_humanized_grammar() {
    assert!((parse_ticks(&ClockTicks::Text("08".into())).unwrap() - 8000.0).abs() < f64::EPSILON);
    assert!((parse_ticks(&ClockTicks::Text("01:00".into())).unwrap() - 60_000.0).abs() < f64::EPSILON);
    assert!(
      (parse_ticks(&ClockTicks::Text("02:34:10".into())).unwrap() - (2.0 * 3600.0 + 34.0 * 60.0 + 10.0) * 1000.0).abs()
        < f64::EPSILON
    );
  }

  #[test]
  fn ticks_rejects_bad_grammar_and_range() {
    assert!(parse_ticks(&ClockTicks::Text("1:00".into())).is_err());
    assert!(parse_ticks(&ClockTicks::Text("61".into())).is_err());
    assert!(parse_ticks(&ClockTicks::Text("00:00:00:00".into())).is_err());
    assert!(parse_ticks(&ClockTicks::Text("abc".into())).is_err());
  }

  #[test]
  fn time_parses_iso_shapes() {
    let day = parse_time(&ClockTime::Text("2024-02-02".into())).unwrap();
    assert!((day - 1_706_832_000_000.0).abs() < f64::EPSILON);
    let with_time = parse_time(&ClockTime::Text("2024-02-02T10:00:00Z".into())).unwrap();
    assert!((with_time - (1_706_832_000_000.0 + 10.0 * 3_600_000.0)).abs() < f64::EPSILON);
    let with_offset = parse_time(&ClockTime::Text("2024-02-02T10:00:00+02:00".into())).unwrap();
    assert!((with_offset - (1_706_832_000_000.0 + 8.0 * 3_600_000.0)).abs() < f64::EPSILON);
  }

  #[test]
  fn time_rejects_garbage() {
    assert!(parse_time(&ClockTime::Text("not a date".into())).is_err());
    assert!(parse_time(&ClockTime::Text("2024-13-01".into())).is_err());
    assert!(parse_time(&ClockTime::Text("Foo 1 2024".into())).is_err());
    assert!(parse_time(&ClockTime::Text("Feb 1".into())).is_err());
    assert!(parse_time(&ClockTime::Text("Feb 32 2024".into())).is_err());
  }

  const FEB1_2024: f64 = 1_706_745_600_000.0;

  #[test]
  fn time_parses_month_name_shapes() {
    for text in [
      "Feb 1 2024",
      "February 1, 2024",
      "1 Feb 2024",
      "1 February 2024",
      "Thu Feb 01 2024",
      "Thu, 01 Feb 2024",
    ] {
      let ms = parse_time(&ClockTime::Text(text.into())).unwrap();
      assert!((ms - FEB1_2024).abs() < f64::EPSILON, "{text} -> {ms}");
    }
  }

  #[test]
  fn time_parses_month_name_with_time_and_zone() {
    let plain = parse_time(&ClockTime::Text("Feb 1 2024 10:30".into())).unwrap();
    assert!((plain - (FEB1_2024 + 10.5 * 3_600_000.0)).abs() < f64::EPSILON);
    let seconds = parse_time(&ClockTime::Text("Mon Feb 01 2024 10:30:15".into())).unwrap();
    assert!((seconds - (FEB1_2024 + 10.5 * 3_600_000.0 + 15_000.0)).abs() < f64::EPSILON);
    let gmt = parse_time(&ClockTime::Text("Thu, 01 Feb 2024 10:30:00 GMT".into())).unwrap();
    assert!((gmt - (FEB1_2024 + 10.5 * 3_600_000.0)).abs() < f64::EPSILON);
    let offset = parse_time(&ClockTime::Text("Feb 1 2024 10:30 GMT+01:00".into())).unwrap();
    assert!((offset - (FEB1_2024 + 9.5 * 3_600_000.0)).abs() < f64::EPSILON);
    let bare_offset = parse_time(&ClockTime::Text("Feb 1 2024 10:30 -0200".into())).unwrap();
    assert!((bare_offset - (FEB1_2024 + 12.5 * 3_600_000.0)).abs() < f64::EPSILON);
  }

  #[test]
  fn time_parses_slash_shapes() {
    let ymd = parse_time(&ClockTime::Text("2024/02/01".into())).unwrap();
    assert!((ymd - FEB1_2024).abs() < f64::EPSILON);
    let us = parse_time(&ClockTime::Text("02/01/2024".into())).unwrap();
    assert!((us - FEB1_2024).abs() < f64::EPSILON);
    let with_time = parse_time(&ClockTime::Text("02/01/2024 08:00".into())).unwrap();
    assert!((with_time - (FEB1_2024 + 8.0 * 3_600_000.0)).abs() < f64::EPSILON);
  }
}
