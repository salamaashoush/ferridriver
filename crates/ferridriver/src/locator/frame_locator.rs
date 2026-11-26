//! `FrameLocator`, Playwright-parity selector builders, and cross-iframe `ariaSnapshot` stitching.

use std::fmt::Write as _;
use std::sync::Arc;

use crate::error::Result;
use crate::options::{RoleOptions, StringOrRegex, TextOptions};
use crate::selectors;

use super::{ANY_FRAME, Locator};

// ── FrameLocator ──────────────────────────────────────────────────────────────

/// A selector-builder that produces parent-frame [`Locator`]s targeting
/// content inside an `<iframe>`. Mirrors Playwright's `FrameLocator`
/// exactly:
///
/// `/tmp/playwright/packages/playwright-core/src/client/locator.ts::FrameLocatorImpl`
///
/// Holds the parent [`crate::Frame`] (the one whose document contains the
/// `<iframe>` element) and the iframe's CSS-selector chain. Every
/// builder method composes a Locator selector with
/// `>> internal:control=enter-frame >>` so the iframe traversal is
/// performed by the selector engine at action time — not eagerly at
/// construction. The resulting `Locator` is the same `Locator` type
/// used everywhere else (no separate iframe-aware locator).
///
/// All methods are synchronous; `internal:control=enter-frame` is the
/// engine-side directive that switches root from the iframe element to
/// its `contentDocument` when a subsequent selector part runs.
#[derive(Clone)]
pub struct FrameLocator {
  /// Parent frame whose document contains the `<iframe>` element.
  /// For top-level `page.frame_locator(sel)` this is the main frame;
  /// nested `frame_locator.frame_locator(sel)` keeps the same parent
  /// frame and just appends to the selector chain.
  frame: crate::frame::Frame,
  /// CSS-selector chain ending at the `<iframe>` element. Composed with
  /// `>> internal:control=enter-frame >>` whenever we step further in.
  frame_selector: String,
}

impl FrameLocator {
  /// Construct a `FrameLocator` for an `<iframe>` matched by
  /// `iframe_selector` inside `parent_frame`'s document. Sync.
  #[must_use]
  pub fn for_iframe_in(parent_frame: crate::frame::Frame, iframe_selector: String) -> Self {
    Self {
      frame: parent_frame,
      frame_selector: iframe_selector,
    }
  }

  fn enter(&self, selector: &str) -> String {
    if self.frame_selector == ANY_FRAME {
      return format!("{ANY_FRAME} >> {selector}");
    }
    format!("{} >> internal:control=enter-frame >> {selector}", self.frame_selector)
  }

  /// Playwright's `_nthSelector`: picking one of the frames needs the
  /// frames to be named by a selector.
  fn nth_selector(&self, nth: &str) -> Result<String> {
    if self.frame_selector == ANY_FRAME {
      return Err(crate::error::FerriError::InvalidArgument {
        name: "frameLocator".into(),
        reason: "Selecting the nth frame is not allowed on frameLocator().".into(),
      });
    }
    Ok(format!("{} >> nth={nth}", self.frame_selector))
  }

  /// Playwright: `frameLocator.locator(selectorOrLocator: string | Locator,
  /// options?: LocatorOptions): Locator` — sync, bound to the parent
  /// frame with an `enter-frame` selector chain. A locator is entered by
  /// its own selector; one from another page cannot match, which the
  /// selector engine reports when the locator is used. The options-bag
  /// form is [`Self::locator_with`].
  #[must_use]
  pub fn locator(&self, selector_or_locator: impl Into<crate::options::LocatorLike>) -> Locator {
    match selector_or_locator.into() {
      crate::options::LocatorLike::Selector(selector) => self.child(&selector),
      crate::options::LocatorLike::Locator(inner) if Arc::ptr_eq(self.frame.page_arc(), inner.frame.page_arc()) => {
        self.child(&inner.selector)
      },
      crate::options::LocatorLike::Locator(_) => self.child("internal:cross-frame-error=true"),
    }
  }

  /// [`Self::locator`] with Playwright's `LocatorOptions` filter bag.
  #[must_use]
  pub fn locator_with(
    &self,
    selector_or_locator: impl Into<crate::options::LocatorLike>,
    options: &crate::options::FilterOptions,
  ) -> Locator {
    self.locator(selector_or_locator).filter_with(options)
  }

  fn child(&self, selector: &str) -> Locator {
    Locator::new(self.frame.clone(), self.enter(selector))
  }

