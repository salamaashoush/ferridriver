//! `performance`: High Resolution Time, User Timing and the Performance
//! Timeline.
//!
//! `now()` reads a monotonic [`Instant`], never the wall clock. That is
//! the whole point of the API — a `Date.now()` delta can go backwards
//! when NTP steps the clock mid-measurement, and a timing number that
//! silently goes backwards is worse than no timing number. The wall
//! clock appears exactly once, as `timeOrigin`, which is what the
//! monotonic readings are relative to.
//!
//! [`monotonic_base`] is also what `process.hrtime` counts from, so the
//! two clocks are correlatable the way Node's are (both derive from one
//! libuv hrtime there).
//!
//! Covered: `now`, `timeOrigin`, `toJSON`, `mark`, `measure`,
//! `clearMarks`, `clearMeasures`, `getEntries`, `getEntriesByName`,
//! `getEntriesByType`, and the `PerformanceEntry` / `PerformanceMark` /
//! `PerformanceMeasure` classes with `PerformanceEntry` as their
//! prototype, so `mark instanceof PerformanceEntry` holds.
//!
//! Not covered: `PerformanceObserver` (it needs a task-queue hook this
//! runtime has no equivalent of), the resource/navigation timing entry
//! types (no document), and Node's `eventLoopUtilization` / `nodeTiming`.
//! A buffer size limit is not implemented either; nothing here evicts,
//! so a program marking in a hot loop grows the buffer until it calls
//! `clearMarks`, exactly as the spec's `maxBufferSize` exists to bound.

use std::time::Instant;

use rquickjs::class::Trace;
use rquickjs::function::Opt;
use rquickjs::{Class, Ctx, Exception, Object, Value};

/// Monotonic base for `performance.now()`, and the wall-clock instant it
/// corresponds to (`performance.timeOrigin`). Both are fixed at first
/// use, which is process start for any real session.
static PROCESS_START: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);
static TIME_ORIGIN: std::sync::LazyLock<f64> = std::sync::LazyLock::new(|| {
  // Touch the monotonic base first so the two are taken together.
  let _ = *PROCESS_START;
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
});

/// The process-wide monotonic origin. `process.hrtime` counts from this
/// too, so a script can line its readings up against `performance.now()`.
#[must_use]
pub fn monotonic_base() -> Instant {
  *PROCESS_START
}

/// `performance.now()` in fractional milliseconds.
#[must_use]
pub fn now_ms() -> f64 {
  PROCESS_START.elapsed().as_secs_f64() * 1000.0
}

/// `performance.timeOrigin`: Unix-epoch milliseconds at the monotonic
/// base.
#[must_use]
pub fn time_origin_ms() -> f64 {
  *TIME_ORIGIN
}

const MARK: &str = "mark";
const MEASURE: &str = "measure";

/// Read `PerformanceMarkOptions` into `(startTime, detail)`.
///
/// Shared by `performance.mark()` and `new PerformanceMark()` so the two
/// cannot disagree about a default or about which values are refused.
fn mark_options<'js>(ctx: &Ctx<'js>, options: Option<&Value<'js>>) -> rquickjs::Result<(f64, Value<'js>)> {
  let mut start_time = now_ms();
  let mut detail = Value::new_null(ctx.clone());
  let Some(options) = options.and_then(rquickjs::Value::as_object) else {
    return Ok((start_time, detail));
  };
  if let Some(given) = options
    .get::<_, Value<'js>>("startTime")
    .ok()
    .filter(|v| !v.is_undefined())
  {
    let Some(n) = given.as_number() else {
      return Err(Exception::throw_type(ctx, "startTime must be a number"));
    };
    if n < 0.0 {
      return Err(Exception::throw_type(ctx, "startTime cannot be negative"));
    }
    start_time = n;
  }
  if let Some(given) = options.get::<_, Value<'js>>("detail").ok().filter(|v| !v.is_undefined()) {
    detail = given;
  }
  Ok((start_time, detail))
}

