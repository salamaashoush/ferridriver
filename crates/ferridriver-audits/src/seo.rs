//! SEO page-quality audits: meta description, canonical, crawlable anchors, link text, status, and crawlability.

use crate::{Anchor, Artifacts, AuditCategory, AuditItem, AuditResult, MainDocument, failed, passed, robots};

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

/// `canonical`: the document names one valid, absolute canonical URL.
///
/// Lighthouse scores this only in navigation mode, because it resolves
/// the canonical against the MAIN RESOURCE's url rather than the
/// document's. For a page that did not redirect the two are the same,
/// which is every fixture here; a redirect would need the network log.
///
/// Ported from `core/audits/seo/canonical.js`, whose three helpers this
/// keeps as three branches: an unusable href, then the conflict and
/// count checks, then the two common mistakes.
#[must_use]
pub fn canonical(artifacts: &Artifacts) -> AuditResult {
  const ID: &str = "canonical";
  const PASS: &str = "Document has a valid `rel=canonical`";
  const FAIL: &str = "Document does not have a valid `rel=canonical`";
  let category = AuditCategory::Seo;

  let mut unique: Vec<String> = Vec::new();
  let mut hreflangs: Vec<String> = Vec::new();
  let mut invalid: Option<&str> = None;
  let mut relative: Option<&str> = None;

  for link in &artifacts.links {
    if link.rel == "canonical" {
      if link.href_raw.is_empty() {
        continue;
      }
      if link.href.is_empty() {
        invalid = Some(&link.href_raw);
        continue;
      }
      // Upstream's test is which of two parses succeed: unparseable
      // even against a base is invalid, parseable only against a base
      // is relative, and parseable alone is the one it keeps.
      if resolve(&link.href_raw, "https://example.com").is_none() {
        invalid = Some(&link.href_raw);
      } else if url::Url::parse(&link.href_raw).is_err() {
        relative = Some(&link.href_raw);
      } else if !unique.contains(&link.href) {
        unique.push(link.href.clone());
      }
    } else if link.rel == "alternate" && !link.href.is_empty() && !link.hreflang.is_empty() {
      hreflangs.push(link.href.clone());
    }
  }

  if let Some(href) = invalid {
    return failed(ID, category, FAIL, Some(&format!("Invalid URL ({href})")), Vec::new());
  }
  if let Some(href) = relative {
    return failed(ID, category, FAIL, Some(&format!("Relative URL ({href})")), Vec::new());
  }
  if unique.is_empty() {
    // Nothing to look at. Lighthouse reports this as notApplicable, and
    // the recorder drops those, so the comparison skips it.
    return passed(ID, category, PASS);
  }
  if unique.len() > 1 {
    return failed(
      ID,
      category,
      FAIL,
      Some(&format!("Multiple conflicting URLs ({})", unique.join(", "))),
      Vec::new(),
    );
  }

  let (Some(canonical), Some(base)) = (url::Url::parse(&unique[0]).ok(), url::Url::parse(&artifacts.url).ok()) else {
    return passed(ID, category, PASS);
  };
  // Two pages that name each other as alternates cannot also name one
  // of them as the canonical of the other.
  if hreflangs.iter().any(|h| h == base.as_str())
    && hreflangs.iter().any(|h| h == canonical.as_str())
    && base != canonical
  {
    return failed(
      ID,
      category,
      FAIL,
      Some("Points to another `hreflang` location"),
      Vec::new(),
    );
  }
  if canonical.origin() == base.origin() && canonical.path() == "/" && base.path() != "/" {
    return failed(ID, category, FAIL, Some("Points to the domain's root URL"), Vec::new());
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

pub(crate) fn anchor_is_uncrawlable(anchor: &Anchor, document_url: &str) -> bool {
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
pub(crate) fn is_javascript_void(raw_href: &str) -> bool {
  raw_href.match_indices("javascript:void").any(|(at, needle)| {
    raw_href
      .get(at + needle.len()..)
      .is_some_and(|rest| rest.strip_prefix('(').unwrap_or(rest).starts_with('0'))
  })
}

/// `new URL(href, base)`: absolute, or relative to the document.
fn resolve(href: &str, base: &str) -> Option<url::Url> {
  if let Ok(absolute) = url::Url::parse(href) {
    return Some(absolute);
  }
  url::Url::parse(base).ok()?.join(href).ok()
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

pub(crate) fn link_text_is_non_descriptive(anchor: &Anchor, document_url: &str) -> bool {
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
  let (Ok(mut a), Ok(mut b)) = (url::Url::parse(a), url::Url::parse(b)) else {
    return false;
  };
  a.set_fragment(None);
  b.set_fragment(None);
  a == b
}

/// `http-status-code`: the main document did not answer 4xx or 5xx.
///
/// `None` when no main-document response is known, which is the whole
/// of the difficulty: the arithmetic is a range check, and the status
/// is the part that has to be carried here from the network log.
///
/// Ported from `core/audits/seo/http-status-code.js`.
#[must_use]
pub fn http_status_code(artifacts: &Artifacts) -> Option<AuditResult> {
  const ID: &str = "http-status-code";
  const PASS: &str = "Page has successful HTTP status code";
  const FAIL: &str = "Page has unsuccessful HTTP status code";
  let category = AuditCategory::Seo;

  let status = artifacts.main_document.as_ref()?.status_code;
  Some(if (400..=599).contains(&status) {
    failed(ID, category, FAIL, Some(&status.to_string()), Vec::new())
  } else {
    passed(ID, category, PASS)
  })
}

/// The crawlers `is-crawlable` asks about. `None` is the generic one,
/// which reads the `robots` meta and an unprefixed `X-Robots-Tag`.
pub(crate) const BOT_USER_AGENTS: [Option<&str>; 5] = [
  None,
  Some("Googlebot"),
  Some("bingbot"),
  Some("DuckDuckBot"),
  Some("archive.org_bot"),
];

/// Directives that keep a page out of an index.
const BLOCKLIST: [&str; 2] = ["noindex", "none"];

const ROBOTS_HEADER: &str = "x-robots-tag";
const UNAVAILABLE_AFTER: &str = "unavailable_after";

/// `is-crawlable`: at least one crawler may still index the page.
///
/// Three sources can block, and the audit only fails when ALL FIVE bots
/// are blocked by at least one of them: a `robots` meta (or one named
/// for a specific bot), an `X-Robots-Tag` response header (optionally
/// prefixed with a user agent), and `/robots.txt`. Reporting only the
/// meta would pass pages the other two block, silently, which is why
/// this waited for the response headers and the fetch to exist.
///
/// `None` when no main-document response is known: two of the three
/// sources are then unreadable, and a verdict from the third alone is
/// the wrong answer rather than a partial one.
///
/// Ported from `core/audits/seo/is-crawlable.js`; the `robots.txt`
/// half is [`robots`].
#[must_use]
pub fn is_crawlable(artifacts: &Artifacts) -> Option<AuditResult> {
  const ID: &str = "is-crawlable";
  const PASS: &str = "Page isn’t blocked from indexing";
  const FAIL: &str = "Page is blocked from indexing";
  let category = AuditCategory::Seo;

  let main = artifacts.main_document.as_ref()?;
  let parsed = artifacts
    .robots_txt
    .as_ref()
    .and_then(|file| file.content.as_ref())
    .zip(robots_txt_url(&main.url))
    .map(|(content, url)| (robots::Robots::parse(&url, content), url));

  let mut blocked = 0usize;
  let mut generic = Vec::new();
  for user_agent in BOT_USER_AGENTS {
    let directives = blocking_directives(user_agent, artifacts, main, parsed.as_ref());
    if !directives.is_empty() {
      blocked += 1;
    }
    if user_agent.is_none() {
      generic = directives;
    }
  }

  Some(if blocked == BOT_USER_AGENTS.len() {
    failed(ID, category, FAIL, None, generic)
  } else {
    passed(ID, category, PASS)
  })
}

/// Everything blocking `user_agent`, as the elements and lines that did it.
fn blocking_directives(
  user_agent: Option<&str>,
  artifacts: &Artifacts,
  main: &MainDocument,
  robots: Option<&(robots::Robots, String)>,
) -> Vec<AuditItem> {
  let mut items = Vec::new();

  // A meta named for this bot wins over the generic one, and only one
  // of the two is consulted -- so `<meta name="googlebot" content="all">`
  // lifts a `<meta name="robots" content="noindex">` for Googlebot.
  let named = user_agent.and_then(|agent| {
    let lowered = agent.to_lowercase();
    artifacts.metas.iter().find(|meta| meta.name == lowered)
  });
  if let Some(meta) = named.or_else(|| artifacts.metas.iter().find(|meta| meta.name == "robots"))
    && has_blocking_directive(&meta.content)
  {
    items.push(AuditItem {
      selector: meta.selector.clone(),
      snippet: format!("<meta name=\"{}\" content=\"{}\" />", meta.name, meta.content),
      detail: meta.content.clone(),
    });
  }

  for header in &main.response_headers {
    if !header.name.eq_ignore_ascii_case(ROBOTS_HEADER) {
      continue;
    }
    // A prefix naming a DIFFERENT bot is not this bot's business; no
    // prefix at all applies to every bot.
    let prefix = header_directive_user_agent(&header.value);
    if prefix.is_some() && prefix != user_agent.map(str::to_string) {
      continue;
    }
    let directives = match user_agent {
      Some(agent) if header.value.starts_with(&format!("{agent}:")) => {
        header.value.replacen(&format!("{agent}:"), "", 1)
      },
      _ => header.value.trim().to_string(),
    };
    if !has_blocking_directive(&directives) {
      continue;
    }
    items.push(AuditItem {
      selector: String::new(),
      snippet: format!("{}: {}", header.name, header.value),
      detail: header.value.clone(),
    });
  }

  if let Some((robots, url)) = robots
    // A URL this file does not govern answers `None`, which the audit
    // reads as blocking -- upstream negates `undefined` and lands in
    // the same place.
    && robots.is_allowed(&main.url, user_agent) != Some(true)
  {
    // Looked up against the generic agent's rules whichever bot was
    // asked about, matching upstream's argument-less call.
    let line = robots.matching_line_number(&main.url, None);
    items.push(AuditItem {
      selector: String::new(),
      snippet: url.clone(),
      detail: format!("line {line}"),
    });
  }

  items
}

/// `<meta name="robots" content="noindex, nofollow">`: any one directive
/// in the list can block.
fn has_blocking_directive(directives: &str) -> bool {
  directives.split(',').map(str::to_lowercase).any(|directive| {
    let directive = directive.trim();
    BLOCKLIST.contains(&directive) || is_unavailable(directive)
  })
}

/// `unavailable_after: <date>` blocks once that date has passed.
pub(crate) fn is_unavailable(directive: &str) -> bool {
  let Some((field, date)) = directive.split_once(':') else {
    return false;
  };
  if field != UNAVAILABLE_AFTER {
    return false;
  }
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX));
  parse_directive_date(date).is_some_and(|when| when < now)
}

/// Seconds since the epoch for the date formats `unavailable_after` is
/// written in.
///
/// `Date.parse`, which is what upstream calls, is specified only for
/// ISO 8601; every engine layers its own dialect on top and none of
/// that is portable. Both formats anyone writes are here: ISO 8601, and
/// the RFC 1123 shape Google's own documentation uses
/// (`25 Jun 2010 15:00:00 PST`), with the weekday optional. Anything
/// else answers `None`, and an unparseable date does not block
/// upstream either.
///
/// A date carrying no zone is read as UTC. `Date.parse` reads it as
/// local time, which would make the verdict depend on the machine's
/// timezone; the difference can only matter for a date within a day of
/// now.
pub(crate) fn parse_directive_date(value: &str) -> Option<i64> {
  let value = value.trim().to_lowercase();
  parse_iso_8601(&value).or_else(|| parse_rfc_1123(&value))
}

const MONTHS: [&str; 12] = [
  "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// `2010-06-25`, optionally `T15:00:00` and a zone.
fn parse_iso_8601(value: &str) -> Option<i64> {
  let (date, rest) = value.split_once(['t', ' ']).unwrap_or((value, ""));
  let mut parts = date.split('-');
  let year: i64 = parts.next()?.parse().ok()?;
  let month: i64 = parts.next()?.parse().ok()?;
  let day: i64 = parts.next()?.parse().ok()?;
  if parts.next().is_some() || !(1..=12).contains(&month) {
    return None;
  }

  let (time, zone) = split_zone(rest);
  let seconds = if time.is_empty() { 0 } else { parse_time(time)? };
  Some(days_from_civil(year, month, day) * 86_400 + seconds - zone_offset(zone)?)
}

/// `[sun,] 25 jun 2010 15:00:00 pst`.
fn parse_rfc_1123(value: &str) -> Option<i64> {
  let flattened = value.replace(',', " ");
  let mut tokens = flattened.split_whitespace().peekable();
  if !tokens.peek()?.starts_with(|c: char| c.is_ascii_digit()) {
    tokens.next();
  }
  let day: i64 = tokens.next()?.parse().ok()?;
  let name = tokens.next()?;
  let month = i64::try_from(MONTHS.iter().position(|m| name.starts_with(m))?).ok()? + 1;
  let year: i64 = tokens.next()?.parse().ok()?;
  let seconds = match tokens.next() {
    Some(time) => parse_time(time)?,
    None => 0,
  };
  Some(days_from_civil(year, month, day) * 86_400 + seconds - zone_offset(tokens.next().unwrap_or(""))?)
}

/// Split a time from the zone that may follow it with no separator
/// (`15:00:00z`, `15:00:00+01:00`).
fn split_zone(value: &str) -> (&str, &str) {
  value
    .find(['z', '+'])
    .or_else(|| value.rfind('-'))
    .and_then(|at| value.split_at_checked(at))
    .unwrap_or((value, ""))
}

/// `hh:mm` or `hh:mm:ss`, with any fractional seconds discarded.
fn parse_time(value: &str) -> Option<i64> {
  let value = value.split('.').next()?;
  let mut parts = value.split(':');
  let hours: i64 = parts.next()?.trim().parse().ok()?;
  let minutes: i64 = parts.next()?.parse().ok()?;
  let seconds: i64 = match parts.next() {
    Some(s) => s.parse().ok()?,
    None => 0,
  };
  Some(hours * 3600 + minutes * 60 + seconds)
}

/// The zone abbreviations RFC 2822 lists as obsolete and still in use,
/// against the hours to SUBTRACT from a local reading to reach UTC.
const NAMED_ZONES: [(&str, i64); 13] = [
  ("", 0),
  ("z", 0),
  ("ut", 0),
  ("utc", 0),
  ("gmt", 0),
  ("est", -5),
  ("edt", -4),
  ("cst", -6),
  ("cdt", -5),
  ("mst", -7),
  ("mdt", -6),
  ("pst", -8),
  ("pdt", -7),
];

/// Seconds to subtract from a local reading to reach UTC.
fn zone_offset(zone: &str) -> Option<i64> {
  let zone = zone.trim();
  match NAMED_ZONES.iter().find(|(name, _)| *name == zone) {
    Some((_, hours)) => Some(hours * 3600),
    None => parse_numeric_zone(zone),
  }
}

/// `+0100`, `-08:00`.
fn parse_numeric_zone(zone: &str) -> Option<i64> {
  let (sign, rest) = match zone.strip_prefix('+') {
    Some(rest) => (1, rest),
    None => (-1, zone.strip_prefix('-')?),
  };
  let digits: String = rest.chars().filter(char::is_ascii_digit).collect();
  if digits.len() != 4 {
    return None;
  }
  let (hours, minutes) = digits.split_at_checked(2)?;
  let hours: i64 = hours.parse().ok()?;
  let minutes: i64 = minutes.parse().ok()?;
  Some(sign * (hours * 3600 + minutes * 60))
}

/// Days from 1970-01-01 to `y-m-d`, by Howard Hinnant's `days_from_civil`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
  let year = if month <= 2 { year - 1 } else { year };
  let era = if year >= 0 { year } else { year - 399 } / 400;
  let year_of_era = year - era * 400;
  let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
  let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
  era * 146_097 + day_of_era - 719_468
}

/// The `googlebot` in `X-Robots-Tag: googlebot: noindex`, if there is
/// one. `unavailable_after:` looks the same and is not one.
pub(crate) fn header_directive_user_agent(value: &str) -> Option<String> {
  // Upstream's `/^([^,:]+):/`: at least one character, none of them a
  // comma, then a colon.
  let head = value.split(':').next()?;
  if head.is_empty() || head.len() == value.len() || head.contains(',') {
    return None;
  }
  if head.eq_ignore_ascii_case(UNAVAILABLE_AFTER) {
    return None;
  }
  Some(head.to_string())
}

/// `/robots.txt` on `document_url`'s origin, when that origin can
/// serve one at all. `about:blank` and `data:` documents cannot, and
/// neither can a page opened from disk.
#[must_use]
pub fn robots_txt_url(document_url: &str) -> Option<String> {
  let base = url::Url::parse(document_url).ok()?;
  if !matches!(base.scheme(), "http" | "https") {
    return None;
  }
  base.join("/robots.txt").ok().map(String::from)
}
