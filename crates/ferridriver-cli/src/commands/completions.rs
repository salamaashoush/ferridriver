//! `ferridriver completions` — emit a shell completion script.
//!
//! Generated from the same `clap` command the binary parses with, so
//! completions cannot describe a flag that no longer exists.

use std::io::Write as _;

use clap::CommandFactory as _;
use clap_complete::Shell;

use crate::cli;
use crate::ui;

pub fn run(args: &cli::CompletionsArgs) -> anyhow::Result<()> {
  let shell = match args.shell {
    Some(shell) => shell,
    None => detect().ok_or_else(|| {
      anyhow::anyhow!("could not detect the shell from $SHELL — name one: bash, zsh, fish, elvish, powershell")
    })?,
  };

  // Generated into a buffer first: `clap_complete` writing straight to stdout
  // panics when the consumer closes the pipe early, which `| head` does.
  let mut script = Vec::new();
  clap_complete::generate(shell, &mut cli::Cli::command(), "ferridriver", &mut script);
  std::io::stdout().write_all(&script)?;

  ui::note(&format!(
    "{shell} completions written to stdout — {}",
    install_hint(shell)
  ));
  Ok(())
}

/// The shell the user is in, from the running shell's own marker first and
/// `$SHELL` second: `$SHELL` is the login shell, which is routinely not the
/// one at the prompt.
fn detect() -> Option<Shell> {
  if std::env::var_os("ZSH_VERSION").is_some() {
    return Some(Shell::Zsh);
  }
  if std::env::var_os("BASH_VERSION").is_some() {
    return Some(Shell::Bash);
  }
  if std::env::var_os("FISH_VERSION").is_some() {
    return Some(Shell::Fish);
  }
  let shell = std::env::var("SHELL").ok()?;
  let name = std::path::Path::new(&shell).file_name()?.to_str()?;
  match name {
    "zsh" => Some(Shell::Zsh),
    "bash" => Some(Shell::Bash),
    "fish" => Some(Shell::Fish),
    "elvish" => Some(Shell::Elvish),
    "pwsh" | "powershell" => Some(Shell::PowerShell),
    _ => None,
  }
}

/// Where this shell expects the script to end up. Printed on stderr so it
/// never lands inside the redirect that captured the script.
fn install_hint(shell: Shell) -> &'static str {
  match shell {
    Shell::Zsh => "put it on your $fpath, e.g. ~/.zfunc/_ferridriver",
    Shell::Bash => "source it from ~/.bashrc",
    Shell::Fish => "save it as ~/.config/fish/completions/ferridriver.fish",
    Shell::PowerShell => "add it to your $PROFILE",
    _ => "install it where this shell looks for completions",
  }
}
