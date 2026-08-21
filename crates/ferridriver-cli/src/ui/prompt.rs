//! Questions asked of the person at the terminal.
//!
//! Every one of them has to answer itself when nobody is there: these commands
//! run in CI, under an agent, and behind a pipe at least as often as they run
//! interactively. A prompt with no terminal takes its default rather than
//! blocking on a stdin that will never produce a line.

use console::{Style, Term};

/// Ask a yes/no question. Returns `default` unchanged when the session is not
/// interactive, so a script never hangs on it.
///
/// # Errors
/// When the terminal cannot be read.
pub fn confirm(question: &str, default: bool) -> anyhow::Result<bool> {
  if !super::interactive() {
    return Ok(default);
  }
  let hint = if default { "Y/n" } else { "y/N" };
  let term = Term::stdout();
  loop {
    term.write_str(&format!(
      "{} {question} {} ",
      Style::new().cyan().apply_to("?"),
      Style::new().dim().apply_to(format!("[{hint}]"))
    ))?;
    term.flush()?;
    let answer = term.read_line()?;
    match answer.trim().to_ascii_lowercase().as_str() {
      "" => return Ok(default),
      "y" | "yes" => return Ok(true),
      "n" | "no" => return Ok(false),
      _ => term.write_line(&super::warning("answer y or n"))?,
    }
  }
}
