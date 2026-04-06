//! Tag expression parser and evaluator, grep filtering.
//!
//! Supports: `@tag`, `not @tag`, `@a and @b`, `@a or @b`, `(@a or @b) and not @c`.

use crate::scenario::ScenarioExecution;

/// AST for tag filter expressions.
#[derive(Debug, Clone)]
pub enum TagExpression {
  /// Matches a single tag (e.g., `@smoke`).
  Tag(String),
  /// Negation: `not @tag`.
  Not(Box<TagExpression>),
  /// Conjunction: `@a and @b`.
  And(Box<TagExpression>, Box<TagExpression>),
  /// Disjunction: `@a or @b`.
  Or(Box<TagExpression>, Box<TagExpression>),
}

impl TagExpression {
  /// Parse a tag expression string.
  ///
  /// Grammar:
  /// ```text
  /// expr     = or_expr
  /// or_expr  = and_expr ("or" and_expr)*
  /// and_expr = not_expr ("and" not_expr)*
  /// not_expr = "not" not_expr | atom
  /// atom     = "@" IDENT | "(" expr ")"
  /// ```
  pub fn parse(input: &str) -> Result<Self, String> {
    let tokens = tokenize(input)?;
    let mut pos = 0;
    let result = parse_or(&tokens, &mut pos)?;
    if pos < tokens.len() {
      return Err(format!("unexpected token: {:?}", tokens[pos]));
    }
    Ok(result)
  }

  /// Evaluate the expression against a set of tags.
  pub fn matches(&self, tags: &[String]) -> bool {
    match self {
      Self::Tag(t) => tags.iter().any(|tag| tag == t),
      Self::Not(inner) => !inner.matches(tags),
      Self::And(a, b) => a.matches(tags) && b.matches(tags),
      Self::Or(a, b) => a.matches(tags) || b.matches(tags),
    }
  }
}

// ── Tokenizer ──

#[derive(Debug, Clone, PartialEq)]
enum Token {
  Tag(String),
  And,
  Or,
  Not,
  LParen,
  RParen,
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
  let mut tokens = Vec::new();
  let mut chars = input.chars().peekable();

  while let Some(&c) = chars.peek() {
    match c {
      ' ' | '\t' | '\n' | '\r' => {
        chars.next();
      }
      '(' => {
        tokens.push(Token::LParen);
        chars.next();
      }
      ')' => {
        tokens.push(Token::RParen);
        chars.next();
      }
      '@' => {
        chars.next();
        let mut name = String::new();
        while let Some(&nc) = chars.peek() {
          if nc.is_alphanumeric() || nc == '_' || nc == '-' {
            name.push(nc);
            chars.next();
          } else {
            break;
          }
        }
        if name.is_empty() {
          return Err("expected tag name after '@'".to_string());
        }
        tokens.push(Token::Tag(format!("@{name}")));
      }
      _ => {
        let mut word = String::new();
        while let Some(&nc) = chars.peek() {
          if nc.is_alphanumeric() || nc == '_' {
            word.push(nc);
            chars.next();
          } else {
            break;
          }
        }
        match word.as_str() {
          "and" => tokens.push(Token::And),
          "or" => tokens.push(Token::Or),
          "not" => tokens.push(Token::Not),
          "" => return Err(format!("unexpected character: '{c}'")),
          _ => return Err(format!("unexpected word: '{word}'")),
        }
      }
    }
  }

  Ok(tokens)
}

// ── Recursive descent parser ──

fn parse_or(tokens: &[Token], pos: &mut usize) -> Result<TagExpression, String> {
  let mut left = parse_and(tokens, pos)?;
  while *pos < tokens.len() && tokens[*pos] == Token::Or {
    *pos += 1;
    let right = parse_and(tokens, pos)?;
    left = TagExpression::Or(Box::new(left), Box::new(right));
  }
  Ok(left)
}

fn parse_and(tokens: &[Token], pos: &mut usize) -> Result<TagExpression, String> {
  let mut left = parse_not(tokens, pos)?;
  while *pos < tokens.len() && tokens[*pos] == Token::And {
    *pos += 1;
    let right = parse_not(tokens, pos)?;
    left = TagExpression::And(Box::new(left), Box::new(right));
  }
  Ok(left)
}

fn parse_not(tokens: &[Token], pos: &mut usize) -> Result<TagExpression, String> {
  if *pos < tokens.len() && tokens[*pos] == Token::Not {
    *pos += 1;
    let inner = parse_not(tokens, pos)?;
    return Ok(TagExpression::Not(Box::new(inner)));
  }
  parse_atom(tokens, pos)
}

fn parse_atom(tokens: &[Token], pos: &mut usize) -> Result<TagExpression, String> {
  if *pos >= tokens.len() {
    return Err("unexpected end of expression".to_string());
  }

  match &tokens[*pos] {
    Token::Tag(name) => {
      let result = TagExpression::Tag(name.clone());
      *pos += 1;
      Ok(result)
    }
    Token::LParen => {
      *pos += 1;
      let inner = parse_or(tokens, pos)?;
      if *pos >= tokens.len() || tokens[*pos] != Token::RParen {
        return Err("expected closing ')'".to_string());
      }
      *pos += 1;
      Ok(inner)
    }
    other => Err(format!("unexpected token: {other:?}")),
  }
}

// ── Scenario filtering ──

/// Filter scenarios by tag expression.
pub fn filter_scenarios(scenarios: &mut Vec<ScenarioExecution>, expr: &TagExpression) {
  scenarios.retain(|s| expr.matches(&s.tags));
}

/// Filter scenarios by grep pattern (scenario name match).
pub fn filter_by_grep(scenarios: &mut Vec<ScenarioExecution>, pattern: &str, invert: bool) {
  let re = match regex::Regex::new(pattern) {
    Ok(r) => r,
    Err(_) => return,
  };

  scenarios.retain(|s| {
    let matches = re.is_match(&s.name);
    if invert {
      !matches
    } else {
      matches
    }
  });
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parse_single_tag() {
    let expr = TagExpression::parse("@smoke").unwrap();
    assert!(expr.matches(&["@smoke".to_string()]));
    assert!(!expr.matches(&["@wip".to_string()]));
  }

  #[test]
  fn parse_not() {
    let expr = TagExpression::parse("not @wip").unwrap();
    assert!(!expr.matches(&["@wip".to_string()]));
    assert!(expr.matches(&["@smoke".to_string()]));
    assert!(expr.matches(&[]));
  }

  #[test]
  fn parse_and() {
    let expr = TagExpression::parse("@smoke and @fast").unwrap();
    assert!(expr.matches(&["@smoke".to_string(), "@fast".to_string()]));
    assert!(!expr.matches(&["@smoke".to_string()]));
  }

  #[test]
  fn parse_or() {
    let expr = TagExpression::parse("@smoke or @fast").unwrap();
    assert!(expr.matches(&["@smoke".to_string()]));
    assert!(expr.matches(&["@fast".to_string()]));
    assert!(!expr.matches(&["@slow".to_string()]));
  }

  #[test]
  fn parse_complex() {
    let expr = TagExpression::parse("(@smoke or @regression) and not @wip").unwrap();
    assert!(expr.matches(&["@smoke".to_string()]));
    assert!(!expr.matches(&["@smoke".to_string(), "@wip".to_string()]));
    assert!(expr.matches(&["@regression".to_string()]));
    assert!(!expr.matches(&["@other".to_string()]));
  }
}
