//! Playwright's `locatorGenerators.ts` for JavaScript, over a port of the
//! parts of `selectorParser.ts` it reads: the code a selector would be
//! written as. A locator prints this way (`Locator.toString`), and call
//! logs and assertion messages name their target with it.

use std::fmt::Write as _;

use serde_json::Value;

const NESTED_SELECTOR_NAMES: &[&str] = &[
  "internal:has",
  "internal:has-not",
  "internal:and",
  "internal:or",
  "internal:chain",
  "left-of",
  "right-of",
  "above",
  "below",
  "near",
];
const NESTED_SELECTOR_NAMES_WITH_DISTANCE: &[&str] = &["left-of", "right-of", "above", "below", "near"];

#[derive(Debug, Clone)]
struct ParsedSelector {
  parts: Vec<Part>,
  capture: Option<usize>,
}

#[derive(Debug, Clone)]
struct Part {
  name: String,
  /// The body as written, which is what stringifying puts back.
  source: String,
  nested: Option<Box<ParsedSelector>>,
}

type Parsed<T> = Result<T, String>;

/// `asLocatorDescription('javascript', selector)`: the custom description
/// a trailing `describe()` set, else the first locator the selector reads
/// as. Input that does not parse prints as itself.
#[must_use]
pub fn as_locator_description(selector: &str) -> String {
  let described = parse_selector(selector).and_then(|parsed| match custom_description(&parsed) {
    Some(description) => Ok(description),
    None => first_locator(&parsed),
  });
  described.unwrap_or_else(|_| selector.to_string())
}

/// `asLocator('javascript', selector)`: as [`as_locator_description`],
/// minus the custom description.
#[must_use]
pub fn as_locator(selector: &str) -> String {
  parse_selector(selector)
    .and_then(|parsed| first_locator(&parsed))
    .unwrap_or_else(|_| selector.to_string())
}

/// `asLocators('javascript', selector, maxOutputSize)`: every way the
/// selector can be written, up to `max`. The JavaScript spelling does not
/// depend on whether it starts from a frame locator.
#[must_use]
pub fn as_locators(selector: &str, max: usize) -> Vec<String> {
  parse_selector(selector)
    .and_then(|parsed| inner_as_locators(&parsed, max))
    .unwrap_or_else(|_| vec![selector.to_string()])
}

/// `locatorCustomDescription`: what the last `describe()` set, if it is
/// the final step of the selector.
#[must_use]
pub fn locator_custom_description(selector: &str) -> Option<String> {
  parse_selector(selector)
    .ok()
    .and_then(|parsed| custom_description(&parsed))
}

fn custom_description(parsed: &ParsedSelector) -> Option<String> {
  let last = parsed.parts.last()?;
  if last.name != "internal:describe" {
    return None;
  }
  serde_json::from_str::<Value>(&last.source)
    .ok()
    .and_then(|v| v.as_str().map(str::to_owned))
}

fn first_locator(parsed: &ParsedSelector) -> Parsed<String> {
  inner_as_locators(parsed, 1)?
    .into_iter()
    .next()
    .ok_or_else(|| "no locator".to_string())
}

// ── selectorParser.ts ───────────────────────────────────────────────────

fn parse_selector(selector: &str) -> Parsed<ParsedSelector> {
  let strings = parse_selector_string(selector)?;
  let mut parts: Vec<Part> = Vec::new();
  for (name, body) in strings.parts {
    if name == "css" || name == "css:light" {
      let source = if name == "css:light" {
        format!(":light({body})")
      } else {
        body
      };
      parts.push(Part {
        name: "css".into(),
        source,
        nested: None,
      });
      continue;
    }
    if NESTED_SELECTOR_NAMES.contains(&name.as_str()) {
      let malformed = || format!("Malformed selector: {name}={body}");
      let unescaped: Vec<Value> = serde_json::from_str(&format!("[{body}]")).map_err(|_| malformed())?;
      let inner = match unescaped.as_slice() {
        [Value::String(inner)] => inner.clone(),
        [Value::String(inner), Value::Number(_)] if NESTED_SELECTOR_NAMES_WITH_DISTANCE.contains(&name.as_str()) => {
          inner.clone()
        },
        _ => return Err(malformed()),
      };
      let mut nested = parse_selector(&inner)?;
      let last_frame = nested
        .parts
        .iter()
        .rposition(|p| p.name == "internal:control" && p.source == "enter-frame");
      let outer: &[Part] = match parts.first() {
        Some(first) if first.name == "internal:control" && first.source == "any-frame" => &parts[1..],
        _ => &parts,
      };
      if let Some(last_frame) = last_frame
        && outer.len() > last_frame
        && stringify(&nested.parts[..=last_frame], None, false) == stringify(&outer[..=last_frame], None, false)
      {
        nested.parts.drain(..=last_frame);
        if let Some(capture) = nested.capture {
          if capture <= last_frame {
            return Err("Can not capture the selector before diving into the frame".into());
          }
          nested.capture = Some(capture - (last_frame + 1));
        }
      }
      parts.push(Part {
        name,
        source: body,
        nested: Some(Box::new(nested)),
      });
      continue;
    }
    parts.push(Part {
      name,
      source: body,
      nested: None,
    });
  }
  if parts
    .first()
    .is_some_and(|p| NESTED_SELECTOR_NAMES.contains(&p.name.as_str()))
  {
    return Err(format!("\"{}\" selector cannot be first", parts[0].name));
  }
  Ok(ParsedSelector {
    parts,
    capture: strings.capture,
  })
}