/// `PerformanceEntry` — the base every timeline entry reads as.
///
/// Marks and measures are their own classes so `entryType` cannot
/// disagree with the constructor, and both chain their prototype here.
#[derive(Trace, Clone)]
#[rquickjs::class(rename = "PerformanceEntry")]
pub struct PerformanceEntryJs {
  #[qjs(skip_trace)]
  name: String,
  #[qjs(skip_trace)]
  entry_type: String,
  #[qjs(skip_trace)]
  start_time: f64,
  #[qjs(skip_trace)]
  duration: f64,
}

#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for PerformanceEntryJs {
  type Changed<'to> = PerformanceEntryJs;
}

#[rquickjs::methods(rename_all = "camelCase")]
impl PerformanceEntryJs {
  /// Exposed as a global but not constructible: its IDL declares no
  /// constructor, and the global has to exist anyway for
  /// `mark instanceof PerformanceEntry` to be answerable. rquickjs only
  /// puts a class on `globalThis` when it HAS a constructor, so the
  /// throwing one is what makes the name reachable.
  #[qjs(constructor)]
  fn constructor(ctx: Ctx<'_>) -> rquickjs::Result<Self> {
    Err(Exception::throw_type(&ctx, "Illegal constructor"))
  }

  #[qjs(get)]
  fn name(&self) -> String {
    self.name.clone()
  }

  #[qjs(get)]
  fn entry_type(&self) -> String {
    self.entry_type.clone()
  }

  #[qjs(get)]
  fn start_time(&self) -> f64 {
    self.start_time
  }

  #[qjs(get)]
  fn duration(&self) -> f64 {
    self.duration
  }

  #[qjs(rename = "toJSON")]
  fn to_json<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    let o = Object::new(ctx)?;
    o.set("name", self.name.clone())?;
    o.set("entryType", self.entry_type.clone())?;
    o.set("startTime", self.start_time)?;
    o.set("duration", self.duration)?;
    Ok(o)
  }
}

/// `PerformanceMark` — a `PerformanceEntry` plus the `detail` its
/// creator attached.
#[derive(Trace)]
#[rquickjs::class(rename = "PerformanceMark")]
pub struct PerformanceMarkJs<'js> {
  #[qjs(skip_trace)]
  name: String,
  #[qjs(skip_trace)]
  start_time: f64,
  detail: Value<'js>,
}

#[allow(unsafe_code)]
unsafe impl<'js> rquickjs::JsLifetime<'js> for PerformanceMarkJs<'js> {
  type Changed<'to> = PerformanceMarkJs<'to>;
}

#[rquickjs::methods(rename_all = "camelCase")]
impl<'js> PerformanceMarkJs<'js> {
  /// `new PerformanceMark(name, { detail?, startTime? })`. Unlike
  /// `performance.mark()`, a directly constructed mark is NOT added to
  /// the timeline — the spec buffers only what `mark()` records.
  #[qjs(constructor)]
  fn constructor(ctx: Ctx<'js>, name: String, options: Opt<Value<'js>>) -> rquickjs::Result<Self> {
    let (start_time, detail) = mark_options(&ctx, options.0.as_ref())?;
    Ok(Self {
      name,
      start_time,
      detail,
    })
  }

  #[qjs(get)]
  fn name(&self) -> String {
    self.name.clone()
  }

  #[qjs(get)]
  fn entry_type(&self) -> &'static str {
    MARK
  }

  #[qjs(get)]
  fn start_time(&self) -> f64 {
    self.start_time
  }

  /// Always 0: a mark is an instant, not an interval.
  #[qjs(get)]
  fn duration(&self) -> f64 {
    0.0
  }

  #[qjs(get)]
  fn detail(&self) -> Value<'js> {
    self.detail.clone()
  }

  /// `detail` is included, matching what browsers serialize. The IDL's
  /// default serializer covers only `PerformanceEntry`'s own
  /// attributes, so this is the more useful reading of an ambiguity
  /// rather than a strict one.
  #[qjs(rename = "toJSON")]
  fn to_json(&self, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    let o = Object::new(ctx)?;
    o.set("name", self.name.clone())?;
    o.set("entryType", MARK)?;
    o.set("startTime", self.start_time)?;
    o.set("duration", 0.0)?;
    o.set("detail", self.detail.clone())?;
    Ok(o)
  }
}

