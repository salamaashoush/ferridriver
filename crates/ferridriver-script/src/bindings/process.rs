//! A deliberately small, sandbox-safe `process` global.
//!
//! Node's `process` is mostly ambient authority; this exposes only the
//! members that are either inert (platform/version/timing) or
//! capability-gated (`env`). Everything that could escape the sandbox
//! (`binding`, `dlopen`, `chdir`, `kill`, `setuid`, real `exit`) is
//! absent or neutered. `process.env` is the operator's allow-list
//! intersected with the real environment — empty by default.

use std::time::Instant;

use rquickjs::function::{Func, Rest};
use rquickjs::{Ctx, Object, Value};

use crate::engine::ScriptCaps;

/// Install `globalThis.process`. Called once per session (the values
/// are session-stable: env comes from resolved config, the
/// monotonic clock anchors at session start).
pub fn install(ctx: &Ctx<'_>, caps: &ScriptCaps, cwd: &str) -> rquickjs::Result<()> {
  let g = ctx.globals();
  let p = Object::new(ctx.clone())?;

  // -- env: the only sensitive surface, default-deny ----------------
  let env = Object::new(ctx.clone())?;
  for (k, v) in &caps.env {
    env.set(k.as_str(), v.as_str())?;
  }
  // Frozen so a script cannot stuff values in and mislead later code
  // into thinking an env var is set.
  freeze(ctx, &env)?;
  p.set("env", env)?;

  // -- inert platform identity --------------------------------------
  p.set("platform", std::env::consts::OS)?; // "linux" | "macos" | ...
  p.set("arch", std::env::consts::ARCH)?; // "x86_64" | "aarch64" | ...
  let fv = env!("CARGO_PKG_VERSION");
  p.set("version", format!("ferridriver-{fv}"))?;
  let versions = Object::new(ctx.clone())?;
  versions.set("ferridriver", fv)?;
  versions.set("quickjs", "rquickjs-0.11")?;
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

  // nextTick -> microtask (QuickJS has queueMicrotask via webapi).
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
  crate::bindings::runtime::mirror_global(ctx, "process")?;
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
