//! `node:path` — POSIX-style pure string operations.
//!
//! `join`, `resolve`, `dirname`, `basename`, `extname`, `normalize`,
//! `relative`, `isAbsolute`, `sep`, `delimiter`. `resolve` roots at
//! `process.cwd()` — the sandbox root this runtime reports, not the real
//! process directory. No win32 flavour, and no `parse` / `format` yet.

use rquickjs::function::{Func, Opt, Rest};
use rquickjs::{Ctx, Object};

fn normalize_str(path: &str) -> String {
  let absolute = path.starts_with('/');
  let mut out: Vec<&str> = Vec::new();
  for seg in path.split('/') {
    match seg {
      "" | "." => {},
      ".." => {
        if matches!(out.last(), Some(&"..")) || (out.is_empty() && !absolute) {
          out.push("..");
        } else {
          out.pop();
        }
      },
      s => out.push(s),
    }
  }
  let joined = out.join("/");
  let trailing = path.len() > 1 && path.ends_with('/') && !joined.is_empty();
  match (absolute, joined.is_empty()) {
    (true, true) => "/".to_string(),
    (true, false) => format!("/{joined}{}", if trailing { "/" } else { "" }),
    (false, true) => ".".to_string(),
    (false, false) => format!("{joined}{}", if trailing { "/" } else { "" }),
  }
}

fn join_segments(segments: &[String]) -> String {
  let parts: Vec<&str> = segments.iter().map(String::as_str).filter(|s| !s.is_empty()).collect();
  if parts.is_empty() {
    return ".".to_string();
  }
  normalize_str(&parts.join("/"))
}

fn dirname_str(path: &str) -> String {
  let trimmed = path.trim_end_matches('/');
  match trimmed.rfind('/') {
    Some(0) => "/".to_string(),
    Some(i) => trimmed[..i].to_string(),
    None => {
      if path.starts_with('/') {
        "/".to_string()
      } else {
        ".".to_string()
      }
    },
  }
}

fn basename_str(path: &str, ext: Option<&str>) -> String {
  let trimmed = path.trim_end_matches('/');
  let base = trimmed.rsplit('/').next().unwrap_or(trimmed);
  match ext {
    Some(e) if base.len() > e.len() && base.ends_with(e) => base[..base.len() - e.len()].to_string(),
    _ => base.to_string(),
  }
}

fn extname_str(path: &str) -> String {
  let base = basename_str(path, None);
  match base.rfind('.') {
    // A leading dot (`.gitignore`) is not an extension.
    Some(i) if i > 0 => base[i..].to_string(),
    _ => String::new(),
  }
}

fn resolve_segments(cwd: &str, segments: &[String]) -> String {
  let mut acc = cwd.to_string();
  for seg in segments {
    if seg.is_empty() {
      continue;
    }
    if seg.starts_with('/') {
      acc.clone_from(seg);
    } else {
      acc = format!("{acc}/{seg}");
    }
  }
  let n = normalize_str(&acc);
  // `resolve` never returns a trailing slash (except root).
  if n.len() > 1 {
    n.trim_end_matches('/').to_string()
  } else {
    n
  }
}

fn relative_str(from: &str, to: &str) -> String {
  let f = normalize_str(from);
  let t = normalize_str(to);
  let fp: Vec<&str> = f.split('/').filter(|s| !s.is_empty()).collect();
  let tp: Vec<&str> = t.split('/').filter(|s| !s.is_empty()).collect();
  let common = fp.iter().zip(tp.iter()).take_while(|(a, b)| a == b).count();
  let mut out: Vec<&str> = vec![".."; fp.len() - common];
  out.extend(&tp[common..]);
  out.join("/")
}

/// The current working directory the JS surface reports: the sandbox
/// root via the `process` shim, falling back to `/`.
fn js_cwd(ctx: &Ctx<'_>) -> String {
  let cwd: rquickjs::Result<String> = (|| {
    let process: Object<'_> = ctx.globals().get("process")?;
    let cwd_fn: rquickjs::Function<'_> = process.get("cwd")?;
    cwd_fn.call(())
  })();
  cwd.unwrap_or_else(|_| "/".to_string())
}

/// Build the `path` module object (fresh per call; only built once per
/// session by the module loader).
pub fn path_object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
  let o = Object::new(ctx.clone())?;
  o.set("sep", "/")?;
  o.set("delimiter", ":")?;
  o.set("join", Func::from(|segs: Rest<String>| join_segments(&segs.0)))?;
  o.set(
    "resolve",
    Func::from(|ctx: Ctx<'_>, segs: Rest<String>| -> String { resolve_segments(&js_cwd(&ctx), &segs.0) }),
  )?;
  o.set("normalize", Func::from(|p: String| normalize_str(&p)))?;
  o.set("dirname", Func::from(|p: String| dirname_str(&p)))?;
  o.set(
    "basename",
    Func::from(|p: String, ext: Opt<String>| basename_str(&p, ext.0.as_deref())),
  )?;
  o.set("extname", Func::from(|p: String| extname_str(&p)))?;
  o.set(
    "relative",
    Func::from(|ctx: Ctx<'_>, from: String, to: String| -> String {
      let cwd = js_cwd(&ctx);
      relative_str(&resolve_segments(&cwd, &[from]), &resolve_segments(&cwd, &[to]))
    }),
  )?;
  o.set("isAbsolute", Func::from(|p: String| p.starts_with('/')))?;
  Ok(o)
}
