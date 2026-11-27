//! Page-quality audit artifact structs and DOM gatherer.

use serde::Deserialize;

use ferridriver_error::{FerriError, Result};

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
  pub links: Vec<Link>,
  #[serde(default)]
  pub viewport: Viewport,
  /// The document's own URL, which both anchor audits resolve against.
  #[serde(default)]
  pub url: String,
  /// The response that produced the current document. Not gathered from
  /// the page -- nothing page-side can read a status or a response
  /// header -- so `check_page_quality` fills it in from the context's
  /// network log and it is `None` for a document that arrived without
  /// one (`about:blank`, `setContent`, a log that has since lapped).
  #[serde(default)]
  pub main_document: Option<MainDocument>,
  /// `/robots.txt` for the document's origin, fetched only when
  /// `is-crawlable` is in the run.
  #[serde(default)]
  pub robots_txt: Option<RobotsTxt>,
}

/// The main document's own response, which is where both the status and
/// `X-Robots-Tag` live.
///
/// Lighthouse reads it out of the `DevtoolsLog` the same way
/// (`core/computed/main-resource.js`): the LAST document request whose
/// URL matches the document's, so a `location.reload()` and a redirect
/// chain both resolve to the response actually on screen.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MainDocument {
  pub url: String,
  pub status_code: i64,
  /// In wire order, duplicates preserved where the engine preserves
  /// them: `X-Robots-Tag` is a header a server sends more than once,
  /// and each copy is its own directive.
  ///
  /// Gecko joins repeated response headers before `WebDriver` `BiDi`
  /// sees them, so on Firefox a header sent twice arrives as one entry
  /// holding both values (Playwright's own `BiDi` backend is in the
  /// same position). It changes how many items `is-crawlable` reports
  /// and not what it concludes, because a comma already separates
  /// directives within one value.
  pub response_headers: Vec<ferridriver_types::HeaderEntry>,
}

/// What `/robots.txt` answered, in the shape Lighthouse's `RobotsTxt`
/// gatherer produces.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RobotsTxt {
  /// `None` when the request could not be made at all.
  #[serde(default)]
  pub status: Option<i64>,
  /// Only present for a 2xx: Lighthouse's fetcher discards the body of
  /// anything else, so a 404 page's HTML is never parsed as rules.
  #[serde(default)]
  pub content: Option<String>,
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
  /// `is-crawlable` reports the offending element, so a `<meta>` needs
  /// the same locating pair every other artifact carries.
  #[serde(default)]
  pub selector: String,
  #[serde(default)]
  pub snippet: String,
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

/// A `<link>` in the document head.
///
/// Body links are dropped by the gatherer, matching Lighthouse's
/// `link.source === 'body'` skip: a canonical outside the head is not
/// one a crawler honours.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
  pub rel: String,
  /// The `href` attribute exactly as written.
  pub href_raw: String,
  /// The resolved `href` property. Empty when it would not resolve.
  pub href: String,
  pub hreflang: String,
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
    selector: selectorFor(meta),
    snippet: snippetFor(meta),
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

  // Head only: Lighthouse skips `link.source === 'body'`, because a
  // canonical a crawler will not honour is not one worth reporting.
  const links = [...document.querySelectorAll('head link')].map(link => ({
    rel: (link.rel || '').toLowerCase(),
    hrefRaw: link.getAttribute('href') || '',
    href: link.href || '',
    hreflang: link.hreflang || '',
  }));

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
    links,
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
