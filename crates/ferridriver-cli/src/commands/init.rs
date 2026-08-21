//! `ferridriver init` — make a directory into a ferridriver project.
//!
//! There was no path from "the binary is installed" to "a suite runs": the
//! config schema, the spec shape and the type declarations were each
//! documented somewhere else, and nothing put them on disk. This writes the
//! smallest set of files that makes `ferridriver test` work, refuses to
//! clobber anything without `--force`, and ends by saying what to run.

use std::path::{Path, PathBuf};

use crate::cli;
use crate::commands::ext::types;
use crate::ui;

/// What happened to one scaffolded file.
enum Wrote {
  Created,
  Skipped,
  Replaced,
}

impl Wrote {
  fn label(&self) -> String {
    match self {
      Self::Created => ui::success("created"),
      Self::Replaced => ui::success("replaced"),
      Self::Skipped => ui::dim("exists"),
    }
  }
}

pub fn run(args: &cli::InitArgs) -> anyhow::Result<()> {
  let root = args.dir.clone().unwrap_or_else(|| PathBuf::from("."));
  std::fs::create_dir_all(&root).map_err(|e| anyhow::anyhow!("create {}: {e}", root.display()))?;

  let mut done: Vec<(PathBuf, Wrote)> = Vec::new();
  let config_name = format!("ferridriver.{}", args.config_format);
  done.push(write(
    &root.join(&config_name),
    &config_document(&args.config_format, args.bdd),
    args.force,
  )?);
  done.push(write(&root.join("tests/example.spec.ts"), EXAMPLE_SPEC, args.force)?);
  done.push(write(&root.join("tests/tsconfig.json"), TSCONFIG, args.force)?);
  if args.bdd {
    done.push(write(
      &root.join("tests/features/example.feature"),
      EXAMPLE_FEATURE,
      args.force,
    )?);
    done.push(write(&root.join("tests/steps/example.ts"), EXAMPLE_STEPS, args.force)?);
  }

  // The declarations an editor resolves `@ferridriver/test` through. Written
  // into `node_modules` because that is where TypeScript already looks, so no
  // `paths` mapping and no npm install are involved.
  let types_root = root.join("node_modules");
  let written = types::materialize(&types_root).map_err(|e| anyhow::anyhow!("write type declarations: {e}"))?;
  for (_, path) in &written {
    done.push((path.clone(), Wrote::Created));
  }

  if ui::json() {
    let payload: Vec<serde_json::Value> = done
      .iter()
      .map(|(path, what)| {
        serde_json::json!({
          "path": path.display().to_string(),
          "status": match what { Wrote::Created => "created", Wrote::Replaced => "replaced", Wrote::Skipped => "skipped" },
        })
      })
      .collect();
    return ui::print_json(&payload);
  }

  ui::section("Scaffolded");
  let mut table = ui::Table::new(&["", "FILE"]).flex(1);
  for (path, what) in &done {
    table.row([what.label(), ui::path(&ui::short_path(path, 80))]);
  }
  table.print(ui::width());

  let skipped = done.iter().filter(|(_, w)| matches!(w, Wrote::Skipped)).count();
  if skipped > 0 {
    ui::say(&format!(
      "\n{}",
      ui::warning(&format!(
        "{skipped} file(s) already existed and were left alone — pass --force to replace them"
      ))
    ));
  }

  ui::next_steps(&[
    ("install a browser", "ferridriver install chromium".to_string()),
    ("run the example", "ferridriver test".to_string()),
    if args.bdd {
      ("run the feature", "ferridriver bdd".to_string())
    } else {
      ("check the setup", "ferridriver doctor".to_string())
    },
  ]);
  Ok(())
}

