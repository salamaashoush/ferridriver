//! Bake this build's identity into the binary.
//!
//! A bug report that says "0.5.0" names a version anyone can be running; one
//! that names the commit, the channel and the target says which build. The
//! channel is what `ferridriver upgrade` follows, so it has to be decided
//! here — at compile time, by the workflow that publishes the artifact —
//! rather than guessed at run time.
//!
//! Set `FERRIDRIVER_CANARY=1` to cut a canary build. Everything else degrades
//! to an empty string, because a crates.io tarball has no `.git` and must
//! still compile.

use std::process::Command;

fn main() {
  let sha = git(&["rev-parse", "--short=9", "HEAD"]).unwrap_or_default();
  let dirty = Command::new("git")
    .args(["status", "--porcelain", "--untracked-files=no"])
    .output()
    .ok()
    .filter(|out| out.status.success())
    .is_some_and(|out| !out.stdout.is_empty());

  let release = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
  let canary = std::env::var("FERRIDRIVER_CANARY").is_ok_and(|v| v == "1");

  // A canary carries the commit in the version itself, the way `bun
  // --version` does, so two canaries from the same release line are still
  // distinguishable — the number alone cannot tell them apart.
  let version = match (canary, sha.is_empty()) {
    (true, false) => format!("{release}-canary.{sha}"),
    (true, true) => format!("{release}-canary"),
    (false, _) => release.clone(),
  };

  emit("FERRIDRIVER_VERSION", &version);
  emit("FERRIDRIVER_RELEASE_VERSION", &release);
  emit("FERRIDRIVER_CHANNEL", if canary { "canary" } else { "stable" });
  emit("FERRIDRIVER_GIT_SHA", &sha);
  emit("FERRIDRIVER_BUILD_DIRTY", if dirty { "-dirty" } else { "" });
  let target = std::env::var("TARGET").unwrap_or_default();
  emit("FERRIDRIVER_BUILD_TARGET", &target);

  // Assembled here rather than at run time because `--version` is a clap
  // attribute, which takes a `&'static str` and cannot call a function.
  let mut detail = Vec::new();
  if !sha.is_empty() {
    detail.push(format!("{sha}{}", if dirty { "-dirty" } else { "" }));
  }
  detail.push(if canary {
    "canary".to_string()
  } else {
    "stable".to_string()
  });
  if !target.is_empty() {
    detail.push(target);
  }
  emit(
    "FERRIDRIVER_LONG_VERSION",
    &format!("{version} ({})", detail.join(", ")),
  );

  // Re-run when the checked-out commit changes or the channel is switched,
  // not on every build.
  println!("cargo:rerun-if-changed=../../.git/HEAD");
  println!("cargo:rerun-if-changed=../../.git/index");
  println!("cargo:rerun-if-env-changed=FERRIDRIVER_CANARY");
}

fn git(args: &[&str]) -> Option<String> {
  let out = Command::new("git").args(args).output().ok()?;
  if !out.status.success() {
    return None;
  }
  Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}

fn emit(key: &str, value: &str) {
  println!("cargo:rustc-env={key}={value}");
}