fn stringify(parts: &[Part], capture: Option<usize>, force_engine_name: bool) -> String {
  parts
    .iter()
    .enumerate()
    .map(|(i, p)| {
      let captured = capture == Some(i);
      let implied =
        p.name == "css" || (p.name == "xpath" && (p.source.starts_with("//") || p.source.starts_with("..")));
      let include_engine = force_engine_name || captured || !implied;
      let star = if captured { "*" } else { "" };
      if include_engine {
        format!("{star}{}={}", p.name, p.source)
      } else {
        format!("{star}{}", p.source)
      }
    })
    .collect::<Vec<_>>()
    .join(" >> ")
}

struct SelectorStrings {
  parts: Vec<(String, String)>,
  capture: Option<usize>,
}

fn is_engine_name(name: &str) -> bool {
  !name.is_empty()
    && name
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | ':' | '*'))
}

fn is_line_terminator(c: char) -> bool {
  matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// `/^\s*text\s*=(.*)$/` with something after the `=`: a text selector
/// whose quotes are part of its text.
fn is_text_selector_with_text(prefix: &str) -> bool {
  let Some(rest) = prefix.trim_start().strip_prefix("text") else {
    return false;
  };
  let Some(text) = rest.trim_start().strip_prefix('=') else {
    return false;
  };
  !text.is_empty() && !text.chars().any(is_line_terminator)
}

fn parse_selector_string(selector: &str) -> Parsed<SelectorStrings> {
  let mut result = SelectorStrings {
    parts: Vec::new(),
    capture: None,
  };
  let mut append = |part: &str| -> Parsed<()> {
    let part = part.trim();
    let (mut name, body) = match part.find('=') {
      Some(eq) if is_engine_name(part[..eq].trim()) => (part[..eq].trim().to_string(), part[eq + 1..].to_string()),
      _ if part.len() > 1 && part.starts_with('"') && part.ends_with('"') => ("text".to_string(), part.to_string()),
      _ if part.len() > 1 && part.starts_with('\'') && part.ends_with('\'') => ("text".to_string(), part.to_string()),
      _ if part.trim_start_matches('(').starts_with("//") || part.starts_with("..") => {
        ("xpath".to_string(), part.to_string())
      },
      _ => ("css".to_string(), part.to_string()),
    };
    let captured = name.starts_with('*');
    if captured {
      name.remove(0);
    }
    result.parts.push((name, body));
    if captured {
      if result.capture.is_some() {
        return Err("Only one of the selectors can capture using * modifier".into());
      }
      result.capture = Some(result.parts.len() - 1);
    }
    Ok(())
  };

  if !selector.contains(">>") {
    append(selector)?;
    return Ok(result);
  }

  let bytes = selector.as_bytes();
  let mut index = 0;
  let mut start = 0;
  let mut quote: Option<u8> = None;
  while index < bytes.len() {
    let c = bytes[index];
    if c == b'\\' && index + 1 < bytes.len() {
      index += 2;
    } else if Some(c) == quote {
      quote = None;
      index += 1;
    } else if quote.is_none()
      && matches!(c, b'"' | b'\'' | b'`')
      && !is_text_selector_with_text(&selector[start..index])
    {
      quote = Some(c);
      index += 1;
    } else if quote.is_none() && c == b'>' && bytes.get(index + 1) == Some(&b'>') {
      append(&selector[start..index])?;
      index += 2;
      start = index;
    } else {
      index += 1;
    }
  }
  append(&selector[start..])?;
  Ok(result)
}

#[derive(Debug, Clone)]
enum AttrValue {
  Str(String),
  Regex(JsRegex),
  Bool(bool),
  Number(f64),
  Null,
}

#[derive(Debug, Clone)]
struct Attr {
  name: String,
  value: AttrValue,
  case_sensitive: bool,
}

struct AttrSelector {
  name: String,
  attributes: Vec<Attr>,
}

/// `parseAttributeSelector(selector, allowUnquotedStrings = true)`.
fn parse_attribute_selector(selector: &str) -> Parsed<AttrSelector> {
  let mut parser = AttrParser {
    selector,
    chars: selector.chars().collect(),
    wp: 0,
  };
  let mut result = AttrSelector {
    name: parser.read_identifier(),
    attributes: Vec::new(),
  };
  parser.skip_spaces();
  while parser.next() == Some('[') {
    result.attributes.push(parser.read_attribute()?);
    parser.skip_spaces();
  }
  if parser.wp < parser.chars.len() || (result.name.is_empty() && result.attributes.is_empty()) {
    return Err(parser.error());
  }
  Ok(result)
}

struct AttrParser<'a> {
  selector: &'a str,
  chars: Vec<char>,
  wp: usize,
}

