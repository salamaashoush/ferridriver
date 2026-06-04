//! The `allow.commands` capability schema and `${var}` resolution.
//!
//! A plugin declares each command it may run; the handler can only
//! invoke declared names (default-deny). A spec is either a shorthand
//! string (a `sh -c` line) or an object with explicit execution policy.
//! Resolution is *strict*: every `${placeholder}` must have a supplied
//! value and every value must be a scalar — a typo fails loudly instead
//! of expanding to empty.
//!
//! Deserializer-agnostic by hand (no `#[serde(untagged)]`): the same
//! types are read from `serde_json` (the MCP manifest round-trip) and
//! from `rquickjs-serde` (a `tool` call), so the impls only use
//! `deserialize_any` + visitors, which both back-ends support.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

/// What to execute. A string runs through `sh -c` (shell features
/// live); an array is executed directly with no shell (each element is
/// one argv entry — no quoting, no metacharacter interpretation).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum CommandRun {
  Shell(String),
  Argv(Vec<String>),
}

impl<'de> Deserialize<'de> for CommandRun {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    struct V;
    impl<'de> Visitor<'de> for V {
      type Value = CommandRun;
      fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a shell string or an argv array of strings")
      }
      fn visit_str<E: de::Error>(self, v: &str) -> Result<CommandRun, E> {
        Ok(CommandRun::Shell(v.to_owned()))
      }
      fn visit_string<E: de::Error>(self, v: String) -> Result<CommandRun, E> {
        Ok(CommandRun::Shell(v))
      }
      fn visit_seq<A: SeqAccess<'de>>(self, mut s: A) -> Result<CommandRun, A::Error> {
        let mut out = Vec::new();
        while let Some(e) = s.next_element::<String>()? {
          out.push(e);
        }
        if out.is_empty() {
          return Err(de::Error::custom("argv array must have at least one element"));
        }
        Ok(CommandRun::Argv(out))
      }
    }
    d.deserialize_any(V)
  }
}

/// How `commands.run` interprets the command's stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandOutput {
  /// Trimmed string (default — no guessing).
  #[default]
  Text,
  /// Parsed as JSON; invalid JSON is an error.
  Json,
  /// Split into an array of non-empty trimmed lines.
  Lines,
}

/// One declared command. Constructed only via deserialization (manifest
/// or `tool`); never hand-built by a handler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandSpec {
  pub run: CommandRun,
  /// Hard wall-clock bound; on expiry the process group is killed.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub timeout_ms: Option<u64>,
  /// Server env var names passed through to the child. The child env is
  /// otherwise scrubbed (only `PATH` is kept so binaries resolve), so a
  /// command never inherits ambient server secrets it did not ask for.
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub env: Vec<String>,
  /// Working directory for the child (absolute, or relative to the
  /// server process cwd). Default: inherit.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub cwd: Option<String>,
  pub output: CommandOutput,
  /// A long-running process (server/watcher): managed via
  /// `start`/`status`/`stop`, lifetime tied to the session, never via
  /// `run`. A one-shot spec rejects `start`/`status`/`stop` and vice
  /// versa.
  #[serde(skip_serializing_if = "std::ops::Not::not")]
  pub persistent: bool,
}

impl<'de> Deserialize<'de> for CommandSpec {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    struct V;
    impl<'de> Visitor<'de> for V {
      type Value = CommandSpec;
      fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a shell-command string or a command spec object")
      }
      fn visit_str<E: de::Error>(self, v: &str) -> Result<CommandSpec, E> {
        Ok(CommandSpec::shell(v))
      }
      fn visit_string<E: de::Error>(self, v: String) -> Result<CommandSpec, E> {
        Ok(CommandSpec::shell(&v))
      }
      fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<CommandSpec, A::Error> {
        let mut run: Option<CommandRun> = None;
        let mut timeout_ms: Option<u64> = None;
        let mut env: Vec<String> = Vec::new();
        let mut cwd: Option<String> = None;
        let mut output = CommandOutput::Text;
        let mut persistent = false;
        while let Some(k) = m.next_key::<String>()? {
          match k.as_str() {
            "run" => run = Some(m.next_value()?),
            "timeoutMs" | "timeout_ms" => timeout_ms = m.next_value()?,
            "env" => env = m.next_value()?,
            "cwd" => cwd = m.next_value()?,
            "output" => output = m.next_value()?,
            "persistent" => persistent = m.next_value()?,
            _ => {
              let _: de::IgnoredAny = m.next_value()?;
            },
          }
        }
        let run = run.ok_or_else(|| de::Error::missing_field("run"))?;
        Ok(CommandSpec {
          run,
          timeout_ms,
          env,
          cwd,
          output,
          persistent,
        })
      }
    }
    d.deserialize_any(V)
  }
}

