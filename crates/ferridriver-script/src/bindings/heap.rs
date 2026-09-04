//! `HeapSnapshotJs`: QuickJS binding for a captured V8 heap.
//!
//! Returned by `page.takeHeapSnapshot()`. A thin delegation to
//! [`ferridriver::heap::HeapSnapshot`]; every question is answered in
//! Rust, and every result crosses as its serde shape rather than being
//! rebuilt here.
//!
//! Chromium-only, because the format is V8's. The other backends throw
//! the typed `Unsupported` the core raises.

use rquickjs::function::Opt;
use rquickjs::{Ctx, Value, class::Trace};

use crate::bindings::convert::{FerriResultCtxExt, serde_from_js, serde_to_js};

#[derive(Trace)]
#[rquickjs::class(rename = "HeapSnapshot")]
pub struct HeapSnapshotJs {
  #[qjs(skip_trace)]
  inner: std::sync::Arc<ferridriver::heap::HeapSnapshot>,
}

// SAFETY: holds only a `'static` `Arc`, so re-stating the unused `'js`
// lifetime is sound -- the same rationale as every other handle here.
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for HeapSnapshotJs {
  type Changed<'to> = HeapSnapshotJs;
}

impl HeapSnapshotJs {
  #[must_use]
  pub fn new(inner: ferridriver::heap::HeapSnapshot) -> Self {
    Self {
      inner: std::sync::Arc::new(inner),
    }
  }
}

#[rquickjs::methods]
impl HeapSnapshotJs {
  /// The snapshot as it would be written to a `.heapsnapshot` file.
  /// Pair with `await artifacts.writeBytes('heap.heapsnapshot', bytes)`
  /// to open it in the `DevTools` Memory panel.
  #[qjs(rename = "bytes")]
  pub fn bytes(&self) -> Vec<u8> {
    self.inner.as_json().as_bytes().to_vec()
  }