impl AttrParser<'_> {
  fn next(&self) -> Option<char> {
    self.chars.get(self.wp).copied()
  }

  fn eat(&mut self) -> Parsed<char> {
    let c = self.next().ok_or_else(|| self.error())?;
    self.wp += 1;
    Ok(c)
  }

  fn error(&self) -> String {
    format!("Error while parsing selector `{}`", self.selector)
  }

  fn skip_spaces(&mut self) {
    while self.next().is_some_and(char::is_whitespace) {
      self.wp += 1;
    }
  }

  fn is_name_char(c: char) -> bool {
    c >= '\u{80}' || c.is_ascii_alphanumeric() || c == '_' || c == '-'
  }

  fn read_identifier(&mut self) -> String {
    self.skip_spaces();
    let mut out = String::new();
    while let Some(c) = self.next().filter(|c| Self::is_name_char(*c)) {
      out.push(c);
      self.wp += 1;
    }
    out
  }

  /// The quoted text with its escapes resolved, quotes removed.
  fn read_quoted(&mut self, quote: char) -> Parsed<String> {
    self.wp += 1;
    let mut out = String::new();
    while self.next().is_some_and(|c| c != quote) {
      if self.next() == Some('\\') {
        self.wp += 1;
      }
      out.push(self.eat()?);
    }
    if self.eat()? != quote {
      return Err(self.error());
    }
    Ok(out)
  }

  fn read_regex(&mut self) -> Parsed<JsRegex> {
    self.wp += 1;
    let mut source = String::new();
    let mut in_class = false;
    while let Some(c) = self.next() {
      if c == '\\' {
        source.push(self.eat()?);
      } else if in_class && c == ']' {
        in_class = false;
      } else if !in_class && c == '[' {
        in_class = true;
      } else if !in_class && c == '/' {
        break;
      }
      source.push(self.eat()?);
    }
    if self.eat()? != '/' {
      return Err(self.error());
    }
    let mut flags = String::new();
    while let Some(c) = self.next().filter(|c| "dgimsuvy".contains(*c)) {
      flags.push(c);
      self.wp += 1;
    }
    Ok(JsRegex::new(&source, &flags))
  }

  fn read_attribute(&mut self) -> Parsed<Attr> {
    self.wp += 1;
    let mut path = Vec::new();
    loop {
      self.skip_spaces();
      let token = match self.next() {
        Some(q @ ('\'' | '"')) => self.read_quoted(q)?,
        _ => self.read_identifier(),
      };
      if token.is_empty() {
        return Err(self.error());
      }
      path.push(token);
      self.skip_spaces();
      if self.next() != Some('.') {
        break;
      }
      self.wp += 1;
    }
    let name = path.join(".");
    if self.next() == Some(']') {
      self.wp += 1;
      return Ok(Attr {
        name,
        value: AttrValue::Null,
        case_sensitive: false,
      });
    }
    let op = self.read_operator()?;
    let (value, case_sensitive) = self.read_value(op == "=")?;
    self.skip_spaces();
    if self.eat()? != ']' || (op != "=" && !matches!(value, AttrValue::Str(_))) {
      return Err(self.error());
    }
    Ok(Attr {
      name,
      value,
      case_sensitive,
    })
  }

  fn read_operator(&mut self) -> Parsed<String> {
    self.skip_spaces();
    let mut op = String::new();
    op.extend(self.eat().ok());
    if op != "=" {
      op.extend(self.eat().ok());
    }
    if ["=", "*=", "^=", "$=", "|=", "~="].contains(&op.as_str()) {
      Ok(op)
    } else {
      Err(self.error())
    }
  }

  /// The value after the operator, and whether it matches case-sensitively.
  fn read_value(&mut self, regex_allowed: bool) -> Parsed<(AttrValue, bool)> {
    self.skip_spaces();
    match self.next() {
      Some('/') if regex_allowed => Ok((AttrValue::Regex(self.read_regex()?), true)),
      Some('/') => Err(self.error()),
      Some(q @ ('\'' | '"')) => {
        let text = self.read_quoted(q)?;
        self.skip_spaces();
        let mut case_sensitive = true;
        match self.next() {
          Some('i' | 'I') => {
            case_sensitive = false;
            self.wp += 1;
          },
          Some('s' | 'S') => self.wp += 1,
          _ => {},
        }
        Ok((AttrValue::Str(text), case_sensitive))
      },
      _ => {
        let mut text = String::new();
        while let Some(c) = self.next().filter(|c| Self::is_name_char(*c) || *c == '+' || *c == '.') {
          text.push(c);
          self.wp += 1;
        }
        let value = match text.as_str() {
          "true" => AttrValue::Bool(true),
          "false" => AttrValue::Bool(false),
          _ => AttrValue::Str(text),
        };
        Ok((value, true))
      },
    }
  }
}

