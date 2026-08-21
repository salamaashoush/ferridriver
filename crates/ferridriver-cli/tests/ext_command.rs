//! `ferridriver ext check` / `ext types`.
//!
//! The TypeScript pass is what turns a mistake inside a handler that never
//! ran — a misspelled binding, a wrong `args` field, a return that does not
//! match the declared shape — into an error at authoring time instead of at
//! the first live tool call. These tests pin the plumbing around the
//! compiler: which files land in the generated program, that diagnostics
//! reach the report and fail the command, and that an absent compiler is
//! reported rather than silently passing.
//!
//! The compiler itself is stubbed (`FERRIDRIVER_TSC`) so the suite does
//! not depend on a TypeScript install or a network fetch; what the real
//! compiler makes of the declarations is covered by the type-contract test
//! in `ferridriver-mcp`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn ok<T, E: std::fmt::Display>(result: Result<T, E>, what: &str) -> T {
  match result {
    Ok(value) => value,
    Err(e) => panic!("{what}: {e}"),
  }
}

fn bin() -> String {
  if let Ok(path) = std::env::var("FERRIDRIVER_BIN") {
    return path;
  }
  let base = format!("{}/../../target", env!("CARGO_MANIFEST_DIR"));
  let debug = format!("{base}/debug/ferridriver");
  if Path::new(&debug).exists() {
    debug
  } else {
    format!("{base}/release/ferridriver")
  }
}

fn write(path: &Path, contents: &str) {
  if let Some(parent) = path.parent() {
    ok(std::fs::create_dir_all(parent), "create parent dir");
  }
  ok(std::fs::write(path, contents), "write file");
}

/// A package with one manifest-declared entry that imports a helper.
fn fixture(root: &Path) -> PathBuf {
  let pkg = root.join("pkg");
  write(
    &pkg.join("package.json"),
    r#"{"name":"@probe/ext","type":"module","ferridriver":{"entries":["./src/tool.ts"]}}"#,
  );
  write(&pkg.join("src/lib/shared.ts"), "export const NAME = 'probe.tool';\n");
  write(
    &pkg.join("src/tool.ts"),
    "import { NAME } from './lib/shared';\n\
     defineTool({ name: NAME, description: 'p', exposeAsTool: true, handler: async () => ({ ok: true }) });\n",
  );
  pkg
}

/// A stub compiler: records its argv, copies the generated tsconfig out of
/// the scratch directory (which the command deletes on exit), prints
/// `stdout_text`, and exits `code`.
fn stub_checker(root: &Path, stdout_text: &str, code: i32) -> PathBuf {
  let path = root.join("stub-tsc");
  let out_file = root.join("stub-stdout.txt");
  write(&out_file, stdout_text);
  write(
    &path,
    &format!(
      "#!/bin/sh\n\
       printf '%s\\n' \"$@\" > {argv}\n\
       for a in \"$@\"; do case \"$a\" in *tsconfig.json) cp \"$a\" {config};; esac; done\n\
       cat {stdout_file}\n\
       exit {code}\n",
      argv = root.join("argv.txt").display(),
      config = root.join("tsconfig.copy.json").display(),
      stdout_file = out_file.display(),
    ),
  );
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o755);
    ok(std::fs::set_permissions(&path, perms), "chmod stub");
  }
  path
}

struct Run {
  stdout: String,
  success: bool,
}

