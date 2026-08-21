//! How a failure reaches the person who has to fix it.
//!
//! `anyhow` stays the carrier — every command already returns it, and the
//! context chain is the useful part of a failure. What was missing is the
//! rendering: the default `Error: …` line prints the chain as debug output,
//! with no colour, no separation between the failure and its causes, and
//! nothing about what to do next. That last part is most of the value, so
//! [`hints`] recognises the failures this binary actually produces and answers
//! them with the command that fixes each one.

use crate::ui;

/// The command failed and has already said so on its own output.
///
/// Carries the non-zero exit status without printing a second banner — for a
/// command whose failure IS its output, like a test run that already printed a
/// reporter summary.
#[derive(Debug)]
pub struct AlreadyReported;

impl std::fmt::Display for AlreadyReported {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("")
  }
}

impl std::error::Error for AlreadyReported {}

/// Print a failure and return the process exit status for it.
#[must_use]
pub fn report(err: &anyhow::Error) -> i32 {
  if err.downcast_ref::<AlreadyReported>().is_some() {
    return 1;
  }
  if ui::json() {
    let causes: Vec<String> = err.chain().skip(1).map(ToString::to_string).collect();
    let payload = serde_json::json!({
      "error": err.to_string(),
      "causes": causes,
      "hints": hints(err).into_iter().map(|(what, cmd)| {
        serde_json::json!({ "what": what, "command": cmd })
      }).collect::<Vec<_>>(),
    });
    // The document is the failure report; a parser reading stdout gets the
    // same shape whether the run succeeded or not.
    if let Ok(text) = serde_json::to_string_pretty(&payload) {
      println!("{text}");
    }
    return 1;
  }

  eprintln!("\n{}", ui::failure(&err.to_string()));
  for cause in err.chain().skip(1) {
    eprintln!("  {} {}", ui::dim("caused by"), cause);
  }
  let hints = hints(err);
  if !hints.is_empty() {
    eprintln!("\n{}", ui::header("Try"));
    for (what, cmd) in hints {
      eprintln!("  {} {}", ui::dim(&format!("{what}:")), ui::code(&cmd));
    }
  }
  eprintln!();
  1
}

/// Commands that address a recognised failure.
///
/// Matched on the whole cause chain rendered flat, because the sentence that
/// identifies a failure is usually not the outermost context — a missing
/// browser surfaces as "failed to launch", with the path only in the cause.
#[must_use]
pub fn hints(err: &anyhow::Error) -> Vec<(&'static str, String)> {
  let text = err
    .chain()
    .map(ToString::to_string)
    .collect::<Vec<_>>()
    .join("\n")
    .to_ascii_lowercase();
  hints_for_text(&text)
}

/// The decision behind [`hints`], over the flattened chain, so it can be
/// tested without constructing error values.
fn hints_for_text(text: &str) -> Vec<(&'static str, String)> {
  let mut out: Vec<(&'static str, String)> = Vec::new();
  let mut push = |what: &'static str, cmd: &str| {
    if !out.iter().any(|(_, existing)| existing == cmd) {
      out.push((what, cmd.to_string()));
    }
  };

  // A named config that is not on disk is answered by writing one, not by
  // asking what resolved — so this is checked before the general config case.
  let missing_config = text.contains("does not exist") && (text.contains("--config") || text.contains("ferridriver."));
  if missing_config {
    push("scaffold a project", "ferridriver init");
    return out;
  }

  // A missing browser names the browser, not the word "browser" — the
  // launcher reports the executable it could not find.
  let names_a_browser = ["browser", "chromium", "chrome", "firefox", "webkit"]
    .iter()
    .any(|name| text.contains(name));
  if names_a_browser && (text.contains("not installed") || text.contains("no such file")) {
    push("install the browser", "ferridriver install chromium");
  }
  // Phrases, not words: "connect" alone matches "disconnected", which every
  // page teardown says, and would offer `doctor` for every clean exit.
  if [
    "failed to launch",
    "browser exited",
    "failed to connect",
    "connection refused",
  ]
  .iter()
  .any(|phrase| text.contains(phrase))
  {
    push("check the setup", "ferridriver doctor");
  }
  if text.contains("typescript compiler") || text.contains("no checker") {
    push("install a type checker", "npm i -D typescript");
  }
  if text.contains("extension") {
    push("inspect the extensions", "ferridriver ext check");
  }
  if text.contains("config error") || text.contains("unknown key") || text.contains("no configuration") {
    push("see what resolved", "ferridriver config");
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_missing_browser_offers_the_installer() {
    let hints = hints_for_text("failed to launch chromium: no such file or directory");
    assert!(
      hints.iter().any(|(_, c)| c == "ferridriver install chromium"),
      "{hints:?}"
    );
    assert!(hints.iter().any(|(_, c)| c == "ferridriver doctor"), "{hints:?}");
  }

  #[test]
  fn an_unrecognised_failure_offers_nothing() {
    assert!(hints_for_text("the disk is on fire").is_empty());
  }

  #[test]
  fn the_same_command_is_never_offered_twice() {
    let hints = hints_for_text("config error: unknown key, no configuration found");
    let count = hints.iter().filter(|(_, c)| c == "ferridriver config").count();
    assert_eq!(count, 1, "{hints:?}");
  }

  #[test]
  fn a_named_config_that_is_missing_is_answered_by_writing_one() {
    let hints = hints_for_text("--config /nope.toml does not exist");
    assert_eq!(hints.len(), 1, "{hints:?}");
    assert_eq!(hints[0].1, "ferridriver init");
  }

  #[test]
  fn a_routine_disconnect_does_not_offer_doctor() {
    // "connect" as a substring matches "disconnected", which page teardown
    // says on every clean run.
    assert!(hints_for_text("target disconnected while closing the page").is_empty());
  }

  #[test]
  fn already_reported_is_silent_but_fatal() {
    let err = anyhow::Error::new(AlreadyReported);
    assert_eq!(err.to_string(), "");
  }
}