// ── locatorGenerators.ts ────────────────────────────────────────────────

/// A JavaScript regular expression as its source and flags.
#[derive(Debug, Clone)]
struct JsRegex {
  source: String,
  flags: String,
}

impl JsRegex {
  fn new(source: &str, flags: &str) -> Self {
    Self {
      source: source.to_string(),
      flags: flags.to_string(),
    }
  }

  /// `String(re)`: the source the way `RegExp.prototype.source` escapes
  /// it, and the flags in the order the `flags` getter lists them.
  fn to_js_string(&self) -> String {
    let mut source = String::new();
    let mut in_class = false;
    let mut chars = self.source.chars();
    while let Some(c) = chars.next() {
      match c {
        '\\' => {
          source.push(c);
          if let Some(escaped) = chars.next() {
            source.push(escaped);
          }
        },
        '[' => {
          in_class = true;
          source.push(c);
        },
        ']' => {
          in_class = false;
          source.push(c);
        },
        '/' if !in_class => source.push_str("\\/"),
        '\n' => source.push_str("\\n"),
        '\r' => source.push_str("\\r"),
        '\u{2028}' => source.push_str("\\u2028"),
        '\u{2029}' => source.push_str("\\u2029"),
        c => source.push(c),
      }
    }
    if source.is_empty() {
      source.push_str("(?:)");
    }
    let flags: String = "dgimsuvy".chars().filter(|f| self.flags.contains(*f)).collect();
    format!("/{source}/{flags}")
  }
}

#[derive(Debug, Clone)]
enum Text {
  Str(String),
  Regex(JsRegex),
}

#[derive(Default)]
struct Options {
  attrs: Vec<(String, AttrValue)>,
  exact: Option<bool>,
  name: Option<AttrValue>,
  description: Option<AttrValue>,
  has_text: Option<Text>,
  has_not_text: Option<Text>,
}

#[derive(Clone, Copy)]
enum Kind {
  Default,
  FrameLocator,
  Frame,
  AnyFrame,
  Nth,
  First,
  Last,
  Visible,
  FilterVisible,
  Role,
  HasText,
  HasNotText,
  Has,
  HasNot,
  And,
  Or,
  Chain,
  TestId,
  Text,
  Alt,
  Placeholder,
  Label,
  Title,
}

/// `detectExact`: a text-engine body as the value and exactness it was
/// written with.
fn detect_exact(text: &str) -> Parsed<(Text, Option<bool>)> {
  if let Some(body) = text.strip_prefix('/')
    && let Some(slash) = body.rfind('/')
    && body[slash + 1..].chars().all(|c| "dgimsuvy".contains(c))
    && !body.chars().any(is_line_terminator)
  {
    return Ok((Text::Regex(JsRegex::new(&body[..slash], &body[slash + 1..])), None));
  }
  let json = |s: &str| serde_json::from_str::<String>(s).map_err(|e| e.to_string());
  if text.ends_with('"') {
    return Ok((Text::Str(json(text)?), Some(true)));
  }
  if let Some(quoted) = text.strip_suffix('s').filter(|t| t.ends_with('"')) {
    return Ok((Text::Str(json(quoted)?), Some(true)));
  }
  if let Some(quoted) = text.strip_suffix('i').filter(|t| t.ends_with('"')) {
    return Ok((Text::Str(json(quoted)?), Some(false)));
  }
  Ok((Text::Str(text.to_string()), Some(false)))
}

fn inner_as_locators(parsed: &ParsedSelector, max: usize) -> Parsed<Vec<String>> {
  let parts = &parsed.parts;
  let mut tokens: Vec<Vec<String>> = Vec::new();
  let mut index = 0;
  while index < parts.len() {
    let part = &parts[index];
    if part.name == "internal:describe" {
      index += 1;
      continue;
    }
    if part.name == "internal:control" && part.source == "enter-frame" {
      let previous = index.checked_sub(1).map(|i| &parts[i]);
      let (Some(previous), Some(last)) = (previous, tokens.last_mut()) else {
        return Err("enter-frame cannot be first".into());
      };
      *last = enter_frame(last, previous);
      index += 1;
      continue;
    }
    if let Some(list) = engine_tokens(part, max)? {
      tokens.push(list);
      index += 1;
      continue;
    }
    let (list, consumed) = selector_tokens(part, parts.get(index + 1))?;
    tokens.push(list);
    index += consumed;
  }
  Ok(combine_tokens(&tokens, max))
}