/// `PerformanceMeasure` — an interval between two points on the
/// timeline.
#[derive(Trace)]
#[rquickjs::class(rename = "PerformanceMeasure")]
pub struct PerformanceMeasureJs<'js> {
  #[qjs(skip_trace)]
  name: String,
  #[qjs(skip_trace)]
  start_time: f64,
  #[qjs(skip_trace)]
  duration: f64,
  detail: Value<'js>,
}

#[allow(unsafe_code)]
unsafe impl<'js> rquickjs::JsLifetime<'js> for PerformanceMeasureJs<'js> {
  type Changed<'to> = PerformanceMeasureJs<'to>;
}

#[rquickjs::methods(rename_all = "camelCase")]
impl<'js> PerformanceMeasureJs<'js> {
  /// Not constructible, same as `PerformanceEntry`: a measure only ever
  /// comes from `performance.measure()`.
  #[qjs(constructor)]
  fn constructor(ctx: Ctx<'js>) -> rquickjs::Result<Self> {
    Err(Exception::throw_type(&ctx, "Illegal constructor"))
  }

  #[qjs(get)]
  fn name(&self) -> String {
    self.name.clone()
  }

  #[qjs(get)]
  fn entry_type(&self) -> &'static str {
    MEASURE
  }

  #[qjs(get)]
  fn start_time(&self) -> f64 {
    self.start_time
  }

  #[qjs(get)]
  fn duration(&self) -> f64 {
    self.duration
  }

  #[qjs(get)]
  fn detail(&self) -> Value<'js> {
    self.detail.clone()
  }

  #[qjs(rename = "toJSON")]
  fn to_json(&self, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    let o = Object::new(ctx)?;
    o.set("name", self.name.clone())?;
    o.set("entryType", MEASURE)?;
    o.set("startTime", self.start_time)?;
    o.set("duration", self.duration)?;
    o.set("detail", self.detail.clone())?;
    Ok(o)
  }
}

/// One buffered entry.
///
/// The name / type / start time are kept Rust-side alongside the JS
/// value so a `measure` resolving a mark name, and every
/// `getEntriesBy*` filter, is a Rust comparison rather than a property
/// read back out of the interpreter for each candidate.
#[derive(Trace)]
struct Buffered<'js> {
  #[qjs(skip_trace)]
  name: String,
  #[qjs(skip_trace)]
  is_mark: bool,
  #[qjs(skip_trace)]
  start_time: f64,
  value: Value<'js>,
}

/// `performance`.
#[derive(Trace)]
#[rquickjs::class(rename = "Performance")]
pub struct PerformanceJs<'js> {
  entries: Vec<Buffered<'js>>,
}

#[allow(unsafe_code)]
unsafe impl<'js> rquickjs::JsLifetime<'js> for PerformanceJs<'js> {
  type Changed<'to> = PerformanceJs<'to>;
}

impl Default for PerformanceJs<'_> {
  fn default() -> Self {
    Self::new()
  }
}

impl<'js> PerformanceJs<'js> {
  #[must_use]
  pub fn new() -> Self {
    Self { entries: Vec::new() }
  }

  /// The start time a mark name resolves to: the MOST RECENT mark with
  /// that name, per User Timing's "convert a mark to a timestamp".
  fn resolve_mark(&self, ctx: &Ctx<'js>, name: &str) -> rquickjs::Result<f64> {
    self
      .entries
      .iter()
      .rev()
      .find(|e| e.is_mark && e.name == name)
      .map(|e| e.start_time)
      .ok_or_else(|| Exception::throw_syntax(ctx, &format!("the mark {name:?} does not exist")))
  }

  /// A `start` / `end` member of `PerformanceMeasureOptions`, or a mark
  /// name. A number must be non-negative; a string names a mark.
  fn resolve_timestamp(&self, ctx: &Ctx<'js>, value: &Value<'js>) -> rquickjs::Result<f64> {
    if let Some(name) = value.as_string() {
      return self.resolve_mark(ctx, &name.to_string()?);
    }
    let Some(n) = value.as_number() else {
      return Err(Exception::throw_type(
        ctx,
        "a performance timestamp must be a mark name or a number",
      ));
    };
    if n < 0.0 {
      return Err(Exception::throw_type(ctx, "a performance timestamp cannot be negative"));
    }
    Ok(n)
  }
}