  /// `getByRole` inside this iframe.
  #[must_use]
  pub fn get_by_role(
    &self,
    role: impl Into<crate::options::Role>,
  ) -> crate::locator_builder::LocatorBuilder<RoleOptions> {
    let fl = self.clone();
    let role = role.into();
    crate::locator_builder::LocatorBuilder::new(move |opts| fl.child(&build_role_selector(role.as_str(), opts)))
  }

  /// `getByText` inside this iframe.
  #[must_use]
  pub fn get_by_text(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.text_like_builder("internal:text", text.into())
  }

  /// `getByTestId` inside this iframe.
  #[must_use]
  pub fn get_by_test_id(&self, test_id: impl Into<StringOrRegex>) -> Locator {
    self.child(&build_testid_selector(&self.frame.test_id_attribute(), &test_id.into()))
  }

  /// `getByLabel` inside this iframe.
  #[must_use]
  pub fn get_by_label(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.text_like_builder("internal:label", text.into())
  }

  /// `getByPlaceholder` inside this iframe.
  #[must_use]
  pub fn get_by_placeholder(
    &self,
    text: impl Into<StringOrRegex>,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.attr_builder("placeholder", text.into())
  }

  /// `getByAltText` inside this iframe.
  #[must_use]
  pub fn get_by_alt_text(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.attr_builder("alt", text.into())
  }

  /// `getByTitle` inside this iframe.
  #[must_use]
  pub fn get_by_title(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.attr_builder("title", text.into())
  }

  fn text_like_builder(
    &self,
    kind: &'static str,
    text: StringOrRegex,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    let fl = self.clone();
    crate::locator_builder::LocatorBuilder::new(move |opts| fl.child(&build_text_like_selector(kind, &text, opts)))
  }

  fn attr_builder(
    &self,
    attr: &'static str,
    text: StringOrRegex,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    let fl = self.clone();
    crate::locator_builder::LocatorBuilder::new(move |opts| fl.child(&build_attr_selector(attr, &text, opts)))
  }

  /// The locator pointing at the `<iframe>` element itself, in the
  /// parent frame's context. Mirrors Playwright's `frameLocator.owner()`.
  #[must_use]
  pub fn owner(&self) -> Locator {
    Locator::new(self.frame.clone(), self.frame_selector.clone())
  }

  /// `frameLocator` for a nested `<iframe>`. Mirrors Playwright's
  /// `frameLocator.frameLocator(selector)` — appends an enter-frame
  /// step plus the next iframe selector.
  #[must_use]
  pub fn frame_locator(&self, selector: &str) -> FrameLocator {
    FrameLocator {
      frame: self.frame.clone(),
      frame_selector: self.enter(selector),
    }
  }

  /// First matching iframe (`nth=0`).
  ///
  /// # Errors
  ///
  /// [`crate::error::FerriError::InvalidArgument`] on `frameLocator()`,
  /// which names no iframe to pick from.
  pub fn first(&self) -> Result<FrameLocator> {
    self.nth_frame("0")
  }

  /// Last matching iframe (`nth=-1`).
  ///
  /// # Errors
  ///
  /// As [`Self::first`].
  pub fn last(&self) -> Result<FrameLocator> {
    self.nth_frame("-1")
  }

  /// Nth matching iframe.
  ///
  /// # Errors
  ///
  /// As [`Self::first`].
  pub fn nth(&self, index: i32) -> Result<FrameLocator> {
    self.nth_frame(&index.to_string())
  }

  fn nth_frame(&self, nth: &str) -> Result<FrameLocator> {
    Ok(FrameLocator {
      frame: self.frame.clone(),
      frame_selector: self.nth_selector(nth)?,
    })
  }
}

impl std::fmt::Debug for FrameLocator {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("FrameLocator")
      .field("frame_selector", &self.frame_selector)
      .field("frame", &self.frame)
      .finish()
  }
}

// ── Selector builders ───────────────────────────────────────────────────────