/// The spellings of a part one of the `internal:` engines (or `nth`,
/// `visible`) wrote, or `None` when the part reads as a raw selector.
fn engine_tokens(part: &Part, max: usize) -> Parsed<Option<Vec<String>>> {
  let body = part.source.as_str();
  let with_text = |kind: Kind| -> Parsed<Vec<String>> {
    let (text, exact) = detect_exact(body)?;
    Ok(vec![generate(
      kind,
      &text,
      &Options {
        exact,
        ..Options::default()
      },
    )])
  };
  Ok(Some(match part.name.as_str() {
    "nth" => match body {
      "0" => vec![gen_plain(Kind::First, ""), gen_plain(Kind::Nth, "0")],
      "-1" => vec![gen_plain(Kind::Last, ""), gen_plain(Kind::Nth, "-1")],
      _ => vec![gen_plain(Kind::Nth, body)],
    },
    "visible" => {
      let mut list = Vec::new();
      if body == "true" {
        list.push(gen_plain(Kind::Visible, ""));
      }
      list.push(gen_plain(Kind::FilterVisible, body));
      list.push(gen_plain(Kind::Default, &format!("visible={body}")));
      list
    },
    "internal:text" => with_text(Kind::Text)?,
    "internal:label" => with_text(Kind::Label)?,
    "internal:has-text" | "internal:has-not-text" => {
      let (value, exact) = detect_exact(body)?;
      // Strict has-text has no locator spelling; it stays a selector.
      if exact == Some(true) {
        return Ok(None);
      }
      let kind = if part.name == "internal:has-text" {
        Kind::HasText
      } else {
        Kind::HasNotText
      };
      vec![generate(kind, &value, &Options::default())]
    },
    "internal:has" | "internal:has-not" | "internal:and" | "internal:or" | "internal:chain" => {
      let nested = part
        .nested
        .as_deref()
        .ok_or_else(|| "nested selector missing".to_string())?;
      let kind = match part.name.as_str() {
        "internal:has" => Kind::Has,
        "internal:has-not" => Kind::HasNot,
        "internal:and" => Kind::And,
        "internal:or" => Kind::Or,
        _ => Kind::Chain,
      };
      inner_as_locators(nested, max)?
        .iter()
        .map(|inner| gen_plain(kind, inner))
        .collect()
    },
    "internal:role" => vec![role_locator(body)?],
    "internal:testid" => {
      let attr = first_attribute(body)?;
      vec![generate(
        Kind::TestId,
        &Text::from_attr(attr.value),
        &Options::default(),
      )]
    },
    "internal:attr" => {
      let attr = first_attribute(body)?;
      let kind = match attr.name.as_str() {
        "placeholder" => Kind::Placeholder,
        "alt" => Kind::Alt,
        "title" => Kind::Title,
        _ => return Ok(None),
      };
      let options = Options {
        exact: Some(attr.case_sensitive),
        ..Options::default()
      };
      vec![generate(kind, &Text::from_attr(attr.value), &options)]
    },
    "internal:control" if body == "any-frame" => vec![gen_plain(Kind::AnyFrame, "")],
    _ => return Ok(None),
  }))
}

fn first_attribute(body: &str) -> Parsed<Attr> {
  parse_attribute_selector(body)?
    .attributes
    .into_iter()
    .next()
    .ok_or_else(|| format!("no attribute in `{body}`"))
}

fn role_locator(body: &str) -> Parsed<String> {
  let selector = parse_attribute_selector(body)?;
  let mut options = Options::default();
  for attr in selector.attributes {
    if attr.name == "name" || attr.name == "description" {
      // Only string values carry exactness; a regex is used as is.
      if matches!(attr.value, AttrValue::Str(_)) {
        if options.exact.is_some_and(|exact| exact != attr.case_sensitive) {
          return Err(format!("Conflicting exactness in internal:role selector: {body}"));
        }
        options.exact = Some(attr.case_sensitive);
      }
      if attr.name == "name" {
        options.name = Some(attr.value);
      } else {
        options.description = Some(attr.value);
      }
      continue;
    }
    let value = match attr.value {
      AttrValue::Str(s) if attr.name == "level" => AttrValue::Number(js_unary_plus(&s)),
      other => other,
    };
    let name = if attr.name == "include-hidden" {
      "includeHidden".to_string()
    } else {
      attr.name
    };
    options.attrs.push((name, value));
  }
  Ok(generate(Kind::Role, &Text::Str(selector.name), &options))
}

/// The previous part's spellings followed by `contentFrame()`, plus
/// `frameLocator(...)` when that part was a plain selector.
fn enter_frame(last: &[String], previous: &Part) -> Vec<String> {
  let frame = gen_plain(Kind::Frame, "");
  let mut transformed: Vec<String> = last.iter().map(|t| format!("{t}.{frame}")).collect();
  if previous.name == "xpath" || previous.name == "css" {
    let one = std::slice::from_ref(previous);
    transformed.push(gen_plain(Kind::FrameLocator, &stringify(one, None, false)));
    transformed.push(gen_plain(Kind::FrameLocator, &stringify(one, None, true)));
  }
  transformed
}

