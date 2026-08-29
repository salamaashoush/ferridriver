//! The Lighthouse audits that read the page as it stands.
//!
//! Lighthouse's non-performance audits fall into three groups. Sixty-
//! seven are axe-core wrappers and are covered by running the engine
//! instead ([`crate::accessibility`]). The performance ones wrap the
//! `DevTools` trace insights, which `ferridriver-perf` has ported. What is
//! left needs artifacts gathered from a live DOM, and of those, seven
//! score on a page that is simply sitting there: no network log, no
//! navigation, no trace.
//!
//! Those seven are here. Each is a pure function over the artifact
//! struct below, ported from Lighthouse's own source and checked against
//! it: `just lh-audit <url>` prints what Lighthouse concludes about a
//! page, and `cargo test -p ferridriver-perf --test lighthouse`
//! compares these verdicts against recorded ones for the fixture pages.
//!
//! The rest of that group -- `is-on-https`, `csp-xss`, `has-hsts`,
//! `canonical`, `is-crawlable`, `http-status-code` -- need a network
//! log, which means navigation mode, and are not here.
//!
//! # The one place this cannot see what Lighthouse sees
//!
//! `crawlable-anchors` asks whether an anchor carrying no `href` and no
//! href-associated attribute has an event listener; Lighthouse answers
//! with CDP's `DOMDebugger.getEventListeners`. Nothing page-side can
//! enumerate listeners, and no `BiDi` command exposes them at all, so
//! [`Anchor::has_inline_handler`] answers the part that IS visible: an
//! `onclick` attribute. Chrome reports inline handlers as listeners too,
//! so the two agree on `<a onclick="...">`; they part company on an
//! anchor whose only handler was registered with `addEventListener`,
//! which Lighthouse fails and this passes. Stated rather than papered
//! over, and the same on all four backends.

use serde::{Deserialize, Serialize};

use crate::error::{FerriError, Result};

/// Which audits to run. Empty runs all seven.
#[derive(Debug, Clone, Default)]
pub struct PageQualityOptions {
  /// Audit ids to run, e.g. `meta-description`. Empty runs every one.
  pub only: Vec<String>,
}

/// Which Lighthouse category an audit belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditCategory {
  Seo,
  BestPractices,
}

/// One element an audit objected to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditItem {
  /// A CSS selector locating the element.
  pub selector: String,
  /// The element's opening tag, truncated.
  pub snippet: String,
  /// Whatever the audit has to say about this element: the offending
  /// link text, the two aspect ratios, the size it should have been.
  #[serde(default)]
  pub detail: String,
}

/// One audit's verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditResult {
  /// Lighthouse's own audit id, e.g. `image-aspect-ratio`.
  pub id: String,
  pub category: AuditCategory,
  /// Lighthouse's title for the state this audit is in.
  pub title: String,
  pub passed: bool,
  /// Why it failed, where the audit has more to say than its title.
  #[serde(default)]
  pub explanation: Option<String>,
  /// The elements behind a failure, which is the part anyone acts on.
  #[serde(default)]
  pub items: Vec<AuditItem>,
}

/// What the seven audits concluded.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageQualityReport {
  pub audits: Vec<AuditResult>,
}

impl PageQualityReport {
  /// The audits that failed, which is usually all anyone wants.
  #[must_use]
  pub fn failures(&self) -> Vec<&AuditResult> {
    self.audits.iter().filter(|audit| !audit.passed).collect()
  }
}

// ── Artifacts ───────────────────────────────────────────────────────────

