//! `robots.txt`, parsed the way `is-crawlable` needs it parsed.
//!
//! Lighthouse does not implement this itself: `core/audits/seo/
//! is-crawlable.js` imports `robots-parser`, so agreeing with
//! Lighthouse means agreeing with that package. This is a port of
//! `robots-parser` 3.0.1 (`Robots.js`), pinned to the version
//! Lighthouse's `^3.0.1` resolves to.
//!
//! Only the two entry points the audit uses are here, [`Robots::is_allowed`]
//! and [`Robots::matching_line_number`]; crawl delay, sitemaps and the
//! preferred host are parsed but not exposed, because `Crawl-delay` and
//! `Sitemap` lines still decide which user agents a group covers.
//!
//! `scripts/perf-diff/record-robots.mjs` runs the real package over a
//! table of (robots.txt, url, user-agent) triples and
//! `cargo test -p ferridriver-perf --test robots` replays the answers.
//! Nothing in here is guessable from the specification: the matcher is
//! Google's `robotstxt` algorithm, longest-pattern-wins with allow
//! breaking ties, and an empty `Disallow:` registers a user agent
//! without contributing a rule, which is what stops the `*` group from
//! applying to it.

use rustc_hash::FxHashMap;

/// One `Allow:` or `Disallow:` line.
#[derive(Debug, Clone)]
struct Rule {
  pattern: Vec<char>,
  /// The pattern's length as `findRule` compares it, which is the
  /// normalised (percent-encoded) form, not what was written.
  length: usize,
  allow: bool,
  /// 1-based, as `robots-parser` reports it.
  line: usize,
}

/// A parsed `robots.txt`, bound to the origin it was served from.
#[derive(Debug, Clone)]
pub struct Robots {
  /// `None` when the URL it was fetched from would not parse, which
  /// makes every query answer "not my origin" — the same conclusion the
  /// original reaches by comparing a real protocol against `undefined`.
  origin: Option<Origin>,
  /// Keyed by normalised user agent. A key with an empty vector is
  /// meaningful: the agent was named, so it does not fall back to `*`.
  rules: FxHashMap<String, Vec<Rule>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Origin {
  scheme: String,
  host: Option<String>,
  /// Already `None` for a scheme's default port, which is where the
  /// original's `url.port = 443` lands too: the URL port setter drops
  /// a value equal to the scheme's default.
  port: Option<u16>,
}

impl Robots {
  /// Parse `contents`, served from `url`.
  #[must_use]
  pub fn parse(url: &str, contents: &str) -> Self {
    let mut robots = Self {
      origin: parse_url(url).map(|parsed| origin_of(&parsed)),
      rules: FxHashMap::default(),
    };

    let mut agents: Vec<String> = Vec::new();
    let mut after_user_agent = false;
    for (index, raw) in split_lines(contents).into_iter().enumerate() {
      let Some((field, value)) = split_line(remove_comments(raw)) else {
        continue;
      };
      if field.is_empty() {
        continue;
      }
      let field = field.to_lowercase();
      match field.as_str() {
        "user-agent" => {
          if !after_user_agent {
            agents.clear();
          }
          if !value.is_empty() {
            agents.push(format_user_agent(value));
          }
        },
        "disallow" => robots.add_rule(&agents, value, false, index + 1),
        "allow" => robots.add_rule(&agents, value, true, index + 1),
        // Neither is read back, but both name the group's agents, and a
        // named agent no longer inherits the `*` rules.
        "crawl-delay" => {
          for agent in &agents {
            robots.rules.entry(agent.clone()).or_default();
          }
        },
        _ => {},
      }
      after_user_agent = field == "user-agent";
    }
    robots
  }

  fn add_rule(&mut self, agents: &[String], pattern: &str, allow: bool, line: usize) {
    for agent in agents {
      let rules = self.rules.entry(agent.clone()).or_default();
      if pattern.is_empty() {
        continue;
      }
      let normalised: Vec<char> = normalise_encoding(pattern).chars().collect();
      rules.push(Rule {
        length: normalised.len(),
        pattern: normalised,
        allow,
        line,
      });
    }
  }