/// Compute the drag press/release point from an element's bounding rect and
/// an optional `position`. When `position` is `Some`, the point is the
/// element's padding-box top-left offset by `(position.x, position.y)` —
/// matching Playwright's `sourcePosition` / `targetPosition` semantics. When
/// `position` is `None`, the element's center is used.
/// Page-side body for [`Locator::drop`]. Runs with `this` bound to the
/// target element and a JSON-literal `arg` ({payloads, data, modifiers,
/// position}) in scope. Mirrors Playwright's `dom.ts::_drop` page-side
/// closure: builds a `DataTransfer`, adds the `File` objects + `setData`
/// entries, then dispatches `dragenter` / `dragover` / `drop`. Returns a
/// status string the Rust side maps to `Ok`/`Err`.
pub(super) const DROP_BODY: &str = r"
  if (!this.isConnected || this.nodeType !== 1) return 'error:notconnected';
  this.scrollIntoViewIfNeeded ? this.scrollIntoViewIfNeeded() : this.scrollIntoView();
  const r = this.getBoundingClientRect();
  const point = arg.position
    ? { x: r.x + arg.position.x, y: r.y + arg.position.y }
    : { x: r.x + r.width / 2, y: r.y + r.height / 2 };
  const dt = new DataTransfer();
  for (const p of arg.payloads) {
    const bin = atob(p.buffer);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    dt.items.add(new File([bytes], p.name, { type: p.mimeType }));
  }
  for (const entry of arg.data) dt.setData(entry.mimeType, entry.value);
  const makeEvent = (type) => new DragEvent(type, {
    bubbles: true,
    cancelable: true,
    composed: true,
    clientX: point.x,
    clientY: point.y,
    altKey: arg.modifiers.alt,
    ctrlKey: arg.modifiers.ctrl,
    metaKey: arg.modifiers.meta,
    shiftKey: arg.modifiers.shift,
    dataTransfer: dt,
  });
  this.dispatchEvent(makeEvent('dragenter'));
  const over = makeEvent('dragover');
  this.dispatchEvent(over);
  if (!over.defaultPrevented) {
    this.dispatchEvent(makeEvent('dragleave'));
    return 'not-accepted';
  }
  this.dispatchEvent(makeEvent('drop'));
  return 'accepted';
";

/// Playwright's `injectedScript.ts::setInputFiles`, with `this` as the
/// node and `payloads` in scope: the files a script names become the
/// input's `FileList`, and the page hears `input` then `change`.
pub(super) const SET_INPUT_FILES_BODY: &str = "
  if (this.nodeType !== Node.ELEMENT_NODE)
    throw new Error('Node is not of type HTMLElement');
  if (this.nodeName !== 'INPUT')
    throw new Error('Not an <input> element');
  if ((this.getAttribute('type') || '').toLowerCase() !== 'file')
    throw new Error('Not an input[type=file] element');
  const transfer = new DataTransfer();
  for (const file of payloads) {
    const bytes = Uint8Array.from(atob(file.buffer), c => c.charCodeAt(0));
    transfer.items.add(new File([bytes], file.name, { type: file.mimeType }));
  }
  this.files = transfer.files;
  this.dispatchEvent(new Event('input', { bubbles: true, composed: true }));
  this.dispatchEvent(new Event('change', { bubbles: true }));
";

/// Lower files into the page-side `{name, mimeType, buffer(base64)}`
/// records that [`DROP_BODY`] and [`SET_INPUT_FILES_BODY`] rebuild into
/// `File` objects. Disk paths are read into buffers (matching
/// Playwright's co-located server path); in-memory payloads carry their
/// bytes already.
pub(super) fn lower_drop_files(files: Option<crate::options::InputFiles>) -> Result<Vec<serde_json::Value>> {
  use base64::Engine as _;
  match files {
    Some(crate::options::InputFiles::Paths(paths)) => {
      let mut out = Vec::with_capacity(paths.len());
      for p in paths {
        let bytes = std::fs::read(&p)
          .map_err(|e| crate::error::FerriError::Backend(format!("failed to read file {}: {e}", p.display())))?;
        let name = p
          .file_name()
          .map(|n| n.to_string_lossy().into_owned())
          .unwrap_or_default();
        out.push(serde_json::json!({
          "name": name,
          "mimeType": guess_mime_type(&name),
          "buffer": base64::engine::general_purpose::STANDARD.encode(&bytes),
        }));
      }
      Ok(out)
    },
    Some(crate::options::InputFiles::Payloads(payloads)) => Ok(
      payloads
        .into_iter()
        .map(|p| {
          let mime = if p.mime_type.is_empty() {
            "application/octet-stream".to_string()
          } else {
            p.mime_type
          };
          serde_json::json!({
            "name": p.name,
            "mimeType": mime,
            "buffer": base64::engine::general_purpose::STANDARD.encode(&p.buffer),
          })
        })
        .collect(),
    ),
    None => Ok(Vec::new()),
  }
}

