#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `--config <file.ts>` — a config written as a module.
//!
//! The claim worth testing at the process boundary is equivalence: a
//! `playwright.config.ts` calling `defineConfig` discovers the same
//! corpus and the same project names as the `ferridriver.toml` that
//! says the same thing. Observed through `ferridriver test --list`,
//! which resolves config, bundles and discovers without launching a
//! browser.
//!
//! Requires a built `ferridriver` binary (`FERRIDRIVER_BIN` or
//! `target/{debug,release}/ferridriver`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> String {
  std::env::var("FERRIDRIVER_BIN").unwrap_or_else(|_| {
    let base = format!("{}/../../target", env!("CARGO_MANIFEST_DIR"));
    let debug = format!("{base}/debug/ferridriver");
    if Path::new(&debug).exists() {
      debug
    } else {
      format!("{base}/release/ferridriver")
    }
  })
}

const SPEC: &str = "
import { test, expect } from '@ferridriver/test';
test('runs', async ({}) => { expect(1).toBe(1); });
";

/// A workspace with two spec files and whatever config the case needs.
///
/// `ferridriver.toml` is always present and always minimal: the module
/// layers ON TOP of the discovered stack rather than replacing it, and
/// a case that wrote everything in one file could not tell the two
/// apart.
fn scratch(case: &str, toml_extra: &str, module: Option<&str>) -> PathBuf {
  let dir = std::env::temp_dir().join(format!("ferri-script-config-{case}-{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(dir.join("specs")).expect("workspace");
  for name in ["alpha", "beta"] {
    std::fs::write(dir.join(format!("specs/{name}.spec.ts")), SPEC).expect("spec");
  }
  std::fs::write(
    dir.join("ferridriver.toml"),
    format!(
      "[test]\n\
       workers = 1\n\
       reporter = [{{ name = \"list\" }}]\n\
       {toml_extra}\n\
       [test.browser]\n\
       browser = \"chromium\"\n\
       backend = \"cdp-pipe\"\n\
       headless = true\n"
    ),
  )
  .expect("config");
  if let Some(source) = module {
    std::fs::write(dir.join("playwright.config.ts"), source).expect("module");
  }
  dir
}

fn list(dir: &Path, extra: &[&str]) -> Output {
  let mut args = vec!["test", "--list"];
  args.extend_from_slice(extra);
  Command::new(bin())
    .current_dir(dir)
    .args(args)
    .output()
    .expect("run ferridriver test --list")
}

fn combined(output: &Output) -> String {
  format!(
    "{}{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  )
}

/// The listed corpus, as a sorted set of lines mentioning a spec, so two
/// runs can be compared without depending on discovery order.
fn corpus(text: &str) -> Vec<String> {
  let mut lines: Vec<String> = text
    .lines()
    .filter(|l| l.contains(".spec.ts"))
    .map(|l| l.trim().to_string())
    .collect();
  lines.sort();
  lines
}

#[test]
fn a_config_module_discovers_what_the_equivalent_toml_discovers() {
  let module = "
import { defineConfig } from '@ferridriver/test';
export default defineConfig({
  testMatch: ['specs/alpha.spec.ts'],
  projects: [{ name: 'one' }, { name: 'two' }],
});
";
  let from_module = scratch("module", "", Some(module));
  let from_toml = scratch(
    "toml",
    "testMatch = [\"specs/alpha.spec.ts\"]\n\
     [[test.projects]]\n\
     name = \"one\"\n\
     [[test.projects]]\n\
     name = \"two\"\n",
    None,
  );

  let module_run = list(&from_module, &["--config", "playwright.config.ts"]);
  let toml_run = list(&from_toml, &[]);
  let module_text = combined(&module_run);
  let toml_text = combined(&toml_run);

  assert!(module_run.status.success(), "{module_text}");
  assert!(toml_run.status.success(), "{toml_text}");
  assert_eq!(
    corpus(&module_text),
    corpus(&toml_text),
    "module:\n{module_text}\n\ntoml:\n{toml_text}"
  );
  assert!(
    module_text.contains("alpha.spec.ts") && !module_text.contains("beta.spec.ts"),
    "the module's own testMatch took effect: {module_text}"
  );
  // `--list` prints the corpus, not the project names, so the second
  // half of the claim is read where the loader reports it: `ferridriver
  // config` names the layer a value came from.
  let resolved = Command::new(bin())
    .current_dir(&from_module)
    .args(["config", "--config", "playwright.config.ts"])
    .output()
    .expect("run ferridriver config");
  let resolved_text = combined(&resolved);
  assert!(resolved.status.success(), "{resolved_text}");
  assert!(
    resolved_text.contains("explicit  playwright.config.ts"),
    "the module is a layer: {resolved_text}"
  );
  assert!(
    resolved_text.contains(r#"test.projects = [{"name":"one"},{"name":"two"}]"#),
    "the module's projects resolved: {resolved_text}"
  );
  assert!(
    resolved_text.contains("<- playwright.config.ts"),
    "and it is named as their source: {resolved_text}"
  );
}

#[test]
fn the_module_layers_above_the_discovered_files() {
  // The toml names one spec, the module the other: whichever the run
  // lists says which layer won, and the module is the explicitly named
  // one.
  let dir = scratch(
    "precedence",
    "testMatch = [\"specs/beta.spec.ts\"]\n",
    Some(
      "
import { defineConfig } from '@ferridriver/test';
export default defineConfig({ testMatch: ['specs/alpha.spec.ts'] });
",
    ),
  );
  let output = list(&dir, &["--config", "playwright.config.ts"]);
  let text = combined(&output);
  assert!(output.status.success(), "{text}");
  assert!(text.contains("alpha.spec.ts"), "{text}");
  assert!(!text.contains("beta.spec.ts"), "{text}");
}

#[test]
fn a_module_may_not_configure_the_loader_that_compiled_it() {
  let dir = scratch(
    "refused",
    "",
    Some(
      "
export default { moduleAliases: { '@acme/test': '@ferridriver/test' } };
",
    ),
  );
  let output = list(&dir, &["--config", "playwright.config.ts"]);
  let text = combined(&output);
  assert!(!output.status.success(), "a refusal must fail the run: {text}");
  assert!(text.contains("test.moduleAliases"), "the key is named: {text}");
}

#[test]
fn a_module_with_no_default_export_says_so() {
  let dir = scratch("no-default", "", Some("export const config = { workers: 1 };\n"));
  let output = list(&dir, &["--config", "playwright.config.ts"]);
  let text = combined(&output);
  assert!(!output.status.success(), "{text}");
  assert!(text.contains("no default export"), "{text}");
}

#[test]
fn a_module_that_throws_fails_the_run_with_its_own_error() {
  let dir = scratch(
    "throws",
    "",
    Some("throw new Error('the config could not decide');\nexport default {};\n"),
  );
  let output = list(&dir, &["--config", "playwright.config.ts"]);
  let text = combined(&output);
  assert!(!output.status.success(), "{text}");
  assert!(text.contains("the config could not decide"), "{text}");
}