/// Everything the seven audits read, gathered in one evaluate.
///
/// Deliberately smaller than Lighthouse's artifacts. Both image audits
/// discard `isCss` images before looking at anything else, so CSS
/// background images are never collected; nothing here needs a node's
/// devtools path, so a CSS selector and a snippet stand in for
/// `NodeDetails`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifacts {
  /// `null` when the document has no doctype at all.
  #[serde(default)]
  pub doctype: Option<Doctype>,
  #[serde(default)]
  pub metas: Vec<Meta>,
  #[serde(default)]
  pub anchors: Vec<Anchor>,
  #[serde(default)]
  pub images: Vec<Image>,
  #[serde(default)]
  pub inputs: Vec<Input>,
  #[serde(default)]
  pub viewport: Viewport,
  /// The document's own URL, which both anchor audits resolve against.
  #[serde(default)]
  pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Doctype {
  pub name: String,
  pub public_id: String,
  pub system_id: String,
  /// `CSS1Compat` for standards mode, `BackCompat` for quirks.
  pub document_compat_mode: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
  /// Lowercased, as Lighthouse's gatherer lowercases it.
  pub name: String,
  pub content: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Anchor {
  /// The resolved `href` property. Empty when it would not resolve.
  pub href: String,
  /// The `href` attribute exactly as written.
  pub raw_href: String,
  pub name: String,
  pub role: String,
  pub id: String,
  pub rel: String,
  /// `innerText`, so hidden text does not count.
  pub text: String,
  /// The language the link text is in, where the document says.
  #[serde(default)]
  pub text_lang: Option<String>,
  pub attribute_names: Vec<String>,
  /// Whether an `onclick` attribute is present. See the module note:
  /// this is the visible half of Lighthouse's listener check.
  pub has_inline_handler: bool,
  pub selector: String,
  pub snippet: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Image {
  /// `currentSrc`, so `srcset` and `media` have already been resolved.
  pub src: String,
  pub srcset: String,
  /// `element.width` / `element.height`: the laid-out size in CSS px.
  pub displayed_width: f64,
  pub displayed_height: f64,
  pub client_rect: Rect,
  /// Absent when the browser's `naturalWidth` cannot be trusted, which
  /// is any `<picture>` child or anything with a `srcset`.
  #[serde(default)]
  pub natural_dimensions: Option<Dimensions>,
  pub object_fit: String,
  pub rendering: String,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
  pub top: f64,
  pub bottom: f64,
  pub left: f64,
  pub right: f64,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dimensions {
  pub width: f64,
  pub height: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Input {
  pub r#type: String,
  /// A cancelable `paste` event was dispatched and something cancelled
  /// it. `None` for a read-only input, which Lighthouse does not test.
  #[serde(default)]
  pub prevents_paste: Option<bool>,
  pub selector: String,
  pub snippet: String,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Viewport {
  pub inner_width: f64,
  pub inner_height: f64,
  pub device_pixel_ratio: f64,
}

// ── The gatherer ────────────────────────────────────────────────────────

/// One evaluate, returning the artifact struct as a JSON string.
///
/// A string and not a structure, for the reason the accessibility audit
/// does the same: each backend re-serialises a returned object through
/// its own remote-value format, which is slower and loses shapes.
///
/// `preventsPaste` is the one thing here that is not a read. Lighthouse
/// dispatches a cancelable `paste` at every input and sees whether
/// anything cancelled it, because an attribute cannot tell you that a
/// listener will call `preventDefault`. A synthetic event carries no
/// clipboard data and `isTrusted: false`, so a handler that acts on the
/// pasted text does nothing with it.
const GATHER: &str = r"
(() => {
  const MAX_SNIPPET = 200;

  const selectorFor = (el) => {
    if (el.id) return '#' + CSS.escape(el.id);
    const parts = [];
    for (let node = el; node && node.nodeType === 1 && parts.length < 5; node = node.parentElement) {
      let part = node.localName;
      if (node.parentElement) {
        const siblings = [...node.parentElement.children].filter(c => c.localName === node.localName);
        if (siblings.length > 1) part += ':nth-of-type(' + (siblings.indexOf(node) + 1) + ')';
      }
      parts.unshift(part);
      if (node.id) { parts[0] = '#' + CSS.escape(node.id); break; }
    }
    return parts.join(' > ');
  };

  const snippetFor = (el) => {
    const html = el.outerHTML ?? '';
    const open = html.slice(0, html.indexOf('>') + 1) || html;
    return open.length > MAX_SNIPPET ? open.slice(0, MAX_SNIPPET) + '…' : open;
  };

  const doctype = document.doctype
    ? {
        name: document.doctype.name,
        publicId: document.doctype.publicId,
        systemId: document.doctype.systemId,
        documentCompatMode: document.compatMode,
      }
    : null;

  const metas = [...document.querySelectorAll('head meta')].map(meta => ({
    name: (meta.name || '').toLowerCase(),
    content: meta.content || '',
  }));

  // Lighthouse's own rule for which lang applies to a link's text: one
  // `[lang]` on <html> or <body> covers the whole document; otherwise the
  // nearest ancestor with one, and nothing when the descendants disagree.
  const langElements = [...document.querySelectorAll('[lang]')];
  const singleLang =
    langElements.length === 1 && (langElements[0].nodeName === 'BODY' || langElements[0].nodeName === 'HTML')
      ? langElements[0].getAttribute('lang')
      : null;
  const langOfInnerText = (node) => {
    let current = null;
    for (const child of node.querySelectorAll('*')) {
      if (!child.textContent) continue;
      const childLang = child.closest('[lang]')?.getAttribute('lang');
      if (!childLang) continue;
      if (!current) { current = childLang; continue; }
      if (current.split('-')[0] !== childLang.split('-')[0]) return null;
    }
    return current ?? node.closest('[lang]')?.getAttribute('lang') ?? null;
  };

  const anchors = [...document.querySelectorAll('a')].map(a => ({
    href: a.href ?? '',
    rawHref: a.getAttribute('href') || '',
    name: a.name || '',
    role: a.getAttribute('role') || '',
    id: a.getAttribute('id') || '',
    rel: a.rel || '',
    text: a.innerText || '',
    textLang: singleLang ?? langOfInnerText(a) ?? null,
    attributeNames: a.getAttributeNames(),
    hasInlineHandler: a.hasAttribute('onclick'),
    selector: selectorFor(a),
    snippet: snippetFor(a),
  }));

  const images = [...document.querySelectorAll('img')].map(img => {
    const style = window.getComputedStyle(img);
    const isPicture = !!img.parentElement && img.parentElement.tagName === 'PICTURE';
    // naturalWidth reports the chosen source, which for a picture or a
    // srcset is not the one `src` names, so Lighthouse refuses to use it.
    const trustNatural = !isPicture && !img.srcset;
    const rect = img.getBoundingClientRect();
    return {
      src: img.currentSrc || '',
      srcset: img.srcset || '',
      displayedWidth: img.width,
      displayedHeight: img.height,
      clientRect: { top: rect.top, bottom: rect.bottom, left: rect.left, right: rect.right },
      naturalDimensions: trustNatural ? { width: img.naturalWidth, height: img.naturalHeight } : null,
      objectFit: style.getPropertyValue('object-fit'),
      rendering: style.getPropertyValue('image-rendering'),
    };
  });

  const inputs = [...document.querySelectorAll('textarea, input, select')].map(el => ({
    type: el.type || '',
    preventsPaste: el.readOnly ? null : !el.dispatchEvent(new ClipboardEvent('paste', { cancelable: true })),
    selector: selectorFor(el),
    snippet: snippetFor(el),
  }));

  return JSON.stringify({
    doctype,
    metas,
    anchors,
    images,
    inputs,
    viewport: {
      innerWidth: window.innerWidth,
      innerHeight: window.innerHeight,
      devicePixelRatio: window.devicePixelRatio,
    },
    url: document.location.href,
  });
})
";

/// The single expression that gathers everything.
#[must_use]
pub fn gather_source() -> &'static str {
  GATHER
}

/// Turn what the page returned into artifacts.
///
/// # Errors
///
/// [`FerriError::Backend`] when the payload is not the shape this asked
/// for.
pub fn parse_artifacts(payload: &str) -> Result<Artifacts> {
  serde_json::from_str(payload).map_err(|e| FerriError::backend(format!("page audit artifacts were unreadable: {e}")))
}

// ── The audits ──────────────────────────────────────────────────────────

/// Every audit id this module implements, in the order they are
/// reported.
pub const AUDIT_IDS: [&str; 7] = [
  "doctype",
  "meta-description",
  "crawlable-anchors",
  "link-text",
  "image-aspect-ratio",
  "image-size-responsive",
  "paste-preventing-inputs",
];

/// Run the audits over gathered artifacts.
#[must_use]
pub fn run(artifacts: &Artifacts, options: &PageQualityOptions) -> PageQualityReport {
  let wanted = |id: &str| options.only.is_empty() || options.only.iter().any(|want| want == id);
  let mut audits = Vec::new();
  if wanted("doctype") {
    audits.push(doctype(artifacts));
  }
  if wanted("meta-description") {
    audits.push(meta_description(artifacts));
  }
  if wanted("crawlable-anchors") {
    audits.push(crawlable_anchors(artifacts));
  }
  if wanted("link-text") {
    audits.push(link_text(artifacts));
  }
  if wanted("image-aspect-ratio") {
    audits.push(image_aspect_ratio(artifacts));
  }
  if wanted("image-size-responsive") {
    audits.push(image_size_responsive(artifacts));
  }
  if wanted("paste-preventing-inputs") {
    audits.push(paste_preventing_inputs(artifacts));
  }
  PageQualityReport { audits }
}

fn passed(id: &str, category: AuditCategory, title: &str) -> AuditResult {
  AuditResult {
    id: id.into(),
    category,
    title: title.into(),
    passed: true,
    explanation: None,
    items: Vec::new(),
  }
}

fn failed(
  id: &str,
  category: AuditCategory,
  title: &str,
  explanation: Option<&str>,
  items: Vec<AuditItem>,
) -> AuditResult {
  AuditResult {
    id: id.into(),
    category,
    title: title.into(),
    passed: false,
    explanation: explanation.map(str::to_string),
    items,
  }
}

/// `doctype`: the page is not in quirks mode.
///
/// Lighthouse also reports limited-quirks mode, which it learns from an
/// `InspectorIssues` quirks-mode issue matched against the trace's main
/// frame. Neither exists here, and Lighthouse's own snapshot mode is in
/// the same position, so both take the same branch.
#[must_use]
pub fn doctype(artifacts: &Artifacts) -> AuditResult {
  const ID: &str = "doctype";
  const PASS: &str = "Page has the HTML doctype";
  const FAIL: &str = "Page lacks the HTML doctype, thus triggering quirks-mode";
  let category = AuditCategory::BestPractices;

  let Some(doctype) = &artifacts.doctype else {
    return failed(ID, category, FAIL, Some("Document must contain a doctype"), Vec::new());
  };
  if doctype.document_compat_mode == "CSS1Compat" {
    return passed(ID, category, PASS);
  }
  let explanation = if !doctype.public_id.is_empty() {
    "Expected publicId to be an empty string"
  } else if !doctype.system_id.is_empty() {
    "Expected systemId to be an empty string"
  } else if doctype.name != "html" {
    "Doctype name must be the string `html`"
  } else {
    "Document contains a `doctype` that triggers `quirks-mode`"
  };
  failed(ID, category, FAIL, Some(explanation), Vec::new())
}

/// `meta-description`: the document has a non-empty meta description.
#[must_use]
pub fn meta_description(artifacts: &Artifacts) -> AuditResult {
  const ID: &str = "meta-description";
  const PASS: &str = "Document has a meta description";
  const FAIL: &str = "Document does not have a meta description";
  let category = AuditCategory::Seo;

  let Some(meta) = artifacts.metas.iter().find(|meta| meta.name == "description") else {
    return failed(ID, category, FAIL, None, Vec::new());
  };
  if meta.content.trim().is_empty() {
    return failed(ID, category, FAIL, Some("Description text is empty."), Vec::new());
  }
  passed(ID, category, PASS)
}

/// Attributes that make an `<a>` behave as a link even without `href`.
const HREF_ASSOCIATED_ATTRIBUTES: [&str; 7] = [
  "target",
  "download",
  "ping",
  "rel",
  "hreflang",
  "type",
  "referrerpolicy",
];

/// `crawlable-anchors`: every link is one a crawler can follow.
///
/// See the module note for the one input this cannot see.
#[must_use]
pub fn crawlable_anchors(artifacts: &Artifacts) -> AuditResult {
  const ID: &str = "crawlable-anchors";
  const PASS: &str = "Links are crawlable";
  const FAIL: &str = "Links are not crawlable";
  let category = AuditCategory::Seo;

  let items: Vec<AuditItem> = artifacts
    .anchors
    .iter()
    .filter(|anchor| anchor_is_uncrawlable(anchor, &artifacts.url))
    .map(|anchor| AuditItem {
      selector: anchor.selector.clone(),
      snippet: anchor.snippet.clone(),
      detail: anchor.raw_href.clone(),
    })
    .collect();

  if items.is_empty() {
    passed(ID, category, PASS)
  } else {
    failed(ID, category, FAIL, None, items)
  }
}

fn anchor_is_uncrawlable(anchor: &Anchor, document_url: &str) -> bool {
  let raw_href: String = anchor.raw_href.chars().filter(|c| !c.is_whitespace()).collect();
  let name = anchor.name.trim();
  let role = anchor.role.trim();

  if !role.is_empty() {
    return false;
  }
  if raw_href.starts_with("mailto:") {
    return false;
  }
  if raw_href.is_empty() && !anchor.id.is_empty() {
    return false;
  }
  if raw_href.starts_with("file:") {
    return true;
  }
  if !name.is_empty() {
    return false;
  }
  let has_href = anchor.attribute_names.iter().any(|attr| attr == "href");
  let has_associated = HREF_ASSOCIATED_ATTRIBUTES
    .iter()
    .any(|wanted| anchor.attribute_names.iter().any(|attr| attr == wanted));
  if !has_href && !has_associated {
    return anchor.has_inline_handler;
  }
  if anchor.href.is_empty() {
    return true;
  }
  if is_javascript_void(&raw_href) {
    return true;
  }
  resolve(&raw_href, document_url).is_none()
}

/// Lighthouse's `/javascript:void(\(|)0(\)|)/`, unanchored, over the
/// whitespace-stripped href.
fn is_javascript_void(raw_href: &str) -> bool {
  let bytes = raw_href.as_bytes();
  for start in 0..bytes.len() {
    let rest = &raw_href[start..];
    let Some(rest) = rest.strip_prefix("javascript:void") else {
      continue;
    };
    let rest = rest.strip_prefix('(').unwrap_or(rest);
    let Some(rest) = rest.strip_prefix('0') else {
      continue;
    };
    let _ = rest.strip_prefix(')');
    return true;
  }
  false
}

/// `new URL(href, base)`: absolute, or relative to the document.
fn resolve(href: &str, base: &str) -> Option<reqwest::Url> {
  if let Ok(absolute) = reqwest::Url::parse(href) {
    return Some(absolute);
  }
  reqwest::Url::parse(base).ok()?.join(href).ok()
}

/// Link text that says nothing about where the link goes, by language.
///
/// Lighthouse's own table, verbatim
/// (`core/audits/seo/link-text.js`). Ported rather than trimmed to
/// English: a page in one of the other eight languages would otherwise
/// silently pass an audit Lighthouse fails.
const NON_DESCRIPTIVE_LINK_TEXTS: &[(&str, &[&str])] = &[
  (
    "en",
    &[
      "click here",
      "click this",
      "go",
      "here",
      "information",
      "learn more",
      "more",
      "more info",
      "more information",
      "right here",
      "read more",
      "see more",
      "start",
      "this",
    ],
  ),
  (
    "ja",
    &[
      "ここをクリック",
      "こちらをクリック",
      "リンク",
      "続きを読む",
      "続く",
      "全文表示",
    ],
  ),
  (
    "es",
    &[
      "click aquí",
      "click aqui",
      "clicka aquí",
      "clicka aqui",
      "pincha aquí",
      "pincha aqui",
      "aquí",
      "aqui",
      "más",
      "mas",
      "más información",
      "más informacion",
      "mas información",
      "mas informacion",
      "este",
      "enlace",
      "este enlace",
      "empezar",
    ],
  ),
  (
    "pt",
    &[
      "clique aqui",
      "ir",
      "mais informação",
      "mais informações",
      "mais",
      "veja mais",
    ],
  ),
  (
    "ko",
    &[
      "여기",
      "여기를 클릭",
      "클릭",
      "링크",
      "자세히",
      "자세히 보기",
      "계속",
      "이동",
      "전체 보기",
    ],
  ),
  (
    "sv",
    &["här", "klicka här", "läs mer", "mer", "mer info", "mer information"],
  ),
  (
    "de",
    &[
      "klicke hier",
      "hier klicken",
      "hier",
      "mehr",
      "siehe",
      "dies",
      "das",
      "weiterlesen",
    ],
  ),
  (
    "ta",
    &[
      "அடுத்த பக்கம்",
      "மறுபக்கம்",
      "முந்தைய பக்கம்",
      "முன்பக்கம்",
      "மேலும் அறிக",
      "மேலும் தகவலுக்கு",
      "மேலும் தரவுகளுக்கு",
      "தயவுசெய்து இங்கே அழுத்தவும்",
      "இங்கே கிளிக் செய்யவும்",
    ],
  ),
  (
    "fa",
    &[
      "اطلاعات بیشتر",
      "اطلاعات",
      "این",
      "اینجا بزنید",
      "اینجا کلیک کنید",
      "اینجا",
      "برو",
      "بیشتر بخوانید",
      "بیشتر بدانید",
      "بیشتر",
      "شروع",
    ],
  ),
];

/// `link-text`: links say where they go.
#[must_use]
pub fn link_text(artifacts: &Artifacts) -> AuditResult {
  const ID: &str = "link-text";
  const PASS: &str = "Links have descriptive text";
  const FAIL: &str = "Links do not have descriptive text";
  let category = AuditCategory::Seo;

  let items: Vec<AuditItem> = artifacts
    .anchors
    .iter()
    .filter(|anchor| link_text_is_non_descriptive(anchor, &artifacts.url))
    .map(|anchor| AuditItem {
      selector: anchor.selector.clone(),
      snippet: anchor.snippet.clone(),
      detail: anchor.text.trim().to_string(),
    })
    .collect();

  if items.is_empty() {
    passed(ID, category, PASS)
  } else {
    failed(ID, category, FAIL, None, items)
  }
}

fn link_text_is_non_descriptive(anchor: &Anchor, document_url: &str) -> bool {
  if anchor.href.is_empty() || anchor.rel.contains("nofollow") {
    return false;
  }
  let href = anchor.href.to_lowercase();
  if href.starts_with("javascript:") || href.starts_with("mailto:") {
    return false;
  }
  // An anchor pointing at the page it is on is a jump link, not a link
  // whose text has to describe a destination.
  if equal_ignoring_fragment(&anchor.href, document_url) {
    return false;
  }
  let search_term = anchor.text.trim().to_lowercase();
  if search_term.is_empty() {
    return false;
  }
  match anchor.text_lang.as_deref() {
    Some(lang) => {
      let lang = lang.split('-').next().unwrap_or(lang);
      NON_DESCRIPTIVE_LINK_TEXTS
        .iter()
        .find(|(key, _)| *key == lang)
        .is_some_and(|(_, texts)| texts.contains(&search_term.as_str()))
    },
    None => NON_DESCRIPTIVE_LINK_TEXTS
      .iter()
      .any(|(_, texts)| texts.contains(&search_term.as_str())),
  }
}

fn equal_ignoring_fragment(a: &str, b: &str) -> bool {
  let (Ok(mut a), Ok(mut b)) = (reqwest::Url::parse(a), reqwest::Url::parse(b)) else {
    return false;
  };
  a.set_fragment(None);
  b.set_fragment(None);
  a == b
}

/// Lighthouse's `URL.guessMimeType`, which both image audits use to skip
/// vector images.
fn guess_mime_type(src: &str) -> Option<&'static str> {
  let url = reqwest::Url::parse(src).ok()?;
  if url.scheme() == "data" {
    let path = url.path();
    let (mime, _) = path.split_once([';', ','])?;
    return match mime {
      "image/png" => Some("image/png"),
      "image/jpeg" => Some("image/jpeg"),
      "image/svg+xml" => Some("image/svg+xml"),
      "image/webp" => Some("image/webp"),
      "image/gif" => Some("image/gif"),
      "image/avif" => Some("image/avif"),
      _ => None,
    };
  }
  let path = url.path().to_lowercase();
  let (_, ext) = path.rsplit_once('.')?;
  match ext {
    "png" => Some("image/png"),
    "jpeg" | "jpg" => Some("image/jpeg"),
    "svg" => Some("image/svg+xml"),
    "webp" => Some("image/webp"),
    "gif" => Some("image/gif"),
    "avif" => Some("image/avif"),
    _ => None,
  }
}

/// How far the drawn box may sit from the image's own aspect ratio
/// before it counts as distorted. Lighthouse's `THRESHOLD_PX`.
const ASPECT_RATIO_THRESHOLD_PX: f64 = 2.0;

/// `image-aspect-ratio`: images are drawn at the shape they actually are.
#[must_use]
pub fn image_aspect_ratio(artifacts: &Artifacts) -> AuditResult {
  const ID: &str = "image-aspect-ratio";
  const PASS: &str = "Displays images with correct aspect ratio";
  const FAIL: &str = "Displays images with incorrect aspect ratio";
  let category = AuditCategory::BestPractices;

  let mut items = Vec::new();
  for image in &artifacts.images {
    let Some(natural) = image.natural_dimensions else {
      continue;
    };
    if guess_mime_type(&image.src) == Some("image/svg+xml")
      || natural.height <= 5.0
      || natural.width <= 5.0
      || image.displayed_width == 0.0
      || image.displayed_height == 0.0
      || image.object_fit != "fill"
    {
      continue;
    }
    let actual_ratio = natural.width / natural.height;
    let target_height = image.displayed_width / actual_ratio;
    let target_width = image.displayed_height * actual_ratio;
    let matches = if target_height < target_width {
      (target_height - image.displayed_height).abs() < ASPECT_RATIO_THRESHOLD_PX
    } else {
      (target_width - image.displayed_width).abs() < ASPECT_RATIO_THRESHOLD_PX
    };
    if !matches {
      items.push(AuditItem {
        selector: String::new(),
        snippet: elide_data_uri(&image.src),
        detail: format!(
          "displayed {} x {}, actual {} x {}",
          image.displayed_width, image.displayed_height, natural.width, natural.height
        ),
      });
    }
  }

  if items.is_empty() {
    passed(ID, category, PASS)
  } else {
    failed(ID, category, FAIL, None, items)
  }
}

/// Below this drawn size an image must carry its full device-pixel
/// count; above it, three quarters will do. Lighthouse's
/// `SMALL_IMAGE_THRESHOLD` / `SMALL_IMAGE_FACTOR` / `LARGE_IMAGE_FACTOR`.
const SMALL_IMAGE_THRESHOLD: f64 = 64.0;
const SMALL_IMAGE_FACTOR: f64 = 1.0;
const LARGE_IMAGE_FACTOR: f64 = 0.75;

/// Device pixel ratios are quantised before use, so a 1.7x screen asks
/// for the same image as a 1.5x one.
fn quantize_dpr(dpr: f64) -> f64 {
  if dpr >= 2.0 {
    2.0
  } else if dpr >= 1.5 {
    1.5
  } else {
    1.0
  }
}

/// `image-size-responsive`: images carry enough pixels for the size they
/// are drawn at.
#[must_use]
pub fn image_size_responsive(artifacts: &Artifacts) -> AuditResult {
  const ID: &str = "image-size-responsive";
  const PASS: &str = "Serves images with appropriate resolution";
  const FAIL: &str = "Serves images with low resolution";
  let category = AuditCategory::BestPractices;
  let dpr = quantize_dpr(artifacts.viewport.device_pixel_ratio);

  let mut results: Vec<(f64, f64, AuditItem, String)> = Vec::new();
  for image in &artifacts.images {
    if !is_size_candidate(image) {
      continue;
    }
    let Some(natural) = image.natural_dimensions else {
      continue;
    };
    let factor = if image.displayed_width > SMALL_IMAGE_THRESHOLD || image.displayed_height > SMALL_IMAGE_THRESHOLD {
      LARGE_IMAGE_FACTOR
    } else {
      SMALL_IMAGE_FACTOR
    };
    let allowed_width = (factor * dpr * image.displayed_width).ceil();
    let allowed_height = (factor * dpr * image.displayed_height).ceil();
    if natural.width >= allowed_width && natural.height >= allowed_height {
      continue;
    }
    if !is_visible(&image.client_rect, &artifacts.viewport)
      || !is_smaller_than_viewport(&image.client_rect, &artifacts.viewport)
    {
      continue;
    }
    let expected_width = (dpr * image.displayed_width).ceil();
    let expected_height = (dpr * image.displayed_height).ceil();
    let expected_pixels = expected_width * expected_height;
    let actual_pixels = natural.width * natural.height;
    let url = elide_data_uri(&image.src);
    results.push((
      expected_pixels,
      actual_pixels,
      AuditItem {
        selector: String::new(),
        snippet: url.clone(),
        detail: format!(
          "displayed {} x {}, actual {} x {}, expected {expected_width} x {expected_height}",
          image.displayed_width, image.displayed_height, natural.width, natural.height
        ),
      },
      url,
    ));
  }

  // Upstream keeps one row per URL (the one asking for most pixels),
  // then orders by how far short each falls.
  results.sort_by(|a, b| a.3.cmp(&b.3));
  let mut deduplicated: Vec<(f64, f64, AuditItem, String)> = Vec::new();
  for result in results {
    match deduplicated.last_mut() {
      Some(previous) if previous.3 == result.3 => {
        if previous.0 < result.0 {
          *previous = result;
        }
      },
      _ => deduplicated.push(result),
    }
  }
  deduplicated.sort_by(|a, b| {
    (b.0 - b.1)
      .partial_cmp(&(a.0 - a.1))
      .unwrap_or(std::cmp::Ordering::Equal)
  });

  let items: Vec<AuditItem> = deduplicated.into_iter().map(|result| result.2).collect();
  if items.is_empty() {
    passed(ID, category, PASS)
  } else {
    failed(ID, category, FAIL, None, items)
  }
}

fn is_size_candidate(image: &Image) -> bool {
  if image.displayed_width <= 1.0 || image.displayed_height <= 1.0 {
    return false;
  }
  let Some(natural) = image.natural_dimensions else {
    return false;
  };
  if natural.width == 0.0 || natural.height == 0.0 {
    return false;
  }
  if guess_mime_type(&image.src) == Some("image/svg+xml") {
    return false;
  }
  if image.object_fit != "fill" {
    return false;
  }
  if matches!(image.rendering.as_str(), "pixelated" | "crisp-edges") {
    return false;
  }
  // A srcset carrying a density descriptor means the author has already
  // said which pixels go with which screen.
  !has_density_descriptor(&image.srcset)
}

/// Lighthouse's `/ \d+(\.\d+)?x/` over the `srcset`.
fn has_density_descriptor(srcset: &str) -> bool {
  let bytes = srcset.as_bytes();
  for (index, byte) in bytes.iter().enumerate() {
    if *byte != b' ' {
      continue;
    }
    let rest = &srcset[index + 1..];
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
      continue;
    }
    let after_digits = &rest[digits..];
    let after_fraction = match after_digits.strip_prefix('.') {
      Some(fraction) => {
        let count = fraction.len() - fraction.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if count == 0 {
          continue;
        }
        &fraction[count..]
      },
      None => after_digits,
    };
    if after_fraction.starts_with('x') {
      return true;
    }
  }
  false
}

fn is_visible(rect: &Rect, viewport: &Viewport) -> bool {
  (rect.bottom - rect.top) * (rect.right - rect.left) > 0.0
    && rect.top <= viewport.inner_height
    && rect.bottom >= 0.0
    && rect.left <= viewport.inner_width
    && rect.right >= 0.0
}

fn is_smaller_than_viewport(rect: &Rect, viewport: &Viewport) -> bool {
  rect.bottom - rect.top <= viewport.inner_height && rect.right - rect.left <= viewport.inner_width
}

fn elide_data_uri(url: &str) -> String {
  if url.starts_with("data:") && url.len() > 100 {
    format!("{}\u{2026}", &url[..99])
  } else {
    url.to_string()
  }
}

/// `paste-preventing-inputs`: nothing stops someone pasting into a field.
#[must_use]
pub fn paste_preventing_inputs(artifacts: &Artifacts) -> AuditResult {
  const ID: &str = "paste-preventing-inputs";
  const PASS: &str = "Allows users to paste into input fields";
  const FAIL: &str = "Prevents users from pasting into input fields";
  let category = AuditCategory::BestPractices;

  let items: Vec<AuditItem> = artifacts
    .inputs
    .iter()
    .filter(|input| input.prevents_paste == Some(true))
    .map(|input| AuditItem {
      selector: input.selector.clone(),
      snippet: input.snippet.clone(),
      detail: input.r#type.clone(),
    })
    .collect();

  if items.is_empty() {
    passed(ID, category, PASS)
  } else {
    failed(ID, category, FAIL, None, items)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn anchor(raw_href: &str, attribute_names: &[&str]) -> Anchor {
    Anchor {
      href: String::new(),
      raw_href: raw_href.into(),
      name: String::new(),
      role: String::new(),
      id: String::new(),
      rel: String::new(),
      text: String::new(),
      text_lang: None,
      attribute_names: attribute_names.iter().map(|a| (*a).to_string()).collect(),
      has_inline_handler: false,
      selector: "a".into(),
      snippet: "<a>".into(),
    }
  }

  #[test]
  fn a_missing_doctype_and_a_quirks_doctype_are_different_failures() {
    let none = doctype(&Artifacts::default());
    assert!(!none.passed);
    assert_eq!(none.explanation.as_deref(), Some("Document must contain a doctype"));

    let quirks = doctype(&Artifacts {
      doctype: Some(Doctype {
        name: "html".into(),
        public_id: "-//W3C//DTD HTML 4.01//EN".into(),
        system_id: String::new(),
        document_compat_mode: "BackCompat".into(),
      }),
      ..Artifacts::default()
    });
    assert!(!quirks.passed);
    assert_eq!(
      quirks.explanation.as_deref(),
      Some("Expected publicId to be an empty string")
    );
  }

  #[test]
  fn standards_mode_passes_whatever_the_doctype_says() {
    // Upstream returns on compatMode before it looks at any of the
    // three fields, so a peculiar-but-standards doctype passes.
    let report = doctype(&Artifacts {
      doctype: Some(Doctype {
        name: "HTML".into(),
        public_id: String::new(),
        system_id: String::new(),
        document_compat_mode: "CSS1Compat".into(),
      }),
      ..Artifacts::default()
    });
    assert!(report.passed);
  }

  #[test]
  fn a_whitespace_only_meta_description_fails_with_its_own_explanation() {
    let report = meta_description(&Artifacts {
      metas: vec![Meta {
        name: "description".into(),
        content: "   ".into(),
      }],
      ..Artifacts::default()
    });
    assert!(!report.passed);
    assert_eq!(report.explanation.as_deref(), Some("Description text is empty."));
  }

  #[test]
  fn a_missing_meta_description_fails_without_one() {
    let report = meta_description(&Artifacts::default());
    assert!(!report.passed);
    assert_eq!(report.explanation, None);
  }

  #[test]
  fn javascript_void_is_recognised_in_every_form_upstream_accepts() {
    for href in [
      "javascript:void0",
      "javascript:void(0",
      "javascript:void0)",
      "javascript:void(0)",
    ] {
      assert!(is_javascript_void(href), "{href}");
    }
    for href in ["javascript:void", "javascript:0", "/void(0)"] {
      assert!(!is_javascript_void(href), "{href}");
    }
  }

  #[test]
  fn an_anchor_with_no_href_fails_only_when_something_handles_it() {
    let bare = anchor("", &[]);
    assert!(!anchor_is_uncrawlable(&bare, "https://example.com/"));

    let mut handled = anchor("", &[]);
    handled.has_inline_handler = true;
    assert!(anchor_is_uncrawlable(&handled, "https://example.com/"));

    // An href-associated attribute takes it out of that branch, and
    // then an empty resolved href fails it outright.
    let mut with_target = anchor("", &["target"]);
    with_target.has_inline_handler = false;
    assert!(anchor_is_uncrawlable(&with_target, "https://example.com/"));
  }

  #[test]
  fn mailto_and_named_anchors_are_left_alone() {
    assert!(!anchor_is_uncrawlable(
      &anchor("mailto:me@example.com", &["href"]),
      "https://example.com/"
    ));
    let mut named = anchor("javascript:void(0)", &["href", "name"]);
    named.name = "top".into();
    assert!(!anchor_is_uncrawlable(&named, "https://example.com/"));
    let mut with_id = anchor("", &["id"]);
    with_id.id = "section".into();
    assert!(!anchor_is_uncrawlable(&with_id, "https://example.com/"));
  }

  #[test]
  fn link_text_matches_any_language_when_the_page_does_not_say_which() {
    let mut link = anchor("/next", &["href"]);
    link.href = "https://example.com/next".into();
    link.text = "  Click Here  ".into();
    assert!(link_text_is_non_descriptive(&link, "https://example.com/"));

    // Told it is German, the English table no longer applies.
    link.text_lang = Some("de-DE".into());
    assert!(!link_text_is_non_descriptive(&link, "https://example.com/"));
    link.text = "hier klicken".into();
    assert!(link_text_is_non_descriptive(&link, "https://example.com/"));
  }

  #[test]
  fn a_link_to_the_page_it_sits_on_is_not_judged_on_its_text() {
    let mut link = anchor("#section", &["href"]);
    link.href = "https://example.com/page#section".into();
    link.text = "here".into();
    assert!(!link_text_is_non_descriptive(&link, "https://example.com/page"));
  }

  #[test]
  fn nofollow_and_javascript_links_are_not_judged_on_their_text() {
    let mut nofollow = anchor("/next", &["href", "rel"]);
    nofollow.href = "https://example.com/next".into();
    nofollow.text = "here".into();
    nofollow.rel = "nofollow".into();
    assert!(!link_text_is_non_descriptive(&nofollow, "https://example.com/"));

    let mut scripted = anchor("javascript:go()", &["href"]);
    scripted.href = "javascript:go()".into();
    scripted.text = "here".into();
    assert!(!link_text_is_non_descriptive(&scripted, "https://example.com/"));
  }

  #[test]
  fn a_stretched_image_fails_and_a_letterboxed_one_does_not() {
    let image = |displayed: (f64, f64), natural: (f64, f64)| Image {
      src: "https://example.com/a.png".into(),
      srcset: String::new(),
      displayed_width: displayed.0,
      displayed_height: displayed.1,
      client_rect: Rect::default(),
      natural_dimensions: Some(Dimensions {
        width: natural.0,
        height: natural.1,
      }),
      object_fit: "fill".into(),
      rendering: "auto".into(),
    };
    let of = |image: Image| {
      image_aspect_ratio(&Artifacts {
        images: vec![image],
        ..Artifacts::default()
      })
      .passed
    };
    assert!(!of(image((200.0, 200.0), (400.0, 100.0))), "square box, wide image");
    assert!(of(image((200.0, 50.0), (400.0, 100.0))), "same ratio, half the size");
    // object-fit takes the image out of the audit entirely.
    let mut contained = image((200.0, 200.0), (400.0, 100.0));
    contained.object_fit = "contain".into();
    assert!(of(contained));
  }

  #[test]
  fn an_image_smaller_than_it_is_drawn_fails_once_it_is_on_screen() {
    let artifacts = |rect: Rect| Artifacts {
      images: vec![Image {
        src: "https://example.com/a.png".into(),
        srcset: String::new(),
        displayed_width: 200.0,
        displayed_height: 200.0,
        client_rect: rect,
        natural_dimensions: Some(Dimensions {
          width: 100.0,
          height: 100.0,
        }),
        object_fit: "fill".into(),
        rendering: "auto".into(),
      }],
      viewport: Viewport {
        inner_width: 800.0,
        inner_height: 600.0,
        device_pixel_ratio: 1.0,
      },
      ..Artifacts::default()
    };
    let on_screen = Rect {
      top: 0.0,
      bottom: 200.0,
      left: 0.0,
      right: 200.0,
    };
    assert!(!image_size_responsive(&artifacts(on_screen)).passed);

    // Scrolled far below the fold, upstream stops caring.
    let off_screen = Rect {
      top: 2000.0,
      bottom: 2200.0,
      left: 0.0,
      right: 200.0,
    };
    assert!(image_size_responsive(&artifacts(off_screen)).passed);
  }

  #[test]
  fn a_density_descriptor_takes_an_image_out_of_the_resolution_audit() {
    assert!(has_density_descriptor("/a.png 2x, /b.png 3x"));
    assert!(has_density_descriptor("/a.png 1.5x"));
    assert!(!has_density_descriptor("/a.png 640w"));
    assert!(!has_density_descriptor("/a.png"));
  }

  #[test]
  fn a_read_only_input_is_never_a_paste_failure() {
    let input = |prevents_paste| Input {
      r#type: "text".into(),
      prevents_paste,
      selector: "input".into(),
      snippet: "<input>".into(),
    };
    let of = |input| {
      paste_preventing_inputs(&Artifacts {
        inputs: vec![input],
        ..Artifacts::default()
      })
      .passed
    };
    assert!(!of(input(Some(true))));
    assert!(of(input(Some(false))));
    assert!(of(input(None)), "read-only, so upstream never dispatched the event");
  }

  #[test]
  fn only_narrows_the_run_and_an_empty_list_runs_everything() {
    let all = run(&Artifacts::default(), &PageQualityOptions::default());
    assert_eq!(all.audits.len(), AUDIT_IDS.len());

    let one = run(
      &Artifacts::default(),
      &PageQualityOptions {
        only: vec!["doctype".into()],
      },
    );
    assert_eq!(one.audits.len(), 1);
    assert_eq!(one.audits[0].id, "doctype");
  }

  #[test]
  fn a_vector_image_is_skipped_by_both_image_audits() {
    assert_eq!(guess_mime_type("https://example.com/a.svg"), Some("image/svg+xml"));
    assert_eq!(guess_mime_type("data:image/svg+xml;base64,AAA"), Some("image/svg+xml"));
    assert_eq!(guess_mime_type("https://example.com/a.PNG"), Some("image/png"));
    assert_eq!(guess_mime_type("https://example.com/a"), None);
  }
}