#[rquickjs::methods(rename_all = "camelCase")]
impl<'js> PerformanceJs<'js> {
  #[qjs(constructor)]
  fn constructor(ctx: Ctx<'js>) -> rquickjs::Result<Self> {
    Err(Exception::throw_type(&ctx, "Illegal constructor"))
  }

  /// Unix-epoch milliseconds the monotonic readings are relative to.
  #[qjs(get)]
  fn time_origin(&self) -> f64 {
    time_origin_ms()
  }

  /// Monotonic milliseconds since `timeOrigin`.
  fn now(&self) -> f64 {
    now_ms()
  }

  #[qjs(rename = "toJSON")]
  fn to_json(&self, ctx: Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    let o = Object::new(ctx)?;
    o.set("timeOrigin", time_origin_ms())?;
    Ok(o)
  }

  /// `mark(name, { detail?, startTime? })`.
  fn mark(&mut self, ctx: Ctx<'js>, name: String, options: Opt<Value<'js>>) -> rquickjs::Result<Value<'js>> {
    let (start_time, detail) = mark_options(&ctx, options.0.as_ref())?;

    let entry = Class::instance(
      ctx.clone(),
      PerformanceMarkJs {
        name: name.clone(),
        start_time,
        detail,
      },
    )?;
    let value = entry.into_value();
    self.entries.push(Buffered {
      name,
      is_mark: true,
      start_time,
      value: value.clone(),
    });
    Ok(value)
  }

  /// `measure(name, startMarkOrOptions?, endMark?)`.
  ///
  /// The three-way overload the spec defines: a bare name measures from
  /// the time origin to now; a string names the start mark; an options
  /// bag carries any two of `start` / `end` / `duration` and the third
  /// is derived.
  fn measure(
    &mut self,
    ctx: Ctx<'js>,
    name: String,
    start_or_options: Opt<Value<'js>>,
    end_mark: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let arg = start_or_options.0.filter(|v| !v.is_undefined());
    let end_arg = end_mark.0.filter(|v| !v.is_undefined());
    let options = arg.as_ref().and_then(|v| v.as_object()).filter(|o| !o.is_array());

    let mut detail = Value::new_null(ctx.clone());
    let (start_time, duration) = if let Some(options) = options {
      let get = |key: &str| -> Option<Value<'js>> {
        options.get::<_, Value<'js>>(key).ok().filter(|v| !v.is_undefined())
      };
      let (start, end, dur) = (get("start"), get("end"), get("duration"));

      // Both an options bag and a trailing endMark is ambiguous about
      // which one wins, so the spec refuses rather than picking.
      if end_arg.is_some() {
        return Err(Exception::throw_type(
          &ctx,
          "measure() takes an endMark or a PerformanceMeasureOptions object, not both",
        ));
      }
      if start.is_none() && end.is_none() && dur.is_none() {
        return Err(Exception::throw_type(
          &ctx,
          "PerformanceMeasureOptions must set at least one of start, end or duration",
        ));
      }
      // All three over-constrain the interval: they can disagree.
      if start.is_some() && end.is_some() && dur.is_some() {
        return Err(Exception::throw_type(
          &ctx,
          "PerformanceMeasureOptions cannot set all of start, end and duration",
        ));
      }
      if let Some(given) = get("detail") {
        detail = given;
      }

      let dur = dur.map(|d| self.resolve_timestamp(&ctx, &d)).transpose()?;
      let start = start.map(|s| self.resolve_timestamp(&ctx, &s)).transpose()?;
      let end = end.map(|e| self.resolve_timestamp(&ctx, &e)).transpose()?;

      match (start, end, dur) {
        (Some(s), Some(e), _) => (s, e - s),
        (Some(s), None, Some(d)) => (s, d),
        (None, Some(e), Some(d)) => (e - d, d),
        (Some(s), None, None) => (s, now_ms() - s),
        (None, Some(e), None) => (0.0, e),
        (None, None, Some(d)) => (now_ms() - d, d),
        (None, None, None) => unreachable!("the all-absent case is refused above"),
      }
    } else {
      let start = match &arg {
        Some(v) => self.resolve_timestamp(&ctx, v)?,
        None => 0.0,
      };
      let end = match &end_arg {
        Some(v) => self.resolve_timestamp(&ctx, v)?,
        None => now_ms(),
      };
      (start, end - start)
    };

    let entry = Class::instance(
      ctx.clone(),
      PerformanceMeasureJs {
        name: name.clone(),
        start_time,
        duration,
        detail,
      },
    )?;
    let value = entry.into_value();
    self.entries.push(Buffered {
      name,
      is_mark: false,
      start_time,
      value: value.clone(),
    });
    Ok(value)
  }

  /// Drop every mark, or every mark with `name`.
  fn clear_marks(&mut self, name: Opt<String>) {
    match name.0 {
      Some(name) => self.entries.retain(|e| !(e.is_mark && e.name == name)),
      None => self.entries.retain(|e| !e.is_mark),
    }
  }

  /// Drop every measure, or every measure with `name`.
  fn clear_measures(&mut self, name: Opt<String>) {
    match name.0 {
      Some(name) => self.entries.retain(|e| !(!e.is_mark && e.name == name)),
      None => self.entries.retain(|e| e.is_mark),
    }
  }

  /// Every entry, in chronological order of `startTime`.
  ///
  /// Insertion order is not enough: `mark(name, { startTime })` can
  /// backdate an entry, so a later call may belong earlier on the
  /// timeline. The sort is stable, so entries sharing a `startTime`
  /// keep the order they were recorded in.
  fn get_entries(&self) -> Vec<Value<'js>> {
    let mut out: Vec<&Buffered<'js>> = self.entries.iter().collect();
    out.sort_by(|a, b| a.start_time.total_cmp(&b.start_time));
    out.into_iter().map(|e| e.value.clone()).collect()
  }

  fn get_entries_by_name(&self, name: String, entry_type: Opt<String>) -> Vec<Value<'js>> {
    let wanted = entry_type.0;
    let mut out: Vec<&Buffered<'js>> = self
      .entries
      .iter()
      .filter(|e| e.name == name)
      .filter(|e| match wanted.as_deref() {
        Some(t) => t == if e.is_mark { MARK } else { MEASURE },
        None => true,
      })
      .collect();
    out.sort_by(|a, b| a.start_time.total_cmp(&b.start_time));
    out.into_iter().map(|e| e.value.clone()).collect()
  }

  fn get_entries_by_type(&self, entry_type: String) -> Vec<Value<'js>> {
    let want_mark = entry_type == MARK;
    if !want_mark && entry_type != MEASURE {
      return Vec::new();
    }
    let mut out: Vec<&Buffered<'js>> = self.entries.iter().filter(|e| e.is_mark == want_mark).collect();
    out.sort_by(|a, b| a.start_time.total_cmp(&b.start_time));
    out.into_iter().map(|e| e.value.clone()).collect()
  }
}