/// Write one file, creating its parent, and report which of the three things
/// happened. Existing content is never destroyed without `--force`.
fn write(path: &Path, contents: &str, force: bool) -> anyhow::Result<(PathBuf, Wrote)> {
  let existed = path.exists();
  if existed && !force {
    return Ok((path.to_path_buf(), Wrote::Skipped));
  }
  // `--force` is the intent to replace, but overwriting someone's config is
  // not undoable — so where there is a person at the terminal, they get to
  // say no to each file. With no terminal `--force` stands on its own.
  if existed && !ui::prompt::confirm(&format!("replace {}?", ui::path(&ui::short_path(path, 60))), false)? {
    return Ok((path.to_path_buf(), Wrote::Skipped));
  }
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent).map_err(|e| anyhow::anyhow!("create {}: {e}", parent.display()))?;
  }
  std::fs::write(path, contents).map_err(|e| anyhow::anyhow!("write {}: {e}", path.display()))?;
  Ok((
    path.to_path_buf(),
    if existed { Wrote::Replaced } else { Wrote::Created },
  ))
}

/// The starting config, in whichever of the three supported syntaxes was
/// asked for. Keys are camelCase on the wire in all of them.
fn config_document(format: &str, bdd: bool) -> String {
  let steps = if bdd { r#""tests/steps/**/*.ts""# } else { "" };
  match format {
    "yaml" => format!(
      "test:\n  \
       testMatch:\n    - 'tests/**/*.spec.ts'\n    - 'tests/**/*.test.ts'\n  \
       {}\
       browser:\n    headless: true\n\n\
       mcp:\n  \
       browser:\n    backend: cdp-pipe\n",
      if bdd {
        "features:\n    - 'tests/features/**/*.feature'\n  steps:\n    - 'tests/steps/**/*.ts'\n  "
      } else {
        ""
      }
    ),
    "json" => format!(
      "{{\n  \"test\": {{\n    \"testMatch\": [\"tests/**/*.spec.ts\", \"tests/**/*.test.ts\"],\n{}    \
       \"browser\": {{ \"headless\": true }}\n  }},\n  \
       \"mcp\": {{ \"browser\": {{ \"backend\": \"cdp-pipe\" }} }}\n}}\n",
      if bdd {
        "    \"features\": [\"tests/features/**/*.feature\"],\n    \"steps\": [\"tests/steps/**/*.ts\"],\n"
      } else {
        ""
      }
    ),
    _ => format!(
      "# What `ferridriver test` and `ferridriver bdd` run.\n\
       # Every key is camelCase; `ferridriver config` shows what a run resolves to.\n\
       [test]\n\
       testMatch = [\"tests/**/*.spec.ts\", \"tests/**/*.test.ts\"]\n{}\n\
       [test.browser]\n\
       headless = true\n\n\
       # What `ferridriver mcp` serves to a coding agent.\n\
       [mcp.browser]\n\
       backend = \"cdp-pipe\"\n",
      if bdd {
        format!("features = [\"tests/features/**/*.feature\"]\nsteps = [{steps}]\n")
      } else {
        String::new()
      }
    ),
  }
}

const EXAMPLE_SPEC: &str = r"import { test, expect } from '@ferridriver/test'

test('the page has a title', async ({ page }) => {
  await page.goto('https://example.com')
  await expect(page).toHaveTitle(/Example/)
})

test('the heading is there', async ({ page }) => {
  await page.goto('https://example.com')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Example Domain')
})
";

const TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "strict": true,
    "noEmit": true,
    "types": [],
    "lib": ["ES2022", "DOM"]
  },
  "include": ["**/*.ts"]
}
"#;

const EXAMPLE_FEATURE: &str = r#"Feature: Example

  Scenario: the page has a title
    Given I open "https://example.com"
    Then the title contains "Example"
"#;

const EXAMPLE_STEPS: &str = r"import { Given, Then, expect } from '@ferridriver/test'

Given('I open {string}', async function (url: string) {
  await this.page.goto(url)
})

Then('the title contains {string}', async function (fragment: string) {
  expect(await this.page.title()).toContain(fragment)
})
";