/// Best-effort MIME type for a filename based on its extension. Used by
/// [`Locator::drop`] when reading `DropPayload` file paths from disk and by
/// `route.fulfill({ path })`, neither of which is given an explicit MIME
/// type. Falls back to `application/octet-stream` for unknown extensions.
#[must_use]
pub fn guess_mime_type(name: &str) -> &'static str {
  let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
  match ext.as_str() {
    "txt" | "text" => "text/plain",
    "html" | "htm" => "text/html",
    "css" => "text/css",
    "csv" => "text/csv",
    "js" | "mjs" => "text/javascript",
    "json" => "application/json",
    "xml" => "application/xml",
    "pdf" => "application/pdf",
    "png" => "image/png",
    "jpg" | "jpeg" => "image/jpeg",
    "gif" => "image/gif",
    "svg" => "image/svg+xml",
    "webp" => "image/webp",
    "zip" => "application/zip",
    _ => "application/octet-stream",
  }
}

pub(crate) fn build_role_selector(role: &str, opts: &RoleOptions) -> String {
  let mut sel = format!("internal:role={role}");
  if let Some(name) = &opts.name {
    let escaped = escape_for_attribute_selector(name, opts.exact == Some(true));
    let _ = write!(sel, "[name={escaped}]");
  }
  if let Some(description) = &opts.description {
    let escaped = escape_for_attribute_selector(description, opts.exact == Some(true));
    let _ = write!(sel, "[description={escaped}]");
  }
  if let Some(checked) = opts.checked {
    let _ = write!(sel, "[checked={checked}]");
  }
  if let Some(disabled) = opts.disabled {
    let _ = write!(sel, "[disabled={disabled}]");
  }
  if let Some(expanded) = opts.expanded {
    let _ = write!(sel, "[expanded={expanded}]");
  }
  if let Some(level) = opts.level {
    let _ = write!(sel, "[level={level}]");
  }
  if let Some(pressed) = opts.pressed {
    let _ = write!(sel, "[pressed={pressed}]");
  }
  if let Some(selected) = opts.selected {
    let _ = write!(sel, "[selected={selected}]");
  }
  if let Some(include_hidden) = opts.include_hidden {
    let _ = write!(sel, "[include-hidden={include_hidden}]");
  }
  sel
}

/// Build a Playwright-native text-engine selector body. `engine_prefix`
/// is one of `internal:text` / `internal:label`. For `text: String` we
/// emit `"quoted"i` / `"quoted"s`; for `text: Regex` we emit
/// `/source/flags`. Mirrors
/// `packages/isomorphic/stringUtils.ts::escapeForTextSelector` from
/// `/tmp/playwright`.
pub(crate) fn build_text_like_selector(engine_prefix: &str, text: &StringOrRegex, opts: &TextOptions) -> String {
  let body = escape_for_text_selector(text, opts.exact == Some(true));
  format!("{engine_prefix}={body}")
}

/// Build a Playwright-native attribute-engine selector body of the
/// form `internal:attr=[<name>=<escaped>]` for `get_by_alt_text`,
/// `get_by_title`, `get_by_placeholder`. Mirrors
/// `packages/isomorphic/locatorUtils.ts::getByAttributeTextSelector`.
pub(crate) fn build_attr_selector(attr: &str, value: &StringOrRegex, opts: &TextOptions) -> String {
  let escaped = escape_for_attribute_selector(value, opts.exact == Some(true));
  format!("internal:attr=[{attr}={escaped}]")
}

/// Build a `get_by_test_id` selector. Testid matches are always exact
/// per Playwright.
pub(crate) fn build_testid_selector(attr_name: &str, testid: &StringOrRegex) -> String {
  let escaped = escape_for_attribute_selector(testid, true);
  // Playwright's `encodeTestIdAttributeName`
  // (`packages/isomorphic/locatorUtils.ts`): the comma form names more
  // than one attribute, and the list has to survive the attribute-
  // selector parser as ONE name, so it is JSON-quoted.
  if attr_name.contains(',') {
    let quoted = serde_json::Value::String(attr_name.to_string()).to_string();
    return format!("internal:testid=[{quoted}={escaped}]");
  }
  format!("internal:testid=[{attr_name}={escaped}]")
}

/// Port of Playwright's `escapeForTextSelector` from
/// `/tmp/playwright/packages/isomorphic/stringUtils.ts`.
///
/// For a literal string returns `"quoted"i` (substring,
/// case-insensitive) or `"quoted"s` (exact, case-sensitive). For a
/// regex returns the `/source/flags` literal form with `>>` escaped so
/// the selector chain operator doesn't collide. `unicode`/`unicodeSets`
/// regex flags are preserved — the injected engine's `RegExp`
/// construction handles them natively.
pub(super) fn escape_for_text_selector(value: &StringOrRegex, exact: bool) -> String {
  match value {
    StringOrRegex::String(s) => {
      let quoted = serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""));
      format!("{quoted}{}", if exact { "s" } else { "i" })
    },
    StringOrRegex::Regex { source, flags } => escape_regex_for_selector(source, flags),
  }
}

