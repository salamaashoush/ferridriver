//! Where in the user's source an API call was written.
//!
//! Core opens an action span several awaits after the JS call that asked
//! for it: an `async fn` binding body first runs when the VM executor
//! polls it, by which time the caller's JS frame is gone. So the position
//! has to be taken at the boundary, while the stack is still live — which
//! is what [`CallSite`] does. It is a parameter type rather than a
//! statement in each body because rquickjs converts parameters
//! synchronously, on the calling stack, before the body exists.
//!
//! A binding takes one and scopes its body with it:
//!
//! ```ignore
//! pub async fn click<'js>(&self, ctx: Ctx<'js>, site: CallSite, opts: Opt<Value<'js>>) -> Result<()> {
//!   site.scope(async move { ... }).await
//! }
//! ```
//!
//! The scope is a task-local rather than a slot on the VM because
//! `Promise.all([a.click(), b.click()])` has two call sites in flight at
//! once, and a slot would report the second for both.
//!
//! Positions arrive as coordinates in the bundle QuickJS actually ran, so
//! they are mapped back through that bundle's source map before anyone
//! sees them: `--debug`'s `pauseAt` matches what the user typed, and the
//! trace viewer's Source tab needs a file that exists on disk.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use ferridriver::trace::{CallOrigin, StackFrame};
use rquickjs::function::{FromParam, ParamRequirement, ParamsAccessor};
use rquickjs::{Ctx, JsLifetime};

use crate::bundle::SourceMapper;

/// The position an API call was written at, captured when the call
/// crossed from JS into Rust.
///
/// Empty unless something is going to read it ([`call_origins_wanted`]),
/// because capturing costs a stack walk per call.
///
/// [`call_origins_wanted`]: ferridriver::trace::call_origins_wanted
pub struct CallSite(CallOrigin);

impl CallSite {
  /// No call site — for a caller reaching a binding method from Rust
  /// rather than from JS, where there is no user source position to
  /// report.
  #[must_use]
  pub fn none() -> Self {
    Self(CallOrigin::default())
  }

  /// Run `fut` attributed to this call site.
  ///
  /// An empty site leaves whatever scope is already in effect alone: one
  /// binding method delegating to another passes [`CallSite::none`], and
  /// the inner call belongs to the outer one's call site, not to nothing.
  ///
  /// Returns the future rather than being an `async fn`, and picks between
  /// the two paths with an `Either` rather than a branch inside one: both
  /// would otherwise hold `fut` in more than one state of the same state
  /// machine, and every browser action would carry two copies of itself.
  pub fn scope<F: std::future::Future>(self, fut: F) -> impl std::future::Future<Output = F::Output> {
    if self.0.location.is_none() && self.0.script.is_none() {
      futures::future::Either::Left(fut)
    } else {
      futures::future::Either::Right(ferridriver::trace::with_call_origin(self.0, fut))
    }
  }

  /// The captured origin, for a caller that has to build the future
  /// separately from awaiting it.
  #[must_use]
  pub fn origin(self) -> CallOrigin {
    self.0
  }
}

impl<'js> FromParam<'js> for CallSite {
  // Consumes no argument: it reads the caller, not what the caller passed.
  fn param_requirement() -> ParamRequirement {
    ParamRequirement::none()
  }

  fn from_param<'a>(params: &mut ParamsAccessor<'a, 'js>) -> rquickjs::Result<Self> {
    Ok(Self(capture(params.ctx())))
  }
}

/// The whole calling stack, in bundled coordinates, captured at the
/// boundary.
///
/// A parameter type for the same reason [`CallSite`] is one, and it
/// matters more here: an `Async` binding body first runs when the VM
/// executor polls it, by which time the frames a boxed `test.step` has
/// to name are gone. Taking it as a parameter runs the capture
/// synchronously, on the calling stack.
pub struct CallFrames(pub Vec<(String, u32, u32)>);

impl<'js> FromParam<'js> for CallFrames {
  fn param_requirement() -> ParamRequirement {
    ParamRequirement::none()
  }

  fn from_param<'a>(params: &mut ParamsAccessor<'a, 'js>) -> rquickjs::Result<Self> {
    Ok(Self(capture_frames(params.ctx())))
  }
}

/// Capture the calling JS frame, mapped back to the original source.
#[must_use]
pub fn capture(ctx: &Ctx<'_>) -> CallOrigin {
  if !ferridriver::trace::call_origins_wanted() {
    return CallOrigin::default();
  }
  CallOrigin {
    location: capture_frame(ctx).and_then(|(file, line, column)| remap(ctx, &file, line, column)),
    script: ctx.userdata::<ScriptIdUd>().map(|ud| Arc::clone(&ud.0)),
  }
}

// ── Bundles registered for remapping ───────────────────────────────────

/// Source maps for the bundles loaded into this VM. Single-threaded VM ⇒
/// `RefCell`, never `Arc`/`Mutex`.
struct SourceMapsUd(RefCell<Vec<SourceMapper>>);

// SAFETY: holds only `'static` data (owned `Arc`s), so re-stating the
// unused `'js` lifetime is sound — same rationale as `TestRegistryUserData`.
#[allow(unsafe_code)]
unsafe impl JsLifetime<'_> for SourceMapsUd {
  type Changed<'to> = SourceMapsUd;
}

/// Identity of the script running in this VM, for [`CallOrigin::script`].
pub(crate) struct ScriptIdUd(pub(crate) Arc<str>);

// SAFETY: holds only `'static` data.
#[allow(unsafe_code)]
unsafe impl JsLifetime<'_> for ScriptIdUd {
  type Changed<'to> = ScriptIdUd;
}