/// A part that reads as `locator('...')`, and how many parts it took: a
/// following loose has-text folds into it as `{ hasText }`.
fn selector_tokens(part: &Part, next_part: Option<&Part>) -> Parsed<(Vec<String>, usize)> {
  let selector_part = stringify(std::slice::from_ref(part), None, false);
  let locator_part = gen_plain(Kind::Default, &selector_part);

  if let Some(next_part) = next_part
    && (next_part.name == "internal:has-text" || next_part.name == "internal:has-not-text")
  {
    let (value, exact) = detect_exact(&next_part.source)?;
    if exact != Some(true) {
      let has_text = next_part.name == "internal:has-text";
      let kind = if has_text { Kind::HasText } else { Kind::HasNotText };
      let next_locator_part = generate(kind, &value, &Options::default());
      let options = if has_text {
        Options {
          has_text: Some(value),
          ..Options::default()
        }
      } else {
        Options {
          has_not_text: Some(value),
          ..Options::default()
        }
      };
      let combined = generate(Kind::Default, &Text::Str(selector_part), &options);
      return Ok((vec![format!("{locator_part}.{next_locator_part}"), combined], 2));
    }
  }

  let mut list = vec![locator_part];
  if part.name == "xpath" || part.name == "css" {
    let with_engine = stringify(std::slice::from_ref(part), None, true);
    list.push(gen_plain(Kind::Default, &with_engine));
  }
  Ok((list, 1))
}

fn combine_tokens(tokens: &[Vec<String>], max: usize) -> Vec<String> {
  fn visit(
    tokens: &[Vec<String>],
    index: usize,
    current: &mut Vec<String>,
    result: &mut Vec<String>,
    max: usize,
  ) -> bool {
    if index == tokens.len() {
      result.push(current.join("."));
      return result.len() < max;
    }
    for taken in &tokens[index] {
      current[index].clone_from(taken);
      if !visit(tokens, index + 1, current, result, max) {
        return false;
      }
    }
    true
  }
  let mut current = vec![String::new(); tokens.len()];
  let mut result = Vec::new();
  visit(tokens, 0, &mut current, &mut result, max);
  result
}

impl Text {
  fn from_attr(value: AttrValue) -> Self {
    match value {
      AttrValue::Regex(re) => Self::Regex(re),
      other => Self::Str(attr_value_display(&other)),
    }
  }
}

fn gen_plain(kind: Kind, body: &str) -> String {
  generate(kind, &Text::Str(body.to_string()), &Options::default())
}

/// `JavaScriptLocatorFactory.generateLocator`.
/// The JavaScript factory writes the same call whatever it chains onto,
/// so upstream's `base` argument has no counterpart here.
fn generate(kind: Kind, body: &Text, options: &Options) -> String {
  let raw = || match body {
    Text::Str(s) => s.clone(),
    Text::Regex(re) => re.to_js_string(),
  };
  match kind {
    Kind::Default => {
      if let Some(text) = &options.has_text {
        format!("locator({}, {{ hasText: {} }})", quote(&raw()), to_has_text(text))
      } else if let Some(text) = &options.has_not_text {
        format!("locator({}, {{ hasNotText: {} }})", quote(&raw()), to_has_text(text))
      } else {
        format!("locator({})", quote(&raw()))
      }
    },
    Kind::FrameLocator => format!("frameLocator({})", quote(&raw())),
    Kind::Frame => "contentFrame()".into(),
    Kind::AnyFrame => "frameLocator()".into(),
    Kind::Nth => format!("nth({})", raw()),
    Kind::First => "first()".into(),
    Kind::Last => "last()".into(),
    Kind::Visible => "visible()".into(),
    Kind::FilterVisible => format!(
      "filter({{ visible: {} }})",
      if raw() == "true" { "true" } else { "false" }
    ),
    Kind::Role => {
      let mut attrs: Vec<String> = Vec::new();
      let string_valued = |v: &Option<AttrValue>| matches!(v, Some(AttrValue::Str(_)));
      for (label, value) in [("name", &options.name), ("description", &options.description)] {
        match value {
          Some(AttrValue::Regex(re)) => attrs.push(format!("{label}: {}", regex_to_source_string(re))),
          Some(AttrValue::Str(s)) => attrs.push(format!("{label}: {}", quote(s))),
          _ => {},
        }
      }
      if options.exact == Some(true) && (string_valued(&options.name) || string_valued(&options.description)) {
        attrs.push("exact: true".into());
      }
      for (name, value) in &options.attrs {
        let shown = match value {
          AttrValue::Str(s) => quote(s),
          other => attr_value_display(other),
        };
        attrs.push(format!("{name}: {shown}"));
      }
      let attr_string = if attrs.is_empty() {
        String::new()
      } else {
        format!(", {{ {} }}", attrs.join(", "))
      };
      format!("getByRole({}{attr_string})", quote(&raw()))
    },
    Kind::HasText => format!("filter({{ hasText: {} }})", to_has_text(body)),
    Kind::HasNotText => format!("filter({{ hasNotText: {} }})", to_has_text(body)),
    Kind::Has => format!("filter({{ has: {} }})", raw()),
    Kind::HasNot => format!("filter({{ hasNot: {} }})", raw()),
    Kind::And => format!("and({})", raw()),
    Kind::Or => format!("or({})", raw()),
    Kind::Chain => format!("locator({})", raw()),
    Kind::TestId => format!("getByTestId({})", to_has_text(body)),
    Kind::Text => call_with_exact("getByText", body, options.exact == Some(true)),
    Kind::Alt => call_with_exact("getByAltText", body, options.exact == Some(true)),
    Kind::Placeholder => call_with_exact("getByPlaceholder", body, options.exact == Some(true)),
    Kind::Label => call_with_exact("getByLabel", body, options.exact == Some(true)),
    Kind::Title => call_with_exact("getByTitle", body, options.exact == Some(true)),
  }
}

