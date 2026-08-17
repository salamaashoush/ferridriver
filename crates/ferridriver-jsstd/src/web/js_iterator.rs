//! The one `{ value, done }` iterator protocol used by every
//! web-platform collection class (`Headers`, `URLSearchParams`,
//! `FormData`).
//!
//! WHATWG iteration is LIVE: mutations made during a loop are observed,
//! so `for (const [k] of params) params.delete(k)` behaves as it does in
//! Node. The iterator therefore holds no snapshot — it re-reads its
//! parent on every `next()`.
//!
//! The parent is carried as a property ON the iterator object, which the
//! JS GC traces, and re-read per call. The native `next` closure captures
//! nothing from the JS heap, per the GC-cycle discipline: a closure that
//! captured the parent `Class` would be invisible to the collector and
//! could strand the runtime at teardown.

use rquickjs::atom::PredefinedAtom;
use rquickjs::function::{Func, This};
use rquickjs::{Class, Ctx, Object, Value, class::JsClass};

/// Yield the entry at `index`, or `None` once the collection is
/// exhausted. Called with the parent freshly borrowed, so it always sees
/// current state.
pub type Project<'js, T> = fn(&Ctx<'js>, &Class<'js, T>, usize) -> rquickjs::Result<Option<Value<'js>>>;

/// Build a live iterator over `parent`, projecting each position through
/// `project`. The result is itself iterable (`[Symbol.iterator]` returns
/// `this`), so it works with `for..of`, spread and `Array.from`.
pub fn live_iterator<'js, T>(
  ctx: &Ctx<'js>,
  parent: Class<'js, T>,
  project: Project<'js, T>,
) -> rquickjs::Result<Object<'js>>
where
  T: JsClass<'js> + 'js,
{
  let it = Object::new(ctx.clone())?;
  it.set("position", 0usize)?;
  it.set("target", parent)?;
  it.set(
    PredefinedAtom::SymbolIterator,
    Func::from(|it: This<Object<'js>>| -> rquickjs::Result<Object<'js>> { Ok(it.0) }),
  )?;
  it.set(
    PredefinedAtom::Next,
    Func::from(
      move |ctx: Ctx<'js>, it: This<Object<'js>>| -> rquickjs::Result<Object<'js>> {
        let position = it.get::<_, usize>("position")?;
        let parent: Class<'js, T> = it.get("target")?;
        let res = Object::new(ctx.clone())?;
        match project(&ctx, &parent, position)? {
          None => {
            res.set(PredefinedAtom::Value, Value::new_undefined(ctx))?;
            res.set(PredefinedAtom::Done, true)?;
          },
          Some(value) => {
            res.set(PredefinedAtom::Value, value)?;
            res.set(PredefinedAtom::Done, false)?;
            it.set("position", position + 1)?;
          },
        }
        Ok(res)
      },
    ),
  )?;
  Ok(it)
}
