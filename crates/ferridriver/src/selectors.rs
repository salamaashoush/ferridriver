//! Playwright-style selector engine.
//!
//! Parses rich selector strings (role=, text=, testid=, css=, etc.) in Rust,
//! then builds a self-contained JS IIFE that executes the query pipeline
//! in the browser context for maximum performance.
//!
//! # Selector Format
//!
//! ```text
//! css=div.container >> role=button[name="Submit"]
//! text="Hello World"
//! role=heading[level=1]
//! testid=login-form
//! label="Email"
//! ```
//!
//! Chaining with `>>` narrows scope: each part's results become the next part's search roots.

use crate::backend::{AnyElement, AnyPage};

// ─── Types ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Selector {
    pub parts: Vec<SelectorPart>,
}

#[derive(Debug, Clone)]
pub struct SelectorPart {
    pub engine: Engine,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Engine {
    Css,
    Text,
    Role,
    TestId,
    Label,
    Placeholder,
    Alt,
    Title,
    XPath,
    Id,
    Nth,
    Visible,
    Has,
    HasText,
    HasNot,
    HasNotText,
}

/// Result of a selector query -- lightweight info returned from JS.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct MatchedElement {
    pub index: usize,
    pub tag: String,
    pub text: String,
}

// ─── Detector ───────────────────────────────────────────────────────────────

/// Check if a selector string uses the rich engine format (not plain CSS).
#[must_use]
pub fn is_rich_selector(s: &str) -> bool {
    let prefixes = [
        "role=", "text=", "testid=", "label=", "placeholder=",
        "alt=", "title=", "xpath=", "id=", "css=", "nth=",
        "visible=", "has=", "has-text=", "has-not=", "has-not-text=",
    ];
    let trimmed = s.trim();
    // Has explicit engine prefix
    if prefixes.iter().any(|p| trimmed.starts_with(p)) {
        return true;
    }
    // Has chaining operator
    if trimmed.contains(" >> ") {
        return true;
    }
    false
}

// ─── Parser ─────────────────────────────────────────────────────────────────

/// Parse a selector string into a Selector AST.
///
/// # Errors
///
/// Returns an error if the selector string is empty or has an invalid chain.
pub fn parse(selector: &str) -> Result<Selector, String> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Err("Selector cannot be empty".into());
    }

    // Split by >> respecting quoted strings
    let raw_parts = split_by_chain(selector);
    let mut parts = Vec::new();

    for raw in raw_parts {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("Empty selector part in chain".into());
        }
        parts.push(parse_part(raw));
    }

    Ok(Selector { parts })
}

fn split_by_chain(s: &str) -> Vec<String> {
    // Fast path: no chain operator, avoid scanning
    if !s.contains(">>") {
        let t = s.trim();
        return if t.is_empty() { Vec::new() } else { vec![t.to_string()] };
    }

    let mut parts = Vec::new();
    let bytes = s.as_bytes();
    let mut start = 0;
    let mut i = 0;
    let mut in_quote: u8 = 0; // 0 = none, b'"' or b'\''

    while i < bytes.len() {
        let c = bytes[i];

        if c == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }

        if in_quote != 0 {
            if c == in_quote { in_quote = 0; }
            i += 1;
            continue;
        }

        if c == b'"' || c == b'\'' {
            in_quote = c;
            i += 1;
            continue;
        }

        if c == b'>' && i + 1 < bytes.len() && bytes[i + 1] == b'>' {
            let part = s[start..i].trim();
            if !part.is_empty() {
                parts.push(part.to_string());
            }
            i += 2;
            while i < bytes.len() && bytes[i] == b' ' { i += 1; }
            start = i;
            continue;
        }

        i += 1;
    }

    let part = s[start..].trim();
    if !part.is_empty() {
        parts.push(part.to_string());
    }

    parts
}

fn parse_part(s: &str) -> SelectorPart {
    // Try each engine prefix
    let engines = [
        ("role=", Engine::Role),
        ("text=", Engine::Text),
        ("testid=", Engine::TestId),
        ("label=", Engine::Label),
        ("placeholder=", Engine::Placeholder),
        ("alt=", Engine::Alt),
        ("title=", Engine::Title),
        ("xpath=", Engine::XPath),
        ("id=", Engine::Id),
        ("css=", Engine::Css),
        ("nth=", Engine::Nth),
        ("visible=", Engine::Visible),
        ("has=", Engine::Has),
        ("has-text=", Engine::HasText),
        ("has-not=", Engine::HasNot),
        ("has-not-text=", Engine::HasNotText),
    ];

    for (prefix, engine) in &engines {
        if let Some(body) = s.strip_prefix(prefix) {
            return SelectorPart {
                engine: engine.clone(),
                body: body.to_string(),
            };
        }
    }

    // Default: treat as CSS selector
    SelectorPart {
        engine: Engine::Css,
        body: s.to_string(),
    }
}

// ─── JS Query Builder ───────────────────────────────────────────────────────