  /// How the heap divides between V8 and what it does not own.
  #[qjs(rename = "statistics")]
  pub fn statistics<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    serde_to_js(&ctx, &self.inner.statistics())
  }

  /// Every byte the snapshot accounts for.
  #[qjs(rename = "totalSize")]
  pub fn total_size(&self) -> f64 {
    self.inner.total_size() as f64
  }

  /// How many nodes the graph holds.
  #[qjs(rename = "nodeCount")]
  pub fn node_count(&self) -> f64 {
    self.inner.node_count() as f64
  }

  /// Every class, heaviest first.
  #[qjs(rename = "classes")]
  pub fn classes<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    serde_to_js(&ctx, &self.inner.classes())
  }

  /// Every object of one class, by the `classKey` from `classes()`.
  #[qjs(rename = "classObjects")]
  pub fn class_objects<'js>(&self, ctx: Ctx<'js>, class_key: String) -> rquickjs::Result<Value<'js>> {
    let objects = self.inner.class_objects(&class_key).into_js_with(&ctx)?;
    serde_to_js(&ctx, &objects)
  }

  /// What one object is.
  #[qjs(rename = "object")]
  pub fn object<'js>(&self, ctx: Ctx<'js>, node_id: f64) -> rquickjs::Result<Value<'js>> {
    let info = self.inner.object(node_id_of(node_id)).into_js_with(&ctx)?;
    serde_to_js(&ctx, &info)
  }

  /// What this object points at.
  #[qjs(rename = "edges")]
  pub fn edges<'js>(&self, ctx: Ctx<'js>, node_id: f64) -> rquickjs::Result<Value<'js>> {
    let edges = self.inner.edges(node_id_of(node_id)).into_js_with(&ctx)?;
    serde_to_js(&ctx, &edges)
  }

  /// What points at this object; each answer names the retainer.
  #[qjs(rename = "retainers")]
  pub fn retainers<'js>(&self, ctx: Ctx<'js>, node_id: f64) -> rquickjs::Result<Value<'js>> {
    let retainers = self.inner.retainers(node_id_of(node_id)).into_js_with(&ctx)?;
    serde_to_js(&ctx, &retainers)
  }

  /// Every route from this object back to a GC root. Options are
  /// `{ maxDepth?, maxNodes?, maxSiblings? }`, defaulting to 30, 5000
  /// and 100 -- the limits `get_heapsnapshot_retaining_paths` sends.
  #[qjs(rename = "retainingPaths")]
  pub fn retaining_paths<'js>(
    &self,
    ctx: Ctx<'js>,
    node_id: f64,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let parsed: JsPathLimits = match options.into_inner() {
      Some(value) if !value.is_undefined() && !value.is_null() => serde_from_js(&ctx, value)?,
      _ => JsPathLimits::default(),
    };
    let paths = self
      .inner
      .retaining_paths(node_id_of(node_id), Some(parsed.into()))
      .into_js_with(&ctx)?;
    serde_to_js(&ctx, &paths)
  }

  /// What would have to let go for this object to be freed, one step at
  /// a time: itself first and the root last.
  #[qjs(rename = "dominators")]
  pub fn dominators<'js>(&self, ctx: Ctx<'js>, node_id: f64) -> rquickjs::Result<Value<'js>> {
    let chain = self.inner.dominators(node_id_of(node_id)).into_js_with(&ctx)?;
    serde_to_js(&ctx, &chain)
  }

  /// Strings the page holds more than one copy of, heaviest first.
  #[qjs(rename = "duplicateStrings")]
  pub fn duplicate_strings<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    serde_to_js(&ctx, &self.inner.duplicate_strings())
  }

  /// Find objects by what they look like: `{ className?, propertyName?,
  /// nodeType?, minSelfSize?, maxSelfSize?, minRetainedSize?,
  /// maxRetainedSize?, isDetached?, sortBy? }`. `className` and
  /// `propertyName` are regular expressions, matched case-insensitively.
  #[qjs(rename = "query")]
  pub fn query<'js>(&self, ctx: Ctx<'js>, options: Opt<Value<'js>>) -> rquickjs::Result<Value<'js>> {
    let parsed: ferridriver::heap::ObjectQuery = match options.into_inner() {
      Some(value) if !value.is_undefined() && !value.is_null() => serde_from_js(&ctx, value)?,
      _ => ferridriver::heap::ObjectQuery::default(),
    };
    let found = self.inner.query(&parsed).into_js_with(&ctx)?;
    serde_to_js(&ctx, &found)
  }

  /// Every class that gained or lost an object since an earlier
  /// snapshot of the same page, by how much it grew.
  #[qjs(rename = "diffSince")]
  pub fn diff_since<'js>(
    &self,
    ctx: Ctx<'js>,
    base: rquickjs::class::Class<'js, Self>,
  ) -> rquickjs::Result<Value<'js>> {
    let base = base.borrow().inner.clone();
    serde_to_js(&ctx, &self.inner.diff_since(&base))
  }
}

/// JS hands numbers as `f64`; an object id is a positive integer well
/// inside what one holds exactly.
fn node_id_of(node_id: f64) -> u64 {
  if node_id.is_finite() && node_id >= 0.0 {
    node_id as u64
  } else {
    0
  }
}

/// The names are upstream's, so a reader coming from
/// `get_heapsnapshot_retaining_paths` writes the same option bag.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct JsPathLimits {
  #[serde(rename = "maxDepth")]
  depth: Option<u32>,
  #[serde(rename = "maxNodes")]
  nodes: Option<u32>,
  #[serde(rename = "maxSiblings")]
  siblings: Option<u32>,
}

impl From<JsPathLimits> for ferridriver::heap::PathLimits {
  fn from(limits: JsPathLimits) -> Self {
    let defaults = Self::default();
    Self {
      depth: limits.depth.map_or(defaults.depth, |v| v as usize),
      nodes: limits.nodes.map_or(defaults.nodes, |v| v as usize),
      siblings: limits.siblings.map_or(defaults.siblings, |v| v as usize),
    }
  }
}