/// Port of Playwright's `escapeForAttributeSelector`.
fn escape_for_attribute_selector(value: &StringOrRegex, exact: bool) -> String {
  match value {
    StringOrRegex::String(s) => {
      let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
      format!("\"{escaped}\"{}", if exact { "s" } else { "i" })
    },
    StringOrRegex::Regex { source, flags } => escape_regex_for_selector(source, flags),
  }
}

/// Port of Playwright's `escapeRegexForSelector`
/// (`injected/isomorphic/stringUtils.ts`). For regexes with `unicode` /
/// `unicodeSets` flags the literal goes out verbatim (identity escapes
/// are not allowed in those modes). Otherwise a quote that is not
/// already escaped gets a backslash, so a regex inside an attribute
/// clause (`internal:role=link[name=/Salama's/]`) does not end the value
/// at its `'`, and `>>` is escaped so the chain operator does not split
/// the literal.
fn escape_regex_for_selector(source: &str, flags: &str) -> String {
  let literal = format!("/{source}/{flags}");
  if flags.contains('u') || flags.contains('v') {
    return literal;
  }
  let mut escaped = String::with_capacity(literal.len() + 4);
  let mut backslashes = 0usize;
  for c in literal.chars() {
    if matches!(c, '"' | '\'' | '`') && backslashes.is_multiple_of(2) {
      escaped.push('\\');
    }
    backslashes = if c == '\\' { backslashes + 1 } else { 0 };
    escaped.push(c);
  }
  escaped.replace(">>", "\\>\\>")
}

/// Produce the Playwright-compatible JSON-quoted form of an inner
/// selector string — matches the output of `JSON.stringify(str)` in JS.
/// Used by [`Locator::filter`] and [`Locator::and`] / [`Locator::or`]
/// when embedding nested selector text in `internal:*` clauses.
///
/// Falls back to a Rust `{:?}` debug form if `serde_json` cannot encode
/// (impossible for valid UTF-8 strings, but kept for defensive symmetry
/// with existing call sites at `and` / `or`).
pub(super) fn json_quote(s: &str) -> String {
  serde_json::to_string(s).unwrap_or_else(|_| format!("{s:?}"))
}

// ── ariaSnapshot cross-iframe stitching ─────────────────────────────────────
//
// Mirrors Playwright's server `ariaSnapshotJSONForFrame`
// (`packages/playwright-core/src/server/page.ts`). The injected
// `ariaSnapshotFrame` builds one frame's tree and reports the `<iframe>`
// nodes it gave refs to (`iframeRefs` / `iframeDepths`); the host resolves
// each into its child browsing context and sets the child's tree as the
// `iframe` node's children.

/// Decoded result of `window.__fd.ariaSnapshotFrame`.
#[derive(serde::Deserialize, Default)]
pub(super) struct AriaRaw {
  #[serde(default)]
  json: Vec<serde_json::Value>,
  /// May contain `null` entries — `mode: 'default'` assigns no refs, so
  /// the renderer pushes `undefined` for un-reffed iframes (filtered out
  /// exactly like Playwright's `ref in iframeDepths` check).
  #[serde(default, rename = "iframeRefs")]
  iframe_refs: Vec<Option<String>>,
  #[serde(default, rename = "iframeDepths")]
  iframe_depths: rustc_hash::FxHashMap<String, i32>,
}

/// Build the `AriaTreeOptions` JSON for the injected call. The ref
/// prefix is NOT one of them: the vendored injected derives it from the
/// per-frame `frameSeq` it was constructed with, and ferridriver injects
/// one script per page, so the host prefixes a child frame's refs while
/// merging instead ([`aria_merge_frame`]).
pub(super) fn aria_opts_json(mode: &str, depth: Option<i32>, boxes: Option<bool>) -> String {
  let mut m = serde_json::Map::new();
  m.insert("mode".into(), serde_json::Value::String(mode.to_string()));
  if let Some(d) = depth {
    m.insert("depth".into(), serde_json::Value::Number(d.into()));
  }
  if boxes == Some(true) {
    m.insert("boxes".into(), serde_json::Value::Bool(true));
  }
  serde_json::Value::Object(m).to_string()
}