fn call_with_exact(method: &str, body: &Text, exact: bool) -> String {
  match body {
    Text::Regex(re) => format!("{method}({})", regex_to_source_string(re)),
    Text::Str(s) if exact => format!("{method}({}, {{ exact: true }})", quote(s)),
    Text::Str(s) => format!("{method}({})", quote(s)),
  }
}

fn to_has_text(body: &Text) -> String {
  match body {
    Text::Regex(re) => regex_to_source_string(re),
    Text::Str(s) => quote(s),
  }
}

/// `normalizeEscapedRegexQuotes(String(re))`.
fn regex_to_source_string(re: &JsRegex) -> String {
  normalize_escaped_regex_quotes(&re.to_js_string())
}

/// Upstream's `normalizeEscapedRegexQuotes`, a global replace of an odd
/// run of backslashes before a quote with `$1$2$3`, match for match. `$2`
/// is the last pair the repetition captured, so a run of five backslashes
/// before a quote keeps two, not four, exactly as upstream.
fn normalize_escaped_regex_quotes(source: &str) -> String {
  let chars: Vec<char> = source.chars().collect();
  let is_quote = |c: Option<&char>| matches!(c, Some('\'' | '"' | '`'));
  let odd_run_then_quote = |from: usize| {
    let run = chars[from..].iter().take_while(|c| **c == '\\').count();
    (run % 2 == 1 && is_quote(chars.get(from + run))).then_some(run)
  };
  let mut out = String::with_capacity(source.len());
  let mut copied = 0;
  let mut p = 0;
  while p < chars.len() {
    let matched = if p == 0
      && let Some(run) = odd_run_then_quote(0)
    {
      Some((None, run, run + 1))
    } else if chars[p] != '\\'
      && let Some(run) = odd_run_then_quote(p + 1)
    {
      Some((Some(chars[p]), run, run + 2))
    } else {
      None
    };
    let Some((lead, run, len)) = matched else {
      p += 1;
      continue;
    };
    out.extend(&chars[copied..p]);
    out.extend(lead);
    if run > 1 {
      out.push_str("\\\\");
    }
    out.push(chars[p + len - 1]);
    p += len;
    copied = p;
  }
  out.extend(&chars[copied..]);
  out
}

/// `escapeWithQuotes(text, "'")`.
fn quote(text: &str) -> String {
  let stringified = serde_json::to_string(text).unwrap_or_default();
  let inner = &stringified[1..stringified.len().saturating_sub(1).max(1)];
  format!("'{}'", inner.replace("\\\"", "\"").replace('\'', "\\'"))
}

/// A non-string attribute value the way a template literal prints it.
fn attr_value_display(value: &AttrValue) -> String {
  match value {
    AttrValue::Str(s) => s.clone(),
    AttrValue::Regex(re) => re.to_js_string(),
    AttrValue::Bool(b) => b.to_string(),
    AttrValue::Number(n) => js_number(*n),
    AttrValue::Null => "null".into(),
  }
}

/// JavaScript's unary `+` on a string.
fn js_unary_plus(text: &str) -> f64 {
  let trimmed = text.trim();
  if trimmed.is_empty() {
    return 0.0;
  }
  trimmed.parse::<f64>().unwrap_or(f64::NAN)
}