/// Define the four classes and install the `performance` instance.
///
/// # Errors
///
/// Propagates the class definitions and the global write.
pub fn init(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  let globals = ctx.globals();
  Class::<PerformanceEntryJs>::define(&globals)?;
  Class::<PerformanceMarkJs>::define(&globals)?;
  Class::<PerformanceMeasureJs>::define(&globals)?;
  Class::<PerformanceJs>::define(&globals)?;
  chain_entry_prototypes(ctx)?;

  let performance = Class::instance(ctx.clone(), PerformanceJs::new())?;
  globals.set("performance", performance)?;
  Ok(())
}

/// Point `PerformanceMark.prototype` and `PerformanceMeasure.prototype`
/// at `PerformanceEntry.prototype`, which is what makes a mark an
/// instance of `PerformanceEntry` — the relationship the timeline's
/// whole type hierarchy is expressed in.
fn chain_entry_prototypes(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  let Some(entry_proto) = Class::<PerformanceEntryJs>::prototype(ctx)? else {
    return Ok(());
  };
  if let Some(mark_proto) = Class::<PerformanceMarkJs>::prototype(ctx)? {
    mark_proto.set_prototype(Some(&entry_proto))?;
  }
  if let Some(measure_proto) = Class::<PerformanceMeasureJs>::prototype(ctx)? {
    measure_proto.set_prototype(Some(&entry_proto))?;
  }
  Ok(())
}