  /// Whether `url` may be crawled by `user_agent` (`None` for `*`).
  ///
  /// `None` means the answer does not apply: `url` is not on the origin
  /// this `robots.txt` was served from, so it governs nothing here.
  /// `is-crawlable` treats that as blocking, because the original
  /// negates `undefined`.
  #[must_use]
  pub fn is_allowed(&self, url: &str, user_agent: Option<&str>) -> Option<bool> {
    match self.rule_for(url, user_agent) {
      Verdict::Foreign => None,
      Verdict::Unmatched => Some(true),
      Verdict::Matched(rule) => Some(rule.allow),
    }
  }

  /// The 1-based line of the rule that decided `url`, or `-1` when no
  /// rule matched or the URL is not on this origin.
  #[must_use]
  pub fn matching_line_number(&self, url: &str, user_agent: Option<&str>) -> i64 {
    match self.rule_for(url, user_agent) {
      Verdict::Matched(rule) => i64::try_from(rule.line).unwrap_or(-1),
      _ => -1,
    }
  }

  fn rule_for(&self, url: &str, user_agent: Option<&str>) -> Verdict<'_> {
    let Some(parsed) = parse_url(url) else {
      return Verdict::Foreign;
    };
    if Some(origin_of(&parsed)) != self.origin {
      return Verdict::Foreign;
    }
    let agent = format_user_agent(user_agent.unwrap_or("*"));
    let rules = self
      .rules
      .get(&agent)
      .or_else(|| self.rules.get("*"))
      .map_or(&[][..], Vec::as_slice);

    let query = match parsed.query() {
      Some(query) if !query.is_empty() => format!("?{query}"),
      _ => String::new(),
    };
    let path: Vec<char> = url_encode_to_upper(&format!("{}{query}", parsed.path()))
      .chars()
      .collect();
    find_rule(&path, rules).map_or(Verdict::Unmatched, Verdict::Matched)
  }
}

/// What a `robots.txt` has to say about one URL. The two ways of saying
/// nothing are different answers, and the original keeps them apart as
/// `undefined` against `null`: a file that governs another origin
/// permits nothing, while one that governs this origin and matched no
/// rule permits everything.
enum Verdict<'a> {
  Foreign,
  Unmatched,
  Matched(&'a Rule),
}

/// The longest matching pattern wins; at equal length, `Allow` does.
fn find_rule<'a>(path: &[char], rules: &'a [Rule]) -> Option<&'a Rule> {
  let mut matched: Option<&Rule> = None;
  for rule in rules {
    if !matches(&rule.pattern, path) {
      continue;
    }
    match matched {
      None => matched = Some(rule),
      Some(best) if rule.length > best.length => matched = Some(rule),
      Some(best) if rule.length == best.length && rule.allow && !best.allow => matched = Some(rule),
      Some(_) => {},
    }
  }
  matched
}

/// Google's `robotstxt` matcher, which is what makes `*` and `$` behave
/// the way crawlers actually treat them.
///
/// It carries every length of `path` that the pattern so far could have
/// consumed, rather than backtracking: `*` widens that set to every
/// remaining length, a literal narrows it to the positions that match,
/// and `$` at the very end demands that the longest of them is the
/// whole path.
fn matches(pattern: &[char], path: &[char]) -> bool {
  let mut lengths = vec![0usize; path.len() + 1];
  let mut count = 1usize;

  for (index, &expected) in pattern.iter().enumerate() {
    if expected == '$' && index + 1 == pattern.len() {
      return lengths[count - 1] == path.len();
    }
    if expected == '*' {
      count = path.len() - lengths[0] + 1;
      for i in 1..count {
        lengths[i] = lengths[i - 1] + 1;
      }
    } else {
      let mut kept = 0usize;
      for i in 0..count {
        if lengths[i] < path.len() && path[lengths[i]] == expected {
          lengths[kept] = lengths[i] + 1;
          kept += 1;
        }
      }
      if kept == 0 {
        return false;
      }
      count = kept;
    }
  }
  true
}

