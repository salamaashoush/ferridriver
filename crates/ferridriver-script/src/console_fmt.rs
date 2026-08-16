//! The `console` global: Node's surface, rendering, and colours.
//!
//! Covers `log`/`info`/`warn`/`error`/`debug`/`trace`, `dir`, `table`,
//! `group`/`groupCollapsed`/`groupEnd`, `count`/`countReset`,
//! `time`/`timeLog`/`timeEnd`, `assert` and `clear`, plus `util.format`
//! specifiers (`%s %d %i %f %j %o %O %c %%`) and a `util.inspect`-shaped value
//! renderer.
//!
//! Values are styled with `util.inspect`'s colours only when the host's
//! [`ConsoleSink`](crate::console::ConsoleSink) reports a terminal; the
//! buffered path renders plain text, so a captured `ScriptResult.console`
//! never carries escape codes.
//!
//! Every string that comes from JS is run through
//! [`strip_ansi`](crate::console::strip_ansi) as it is written, so page
//! content cannot smuggle terminal control codes into the output while our
//! own styling survives.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rquickjs::function::{Func, Rest};
use rquickjs::{Ctx, Object, Value};
use rustc_hash::FxHashMap;

use ferridriver_jsstd::node::inspect::{Inspector, MAX_DIR_DEPTH};

use crate::console::{ConsoleCapture, strip_ansi};
use crate::result::ConsoleLevel;

/// Per-context console bookkeeping: `group` indentation, `count` tallies and
/// `time` marks. Plain Rust state behind an `Arc` — it holds no JS values, so
/// it cannot form a GC cycle with the closures that capture it.
#[derive(Default)]
struct ConsoleState {
  indent: AtomicUsize,
  counts: Mutex<FxHashMap<String, u64>>,
  timers: Mutex<FxHashMap<String, Instant>>,
}

impl ConsoleState {
  fn indent_prefix(&self) -> String {
    " ".repeat(self.indent.load(Ordering::Relaxed) * 2)
  }
}

/// Push one rendered message, indented by the current `console.group` depth
/// (Node indents every line of a multi-line message).
fn emit(capture: &ConsoleCapture, state: &ConsoleState, level: ConsoleLevel, message: &str) {
  let prefix = state.indent_prefix();
  if prefix.is_empty() {
    capture.push(level, message);
    return;
  }
  let indented = message
    .split('\n')
    .map(|line| format!("{prefix}{line}"))
    .collect::<Vec<_>>()
    .join("\n");
  capture.push(level, indented);
}

/// A `label`-taking console method's argument: Node coerces a missing label to
/// `"default"`.
fn label_or_default(label: Option<String>) -> String {
  label.filter(|l| !l.is_empty()).unwrap_or_else(|| "default".to_string())
}

/// Node's `label: 1.234ms` duration rendering.
fn format_duration(elapsed: std::time::Duration) -> String {
  let ms = elapsed.as_secs_f64() * 1000.0;
  if ms >= 1000.0 {
    format!("{:.3}s", ms / 1000.0)
  } else {
    format!("{ms:.3}ms")
  }
}

/// One `console.table` cell / column set built from the JS value.
struct Table {
  columns: Vec<String>,
  has_values_column: bool,
  rows: Vec<TableRow>,
}

struct TableRow {
  index: String,
  cells: Vec<(String, String)>,
  value: Option<String>,
}

/// Column header Node uses for a row that is a primitive rather than an object.
const VALUES_COLUMN: &str = "Values";
const INDEX_COLUMN: &str = "(index)";

