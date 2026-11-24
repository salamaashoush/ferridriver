//! `HeapSnapshotJs`: `QuickJS` binding for a captured V8 heap.
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

#[derive(Trace, rquickjs::JsLifetime)]
#[rquickjs::class(rename = "HeapSnapshot")]
pub struct HeapSnapshotJs {
  #[qjs(skip_trace)]
  inner: std::sync::Arc<ferridriver::heap::HeapSnapshot>,
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
  #[must_use]
  pub fn bytes(&self) -> Vec<u8> {
    self.inner.as_json().as_bytes().to_vec()
  }

  /// How the heap divides between V8 and what it does not own.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when the result cannot be converted to a JS value.
  #[qjs(rename = "statistics")]
  pub fn statistics<'js>(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: Ctx<'js>,
  ) -> rquickjs::Result<Value<'js>> {
    serde_to_js(&ctx, &self.inner.statistics())
  }

  /// Every byte the snapshot accounts for.
  #[qjs(rename = "totalSize")]
  #[must_use]
  pub fn total_size(&self) -> f64 {
    crate::bindings::convert::u64_to_f64(self.inner.total_size())
  }

  /// How many nodes the graph holds.
  #[qjs(rename = "nodeCount")]
  #[must_use]
  pub fn node_count(&self) -> f64 {
    crate::bindings::convert::usize_to_f64(self.inner.node_count())
  }

  /// Every class, heaviest first.
  ///
  /// Options narrow it to what one filter keeps:
  /// `{ filterName?, objectId? }`, where `filterName` is one of
  /// `objectsRetainedByContexts`, `objectsRetainedByDetachedDomNodes`,
  /// `objectsRetainedByConsole`, `objectsRetainedByEventHandlers`,
  /// `sharedNativeContext`, `noNativeContext` or
  /// `attributedToNativeContext`, and `objectId` names the realm the
  /// last of those attributes to.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` for a malformed options bag, or the core error for an unknown `filterName`
  /// or a filter the snapshot cannot apply.
  #[qjs(rename = "classes")]
  pub fn classes<'js>(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let filter = parse_filter(&ctx, options)?;
    let classes = self.inner.classes_with_filter(filter).into_js_with(&ctx)?;
    serde_to_js(&ctx, &classes)
  }

  /// Every object of one class, by the `classKey` from `classes()`.
  ///
  /// Takes the same options, and the key has to have come from
  /// `classes()` under the SAME filter: a filter changes which classes
  /// there are.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` for a malformed options bag, or the core error for an unknown `filterName`,
  /// a filter the snapshot cannot apply, or a `classKey` it does not hold.
  #[qjs(rename = "classObjects")]
  pub fn class_objects<'js>(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: Ctx<'js>,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs extracts JS arguments by value; a `&T` parameter would mean a borrowed class instance"
    )]
    class_key: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let filter = parse_filter(&ctx, options)?;
    let objects = self
      .inner
      .class_objects_with_filter(&class_key, filter)
      .into_js_with(&ctx)?;
    serde_to_js(&ctx, &objects)
  }

  /// Every JavaScript realm, and how much of the heap each one owns.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when the result cannot be converted to a JS value.
  #[qjs(rename = "nativeContexts")]
  pub fn native_contexts<'js>(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: Ctx<'js>,
  ) -> rquickjs::Result<Value<'js>> {
    serde_to_js(&ctx, &self.inner.native_contexts())
  }

  /// How much of the heap only a closure's captured scope is holding.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when the result cannot be converted to a JS value.
  #[qjs(rename = "contextSummary")]
  pub fn context_summary<'js>(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: Ctx<'js>,
  ) -> rquickjs::Result<Value<'js>> {
    serde_to_js(&ctx, &self.inner.context_summary())
  }

  /// What one object is.
  ///
  /// # Errors
  ///
  /// Throws the core error when the snapshot holds no node with that id.
  #[qjs(rename = "object")]
  pub fn object<'js>(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: Ctx<'js>,
    node_id: f64,
  ) -> rquickjs::Result<Value<'js>> {
    let info = self.inner.object(node_id_of(node_id)).into_js_with(&ctx)?;
    serde_to_js(&ctx, &info)
  }

  /// What this object points at.
  ///
  /// # Errors
  ///
  /// Throws the core error when the snapshot holds no node with that id.
  #[qjs(rename = "edges")]
  pub fn edges<'js>(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: Ctx<'js>,
    node_id: f64,
  ) -> rquickjs::Result<Value<'js>> {
    let edges = self.inner.edges(node_id_of(node_id)).into_js_with(&ctx)?;
    serde_to_js(&ctx, &edges)
  }

  /// What points at this object; each answer names the retainer.
  ///
  /// # Errors
  ///
  /// Throws the core error when the snapshot holds no node with that id.
  #[qjs(rename = "retainers")]
  pub fn retainers<'js>(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: Ctx<'js>,
    node_id: f64,
  ) -> rquickjs::Result<Value<'js>> {
    let retainers = self.inner.retainers(node_id_of(node_id)).into_js_with(&ctx)?;
    serde_to_js(&ctx, &retainers)
  }

  /// Every route from this object back to a GC root. Options are
  /// `{ maxDepth?, maxNodes?, maxSiblings? }`, defaulting to 30, 5000
  /// and 100 -- the limits `get_heapsnapshot_retaining_paths` sends.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` for a malformed options bag, or the core error when the snapshot holds no
  /// node with that id.
  #[qjs(rename = "retainingPaths")]
  pub fn retaining_paths<'js>(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
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
  ///
  /// # Errors
  ///
  /// Throws the core error when the snapshot holds no node with that id.
  #[qjs(rename = "dominators")]
  pub fn dominators<'js>(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: Ctx<'js>,
    node_id: f64,
  ) -> rquickjs::Result<Value<'js>> {
    let chain = self.inner.dominators(node_id_of(node_id)).into_js_with(&ctx)?;
    serde_to_js(&ctx, &chain)
  }

  /// Strings the page holds more than one copy of, heaviest first.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when the result cannot be converted to a JS value.
  #[qjs(rename = "duplicateStrings")]
  pub fn duplicate_strings<'js>(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: Ctx<'js>,
  ) -> rquickjs::Result<Value<'js>> {
    serde_to_js(&ctx, &self.inner.duplicate_strings())
  }

  /// Find objects by what they look like: `{ className?, propertyName?,
  /// nodeType?, minSelfSize?, maxSelfSize?, minRetainedSize?,
  /// maxRetainedSize?, isDetached?, sortBy? }`. `className` and
  /// `propertyName` are regular expressions, matched case-insensitively.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` for a malformed query, or the core error for a pattern that does not
  /// compile.
  #[qjs(rename = "query")]
  pub fn query<'js>(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let parsed: ferridriver::heap::ObjectQuery = match options.into_inner() {
      Some(value) if !value.is_undefined() && !value.is_null() => serde_from_js(&ctx, value)?,
      _ => ferridriver::heap::ObjectQuery::default(),
    };
    let found = self.inner.query(&parsed).into_js_with(&ctx)?;
    serde_to_js(&ctx, &found)
  }

  /// Every class that gained or lost an object since an earlier
  /// snapshot of the same page, by how much it grew.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when the result cannot be converted to a JS value.
  #[qjs(rename = "diffSince")]
  pub fn diff_since<'js>(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: Ctx<'js>,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs extracts JS arguments by value; a `&T` parameter would mean a borrowed class instance"
    )]
    base: rquickjs::class::Class<'js, Self>,
  ) -> rquickjs::Result<Value<'js>> {
    let base = std::sync::Arc::clone(&base.borrow().inner);
    serde_to_js(&ctx, &self.inner.diff_since(&base))
  }
}

/// `{ filterName?, objectId? }` into the typed filter, through the
/// core's own parser so this and the NAPI binding cannot disagree about
/// an unknown name.
fn parse_filter<'js>(ctx: &Ctx<'js>, options: Opt<Value<'js>>) -> rquickjs::Result<ferridriver::heap::NodeFilter> {
  let parsed: JsNodeFilter = match options.into_inner() {
    Some(value) if !value.is_undefined() && !value.is_null() => serde_from_js(ctx, value)?,
    _ => JsNodeFilter::default(),
  };
  ferridriver::heap::NodeFilter::parse(parsed.filter_name.as_deref(), parsed.object_id.map(node_id_of)).map_err(|e| {
    crate::bindings::convert::ferri_throw(ctx, &ferridriver::FerriError::invalid_argument("filter", e.to_string()))
  })
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct JsNodeFilter {
  filter_name: Option<String>,
  object_id: Option<f64>,
}

/// JS hands numbers as `f64`; an object id is a positive integer well
/// inside what one holds exactly.
fn node_id_of(node_id: f64) -> u64 {
  if node_id.is_finite() && node_id >= 0.0 {
    crate::bindings::convert::f64_to_u64(node_id)
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