/// JS to inject the unified runtime once. Idempotent -- safe to call multiple times.
#[must_use]
pub fn build_inject_js() -> String {
    format!(
        "(function() {{ if (window.__fd) return; {ENGINE_JS}\n\
        window.__fd = {{\n\
          _exec: executeSelector,\n\
          sel: function(parts) {{\n\
            try {{\n\
              var results = executeSelector(parts, document);\n\
              results.forEach(function(el, i) {{ el.setAttribute('data-fd-sel', '' + i); }});\n\
              return JSON.stringify(results.map(function(el, i) {{\n\
                var text = (el.textContent || '').trim();\n\
                return {{index: i, tag: el.tagName.toLowerCase(), text: text.length > 100 ? text.slice(0, 100) + '...' : text}};\n\
              }}));\n\
            }} catch (e) {{ return JSON.stringify({{error: e.message}}); }}\n\
          }},\n\
          selOne: function(parts) {{ var r = executeSelector(parts, document); return r.length > 0 ? r[0] : null; }},\n\
          selAll: function(parts) {{ return executeSelector(parts, document); }},\n\
          selCount: function(parts) {{ return executeSelector(parts, document).length; }},\n\
          clearAndDispatch: clearAndDispatch,\n\
          dispatchInputEvents: dispatchInputEvents,\n\
          clickGuard: clickGuard,\n\
          selectOption: selectOption,\n\
          getOptions: getOptions,\n\
          searchPage: searchPage,\n\
          findElementsCSS: findElementsCSS,\n\
          scrollInfo: scrollInfo,\n\
          suggestSelectors: suggestSelectors,\n\
          consoleErrors: consoleErrors,\n\
          waitForActionable: waitForActionable,\n\
          extractMarkdown: extractMarkdown,\n\
          dismissDialogs: dismissDialogs,\n\
          allElements: allElements,\n\
        }};\n\
        }})()"
    )
}

/// Build a lightweight query call (runtime must already be injected).
fn build_query_js(selector: &Selector) -> String {
    let parts_json = build_parts_json(selector);
    format!("window.__fd.sel({parts_json})")
}