fn run_check(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Run {
  let mut cmd = Command::new(bin());
  cmd.arg("--no-inherit").arg("ext").args(args).current_dir(cwd);
  for (k, v) in envs {
    cmd.env(k, v);
  }
  let out = ok(cmd.output(), "spawn ferridriver ext");
  Run {
    stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
    success: out.status.success(),
  }
}

#[test]
fn check_reports_the_declared_entries_and_registered_tools() {
  let dir = ok(tempfile::tempdir(), "tempdir");
  let pkg = fixture(dir.path());
  let stub = stub_checker(dir.path(), "", 0);

  let run = run_check(
    dir.path(),
    &["check", "./pkg"],
    &[("FERRIDRIVER_TSC", &stub.display().to_string())],
  );

  assert!(run.success, "a clean package must pass: {}", run.stdout);
  assert!(run.stdout.contains("1 declared entry/entries"), "{}", run.stdout);
  // The tool, and the fact that it is promoted to an MCP tool rather than
  // staying a script-only binding. Reported as two columns of one row now,
  // so the row is matched rather than the old `name [mcp tool]` string.
  let row = run
    .stdout
    .lines()
    .find(|l| l.contains("probe.tool"))
    .unwrap_or_else(|| panic!("the tool is reported: {}", run.stdout));
  assert!(row.contains("mcp tool"), "and reported as promoted: {row}");
  assert!(
    !run.stdout.contains("shared.ts"),
    "the helper is bundled through the entry, not listed as one: {}",
    run.stdout
  );

  // The generated program must carry the entry AND the embedded
  // declaration — without the latter, a bare `defineTool(...)` would not
  // even resolve and every extension would "fail" type checking.
  let argv = ok(std::fs::read_to_string(dir.path().join("argv.txt")), "read stub argv");
  assert!(argv.contains("--noEmit"), "{argv}");
  assert!(
    argv.lines().any(|l| l.ends_with("tsconfig.json")),
    "the checker was not given a tsconfig: {argv}"
  );
  let config = ok(
    std::fs::read_to_string(dir.path().join("tsconfig.copy.json")),
    "read generated tsconfig",
  );
  assert!(config.contains("src/tool.ts"), "entry must be in the program: {config}");
  assert!(
    config.contains("@ferridriver/extension/index.d.ts"),
    "the embedded declaration must be in the program: {config}"
  );
  assert!(
    config.contains(&pkg.join("tsconfig.json").display().to_string()) || !config.contains("extends"),
    "an author tsconfig is inherited when present: {config}"
  );
}

#[test]
fn a_type_error_fails_the_check_and_is_reported_verbatim() {
  let dir = ok(tempfile::tempdir(), "tempdir");
  fixture(dir.path());
  let stub = stub_checker(
    dir.path(),
    "src/tool.ts(2,10): error TS2339: Property 'put' does not exist on type 'Vars'.",
    1,
  );

  let run = run_check(
    dir.path(),
    &["check", "./pkg"],
    &[("FERRIDRIVER_TSC", &stub.display().to_string())],
  );

  assert!(!run.success, "a type error must fail the command: {}", run.stdout);
  assert!(run.stdout.contains("error TS2339"), "{}", run.stdout);
  assert!(run.stdout.contains("failed"), "{}", run.stdout);
  // The extension still loaded — the failure is the type pass, and the
  // report has to say which.
  assert!(run.stdout.contains("probe.tool"), "{}", run.stdout);
}

#[test]
fn no_typecheck_skips_the_pass_and_says_so() {
  let dir = ok(tempfile::tempdir(), "tempdir");
  fixture(dir.path());
  let stub = stub_checker(dir.path(), "error: should not run", 1);

  let run = run_check(
    dir.path(),
    &["check", "./pkg", "--no-typecheck"],
    &[("FERRIDRIVER_TSC", &stub.display().to_string())],
  );

  assert!(run.success, "{}", run.stdout);
  assert!(run.stdout.contains("skipped: --no-typecheck"), "{}", run.stdout);
  assert!(
    !dir.path().join("argv.txt").exists(),
    "the checker must not be invoked at all"
  );
}

#[test]
fn an_absent_compiler_is_reported_not_silently_passed() {
  let dir = ok(tempfile::tempdir(), "tempdir");
  fixture(dir.path());
  let empty_path = dir.path().join("empty-bin");
  ok(std::fs::create_dir_all(&empty_path), "mkdir empty PATH dir");

  // No compiler on PATH and no opt-in to fetching one: the pass cannot
  // run, and must say so instead of reading as verified.
  let run = run_check(
    dir.path(),
    &["check", "./pkg", "--json"],
    &[("PATH", &empty_path.display().to_string())],
  );

  let payload: serde_json::Value = ok(serde_json::from_str(&run.stdout), "parse --json report");
  let skipped = payload["typecheck"]["skipped"].as_str().unwrap_or_default();
  assert!(
    skipped.contains("no TypeScript compiler available"),
    "the report must name the reason: {}",
    run.stdout
  );
  assert!(skipped.contains("typescript"), "and how to get one: {}", run.stdout);
  // A missing compiler is not an extension defect, so the check still
  // passes — it just cannot claim the types were verified.
  assert_eq!(payload["typecheck"]["passed"], true, "{}", run.stdout);
  assert_eq!(payload["ok"], true, "{}", run.stdout);
  assert!(
    skipped.contains("FERRIDRIVER_TS_DOWNLOAD"),
    "and that fetching one is opt-in: {}",
    run.stdout
  );
}

/// A pre-commit / CI gate must not pull and execute a package from the
/// registry on its own; the runner is only reached once the operator
/// opts in.
#[test]
fn a_package_runner_is_not_used_unless_the_operator_opts_in() {
  let dir = ok(tempfile::tempdir(), "tempdir");
  fixture(dir.path());
  let fake_bin = dir.path().join("fake-bin");
  ok(std::fs::create_dir_all(&fake_bin), "mkdir fake bin dir");
  // An `npx` that records being called. Finding it must not be enough.
  let npx = fake_bin.join("npx");
  write(
    &npx,
    &format!(
      "#!/bin/sh\necho called >> {}\nexit 0\n",
      dir.path().join("npx-called.txt").display()
    ),
  );
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    ok(
      std::fs::set_permissions(&npx, std::fs::Permissions::from_mode(0o755)),
      "chmod npx",
    );
  }

  let path = fake_bin.display().to_string();
  let run = run_check(dir.path(), &["check", "./pkg", "--json"], &[("PATH", &path)]);
  assert!(
    !dir.path().join("npx-called.txt").exists(),
    "npx must not be invoked without FERRIDRIVER_TS_DOWNLOAD: {}",
    run.stdout
  );
}