/// Build the table model from `data`, or `None` when `data` is not tabular
/// (a primitive), in which case the caller logs it plainly like Node does.
fn build_table(inspector: Inspector, data: &Value<'_>, filter: Option<&[String]>) -> rquickjs::Result<Option<Table>> {
  let mut columns: Vec<String> = Vec::new();
  let mut rows: Vec<TableRow> = Vec::new();
  let mut has_values_column = false;

  let entries: Vec<(String, Value<'_>)> = if let Some(array) = data.as_array() {
    array
      .iter::<Value<'_>>()
      .enumerate()
      .map(|(i, v)| v.map(|v| (i.to_string(), v)))
      .collect::<rquickjs::Result<_>>()?
  } else if let Some(object) = data.as_object().filter(|o| !o.is_function()) {
    object
      .props::<String, Value<'_>>()
      .collect::<rquickjs::Result<Vec<_>>>()?
  } else {
    return Ok(None);
  };

  for (index, value) in entries {
    let inner = value
      .as_array()
      .map(|a| {
        a.iter::<Value<'_>>()
          .enumerate()
          .map(|(i, v)| v.map(|v| (i.to_string(), v)))
          .collect::<rquickjs::Result<Vec<_>>>()
      })
      .or_else(|| {
        value
          .as_object()
          .filter(|o| !o.is_function())
          .map(|o| o.props::<String, Value<'_>>().collect::<rquickjs::Result<Vec<_>>>())
      })
      .transpose()?;

    if let Some(props) = inner {
      let mut cells = Vec::with_capacity(props.len());
      for (key, val) in props {
        if filter.is_some_and(|f| !f.contains(&key)) {
          continue;
        }
        if !columns.contains(&key) {
          columns.push(key.clone());
        }
        let mut rendered = String::new();
        inspector.value(&mut rendered, &val, 1)?;
        cells.push((key, rendered));
      }
      rows.push(TableRow {
        index,
        cells,
        value: None,
      });
    } else {
      has_values_column = true;
      let mut rendered = String::new();
      inspector.value(&mut rendered, &value, 1)?;
      rows.push(TableRow {
        index,
        cells: Vec::new(),
        value: Some(rendered),
      });
    }
  }

  Ok(Some(Table {
    columns,
    has_values_column,
    rows,
  }))
}

/// Render the table model with Node's box-drawing frame. Column widths are
/// counted in `char`s: wide CJK / emoji cells under-measure, exactly as they
/// do in Node's own implementation.
fn render_table(table: &Table) -> String {
  let mut headers = vec![INDEX_COLUMN.to_string()];
  headers.extend(table.columns.iter().cloned());
  if table.has_values_column {
    headers.push(VALUES_COLUMN.to_string());
  }

  let body: Vec<Vec<String>> = table
    .rows
    .iter()
    .map(|row| {
      let mut cells = vec![row.index.clone()];
      for column in &table.columns {
        cells.push(
          row
            .cells
            .iter()
            .find(|(k, _)| k == column)
            .map(|(_, v)| v.clone())
            .unwrap_or_default(),
        );
      }
      if table.has_values_column {
        cells.push(row.value.clone().unwrap_or_default());
      }
      cells
    })
    .collect();

  let widths: Vec<usize> = headers
    .iter()
    .enumerate()
    .map(|(i, h)| {
      body
        .iter()
        .filter_map(|row| row.get(i))
        .map(|c| c.chars().count())
        .chain(std::iter::once(h.chars().count()))
        .max()
        .unwrap_or(0)
    })
    .collect();

  let rule = |left: &str, mid: &str, right: &str| {
    let mut s = String::from(left);
    for (i, w) in widths.iter().enumerate() {
      if i > 0 {
        s.push_str(mid);
      }
      s.push_str(&"─".repeat(w + 2));
    }
    s.push_str(right);
    s
  };
  let line = |cells: &[String]| {
    let mut s = String::from("│");
    for (i, w) in widths.iter().enumerate() {
      let cell = cells.get(i).map(String::as_str).unwrap_or_default();
      let pad = w.saturating_sub(cell.chars().count());
      s.push(' ');
      s.push_str(cell);
      s.push_str(&" ".repeat(pad));
      s.push_str(" │");
    }
    s
  };

  let mut out = vec![rule("┌", "┬", "┐"), line(&headers), rule("├", "┼", "┤")];
  out.extend(body.iter().map(|row| line(row)));
  out.push(rule("└", "┴", "┘"));
  out.join("\n")
}

/// Install the `console` global backed by `capture`.
pub(crate) fn install_console(ctx: &Ctx<'_>, capture: Arc<ConsoleCapture>) -> rquickjs::Result<()> {
  let state = Arc::new(ConsoleState::default());
  // `log` and `error` land on different streams, so whether ANSI styling is
  // safe is decided per level, not once for the whole console.
  let log_inspector = Inspector::new(capture.styled_for(ConsoleLevel::Log));
  let error_inspector = Inspector::new(capture.styled_for(ConsoleLevel::Error));
  let trace_inspector = Inspector::new(capture.styled_for(ConsoleLevel::Trace));
  let console = Object::new(ctx.clone())?;

  for (name, level) in [
    ("log", ConsoleLevel::Log),
    ("info", ConsoleLevel::Info),
    ("warn", ConsoleLevel::Warn),
    ("error", ConsoleLevel::Error),
    ("debug", ConsoleLevel::Debug),
  ] {
    let cap = capture.clone();
    let st = state.clone();
    let inspector = Inspector::new(capture.styled_for(level));
    console.set(
      name,
      Func::from(move |args: Rest<Value<'_>>| -> rquickjs::Result<()> {
        let mut msg = String::new();
        inspector.args(&mut msg, &args.0)?;
        emit(&cap, &st, level, &msg);
        Ok(())
      }),
    )?;
  }

  {
    let cap = capture.clone();
    let st = state.clone();
    console.set(
      "trace",
      Func::from(move |ctx: Ctx<'_>, args: Rest<Value<'_>>| -> rquickjs::Result<()> {
        let mut msg = String::from("Trace");
        if !args.0.is_empty() {
          msg.push_str(": ");
          trace_inspector.args(&mut msg, &args.0)?;
        }
        // The capture-site stack: an Exception built here carries the JS
        // frames below this native call.
        if let Some(stack) = rquickjs::Exception::from_message(ctx, "")
          .ok()
          .and_then(|e| e.stack())
          .filter(|s| !s.is_empty())
        {
          msg.push('\n');
          msg.push_str(strip_ansi(&stack).trim_end());
        }
        emit(&cap, &st, ConsoleLevel::Trace, &msg);
        Ok(())
      }),
    )?;
  }

  {
    let cap = capture.clone();
    let st = state.clone();
    console.set(
      "dir",
      Func::from(
        move |value: Value<'_>, options: rquickjs::function::Opt<Value<'_>>| -> rquickjs::Result<()> {
          // `{ depth: null }` means unlimited in Node; we cap it so a huge
          // graph cannot hang the VM inside a log call.
          let depth = options
            .0
            .as_ref()
            .and_then(Value::as_object)
            .map_or(Some(2), |o| match o.get::<_, Value<'_>>("depth") {
              Ok(v) if v.is_null() => None,
              Ok(v) => v.as_number().map(|n| n as usize),
              Err(_) => Some(2),
            })
            .unwrap_or(MAX_DIR_DEPTH)
            .min(MAX_DIR_DEPTH);
          // Node's `dir` defaults `colors: false` — unlike `log`, which
          // colours whenever the stream is a terminal.
          let colors = options
            .0
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|o| o.get::<_, Value<'_>>("colors").ok())
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
          let mut msg = String::new();
          // `dir` quotes a bare string, unlike `log`.
          Inspector::new(colors && log_inspector.styled)
            .quoted()
            .with_depth(depth)
            .value(&mut msg, &value, 0)?;
          emit(&cap, &st, ConsoleLevel::Log, &msg);
          Ok(())
        },
      ),
    )?;
  }

  {
    let cap = capture.clone();
    let st = state.clone();
    console.set(
      "table",
      Func::from(
        move |data: Value<'_>, columns: rquickjs::function::Opt<Value<'_>>| -> rquickjs::Result<()> {
          let filter: Option<Vec<String>> = columns
            .0
            .as_ref()
            .and_then(Value::as_array)
            .map(|a| a.iter::<String>().collect::<rquickjs::Result<Vec<_>>>())
            .transpose()?;
          let mut msg = String::new();
          match build_table(log_inspector, &data, filter.as_deref())? {
            Some(table) => msg.push_str(&render_table(&table)),
            // A primitive has no table shape — Node logs it as-is.
            None => log_inspector.quoted().value(&mut msg, &data, 0)?,
          }
          emit(&cap, &st, ConsoleLevel::Log, &msg);
          Ok(())
        },
      ),
    )?;
  }

  for name in ["group", "groupCollapsed"] {
    let cap = capture.clone();
    let st = state.clone();
    console.set(
      name,
      Func::from(move |args: Rest<Value<'_>>| -> rquickjs::Result<()> {
        if !args.0.is_empty() {
          let mut msg = String::new();
          log_inspector.args(&mut msg, &args.0)?;
          emit(&cap, &st, ConsoleLevel::Log, &msg);
        }
        st.indent.fetch_add(1, Ordering::Relaxed);
        Ok(())
      }),
    )?;
  }

  {
    let st = state.clone();
    console.set(
      "groupEnd",
      Func::from(move || {
        let _ = st
          .indent
          .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |d| Some(d.saturating_sub(1)));
      }),
    )?;
  }

  {
    let cap = capture.clone();
    let st = state.clone();
    console.set(
      "count",
      Func::from(move |label: rquickjs::function::Opt<String>| -> rquickjs::Result<()> {
        let label = label_or_default(label.0);
        let n = st.counts.lock().map_or(0, |mut c| {
          let n = c.entry(label.clone()).or_insert(0);
          *n += 1;
          *n
        });
        emit(&cap, &st, ConsoleLevel::Log, &format!("{}: {n}", strip_ansi(&label)));
        Ok(())
      }),
    )?;
  }

  {
    let st = state.clone();
    console.set(
      "countReset",
      Func::from(move |label: rquickjs::function::Opt<String>| {
        let label = label_or_default(label.0);
        if let Ok(mut counts) = st.counts.lock() {
          counts.remove(&label);
        }
      }),
    )?;
  }

  {
    let cap = capture.clone();
    let st = state.clone();
    console.set(
      "time",
      Func::from(move |label: rquickjs::function::Opt<String>| {
        let label = label_or_default(label.0);
        let duplicate = st
          .timers
          .lock()
          .is_ok_and(|mut t| t.insert(label.clone(), Instant::now()).is_some());
        if duplicate {
          emit(
            &cap,
            &st,
            ConsoleLevel::Warn,
            &format!("Label '{}' already exists for console.time()", strip_ansi(&label)),
          );
        }
      }),
    )?;
  }

  for (name, remove) in [("timeEnd", true), ("timeLog", false)] {
    let cap = capture.clone();
    let st = state.clone();
    console.set(
      name,
      Func::from(
        move |label: rquickjs::function::Opt<String>, args: Rest<Value<'_>>| -> rquickjs::Result<()> {
          let label = label_or_default(label.0);
          let started = st.timers.lock().ok().and_then(|mut t| {
            if remove {
              t.remove(&label)
            } else {
              t.get(&label).copied()
            }
          });
          let Some(started) = started else {
            // Node emits this through `process.emitWarning`, which lands on
            // stderr with exactly this text.
            emit(
              &cap,
              &st,
              ConsoleLevel::Warn,
              &format!("No such label '{}' for console.{name}()", strip_ansi(&label)),
            );
            return Ok(());
          };
          let mut msg = format!("{}: {}", strip_ansi(&label), format_duration(started.elapsed()));
          if !args.0.is_empty() {
            msg.push(' ');
            log_inspector.args(&mut msg, &args.0)?;
          }
          emit(&cap, &st, ConsoleLevel::Log, &msg);
          Ok(())
        },
      ),
    )?;
  }

  {
    let cap = capture.clone();
    let st = state.clone();
    console.set(
      "assert",
      Func::from(
        move |condition: rquickjs::function::Opt<Value<'_>>, args: Rest<Value<'_>>| -> rquickjs::Result<()> {
          if condition.0.as_ref().is_some_and(is_truthy) {
            return Ok(());
          }
          let mut msg = String::from("Assertion failed");
          if !args.0.is_empty() {
            msg.push_str(": ");
            error_inspector.args(&mut msg, &args.0)?;
          }
          emit(&cap, &st, ConsoleLevel::Error, &msg);
          Ok(())
        },
      ),
    )?;
  }

  {
    let cap = capture.clone();
    let st = state.clone();
    console.set(
      "clear",
      Func::from(move || {
        // Node resets group indentation on clear whether or not the stream
        // can actually be cleared.
        st.indent.store(0, Ordering::Relaxed);
        cap.clear();
      }),
    )?;
  }

  {
    // Node's `dirxml` has no XML rendering outside a browser devtools host:
    // it is documented as an alias for `console.log`.
    let cap = capture.clone();
    let st = state.clone();
    console.set(
      "dirxml",
      Func::from(move |args: Rest<Value<'_>>| -> rquickjs::Result<()> {
        let mut msg = String::new();
        log_inspector.args(&mut msg, &args.0)?;
        emit(&cap, &st, ConsoleLevel::Log, &msg);
        Ok(())
      }),
    )?;
  }

  // Inspector-only in Node: without an attached inspector these are no-ops
  // that exist so instrumented code runs unmodified. We have no inspector,
  // so they are always no-ops — present, and honest about doing nothing.
  for name in ["profile", "profileEnd", "timeStamp"] {
    console.set(name, Func::from(|_label: rquickjs::function::Opt<String>| {}))?;
  }

  ctx.globals().set("console", console)?;
  Ok(())
}

/// JS truthiness for `console.assert`'s condition.
fn is_truthy(value: &Value<'_>) -> bool {
  use rquickjs::Type;
  match value.type_of() {
    Type::Bool => value.as_bool().unwrap_or(false),
    Type::Int => value.as_int().unwrap_or(0) != 0,
    Type::Float => value.as_float().is_some_and(|f| f != 0.0 && !f.is_nan()),
    Type::String => value
      .as_string()
      .and_then(|s| s.to_string().ok())
      .is_some_and(|s| !s.is_empty()),
    Type::Null | Type::Undefined | Type::Uninitialized => false,
    _ => true,
  }
}