/// Builds a JSON array of selector parts for the injected engine.
#[must_use]
pub fn build_parts_json(selector: &Selector) -> String {
    let parts: Vec<String> = selector.parts.iter().map(|p| {
        let engine = match p.engine {
            Engine::Css => "css",
            Engine::Text => "text",
            Engine::Role => "role",
            Engine::TestId => "testid",
            Engine::Label => "label",
            Engine::Placeholder => "placeholder",
            Engine::Alt => "alt",
            Engine::Title => "title",
            Engine::XPath => "xpath",
            Engine::Id => "id",
            Engine::Nth => "nth",
            Engine::Visible => "visible",
            Engine::Has => "has",
            Engine::HasText => "has-text",
            Engine::HasNot => "has-not",
            Engine::HasNotText => "has-not-text",
        };
        let body_escaped = serde_json::to_string(&p.body).unwrap_or_else(|_| format!("\"{}\"", p.body));
        format!(r#"{{"engine":"{engine}","body":{body_escaped}}}"#)
    }).collect();
    format!("[{}]", parts.join(","))
}

/// The injected JS engine -- all selector logic runs in one `evaluate()` call.
const ENGINE_JS: &str = r#"
// ── Whitespace normalization ──
function normalizeWS(s) {
    return (s || '').replace(/[\u200b\u00ad]/g, '').trim().replace(/\s+/g, ' ');
}

// ── Text content extraction with caching ──
var textCache = new Map();
function getElementText(el) {
    if (textCache.has(el)) return textCache.get(el);
    var result = {full: '', normalized: '', immediate: []};
    if (el.nodeName === 'SCRIPT' || el.nodeName === 'STYLE' || el.nodeName === 'NOSCRIPT') {
        textCache.set(el, result);
        return result;
    }
    if (el instanceof HTMLInputElement && (el.type === 'submit' || el.type === 'button')) {
        result = {full: el.value, normalized: normalizeWS(el.value), immediate: [el.value]};
        textCache.set(el, result);
        return result;
    }
    var currentImm = '';
    for (var child = el.firstChild; child; child = child.nextSibling) {
        if (child.nodeType === 3) {
            result.full += child.nodeValue || '';
            currentImm += child.nodeValue || '';
        } else if (child.nodeType === 1) {
            if (currentImm) { result.immediate.push(currentImm); currentImm = ''; }
            result.full += getElementText(child).full;
        }
    }
    if (currentImm) result.immediate.push(currentImm);
    if (el.shadowRoot) result.full += getElementText(el.shadowRoot).full;
    result.normalized = normalizeWS(result.full);
    textCache.set(el, result);
    return result;
}

// ── Text matching (Playwright-compatible) ──
function createTextMatcher(selector) {
    // Regex: /pattern/flags
    if (selector[0] === '/' && selector.lastIndexOf('/') > 0) {
        var li = selector.lastIndexOf('/');
        var re = new RegExp(selector.substring(1, li), selector.substring(li + 1));
        return {match: function(et) { return re.test(et.full); }, kind: 'regex'};
    }
    // Quoted: exact match
    if ((selector[0] === '"' && selector[selector.length-1] === '"') ||
        (selector[0] === "'" && selector[selector.length-1] === "'")) {
        var exact = selector.slice(1, -1);
        exact = normalizeWS(exact);
        return {match: function(et) { return et.normalized === exact; }, kind: 'strict'};
    }
    // Unquoted: case-insensitive substring
    var lower = normalizeWS(selector).toLowerCase();
    return {match: function(et) { return et.normalized.toLowerCase().includes(lower); }, kind: 'lax'};
}

// ── Accessible name computation (simplified but effective) ──
function getAccessibleName(el) {
    // aria-labelledby
    var labelledBy = el.getAttribute('aria-labelledby');
    if (labelledBy) {
        var names = labelledBy.split(/\s+/).map(function(id) {
            var refEl = document.getElementById(id);
            return refEl ? getElementText(refEl).normalized : '';
        }).filter(Boolean);
        if (names.length) return names.join(' ');
    }
    // aria-label
    var ariaLabel = el.getAttribute('aria-label');
    if (ariaLabel && ariaLabel.trim()) return normalizeWS(ariaLabel);
    // For inputs: associated <label>
    if (el.labels && el.labels.length) {
        return Array.from(el.labels).map(function(l) { return getElementText(l).normalized; }).join(' ');
    }
    // title attribute
    if (el.title) return normalizeWS(el.title);
    // Text content
    return getElementText(el).normalized;
}

// ── ARIA role computation ──
var implicitRoles = {
    A: function(e) { return e.hasAttribute('href') ? 'link' : null; },
    AREA: function(e) { return e.hasAttribute('href') ? 'link' : null; },
    ARTICLE: function() { return 'article'; },
    ASIDE: function() { return 'complementary'; },
    BUTTON: function() { return 'button'; },
    DATALIST: function() { return 'listbox'; },
    DETAILS: function() { return 'group'; },
    DIALOG: function() { return 'dialog'; },
    FIELDSET: function() { return 'group'; },
    FIGURE: function() { return 'figure'; },
    FOOTER: function(e) { return isLandmark(e) ? 'contentinfo' : null; },
    FORM: function(e) { return e.hasAttribute('aria-label') || e.hasAttribute('aria-labelledby') || e.hasAttribute('name') ? 'form' : null; },
    H1: function() { return 'heading'; }, H2: function() { return 'heading'; },
    H3: function() { return 'heading'; }, H4: function() { return 'heading'; },
    H5: function() { return 'heading'; }, H6: function() { return 'heading'; },
    HEADER: function(e) { return isLandmark(e) ? 'banner' : null; },
    HR: function() { return 'separator'; },
    IMG: function(e) { return (e.alt || e.getAttribute('alt') !== null) ? 'img' : 'presentation'; },
    INPUT: function(e) {
        var t = (e.type || 'text').toLowerCase();
        var map = {button:'button',checkbox:'checkbox',email:'textbox',image:'button',
            number:'spinbutton',radio:'radio',range:'slider',reset:'button',
            search:'searchbox',submit:'button',tel:'textbox',text:'textbox',url:'textbox'};
        return map[t] || 'textbox';
    },
    LI: function() { return 'listitem'; },
    MAIN: function() { return 'main'; },
    MATH: function() { return 'math'; },
    MENU: function() { return 'list'; },
    METER: function() { return 'meter'; },
    NAV: function() { return 'navigation'; },
    OL: function() { return 'list'; },
    OPTGROUP: function() { return 'group'; },
    OPTION: function() { return 'option'; },
    OUTPUT: function() { return 'status'; },
    P: function() { return 'paragraph'; },
    PROGRESS: function() { return 'progressbar'; },
    SECTION: function(e) { return e.hasAttribute('aria-label') || e.hasAttribute('aria-labelledby') ? 'region' : null; },
    SELECT: function(e) { return e.multiple || (e.size && e.size > 1) ? 'listbox' : 'combobox'; },
    SUMMARY: function() { return 'button'; },
    TABLE: function() { return 'table'; },
    TBODY: function() { return 'rowgroup'; },
    TD: function() { return 'cell'; },
    TEXTAREA: function() { return 'textbox'; },
    TFOOT: function() { return 'rowgroup'; },
    TH: function() { return 'columnheader'; },
    THEAD: function() { return 'rowgroup'; },
    TR: function() { return 'row'; },
    UL: function() { return 'list'; },
};
function isLandmark(e) {
    var p = e.parentElement;
    while (p) { if (['ARTICLE','ASIDE','MAIN','NAV','SECTION'].includes(p.tagName)) return false; p = p.parentElement; }
    return true;
}
function getAriaRole(el) {
    var explicit = el.getAttribute('role');
    if (explicit) return explicit.trim().split(/\s+/)[0].toLowerCase();
    var fn = implicitRoles[el.tagName];
    return fn ? fn(el) : null;
}

// ── Heading level ──
function getAriaLevel(el) {
    var level = el.getAttribute('aria-level');
    if (level) return parseInt(level, 10);
    var m = el.tagName.match(/^H(\d)$/);
    return m ? parseInt(m[1], 10) : 0;
}

// ── ARIA states ──
function getAriaChecked(el) {
    var v = el.getAttribute('aria-checked');
    if (v === 'true') return true;
    if (v === 'mixed') return 'mixed';
    if (v === 'false') return false;
    if (el instanceof HTMLInputElement && (el.type === 'checkbox' || el.type === 'radio'))
        return el.checked;
    return false;
}
function getAriaDisabled(el) {
    if (el.hasAttribute('aria-disabled')) return el.getAttribute('aria-disabled') === 'true';
    return el.disabled === true;
}
function getAriaExpanded(el) {
    var v = el.getAttribute('aria-expanded');
    if (v === 'true') return true;
    if (v === 'false') return false;
    return undefined;
}
function getAriaSelected(el) {
    var v = el.getAttribute('aria-selected');
    if (v === 'true') return true;
    if (v === 'false') return false;
    return undefined;
}
function getAriaPressed(el) {
    var v = el.getAttribute('aria-pressed');
    if (v === 'true') return true;
    if (v === 'mixed') return 'mixed';
    if (v === 'false') return false;
    return undefined;
}

// ── Visibility check ──
function isVisible(el) {
    if (!el.offsetParent && el.tagName !== 'BODY' && el.tagName !== 'HTML' &&
        getComputedStyle(el).position !== 'fixed' && getComputedStyle(el).position !== 'sticky')
        return false;
    var style = getComputedStyle(el);
    if (style.visibility === 'hidden' || style.display === 'none' || style.opacity === '0')
        return false;
    return true;
}

// ── Engine implementations ──
var engines = {
    css: function(roots, body) {
        var results = [];
        roots.forEach(function(root) {
            var scope = root === document ? document : root;
            try { results.push.apply(results, Array.from(scope.querySelectorAll(body))); } catch(e) {}
            // Pierce shadow DOM
            if (root !== document) {
                if (root.shadowRoot) {
                    try { results.push.apply(results, Array.from(root.shadowRoot.querySelectorAll(body))); } catch(e) {}
                }
            }
        });
        return results;
    },
    text: function(roots, body) {
        var matcher = createTextMatcher(body);
        var results = [];
        roots.forEach(function(root) {
            var scope = root === document ? document.body : root;
            if (!scope) return;
            var all = allElements(scope);
            for (var i = 0; i < all.length; i++) {
                var et = getElementText(all[i]);
                if (matcher.match(et)) {
                    // For lax: prefer deepest match (avoid parent + child duplicates)
                    if (matcher.kind === 'lax') {
                        var dominated = false;
                        for (var j = results.length - 1; j >= 0; j--) {
                            if (results[j].contains(all[i])) { dominated = true; break; }
                            if (all[i].contains(results[j])) { results.splice(j, 1); }
                        }
                        if (!dominated) results.push(all[i]);
                    } else {
                        results.push(all[i]);
                    }
                }
            }
        });
        return results;
    },
    role: function(roots, body) {
        // Parse: role_name[attr=value][attr2=value2]
        var m = body.match(/^([a-z]+)(.*)/);
        if (!m) return [];
        var role = m[1];
        var attrStr = m[2];
        var opts = {};
        var re = /\[([a-z-]+)(?:=(?:"([^"]*)"|'([^']*)'|([^\]]*)))?\]/gi;
        var am;
        while ((am = re.exec(attrStr)) !== null) {
            var k = am[1].toLowerCase();
            var v = am[2] !== undefined ? am[2] : am[3] !== undefined ? am[3] : am[4] !== undefined ? am[4] : true;
            if (v === 'true') v = true;
            if (v === 'false') v = false;
            if (k === 'level') v = parseInt(v, 10);
            opts[k] = v;
        }
        var results = [];
        roots.forEach(function(root) {
            var scope = root === document ? document.body : root;
            if (!scope) return;
            var all = allElements(scope);
            for (var i = 0; i < all.length; i++) {
                var el = all[i];
                if (getAriaRole(el) !== role) continue;
                if (opts.checked !== undefined && getAriaChecked(el) !== opts.checked) continue;
                if (opts.pressed !== undefined && getAriaPressed(el) !== opts.pressed) continue;
                if (opts.selected !== undefined && getAriaSelected(el) !== opts.selected) continue;
                if (opts.expanded !== undefined && getAriaExpanded(el) !== opts.expanded) continue;
                if (opts.level !== undefined && getAriaLevel(el) !== opts.level) continue;
                if (opts.disabled !== undefined && getAriaDisabled(el) !== opts.disabled) continue;
                if (opts['include-hidden'] !== true && !isVisible(el)) continue;
                if (opts.name !== undefined) {
                    var name = getAccessibleName(el);
                    var target = normalizeWS(String(opts.name));
                    // Substring match by default (like Playwright internal:role)
                    if (!name.toLowerCase().includes(target.toLowerCase())) continue;
                }
                results.push(el);
            }
        });
        return results;
    },
    testid: function(roots, body) {
        return engines.css(roots, '[data-testid=' + JSON.stringify(body) + ']');
    },
    id: function(roots, body) {
        return engines.css(roots, '[id=' + JSON.stringify(body) + ']');
    },
    label: function(roots, body) {
        var matcher = createTextMatcher(body);
        var results = [];
        roots.forEach(function(root) {
            var scope = root === document ? document : root;
            // Find by aria-label (pierces shadow DOM)
            var all = allElements(scope);
            for (var i = 0; i < all.length; i++) {
                var el = all[i];
                // Check aria-label
                var ariaLabel = el.getAttribute('aria-label');
                if (ariaLabel && matcher.match({full: ariaLabel, normalized: normalizeWS(ariaLabel), immediate: [ariaLabel]})) {
                    results.push(el);
                    continue;
                }
                // Check associated <label>
                if (el.labels && el.labels.length) {
                    for (var j = 0; j < el.labels.length; j++) {
                        var lt = getElementText(el.labels[j]);
                        if (matcher.match(lt)) { results.push(el); break; }
                    }
                }
            }
        });
        return results;
    },
    placeholder: function(roots, body) {
        return engines.css(roots, '[placeholder=' + JSON.stringify(body) + ']');
    },
    alt: function(roots, body) {
        return engines.css(roots, '[alt=' + JSON.stringify(body) + ']');
    },
    title: function(roots, body) {
        return engines.css(roots, '[title=' + JSON.stringify(body) + ']');
    },
    xpath: function(roots, body) {
        var results = [];
        roots.forEach(function(root) {
            var doc = root.ownerDocument || root;
            var iter = doc.evaluate(body, root === document ? doc : root, null, XPathResult.ORDERED_NODE_ITERATOR_TYPE, null);
            var node;
            while ((node = iter.iterateNext()) !== null) {
                if (node.nodeType === 1) results.push(node);
            }
        });
        return results;
    },
    nth: function(roots, body) {
        var idx = parseInt(body, 10);
        var arr = Array.from(roots);
        if (idx < 0) idx = arr.length + idx;
        return (idx >= 0 && idx < arr.length) ? [arr[idx]] : [];
    },
    visible: function(roots, body) {
        var want = body === 'true';
        return Array.from(roots).filter(function(el) { return isVisible(el) === want; });
    },
    'has-text': function(roots, body) {
        var matcher = createTextMatcher(body);
        return Array.from(roots).filter(function(el) {
            return matcher.match(getElementText(el));
        });
    },
    'has-not-text': function(roots, body) {
        var matcher = createTextMatcher(body);
        return Array.from(roots).filter(function(el) {
            return !matcher.match(getElementText(el));
        });
    },
    has: function(roots, body) {
        // body is a mini-selector to check for descendants
        return Array.from(roots).filter(function(el) {
            try { return el.querySelector(body) !== null; } catch(e) { return false; }
        });
    },
    'has-not': function(roots, body) {
        return Array.from(roots).filter(function(el) {
            try { return el.querySelector(body) === null; } catch(e) { return true; }
        });
    },
};

// ── Pipeline executor ──
function executeSelector(parts, root) {
    textCache = new Map();
    var roots = [root];
    for (var i = 0; i < parts.length; i++) {
        var part = parts[i];
        var eng = engines[part.engine];
        if (!eng) throw new Error('Unknown selector engine: ' + part.engine);
        roots = eng(roots, part.body);
        if (roots.length === 0) return [];
    }
    return roots;
}

// ══════════════════════════════════════════════════════════════════════════════
// ── Interaction helpers (called via element.call_js_fn or page.evaluate) ──
// ══════════════════════════════════════════════════════════════════════════════

// Clear an input's value and dispatch input+change events (for fill)
function clearAndDispatch(el) {
    try { el.select(); } catch(e) {}
    el.value = '';
    el.dispatchEvent(new Event('input', {bubbles: true}));
    el.dispatchEvent(new Event('change', {bubbles: true}));
}

// Dispatch input+change events (after typing)
function dispatchInputEvents(el) {
    el.dispatchEvent(new Event('input', {bubbles: true}));
    el.dispatchEvent(new Event('change', {bubbles: true}));
}

// Check element tag for click guard -- returns '' | 'select' | 'file'
function clickGuard(el) {
    if (el.tagName === 'SELECT') return 'select';
    if (el.tagName === 'INPUT' && el.type === 'file') return 'file';
    return '';
}

// Select a dropdown option by text/value. Returns JSON string.
function selectOption(el, target) {
    if (el.tagName !== 'SELECT')
        return JSON.stringify({error: 'Not a <select> element'});
    var options = Array.from(el.options);
    var tl = target.toLowerCase();
    for (var i = 0; i < options.length; i++) {
        var opt = options[i];
        if (opt.text.trim().toLowerCase() === tl || opt.value.toLowerCase() === tl) {
            el.value = opt.value;
            el.dispatchEvent(new Event('change', {bubbles: true}));
            el.dispatchEvent(new Event('input', {bubbles: true}));
            return JSON.stringify({selected: opt.text.trim(), value: opt.value});
        }
    }
    var available = options.map(function(o) { return o.text.trim(); });
    return JSON.stringify({error: 'Option not found', available: available});
}

// Get all dropdown options. Returns JSON string.
function getOptions(el) {
    if (el.tagName !== 'SELECT')
        return JSON.stringify({error: 'Not a <select> element'});
    var opts = Array.from(el.options).map(function(o, i) {
        return {index: i, text: o.text.trim(), value: o.value, selected: o.selected};
    });
    return JSON.stringify({options: opts});
}

// Search page text. Returns JSON string.
function searchPage(pattern, isRegex, caseSensitive, contextChars, cssScope, maxResults) {
    try {
        var scope = cssScope ? document.querySelector(cssScope) : document.body;
        if (!scope) return JSON.stringify({error: 'CSS scope not found', matches: [], total: 0});
        var walker = document.createTreeWalker(scope, NodeFilter.SHOW_TEXT);
        var fullText = '';
        var nodeOffsets = [];
        while (walker.nextNode()) {
            var node = walker.currentNode;
            var text = node.textContent;
            if (text && text.trim()) {
                nodeOffsets.push({offset: fullText.length, length: text.length, node: node});
                fullText += text;
            }
        }
        var re;
        try {
            var flags = caseSensitive ? 'g' : 'gi';
            if (isRegex) re = new RegExp(pattern, flags);
            else re = new RegExp(pattern.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), flags);
        } catch (e) {
            return JSON.stringify({error: 'Invalid regex: ' + e.message, matches: [], total: 0});
        }
        var matches = [];
        var match;
        var totalFound = 0;
        while ((match = re.exec(fullText)) !== null) {
            totalFound++;
            if (matches.length < maxResults) {
                var start = Math.max(0, match.index - contextChars);
                var end = Math.min(fullText.length, match.index + match[0].length + contextChars);
                var context = fullText.slice(start, end);
                var elementPath = '';
                for (var i = 0; i < nodeOffsets.length; i++) {
                    var no = nodeOffsets[i];
                    if (no.offset <= match.index && no.offset + no.length > match.index) {
                        var parts = [];
                        var cur = no.node.parentElement;
                        while (cur && cur !== document.body && cur !== document) {
                            var desc = cur.tagName ? cur.tagName.toLowerCase() : '';
                            if (!desc) break;
                            if (cur.id) desc += '#' + cur.id;
                            else if (cur.className && typeof cur.className === 'string') {
                                var cls = cur.className.trim().split(/\s+/).slice(0,2).join('.');
                                if (cls) desc += '.' + cls;
                            }
                            parts.unshift(desc);
                            cur = cur.parentElement;
                        }
                        elementPath = parts.join(' > ');
                        break;
                    }
                }
                matches.push({
                    match_text: match[0],
                    context: (start > 0 ? '...' : '') + context + (end < fullText.length ? '...' : ''),
                    element_path: elementPath,
                    char_position: match.index
                });
            }
            if (match[0].length === 0) re.lastIndex++;
        }
        return JSON.stringify({matches: matches, total: totalFound, has_more: totalFound > maxResults});
    } catch (e) {
        return JSON.stringify({error: 'search error: ' + e.message, matches: [], total: 0});
    }
}

// Query DOM elements by CSS selector. Returns JSON string.
function findElementsCSS(selector, attributes, maxResults, includeText) {
    try {
        var elements;
        try { elements = document.querySelectorAll(selector); }
        catch (e) { return JSON.stringify({error: 'Invalid CSS selector: ' + e.message, elements: [], total: 0}); }
        var total = elements.length;
        var limit = Math.min(total, maxResults);
        var results = [];
        for (var i = 0; i < limit; i++) {
            var el = elements[i];
            var item = {index: i, tag: el.tagName.toLowerCase()};
            if (includeText) {
                var text = (el.textContent || '').trim();
                item.text = text.length > 300 ? text.slice(0, 300) + '...' : text;
            }
            if (attributes && attributes.length > 0) {
                item.attrs = {};
                for (var j = 0; j < attributes.length; j++) {
                    var attrName = attributes[j];
                    var val;
                    if ((attrName === 'src' || attrName === 'href') && typeof el[attrName] === 'string' && el[attrName] !== '')
                        val = el[attrName];
                    else
                        val = el.getAttribute(attrName);
                    if (val !== null)
                        item.attrs[attrName] = val.length > 500 ? val.slice(0, 500) + '...' : val;
                }
            }
            item.children_count = el.children.length;
            results.push(item);
        }
        return JSON.stringify({elements: results, total: total, showing: limit});
    } catch (e) {
        return JSON.stringify({error: 'find error: ' + e.message, elements: [], total: 0});
    }
}

// Get scroll position info. Returns JSON string.
function scrollInfo() {
    return JSON.stringify({
        scrollY: Math.round(window.scrollY),
        scrollHeight: document.body ? document.body.scrollHeight : 0,
        viewportHeight: window.innerHeight
    });
}

// Suggest available selectors on the page. Returns JSON string.
function suggestSelectors() {
    var ids = [];
    document.querySelectorAll('[id]').forEach(function(e, i) { if (i < 10) ids.push('#' + e.id); });
    var inputs = [];
    document.querySelectorAll('input,button,select,textarea,a').forEach(function(e, i) {
        if (i >= 10) return;
        if (e.id) inputs.push('#' + e.id);
        else if (e.name) inputs.push(e.tagName.toLowerCase() + '[name="' + e.name + '"]');
        else if (e.className) inputs.push(e.tagName.toLowerCase() + '.' + e.className.split(' ')[0]);
        else inputs.push(e.tagName.toLowerCase());
    });
    return JSON.stringify({ids: ids, inputs: inputs});
}

// Console error interceptor + counter. Idempotent. Returns error count.
function consoleErrors() {
    if (!window.__fd_errs) {
        window.__fd_errs = [];
        var orig = console.error;
        console.error = function() {
            window.__fd_errs.push(Array.from(arguments).map(String).join(' '));
            orig.apply(console, arguments);
        };
    }
    return window.__fd_errs.length;
}

// ══════════════════════════════════════════════════════════════════════════════
// ── Shadow DOM helper ──
// ══════════════════════════════════════════════════════════════════════════════

// Recursively collect all elements including those inside shadow roots.
function allElements(root) {
    var result = [];
    var seen = new Set();
    function walk(node) {
        var children;
        if (node.shadowRoot) {
            children = node.shadowRoot.querySelectorAll('*');
            for (var i = 0; i < children.length; i++) {
                if (!seen.has(children[i])) {
                    seen.add(children[i]);
                    result.push(children[i]);
                    walk(children[i]);
                }
            }
        }
        children = node.querySelectorAll ? node.querySelectorAll('*') : [];
        for (var j = 0; j < children.length; j++) {
            if (!seen.has(children[j])) {
                seen.add(children[j]);
                result.push(children[j]);
                walk(children[j]);
            }
        }
    }
    walk(root === document ? document.body || document.documentElement : root);
    return result;
}

// ══════════════════════════════════════════════════════════════════════════════
// ── Auto-waiting (Playwright-style RAF stability + visibility + enabled) ──
// ══════════════════════════════════════════════════════════════════════════════

function waitForActionable(el, timeout) {
    return new Promise(function(resolve, reject) {
        var deadline = Date.now() + (timeout || 5000);
        var lastRect = null;
        var stableCount = 0;

        function check() {
            if (Date.now() > deadline) { reject(new Error('Timeout: element not actionable')); return; }
            if (!el.isConnected) { reject(new Error('Element detached from DOM')); return; }

            // Visible check
            var style = getComputedStyle(el);
            if (style.display === 'none' || style.visibility === 'hidden' || style.opacity === '0') {
                setTimeout(check, 100); return;
            }
            if (!el.offsetParent && style.position !== 'fixed' && style.position !== 'sticky'
                && el.tagName !== 'BODY' && el.tagName !== 'HTML') {
                setTimeout(check, 100); return;
            }

            // Enabled check
            if (el.disabled) { setTimeout(check, 100); return; }

            // Stability check: compare bounding box across 2 measurements.
            // Uses setTimeout fallback (RAF doesn't fire for background tabs in headless).
            var measure = function(cb) {
                var done = false;
                // Try RAF first
                requestAnimationFrame(function() { if (!done) { done = true; cb(); } });
                // Fallback: if RAF doesn't fire within 100ms, use setTimeout
                setTimeout(function() { if (!done) { done = true; cb(); } }, 100);
            };

            measure(function() {
                if (!el.isConnected) { reject(new Error('Element detached')); return; }
                var r = el.getBoundingClientRect();
                var rect = {x: r.x, y: r.y, w: r.width, h: r.height};
                if (lastRect && rect.x === lastRect.x && rect.y === lastRect.y
                    && rect.w === lastRect.w && rect.h === lastRect.h) {
                    stableCount++;
                    if (stableCount >= 2) { resolve(); return; }
                } else {
                    stableCount = 0;
                }
                lastRect = rect;
                measure(function() {
                    if (!el.isConnected) { reject(new Error('Element detached')); return; }
                    var r2 = el.getBoundingClientRect();
                    if (r2.x === rect.x && r2.y === rect.y && r2.width === rect.w && r2.height === rect.h) {
                        resolve(); return;
                    }
                    setTimeout(check, 50);
                });
            });
        }
        check();
    });
}

// ══════════════════════════════════════════════════════════════════════════════
// ── Markdown extraction ──
// ══════════════════════════════════════════════════════════════════════════════

function extractMarkdown() {
    var clone = document.body.cloneNode(true);
    // Remove noise elements
    clone.querySelectorAll('script,style,noscript,link,meta,svg,template').forEach(function(e) { e.remove(); });

    function convertTable(table) {
        var rows = table.querySelectorAll('tr');
        if (!rows.length) return '';
        var lines = [];
        rows.forEach(function(row, ri) {
            var cells = row.querySelectorAll('th,td');
            var line = '| ' + Array.from(cells).map(function(c) { return c.textContent.trim().replace(/\|/g, '\\|'); }).join(' | ') + ' |';
            lines.push(line);
            if (ri === 0) {
                lines.push('| ' + Array.from(cells).map(function() { return '---'; }).join(' | ') + ' |');
            }
        });
        return lines.join('\n');
    }

    function convert(node) {
        if (node.nodeType === 3) return node.textContent || '';
        if (node.nodeType !== 1) return '';
        var tag = node.tagName;
        if (tag === 'BR') return '\n';
        if (tag === 'HR') return '\n---\n';
        if (tag === 'IMG') {
            var alt = node.getAttribute('alt') || '';
            var src = node.getAttribute('src') || '';
            return src ? '![' + alt + '](' + src + ')' : '';
        }

        var childText = Array.from(node.childNodes).map(convert).join('');
        childText = childText.trim();
        if (!childText && !['BR','HR','IMG'].includes(tag)) return '';

        // Headings
        if (/^H[1-6]$/.test(tag)) return '\n' + '#'.repeat(parseInt(tag[1])) + ' ' + childText + '\n\n';
        // Paragraphs
        if (tag === 'P') return childText + '\n\n';
        // Lists
        if (tag === 'LI') {
            var parent = node.parentElement;
            var prefix = (parent && parent.tagName === 'OL') ?
                (Array.from(parent.children).indexOf(node) + 1) + '. ' : '- ';
            return prefix + childText + '\n';
        }
        if (tag === 'UL' || tag === 'OL') return '\n' + childText + '\n';
        // Links
        if (tag === 'A') {
            var href = node.getAttribute('href') || '';
            return href ? '[' + childText + '](' + href + ')' : childText;
        }
        // Formatting
        if (tag === 'STRONG' || tag === 'B') return '**' + childText + '**';
        if (tag === 'EM' || tag === 'I') return '*' + childText + '*';
        if (tag === 'CODE') return '`' + childText + '`';
        if (tag === 'PRE') return '\n```\n' + (node.textContent || '').trim() + '\n```\n\n';
        // Tables
        if (tag === 'TABLE') return '\n' + convertTable(node) + '\n\n';
        if (tag === 'THEAD' || tag === 'TBODY' || tag === 'TFOOT' || tag === 'TR' ||
            tag === 'TH' || tag === 'TD') return childText;
        // Block elements
        if (['DIV','SECTION','ARTICLE','MAIN','ASIDE','HEADER','FOOTER','NAV','FORM','BLOCKQUOTE'].includes(tag)) {
            if (tag === 'BLOCKQUOTE') return childText.split('\n').map(function(l) { return '> ' + l; }).join('\n') + '\n\n';
            return childText + '\n';
        }
        return childText;
    }

    var md = convert(clone);
    // Collapse excessive whitespace
    md = md.replace(/\n{3,}/g, '\n\n').trim();
    // Remove lines that look like JSON blobs
    md = md.split('\n').filter(function(line) {
        var s = line.trim();
        if ((s[0] === '{' || s[0] === '[') && s.length > 200) return false;
        return true;
    }).join('\n');
    return md;
}

// ══════════════════════════════════════════════════════════════════════════════
// ── Dialog override for WebKit (no CDP event support) ──
// ══════════════════════════════════════════════════════════════════════════════

function dismissDialogs() {
    if (window.__fd_dialogs_installed) return;
    window.__fd_dialogs_installed = true;
    window.__fd_dialog_log = window.__fd_dialog_log || [];
    var origAlert = window.alert;
    var origConfirm = window.confirm;
    var origPrompt = window.prompt;
    window.alert = function(msg) { window.__fd_dialog_log.push({type:'alert',message:String(msg||''),action:'accepted'}); };
    window.confirm = function(msg) { window.__fd_dialog_log.push({type:'confirm',message:String(msg||''),action:'accepted'}); return true; };
    window.prompt = function(msg) { window.__fd_dialog_log.push({type:'prompt',message:String(msg||''),action:'dismissed'}); return null; };
}
"#;

// ─── Query functions ────────────────────────────────────────────────────────

/// Query all elements matching a rich selector. Returns lightweight info.
/// Injects the engine JS on first use, then subsequent calls are lightweight.
///
/// # Errors
///
/// Returns an error if selector parsing or JS evaluation fails.
pub async fn query_all(
    page: &AnyPage,
    selector: &str,
) -> Result<Vec<MatchedElement>, String> {
    let parsed = parse(selector)?;
    // Ensure engine is injected (idempotent)
    let js = build_query_js(&parsed);
    let result_str = page.evaluate(&js).await?
        .and_then(|v| v.as_str().map(std::string::ToString::to_string))
        .unwrap_or_else(|| "[]".into());

    // Check for error
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&result_str) {
        if let Some(err) = val.get("error").and_then(|e| e.as_str()) {
            return Err(err.to_string());
        }
    }

    let elements: Vec<MatchedElement> = serde_json::from_str(&result_str)
        .map_err(|e| format!("Parse selector results: {e}"))?;
    Ok(elements)
}