pub(super) fn parse_aria_raw(s: &str) -> Result<AriaRaw> {
  if s.trim().is_empty() {
    return Ok(AriaRaw::default());
  }
  serde_json::from_str(s).map_err(|e| crate::error::FerriError::evaluation(format!("ariaSnapshot parse: {e}")))
}

/// `frame`'s tree (already built into `raw`) with every rendered child
/// iframe's tree merged under its `iframe` node, and this frame's refs
/// given `ref_prefix`. Boxed so the frame -> child-frame recursion has a
/// finite future size.
pub(super) fn aria_merge_frame(
  frame: crate::frame::Frame,
  raw: AriaRaw,
  mode: String,
  depth: Option<i32>,
  boxes: Option<bool>,
  ref_prefix: String,
  seq: Arc<std::sync::atomic::AtomicU32>,
) -> futures::future::BoxFuture<'static, Result<Vec<serde_json::Value>>> {
  Box::pin(async move {
    // Playwright: `iframeRefs.filter(ref => ref in iframeDepths)`.
    let rendered: Vec<String> = raw
      .iframe_refs
      .iter()
      .flatten()
      .filter(|r| raw.iframe_depths.contains_key(*r))
      .cloned()
      .collect();
    let mut children = rustc_hash::FxHashMap::with_capacity_and_hasher(rendered.len(), rustc_hash::FxBuildHasher);
    for refv in &rendered {
      let child = aria_child_snapshot(&frame, refv, &mode, depth, boxes, &raw.iframe_depths, &seq).await?;
      children.insert(refv.clone(), child);
    }
    let mut json = raw.json;
    for node in &mut json {
      merge_iframe_children(node, &mut children, &ref_prefix);
    }
    Ok(json)
  })
}

/// Playwright's `mergeIframeChildren`, which also namespaces this frame's
/// refs: ferridriver's injected script is built per page rather than per
/// frame, so the `f<n>` prefix Playwright mints inside the renderer is
/// applied here. A child tree arrives with its own prefix already.
fn merge_iframe_children(
  node: &mut serde_json::Value,
  children: &mut rustc_hash::FxHashMap<String, Vec<serde_json::Value>>,
  ref_prefix: &str,
) {
  let Some(object) = node.as_object_mut() else {
    return;
  };
  let own_ref = object.get("ref").and_then(serde_json::Value::as_str).map(str::to_owned);
  if let Some(own_ref) = &own_ref
    && !ref_prefix.is_empty()
  {
    object.insert(
      "ref".into(),
      serde_json::Value::String(format!("{ref_prefix}{own_ref}")),
    );
  }
  if object.get("role").and_then(serde_json::Value::as_str) == Some("iframe")
    && let Some(own_ref) = own_ref
  {
    if let Some(child) = children.remove(&own_ref).filter(|c| !c.is_empty()) {
      object.insert("children".into(), serde_json::Value::Array(child));
    }
    return;
  }
  if let Some(serde_json::Value::Array(kids)) = object.get_mut("children") {
    for kid in kids {
      merge_iframe_children(kid, children, ref_prefix);
    }
  }
}

/// Render a merged tree as YAML, in `frame`, with the renderer the
/// injected script ships (`renderAriaSnapshotAsYaml`).
pub(super) async fn render_aria_snapshot(frame: &crate::frame::Frame, json: &[serde_json::Value]) -> Result<String> {
  let frame_id = (!frame.is_main_frame()).then(|| frame.id().to_string());
  selectors::ensure_aria_support(frame.page_arc().inner(), frame_id.as_deref()).await?;
  let literal =
    serde_json::to_string(&serde_json::to_string(json).map_err(|e| crate::error::FerriError::Json(e.to_string()))?)
      .map_err(|e| crate::error::FerriError::Json(e.to_string()))?;
  let js = format!("() => window.__fd.renderAriaSnapshot(JSON.parse({literal}))");
  let rendered = frame
    .evaluate(&js, crate::protocol::SerializedArgument::default(), Some(true))
    .await?;
  Ok(rendered.as_str().map(str::to_owned).unwrap_or_default())
}