/// Record a bundle's source map so call sites taken while it runs report
/// the file the user wrote.
///
/// Called wherever a bundle is loaded into a VM. Loading the same bundle
/// twice is not an error — a session re-running a module keeps one entry.
pub fn register_bundle(ctx: &Ctx<'_>, mapper: SourceMapper) {
  if ctx.userdata::<SourceMapsUd>().is_none() {
    let _ = ctx.store_userdata(SourceMapsUd(RefCell::new(Vec::new())));
  }
  let Some(ud) = ctx.userdata::<SourceMapsUd>() else {
    return;
  };
  let mut maps = ud.0.borrow_mut();
  if maps.iter().any(|m| m.module_name == mapper.module_name) {
    return;
  }
  maps.push(mapper);
}

/// Install the script identity a gate uses to recognise its own calls.
pub fn set_script_id(ctx: &Ctx<'_>, id: &str) {
  let _ = ctx.store_userdata(ScriptIdUd(Arc::from(id)));
}

/// Translate a bundled `line:col` back to the original source through the
/// map of the bundle the frame names.
fn remap(ctx: &Ctx<'_>, file: &str, line: u32, column: u32) -> Option<StackFrame> {
  let maps = ctx.userdata::<SourceMapsUd>()?;
  let maps = maps.0.borrow();
  // The frame names the module QuickJS ran, which is the bundle's own
  // module name. A VM with exactly one bundle skips the match: a `run`
  // labels its module after the entry file, and matching the label
  // against itself buys nothing.
  let mapper = match maps.as_slice() {
    [only] => Some(only),
    many => many.iter().find(|m| m.module_name == file),
  }?;
  let (src, src_line, src_col) = mapper.remap(line, column)?;
  Some(StackFrame {
    file: absolute(&src),
    line: src_line,
    column: src_col,
  })
}

/// Source-map sources are relative to the bundle's virtual location; the
/// trace viewer's Source tab reads the file off disk and `pauseAt` is
/// matched against it, so both want the real path.
fn absolute(source: &str) -> String {
  static CWD: OnceLock<Option<PathBuf>> = OnceLock::new();
  match CWD.get_or_init(|| std::env::current_dir().ok()) {
    Some(cwd) => crate::bundle::resolve_source(cwd, source)
      .to_string_lossy()
      .into_owned(),
    None => source.to_string(),
  }
}

// ── Stack capture ──────────────────────────────────────────────────────

/// `file`, `line`, `col` of the innermost JS frame in a fresh stack
/// trace — the caller's own position, in bundled coordinates.
///
/// Synthetic frames (`<eval>`, `native`) are skipped: the capture itself
/// runs through `ctx.eval`, whose frame sits below the native binding
/// frame, and the caller's frame is the first that names a module.
pub(crate) fn capture_frame(ctx: &Ctx<'_>) -> Option<(String, u32, u32)> {
  capture_frames(ctx).into_iter().next()
}

/// Every JS frame of a fresh stack trace, innermost first.
///
/// More than the innermost because `test.step(…, { box: true })` names
/// the frame ABOVE the call site: the line that called the function the
/// step is written in.
pub(crate) fn capture_frames(ctx: &Ctx<'_>) -> Vec<(String, u32, u32)> {
  let Ok(stack) = ctx.eval::<String, _>("new Error().stack") else {
    return Vec::new();
  };
  parse_js_frames(&stack)
}

pub(crate) fn parse_js_frames(stack: &str) -> Vec<(String, u32, u32)> {
  use std::sync::OnceLock;

  use regex::Regex;
  static RE: OnceLock<Option<Regex>> = OnceLock::new();
  let Some(re) = RE.get_or_init(|| Regex::new(r"([^\s()]+):(\d+):(\d+)").ok()).as_ref() else {
    return Vec::new();
  };
  let mut frames = Vec::new();
  for line in stack.lines() {
    let Some(caps) = re.captures(line) else { continue };
    let file = &caps[1];
    let is_module = Path::new(file)
      .extension()
      .is_some_and(|e| ["js", "mjs", "cjs", "ts"].iter().any(|x| e.eq_ignore_ascii_case(x)));
    if !is_module {
      continue;
    }
    if let (Ok(l), Ok(c)) = (caps[2].parse::<u32>(), caps[3].parse::<u32>()) {
      frames.push((file.to_string(), l, c));
    }
  }
  frames
}

#[cfg(test)]
mod tests {
  use super::parse_js_frames;

  #[test]
  fn skips_the_capture_frame_and_reads_the_caller() {
    // The `eval_script` frame is the capture itself and `native` is the
    // binding it was called from; the caller is the first module frame.
    let stack = "Error\n    at eval_script:1:4\n    at register (native)\n    at ferridriver-tests.js:42:7\n    at ferridriver-tests.js:1:1";
    let (file, line, col) = parse_js_frames(stack).into_iter().next().expect("a module frame");
    assert_eq!((file.as_str(), line, col), ("ferridriver-tests.js", 42, 7));
  }

  #[test]
  fn a_stack_with_no_module_frame_yields_nothing() {
    assert!(parse_js_frames("    at native\n").is_empty());
  }

  #[test]
  fn every_module_frame_is_kept_for_a_boxed_step() {
    let stack = "Error\n    at eval_script:1:4\n    at step (native)\n    at helpers.ts:9:5\n    at spec.ts:15:3\n    at spec.ts:40:1";
    let frames = parse_js_frames(stack);
    assert_eq!(
      frames.iter().map(|(f, l, _)| (f.as_str(), *l)).collect::<Vec<_>>(),
      vec![("helpers.ts", 9), ("spec.ts", 15), ("spec.ts", 40)]
    );
  }
}
