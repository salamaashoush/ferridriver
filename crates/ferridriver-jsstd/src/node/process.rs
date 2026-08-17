//! A deliberately small, sandbox-safe `process`, and the `node:process`
//! module form of it.
//!
//! Node's `process` is mostly ambient authority; this exposes only the
//! members that are either inert (platform/version/timing) or supplied by
//! the host (`env`, `cwd`). Everything that could escape a sandbox
//! (`binding`, `dlopen`, `chdir`, `kill`, `setuid`, real `exit`) is absent
//! or neutered, and `env` carries exactly the variables the host passes —
//! empty unless it passes some.
//!
//! The module form is not a second implementation: the object it hands
//! back IS `globalThis.process`, so `import process from 'node:process'`,
//! `require('process')` and the bare global are one object, as in Node.

use std::time::Instant;

use rquickjs::function::{Func, Rest};
use rquickjs::{Ctx, Object, Result, Value};

/// The names the module re-exports. `process` itself is installed by the
/// host; anything it does not set simply does not appear.
pub const PROCESS_MEMBERS: &[&str] = &[
  "argv",
  "argv0",
  "arch",
  "cwd",
  "env",
  "exit",
  "hrtime",
  "nextTick",
  "pid",
  "platform",
  "release",
  "stderr",
  "stdout",
  "version",
  "versions",
];

/// `globalThis.process`.
///
/// # Errors
///
/// When the host installed no `process` global.
pub fn process_object<'js>(ctx: &Ctx<'js>) -> Result<Object<'js>> {
  ctx.globals().get("process")
}