/// Resolve one child iframe (`refv`) into its content frame, snapshot
/// its `<body>`, and recurse. A missing element or unresolvable content
/// frame yields an empty tree, and the parent keeps its bare `iframe`
/// node, as Playwright's does.
async fn aria_child_snapshot(
  frame: &crate::frame::Frame,
  refv: &str,
  mode: &str,
  depth: Option<i32>,
  boxes: Option<bool>,
  depths: &rustc_hash::FxHashMap<String, i32>,
  seq: &Arc<std::sync::atomic::AtomicU32>,
) -> Result<Vec<serde_json::Value>> {
  // Tag the iframe in JS, then re-resolve it through the normal
  // selector + content-frame path (the same one `frameLocator` uses).
  // Passing a utility-eval JSHandle straight into `content_frame()`
  // breaks on BiDi ("no such handle" — the cross-context handle is not
  // valid for the contentWindow call); a freshly queried element is.
  const ARIA_FRAME_ATTR: &str = "data-fd-aria-ref";
  let ref_json = serde_json::to_string(refv).unwrap_or_else(|_| format!("{refv:?}"));
  let mark_js = format!("() => window.__fd.markIframeByAriaRef({ref_json}, \"{ARIA_FRAME_ATTR}\")");
  let marked = matches!(
    frame
      .evaluate(&mark_js, crate::protocol::SerializedArgument::default(), Some(true))
      .await?,
    crate::protocol::SerializedValue::Bool(true)
  );
  if !marked {
    return Ok(Vec::new());
  }
  let page = frame.page_arc();
  let frame_id: Option<&str> = if frame.is_main_frame() { None } else { Some(frame.id()) };
  let sel = format!("[{ARIA_FRAME_ATTR}=\"{refv}\"]");
  let Ok(iframe_node) = selectors::query_one(page.inner(), &sel, false, frame_id).await else {
    return Ok(Vec::new());
  };
  let iframe_handle = crate::element_handle::ElementHandle::from_any_element(Arc::clone(page), iframe_node).await?;
  let Some(child_frame) = iframe_handle.content_frame().await? else {
    return Ok(Vec::new());
  };

  // Playwright: `childDepth = options.depth ? depth - iframeDepth - 1
  // : undefined` — `0` is falsy there too, so it maps to "unlimited".
  let iframe_depth = depths.get(refv).copied().unwrap_or(0);
  let child_depth = match depth {
    Some(d) if d != 0 => Some(d - iframe_depth - 1),
    _ => None,
  };

  // Unique per-frame ref prefix so `[ref=...]` never collides across
  // frames (Playwright uses `frame.seq`; we just need uniqueness).
  let n = seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
  let prefix = format!("f{n}");
  let copts = aria_opts_json(mode, child_depth, boxes);
  let child_frame_id = (!child_frame.is_main_frame()).then(|| child_frame.id().to_string());
  selectors::ensure_aria_support(child_frame.page_arc().inner(), child_frame_id.as_deref()).await?;
  let body_js = format!(
    "async () => {{
      if (!document.body) {{
        await new Promise(resolve => {{
          const observer = new MutationObserver(() => {{
            if (document.body) {{ observer.disconnect(); resolve(); }}
          }});
          observer.observe(document, {{ childList: true, subtree: true }});
        }});
      }}
      return JSON.stringify(window.__fd.ariaSnapshotFrame(document.body, {copts}));
    }}"
  );
  let raw_s = child_frame
    .evaluate(&body_js, crate::protocol::SerializedArgument::default(), Some(true))
    .await?
    .as_str()
    .map(std::string::ToString::to_string)
    .unwrap_or_default();
  if raw_s.is_empty() {
    return Ok(Vec::new());
  }
  let child_raw = parse_aria_raw(&raw_s)?;
  aria_merge_frame(
    child_frame,
    child_raw,
    mode.to_string(),
    child_depth,
    boxes,
    prefix,
    Arc::clone(seq),
  )
  .await
}

#[cfg(test)]
mod test_id_selector_tests {
  use super::build_testid_selector;

  #[test]
  fn a_test_id_selector_names_its_attribute() {
    assert_eq!(
      build_testid_selector("data-testid", &"alpha".into()),
      r#"internal:testid=[data-testid="alpha"s]"#
    );
    assert_eq!(
      build_testid_selector("data-test-id", &"alpha".into()),
      r#"internal:testid=[data-test-id="alpha"s]"#
    );
    // Playwright's `encodeTestIdAttributeName`: the comma form is ONE
    // name to the attribute-selector parser, so it is JSON-quoted.
    assert_eq!(
      build_testid_selector("data-pw,data-ti", &"alpha".into()),
      r#"internal:testid=["data-pw,data-ti"="alpha"s]"#
    );
  }
}

#[cfg(test)]
mod retry_classify_tests {
  use super::super::is_retryable_action_error;