/// Query a single element. If strict=true, errors when 0 or >1 matches.
///
/// # Errors
///
/// Returns an error if selector parsing fails, no element is found, or (in strict mode)
/// multiple elements match.
pub async fn query_one(
    page: &AnyPage,
    selector: &str,
    strict: bool,
) -> Result<AnyElement, String> {
    let parsed = parse(selector)?;
    let parts_json = build_parts_json(&parsed);

    if strict {
        // Strict mode: need count check, use the full query_all path
        let matches = query_all(page, selector).await?;
        if matches.is_empty() {
            return Err(format!("No element found for selector: {selector}"));
        }
        if matches.len() > 1 {
            cleanup_tags(page).await;
            return Err(format!(
                "Selector \"{selector}\" resolved to {} elements. Use a more specific selector.",
                matches.len()
            ));
        }
        let el = page.find_element("[data-fd-sel='0']").await
            .map_err(|_| format!("Could not resolve matched element for: {selector}"))?;
        cleanup_tags(page).await;
        return Ok(el);
    }

    // Fast path: engine already injected via addScriptToEvaluateOnNewDocument.
    // Returns the DOM element directly (no tagging/cleanup).
    let js = format!("window.__fd.selOne({parts_json})");

    page.evaluate_to_element(&js).await
        .map_err(|_| format!("No element found for selector: {selector}"))
}

/// Clean up any leftover selector tags (call after operations).
pub async fn cleanup_tags(page: &AnyPage) {
    let _ = page.evaluate("(function() { \
        document.querySelectorAll('[data-fd-sel]').forEach(function(e) { \
            e.removeAttribute('data-fd-sel'); \
        }); \
    })()").await;
}