fn js_number(n: f64) -> String {
  if n.is_nan() {
    return "NaN".into();
  }
  if n.is_infinite() {
    return if n > 0.0 { "Infinity".into() } else { "-Infinity".into() };
  }
  let mut out = String::new();
  if n.fract() == 0.0 && n.abs() < 1e21 {
    let _ = write!(out, "{n:.0}");
  } else {
    let _ = write!(out, "{n}");
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn reverse_engineers_internal_selectors() {
    for (selector, expected) in [
      (
        r#"internal:role=alert[name=/Upload/][description="doc.pdf"i]"#,
        "getByRole('alert', { name: /Upload/, description: 'doc.pdf' })",
      ),
      (
        r#"internal:role=alert[name="Upload"i][description=/doc\.pdf/]"#,
        r"getByRole('alert', { name: 'Upload', description: /doc\.pdf/ })",
      ),
      ("internal:text=/a.b/ims", "getByText(/a.b/ims)"),
      (
        "internal:role=button[name=/a.b/s]",
        "getByRole('button', { name: /a.b/s })",
      ),
      (
        r#"div >> internal:and="span >> article""#,
        "locator('div').and(locator('span').locator('article'))",
      ),
      (
        r#"div >> internal:or="span >> article""#,
        "locator('div').or(locator('span').locator('article'))",
      ),
      (
        r#"div >> internal:chain="span >> article""#,
        "locator('div').locator(locator('span').locator('article'))",
      ),
      (
        "//*[contains(normalizer-text(), 'foo']",
        r"locator('//*[contains(normalizer-text(), \'foo\']')",
      ),
      (
        "div >> iframe >> internal:control=enter-frame >> span",
        "locator('div').locator('iframe').contentFrame().locator('span')",
      ),
    ] {
      assert_eq!(as_locator(selector), expected, "{selector}");
    }
    assert_eq!(
      as_locators(
        r#"internal:attr=[title="iframe title"i] >> internal:control=enter-frame"#,
        20
      ),
      vec!["getByTitle('iframe title').contentFrame()".to_string()]
    );
  }

  #[test]
  fn conflicting_role_exactness_prints_the_selector() {
    for selector in [
      r#"internal:role=row[name="abc"i][description="d"s]"#,
      r#"internal:role=row[name="abc"s][description="d"i]"#,
    ] {
      assert_eq!(as_locator(selector), selector);
    }
  }

  #[test]
  fn descriptions_follow_the_last_describe() {
    assert_eq!(as_locator_description(r#"body >> internal:describe="desc""#), "desc");
    assert_eq!(
      as_locator_description("body >> internal:describe=12"),
      "locator('body')"
    );
    assert_eq!(
      as_locator_description(r#"body >> internal:describe="desc" >> div"#),
      "locator('body').locator('div')"
    );
    assert_eq!(as_locator(r#"body >> internal:describe="desc""#), "locator('body')");
    assert_eq!(
      locator_custom_description(r#"#a >> internal:describe="a >> b""#).as_deref(),
      Some("a >> b")
    );
  }

  #[test]
  fn generates_every_spelling_up_to_the_limit() {
    let selector =
      r#"div >> internal:has-text="foo"i >> nth=0 >> internal:has="span >> internal:has-not-text=\"bar\"i >> nth=-1""#;
    assert_eq!(
      as_locators(selector, 20),
      [
        "locator('div').filter({ hasText: 'foo' }).first().filter({ has: locator('span').filter({ hasNotText: 'bar' }).last() })",
        "locator('div').filter({ hasText: 'foo' }).first().filter({ has: locator('span').filter({ hasNotText: 'bar' }).nth(-1) })",
        "locator('div').filter({ hasText: 'foo' }).first().filter({ has: locator('span', { hasNotText: 'bar' }).last() })",
        "locator('div').filter({ hasText: 'foo' }).first().filter({ has: locator('span', { hasNotText: 'bar' }).nth(-1) })",
        "locator('div').filter({ hasText: 'foo' }).nth(0).filter({ has: locator('span').filter({ hasNotText: 'bar' }).last() })",
        "locator('div').filter({ hasText: 'foo' }).nth(0).filter({ has: locator('span').filter({ hasNotText: 'bar' }).nth(-1) })",
        "locator('div').filter({ hasText: 'foo' }).nth(0).filter({ has: locator('span', { hasNotText: 'bar' }).last() })",
        "locator('div').filter({ hasText: 'foo' }).nth(0).filter({ has: locator('span', { hasNotText: 'bar' }).nth(-1) })",
        "locator('div', { hasText: 'foo' }).first().filter({ has: locator('span').filter({ hasNotText: 'bar' }).last() })",
        "locator('div', { hasText: 'foo' }).first().filter({ has: locator('span').filter({ hasNotText: 'bar' }).nth(-1) })",
        "locator('div', { hasText: 'foo' }).first().filter({ has: locator('span', { hasNotText: 'bar' }).last() })",
        "locator('div', { hasText: 'foo' }).first().filter({ has: locator('span', { hasNotText: 'bar' }).nth(-1) })",
        "locator('div', { hasText: 'foo' }).nth(0).filter({ has: locator('span').filter({ hasNotText: 'bar' }).last() })",
        "locator('div', { hasText: 'foo' }).nth(0).filter({ has: locator('span').filter({ hasNotText: 'bar' }).nth(-1) })",
        "locator('div', { hasText: 'foo' }).nth(0).filter({ has: locator('span', { hasNotText: 'bar' }).last() })",
        "locator('div', { hasText: 'foo' }).nth(0).filter({ has: locator('span', { hasNotText: 'bar' }).nth(-1) })",
      ]
    );
  }

  #[test]
  fn regex_sources_print_like_javascript() {
    assert_eq!(JsRegex::new("", "").to_js_string(), "/(?:)/");
    assert_eq!(JsRegex::new("a/b[/]", "si").to_js_string(), r"/a\/b[/]/is");
    assert_eq!(regex_to_source_string(&JsRegex::new(r#"hel\"lo"#, "")), r#"/hel"lo/"#);
    assert_eq!(
      regex_to_source_string(&JsRegex::new(r#"He\\"llo"#, "")),
      r#"/He\\"llo/"#
    );
  }
}