  #[test]
  fn stale_and_actionability_errors_are_retryable() {
    for msg in [
      // Actionability signals (pre-existing behaviour).
      "error:notvisible",
      "error:notenabled",
      "element is not connected",
      "element is detached",
      // Stale-handle / navigated-away signals (the fix): a resolved
      // node vanished mid-action and must be re-resolved, not surfaced
      // as a hard error.
      "protocol error (CDP): Object id doesn't reference a Node",
      "Could not find node with given id",
      "No node with given id found",
      "Node with given id does not belong to the document",
      "Execution context was destroyed.",
      "Cannot find context with specified id",
      "Node was not found",
      "error:optionsnotfound",
      "error:optionnotenabled",
    ] {
      assert!(is_retryable_action_error(msg), "should be retryable: {msg}");
    }
  }

  #[test]
  fn genuine_failures_are_not_retryable() {
    for msg in [
      "strict mode violation: 3 elements",
      "Timeout 30000ms exceeded",
      "Target closed",
      "boom",
    ] {
      assert!(!is_retryable_action_error(msg), "should NOT be retryable: {msg}");
    }
  }
}

#[cfg(test)]
mod tests {
  use super::super::retries;
  use crate::error::FerriError;

  #[test]
  fn an_attempt_whose_input_was_sent_is_never_tried_again() {
    assert!(retries(&FerriError::backend("No node with given id found")));
    assert!(retries(&FerriError::backend("error:notvisible")));
    assert!(!retries(&FerriError::InputSent {
      action: "click".into(),
      message: "No node with given id found".into(),
    }));
    assert!(!retries(&FerriError::InputSent {
      action: "click".into(),
      message: "transport closed".into(),
    }));
  }
}

/// The page side of [`Locator::dispatch_event`]: Playwright's
/// `injectedScript.dispatchEvent`, with `__TYPE__` the event type as a
/// JSON string.
pub(super) const DISPATCH_EVENT_BODY: &str = "\
  var type = __TYPE__; \
  var init = Object.assign({bubbles: true, cancelable: true, composed: true}, eventInit); \
  var ev; \
  if (['click','dblclick','mousedown','mouseup','mouseenter','mouseleave','mousemove','mouseover','mouseout','contextmenu','auxclick'].includes(type)) { \
    ev = new MouseEvent(type, init); \
  } else if (['keydown','keyup','keypress'].includes(type)) { \
    ev = new KeyboardEvent(type, init); \
  } else if (['touchstart','touchend','touchmove','touchcancel'].includes(type) && typeof TouchEvent !== 'undefined') { \
    ev = new TouchEvent(type, init); \
  } else if (['pointerdown','pointerup','pointermove','pointerover','pointerout','pointerenter','pointerleave','pointercancel','gotpointercapture','lostpointercapture'].includes(type)) { \
    ev = new PointerEvent(type, init); \
  } else if (['dragstart','drag','dragenter','dragleave','dragover','drop','dragend'].includes(type)) { \
    ev = new DragEvent(type, init); \
  } else if (['focus','blur','focusin','focusout'].includes(type)) { \
    ev = new FocusEvent(type, init); \
  } else if (['input','beforeinput'].includes(type)) { \
    ev = new InputEvent(type, init); \
  } else if (type === 'wheel') { \
    ev = new WheelEvent(type, init); \
  } else if (['deviceorientation','deviceorientationabsolute'].includes(type)) { \
    ev = new DeviceOrientationEvent(type, init); \
  } else { \
    ev = new Event(type, init); \
  } \
  el.dispatchEvent(ev);";

#[cfg(test)]
mod regex_selector_tests {
  use super::escape_regex_for_selector;

  /// Playwright's `escapeRegexForSelector`: an unescaped quote gains a
  /// backslash, an escaped one keeps its own, and `>>` is escaped.
  #[test]
  fn quotes_in_a_regex_are_escaped_for_the_selector() {
    assert_eq!(
      escape_regex_for_selector("Salama's summary", "i"),
      r"/Salama\'s summary/i"
    );
    assert_eq!(escape_regex_for_selector(r#"a"b`c"#, ""), r#"/a\"b\`c/"#);
    assert_eq!(escape_regex_for_selector(r"it\'s", ""), r"/it\'s/");
    assert_eq!(escape_regex_for_selector(r"a\\'b", ""), r"/a\\\'b/");
    assert_eq!(escape_regex_for_selector("a>>b", ""), r"/a\>\>b/");
    assert_eq!(escape_regex_for_selector("it's", "u"), "/it's/u");
  }
}