/// `\r\n`, `\r` or `\n`, any of which ends a line.
///
/// `\r\n` has to end ONE line rather than two. Splitting on both
/// characters would insert an empty line between them, and an empty
/// line parses to nothing either way — but it shifts every line number
/// after it, and a line number is what `is-crawlable` reports.
fn split_lines(contents: &str) -> Vec<&str> {
  let bytes = contents.as_bytes();
  let mut lines = Vec::new();
  let (mut start, mut i) = (0usize, 0usize);
  while i < bytes.len() {
    match bytes[i] {
      b'\r' | b'\n' => {
        lines.push(&contents[start..i]);
        i += usize::from(bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n')) + 1;
        start = i;
      },
      _ => i += 1,
    }
  }
  lines.push(&contents[start..]);
  lines
}

fn remove_comments(line: &str) -> &str {
  match line.find('#') {
    Some(at) => &line[..at],
    None => line,
  }
}

/// Split at the first `:`, trimming both halves. `None` when the line
/// carries no `:` at all.
fn split_line(line: &str) -> Option<(&str, &str)> {
  let at = line.find(':')?;
  Some((trim(&line[..at]), trim(&line[at + 1..])))
}

/// JavaScript's `String.prototype.trim`, which also strips the byte
/// order mark.
fn trim(value: &str) -> &str {
  value.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

/// Lowercased, with any `robot/1.0` version suffix dropped.
fn format_user_agent(user_agent: &str) -> String {
  let lowered = user_agent.to_lowercase();
  let head = match lowered.find('/') {
    Some(at) => &lowered[..at],
    None => &lowered[..],
  };
  trim(head).to_string()
}

/// Patterns are compared against an already percent-encoded path, so
/// they are encoded the same way first. The `%25` step puts back a `%`
/// the author wrote themselves, which would otherwise become a literal
/// `%25` and match nothing.
fn normalise_encoding(pattern: &str) -> String {
  url_encode_to_upper(&encode_uri(pattern).replace("%25", "%"))
}

/// `encodeURI`: everything outside the unreserved and reserved sets
/// becomes its UTF-8 bytes, percent-encoded. `*` and `$` survive, which
/// is the whole reason this is not a generic escape.
fn encode_uri(value: &str) -> String {
  const KEEP: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789;,/?:@&=+$-_.!~*'()#";
  let mut out = String::with_capacity(value.len());
  for ch in value.chars() {
    if ch.is_ascii() && KEEP.contains(ch) {
      out.push(ch);
    } else {
      let mut buf = [0u8; 4];
      for byte in ch.encode_utf8(&mut buf).as_bytes() {
        out.push('%');
        out.push(
          char::from_digit(u32::from(byte >> 4), 16)
            .unwrap_or('0')
            .to_ascii_uppercase(),
        );
        out.push(
          char::from_digit(u32::from(byte & 0xf), 16)
            .unwrap_or('0')
            .to_ascii_uppercase(),
        );
      }
    }
  }
  out
}

/// `%2a` and `%2A` address the same byte, so both sides are raised to
/// upper case before they are compared.
fn url_encode_to_upper(value: &str) -> String {
  let chars: Vec<char> = value.chars().collect();
  let mut out = String::with_capacity(value.len());
  let mut i = 0;
  while i < chars.len() {
    if chars[i] == '%' && i + 2 < chars.len() && chars[i + 1].is_ascii_hexdigit() && chars[i + 2].is_ascii_hexdigit() {
      out.push('%');
      out.push(chars[i + 1].to_ascii_uppercase());
      out.push(chars[i + 2].to_ascii_uppercase());
      i += 3;
    } else {
      out.push(chars[i]);
      i += 1;
    }
  }
  out
}

/// The original resolves relative input against a domain it says can
/// never collide with a real one. Keeping the same base keeps the same
/// answers: two relative URLs land on one origin and compare equal.
const RELATIVE_BASE: &str = "http://robots-relative.samclarke.com/";

fn parse_url(url: &str) -> Option<reqwest::Url> {
  let base = reqwest::Url::parse(RELATIVE_BASE).ok()?;
  reqwest::Url::options().base_url(Some(&base)).parse(url).ok()
}

fn origin_of(url: &reqwest::Url) -> Origin {
  Origin {
    scheme: url.scheme().to_string(),
    host: url.host_str().map(std::string::ToString::to_string),
    port: url.port(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const SITE: &str = "http://example.com/robots.txt";

  fn robots(contents: &str) -> Robots {
    Robots::parse(SITE, contents)
  }

  #[test]
  fn an_empty_file_allows_everything() {
    let robots = robots("");
    assert_eq!(robots.is_allowed("http://example.com/anything", None), Some(true));
  }

  #[test]
  fn a_url_on_another_origin_is_not_this_files_business() {
    let robots = robots("User-agent: *\nDisallow: /");
    assert_eq!(robots.is_allowed("http://elsewhere.com/", None), None);
    assert_eq!(robots.is_allowed("https://example.com/", None), None);
    assert_eq!(robots.is_allowed("http://example.com:8080/", None), None);
  }

  #[test]
  fn the_default_port_is_the_same_origin_as_no_port() {
    let robots = Robots::parse("http://example.com:80/robots.txt", "User-agent: *\nDisallow: /x");
    assert_eq!(robots.is_allowed("http://example.com/x", None), Some(false));
  }

  #[test]
  fn the_longest_pattern_decides_and_allow_breaks_a_tie() {
    let robots = robots("User-agent: *\nDisallow: /a/\nAllow: /a/b/");
    assert_eq!(robots.is_allowed("http://example.com/a/c", None), Some(false));
    assert_eq!(robots.is_allowed("http://example.com/a/b/c", None), Some(true));

    let tied = Robots::parse(SITE, "User-agent: *\nDisallow: /page\nAllow: /page");
    assert_eq!(tied.is_allowed("http://example.com/page", None), Some(true));
  }

  #[test]
  fn a_star_matches_across_segments_and_a_trailing_dollar_anchors() {
    let robots = robots("User-agent: *\nDisallow: /*.pdf$");
    assert_eq!(robots.is_allowed("http://example.com/a/b/c.pdf", None), Some(false));
    assert_eq!(robots.is_allowed("http://example.com/a/b/c.pdf.html", None), Some(true));
  }

  #[test]
  fn the_query_string_is_part_of_the_path_a_pattern_matches() {
    let robots = robots("User-agent: *\nDisallow: /*?session=");
    assert_eq!(robots.is_allowed("http://example.com/a?session=1", None), Some(false));
    assert_eq!(robots.is_allowed("http://example.com/a", None), Some(true));
  }

  #[test]
  fn a_named_agent_takes_its_own_group_and_not_the_wildcards() {
    let robots = robots("User-agent: *\nDisallow: /\n\nUser-agent: Googlebot\nAllow: /");
    assert_eq!(robots.is_allowed("http://example.com/x", None), Some(false));
    assert_eq!(robots.is_allowed("http://example.com/x", Some("Googlebot")), Some(true));
    // Case and a version suffix are both normalised away.
    assert_eq!(
      robots.is_allowed("http://example.com/x", Some("googlebot/2.1")),
      Some(true)
    );
  }

  #[test]
  fn an_empty_disallow_registers_the_agent_without_a_rule() {
    // Which is how `Disallow:` means "allowed": the agent now has its
    // own (empty) group, so the wildcard group no longer reaches it.
    let robots = robots("User-agent: *\nDisallow: /\n\nUser-agent: bingbot\nDisallow:");
    assert_eq!(robots.is_allowed("http://example.com/x", Some("bingbot")), Some(true));
    assert_eq!(robots.is_allowed("http://example.com/x", Some("other")), Some(false));
  }

  #[test]
  fn a_crawl_delay_alone_also_claims_the_agent() {
    let robots = robots("User-agent: *\nDisallow: /\n\nUser-agent: slowbot\nCrawl-delay: 10");
    assert_eq!(robots.is_allowed("http://example.com/x", Some("slowbot")), Some(true));
  }

  #[test]
  fn consecutive_user_agent_lines_share_one_group() {
    let robots = robots("User-agent: a\nUser-agent: b\nDisallow: /x");
    assert_eq!(robots.is_allowed("http://example.com/x", Some("a")), Some(false));
    assert_eq!(robots.is_allowed("http://example.com/x", Some("b")), Some(false));
  }

  #[test]
  fn a_comment_ends_the_line_it_appears_on() {
    let robots = robots("User-agent: * # everyone\nDisallow: /x # not this\n# nothing here");
    assert_eq!(robots.is_allowed("http://example.com/x", None), Some(false));
  }

  #[test]
  fn percent_encoding_is_compared_in_one_case() {
    let robots = robots("User-agent: *\nDisallow: /a%2fb");
    assert_eq!(robots.is_allowed("http://example.com/a%2Fb", None), Some(false));
  }

  #[test]
  fn the_line_number_is_the_one_the_rule_was_written_on() {
    let robots = robots("User-agent: *\nDisallow: /x");
    assert_eq!(robots.matching_line_number("http://example.com/x", None), 2);
    assert_eq!(robots.matching_line_number("http://example.com/y", None), -1);
  }

  #[test]
  fn a_carriage_return_ends_a_line_and_still_counts_as_one() {
    let robots = robots("User-agent: *\r\nDisallow: /x");
    assert_eq!(robots.matching_line_number("http://example.com/x", None), 2);
  }
}