/// Install `globalThis.process`. Called once per session (the values are
/// session-stable: `env` is the host's resolved allow-list, `cwd` its
/// sandbox root, and the monotonic clock anchors here).
///
/// # Errors
///
/// Propagates the property writes.
pub fn install<'js, I, K, V>(ctx: &Ctx<'js>, env: I, cwd: &str) -> rquickjs::Result<()>
where
  I: IntoIterator<Item = (K, V)>,
  K: AsRef<str>,
  V: AsRef<str>,
{
  let g = ctx.globals();
  let p = Object::new(ctx.clone())?;

  // -- env: the only sensitive surface, default-deny ----------------
  let vars = env;
  let env = Object::new(ctx.clone())?;
  for (k, v) in vars {
    env.set(k.as_ref(), v.as_ref())?;
  }
  // Frozen so a script cannot stuff values in and mislead later code
  // into thinking an env var is set.
  freeze(ctx, &env)?;
  p.set("env", env)?;

  // -- inert platform identity --------------------------------------
  // Node's spelling, not Rust's: a suite branching on `process.platform
  // === 'darwin'` (or comparing it to `os.platform()`) must see one
  // answer, so both read the same constants.
  p.set("platform", crate::utils::sysinfo::PLATFORM)?;
  p.set("arch", crate::utils::sysinfo::ARCH)?;
  let fv = env!("CARGO_PKG_VERSION");
  p.set("version", format!("ferridriver-{fv}"))?;
  let versions = Object::new(ctx.clone())?;
  versions.set("ferridriver", fv)?;
  versions.set("quickjs", "rquickjs-0.12")?;
  freeze(ctx, &versions)?;
  p.set("versions", versions)?;
  let release = Object::new(ctx.clone())?;
  release.set("name", "ferridriver")?;
  freeze(ctx, &release)?;
  p.set("release", release)?;

  // argv: scripts get their inputs via the `args` global, not argv;
  // expose a minimal, stable shape only for packages that read it.
  let argv = rquickjs::Array::new(ctx.clone())?;
  argv.set(0, "ferridriver")?;
  argv.set(1, "script")?;
  p.set("argv", argv)?;
  p.set("argv0", "ferridriver")?;
  p.set("pid", i64::from(std::process::id()))?;

  // cwd(): the sandbox root, never the real process cwd (no path leak).
  let root = cwd.to_string();
  p.set("cwd", Func::from(move || root.clone()))?;

  // nextTick -> microtask; the host installs `queueMicrotask`.
  let next_tick = ctx.eval::<Value<'_>, _>(
    "((cb, ...a) => { if (typeof cb !== 'function') throw new TypeError('callback required'); \
       queueMicrotask(() => cb(...a)); })",
  )?;
  p.set("nextTick", next_tick)?;

  // stdout/stderr: only `.write(chunk)` — routed into the same console
  // capture the `console` global feeds (so output surfaces in
  // `ScriptResult.console[]`), one trailing newline trimmed so a
  // `write("x\n")` is one line, not a line + blank. Returns `true`
  // (Node's "not backpressured"). No fd, not a TTY.
  for (name, level) in [("stdout", "log"), ("stderr", "error")] {
    let stream = Object::new(ctx.clone())?;
    let f = rquickjs::Function::new(
      ctx.clone(),
      move |c: Ctx<'_>, chunk: Value<'_>| -> rquickjs::Result<bool> {
        let s = chunk
          .as_string()
          .and_then(|v| v.to_string().ok())
          .or_else(|| chunk.as_number().map(|n| n.to_string()))
          .unwrap_or_default();
        let s = s.strip_suffix('\n').unwrap_or(&s).to_string();
        let console: Object<'_> = c.globals().get("console")?;
        let sink: rquickjs::Function<'_> = console.get(level)?;
        sink.call::<_, ()>((s,))?;
        Ok(true)
      },
    )?;
    stream.set("write", f)?;
    stream.set("isTTY", false)?;
    p.set(name, stream)?;
  }

  // hrtime([prev]) -> [seconds, nanos], monotonic from session start;
  // hrtime.bigint() -> BigInt nanoseconds (Node parity).
  let start = Instant::now();
  let hrtime = rquickjs::Function::new(ctx.clone(), move |prev: Rest<Value<'_>>| -> Vec<i64> {
    let now = start.elapsed();
    let (mut s, mut n) = (
      i64::try_from(now.as_secs()).unwrap_or(i64::MAX),
      i64::from(now.subsec_nanos()),
    );
    if let Some(arr) = prev.0.first().and_then(|v| v.as_array()) {
      let ps = arr.get::<i64>(0).unwrap_or(0);
      let pn = arr.get::<i64>(1).unwrap_or(0);
      s -= ps;
      n -= pn;
      if n < 0 {
        s -= 1;
        n += 1_000_000_000;
      }
    }
    vec![s, n]
  })?;
  // Forward into a generic fn so the `Ctx` and the returned `Value`
  // share one `'js` (an inline closure gives each its own lifetime).
  let bigint = rquickjs::Function::new(ctx.clone(), move |c| hrtime_bigint(c, start))?;
  hrtime.set("bigint", bigint)?;
  p.set("hrtime", hrtime)?;

  // exit(): never kill the server — surface intent as an error so a
  // script that relies on it fails loudly instead of silently no-oping.
  p.set(
    "exit",
    Func::from(|code: Rest<Value<'_>>| -> rquickjs::Result<()> {
      let c = code.0.first().and_then(rquickjs::Value::as_int).unwrap_or(0);
      Err(rquickjs::Error::new_from_js_message(
        "process.exit",
        "Error",
        format!("process.exit({c}) is not allowed in the ferridriver sandbox"),
      ))
    }),
  )?;

  g.set("process", p)?;
  Ok(())
}

/// `process.hrtime.bigint()` — nanoseconds since session start as a
/// JS `BigInt`. Free fn so the closure's `Ctx`/return share `'js`.
fn hrtime_bigint(ctx: Ctx<'_>, start: Instant) -> rquickjs::Result<Value<'_>> {
  let nanos = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
  Ok(rquickjs::BigInt::from_u64(ctx, nanos)?.into_value())
}

fn freeze<'js>(ctx: &Ctx<'js>, obj: &Object<'js>) -> rquickjs::Result<()> {
  let freeze: rquickjs::Function<'js> = ctx.globals().get::<_, Object<'js>>("Object")?.get("freeze")?;
  freeze.call::<_, Value<'js>>((obj.clone(),))?;
  Ok(())
}