/// A command resolved against caller `vars`: ready to spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedExec {
  /// `sh -c <line>` — values shell-escaped.
  Shell(String),
  /// Direct argv — no shell, values substituted literally per element.
  Argv(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct ResolvedCommand {
  pub exec: ResolvedExec,
  pub timeout_ms: Option<u64>,
  pub env: Vec<String>,
  pub cwd: Option<String>,
  pub output: CommandOutput,
  pub persistent: bool,
}

impl CommandSpec {
  fn shell(s: &str) -> Self {
    Self {
      run: CommandRun::Shell(s.to_owned()),
      timeout_ms: None,
      env: Vec::new(),
      cwd: None,
      output: CommandOutput::Text,
      persistent: false,
    }
  }

  /// Strictly substitute `${name}` placeholders with `vars`. Every
  /// placeholder must be supplied and scalar; an unknown placeholder or
  /// a non-scalar value is an error (no silent empty, no JSON blob).
  ///
  /// # Errors
  /// A `${name}` with no value, or a value that is an object/array/null.
  pub fn resolve(&self, vars: &BTreeMap<String, serde_json::Value>) -> Result<ResolvedCommand, String> {
    let scalar = |k: &str| -> Result<String, String> {
      match vars.get(k) {
        None => Err(format!("missing value for `${{{k}}}`")),
        Some(serde_json::Value::String(s)) => Ok(s.clone()),
        Some(serde_json::Value::Number(n)) => Ok(n.to_string()),
        Some(serde_json::Value::Bool(b)) => Ok(b.to_string()),
        Some(_) => Err(format!("value for `${{{k}}}` must be a string, number, or boolean")),
      }
    };

    let exec = match &self.run {
      CommandRun::Shell(tpl) => ResolvedExec::Shell(subst(tpl, |k| scalar(k).map(|v| shell_single_quote(&v)))?),
      CommandRun::Argv(args) => {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
          out.push(subst(a, &scalar)?);
        }
        ResolvedExec::Argv(out)
      },
    };
    Ok(ResolvedCommand {
      exec,
      timeout_ms: self.timeout_ms,
      env: self.env.clone(),
      cwd: self.cwd.clone(),
      output: self.output,
      persistent: self.persistent,
    })
  }
}

/// Replace every `${name}` in `tpl`. `repl` produces the replacement (it
/// errors on an unknown name). A lone `$`, or `${` with no closing `}`,
/// is a literal. `$${x}` is NOT special — `${x}` still substitutes;
/// templates that need a literal `${` should use argv form.
fn subst(tpl: &str, mut repl: impl FnMut(&str) -> Result<String, String>) -> Result<String, String> {
  let mut out = String::with_capacity(tpl.len());
  let b = tpl.as_bytes();
  let mut i = 0;
  while i < b.len() {
    if b[i] == b'$' && i + 1 < b.len() && b[i + 1] == b'{' {
      if let Some(end_rel) = tpl[i + 2..].find('}') {
        let name = &tpl[i + 2..i + 2 + end_rel];
        out.push_str(&repl(name)?);
        i = i + 2 + end_rel + 1;
        continue;
      }
    }
    // push one UTF-8 char from i
    let ch = tpl[i..].chars().next().unwrap_or('\u{FFFD}');
    out.push(ch);
    i += ch.len_utf8();
  }
  Ok(out)
}

fn shell_single_quote(s: &str) -> String {
  format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn vars(pairs: &[(&str, serde_json::Value)]) -> BTreeMap<String, serde_json::Value> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
  }

  #[test]
  fn shorthand_string_is_a_shell_oneshot() {
    let s: CommandSpec = serde_json::from_str(r#""git status""#).unwrap();
    assert_eq!(s.run, CommandRun::Shell("git status".into()));
    assert!(!s.persistent);
    assert_eq!(s.output, CommandOutput::Text);
  }

  #[test]
  fn object_with_argv_and_policy() {
    let s: CommandSpec = serde_json::from_str(
      r#"{"run":["git","-C","${repo}","rev-parse","HEAD"],"timeoutMs":5000,"env":["HOME"],"output":"json"}"#,
    )
    .unwrap();
    assert_eq!(s.timeout_ms, Some(5000));
    assert_eq!(s.env, vec!["HOME"]);
    assert_eq!(s.output, CommandOutput::Json);
    let r = s.resolve(&vars(&[("repo", serde_json::json!("/srv/app"))])).unwrap();
    assert_eq!(
      r.exec,
      ResolvedExec::Argv(vec![
        "git".into(),
        "-C".into(),
        "/srv/app".into(),
        "rev-parse".into(),
        "HEAD".into()
      ])
    );
  }

  #[test]
  fn argv_substitution_is_not_shell_escaped() {
    let s: CommandSpec = serde_json::from_str(r#"{"run":["echo","${msg}"]}"#).unwrap();
    let r = s.resolve(&vars(&[("msg", serde_json::json!("a; rm -rf /"))])).unwrap();
    // Raw, single argv element — no shell, so the metacharacters are inert.
    assert_eq!(r.exec, ResolvedExec::Argv(vec!["echo".into(), "a; rm -rf /".into()]));
  }

  #[test]
  fn shell_substitution_is_single_quoted() {
    let s = CommandSpec::shell("echo ${msg}");
    let r = s
      .resolve(&vars(&[("msg", serde_json::json!("a'b; rm -rf /"))]))
      .unwrap();
    assert_eq!(r.exec, ResolvedExec::Shell(r"echo 'a'\''b; rm -rf /'".to_string()));
  }

  #[test]
  fn missing_placeholder_is_an_error() {
    let s = CommandSpec::shell("deploy ${env} ${tag}");
    let e = s.resolve(&vars(&[("env", serde_json::json!("prod"))])).unwrap_err();
    assert!(e.contains("${tag}"), "{e}");
  }

  #[test]
  fn non_scalar_value_is_an_error() {
    let s = CommandSpec::shell("x ${o}");
    let e = s.resolve(&vars(&[("o", serde_json::json!({"a":1}))])).unwrap_err();
    assert!(e.contains("must be a string, number, or boolean"), "{e}");
  }

  #[test]
  fn persistent_flag_round_trips() {
    let s: CommandSpec = serde_json::from_str(r#"{"run":"node server.js","persistent":true}"#).unwrap();
    assert!(s.persistent);
  }
}