#[test]
fn unmet_requirements_block_the_package_and_fail_the_check() {
  let dir = ok(tempfile::tempdir(), "tempdir");
  let pkg = fixture(dir.path());
  write(
    &pkg.join("package.json"),
    r#"{"name":"@probe/ext","type":"module","ferridriver":{
         "entries":["./src/tool.ts"],
         "requires":{"commands":["definitely-not-a-real-binary-xyz"]}}}"#,
  );
  let stub = stub_checker(dir.path(), "", 0);

  let run = run_check(
    dir.path(),
    &["check", "./pkg"],
    &[("FERRIDRIVER_TSC", &stub.display().to_string())],
  );

  assert!(!run.success, "{}", run.stdout);
  assert!(run.stdout.contains("unmet:"), "{}", run.stdout);
  assert!(
    run.stdout.contains("definitely-not-a-real-binary-xyz"),
    "{}",
    run.stdout
  );
  assert!(run.stdout.contains("skipped:"), "{}", run.stdout);
}

#[test]
fn types_writes_resolvable_declaration_packages() {
  let dir = ok(tempfile::tempdir(), "tempdir");
  let out = dir.path().join("node_modules");

  let run = run_check(dir.path(), &["types", "--out", &out.display().to_string()], &[]);
  assert!(run.success, "{}", run.stdout);

  for name in ["@ferridriver/extension", "@ferridriver/test"] {
    let dts = out.join(name).join("index.d.ts");
    let pkg_json = out.join(name).join("package.json");
    assert!(dts.is_file(), "{name} declaration missing at {}", dts.display());
    assert!(pkg_json.is_file(), "{name} package.json missing");
    let manifest = ok(std::fs::read_to_string(&pkg_json), "read package.json");
    assert!(manifest.contains(name), "{manifest}");
    assert!(manifest.contains("index.d.ts"), "{manifest}");
  }

  let declaration = ok(
    std::fs::read_to_string(out.join("@ferridriver/extension/index.d.ts")),
    "read extension declaration",
  );
  assert!(declaration.contains("function defineTool"), "{declaration}");
  assert!(declaration.contains("interface ToolContext"), "{declaration}");
}
