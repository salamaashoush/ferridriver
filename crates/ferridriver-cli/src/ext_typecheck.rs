//! TypeScript pass for `ferridriver ext check` / `ext dev`.
//!
//! Loading an extension proves it bundles and registers; it says nothing
//! about the calls inside a handler that never ran. A misspelled binding
//! (`ctx.vars.put(...)`), a wrong option field, an `args` shape that does
//! not match the declared `inputSchema` — all of it surfaces only when the
//! tool is finally invoked, usually against a live browser.
//!
//! So the check also type-checks the entry files with `tsc`, against the
//! declarations embedded in this binary. The author needs no
//! `node_modules`: a generated `tsconfig.json` in a scratch directory
//! points at the embedded `@ferridriver/extension` / `@ferridriver/test`
//! copies and extends the package's own tsconfig when it has one, so the
//! author's compiler options still apply.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::ext_types;

/// What the TypeScript pass produced.
pub struct TypecheckOutcome {
  /// The checker that ran, e.g. `tsc` — `None` when none was found.
  pub checker: Option<String>,
  /// Diagnostic lines, verbatim from the compiler.
  pub diagnostics: Vec<String>,
  /// `true` when a checker ran and reported no diagnostics.
  pub passed: bool,
  /// Why the pass did not run (no checker installed, no TS entry files).
  pub skipped: Option<String>,
}

impl TypecheckOutcome {
  fn skipped(reason: impl Into<String>) -> Self {
    Self {
      checker: None,
      diagnostics: Vec::new(),
      passed: true,
      skipped: Some(reason.into()),
    }
  }
}

/// The npm package that ships the compiler.
const TS_PACKAGE: &str = "typescript";

/// The bin `TS_PACKAGE` exposes.
const TS_PACKAGE_BIN: &str = "tsc";

/// Where a package runner fetches `TS_PACKAGE` from.
///
/// Pinned rather than inherited: a corporate registry commonly proxies a
/// curated subset and answers 403 for the rest, which turns the type pass
/// into a failing gate on a machine whose npm is configured normally.
const PUBLIC_REGISTRY: &str = "https://registry.npmjs.org/";

/// Wall-clock ceiling for the compiler run.
///
/// A package runner may block on a registry that never answers, and
/// `ext dev` reruns this on every save — an unbounded `output()` turns
/// one bad network moment into a watch loop that never reports again.
const CHECK_TIMEOUT: Duration = Duration::from_mins(3);

/// Whether an opt-in environment flag is set to something truthy.
fn env_flag(name: &str) -> bool {
  std::env::var(name).is_ok_and(|v| {
    let v = v.trim();
    !(v.is_empty() || v == "0" || v.eq_ignore_ascii_case("false"))
  })
}

/// A resolved TypeScript checker: the program plus the args that precede
/// the compiler's own (a package runner needs the package + bin first).
struct Checker {
  label: String,
  program: PathBuf,
  leading: Vec<String>,
  /// Environment the runner needs, e.g. the registry to fetch from.
  env: Vec<(String, String)>,
  /// True when running it may fetch the compiler over the network.
  fetches: bool,
}

/// Find a checker, cheapest first:
///
/// 1. `FERRIDRIVER_TSC` — an explicit binary.
/// 2. `tsc` on `PATH`.
/// 3. `tsc` in a `node_modules/.bin` above any search root — an
///    already-installed compiler beats a download, even an older one.
/// 4. `npx`/`bunx`, which FETCHES AND EXECUTES `tsc` from the registry.
///    Opt-in (`FERRIDRIVER_TS_DOWNLOAD=1`): `ext check` is documented as a
///    pre-commit / CI gate, and a gate that silently pulls and runs a
///    package from the network is a supply-chain decision the operator
///    has to make, not a convenience the tool takes on their behalf.
///    Without it the pass is skipped with a message naming the flag.
fn find_checker(search_roots: &[PathBuf]) -> Option<Checker> {
  let direct = |label: String, program: PathBuf| Checker {
    label,
    program,
    leading: Vec::new(),
    env: Vec::new(),
    fetches: false,
  };

  if let Some(explicit) = std::env::var_os("FERRIDRIVER_TSC") {
    let path = PathBuf::from(explicit);
    if path.is_file() {
      return Some(direct(format!("{TS_PACKAGE_BIN} (FERRIDRIVER_TSC)"), path));
    }
  }

  if let Ok(path) = which::which(TS_PACKAGE_BIN) {
    return Some(direct(TS_PACKAGE_BIN.to_string(), path));
  }
  for root in search_roots {
    let mut dir = Some(root.as_path());
    while let Some(current) = dir {
      let candidate = current.join("node_modules/.bin").join(TS_PACKAGE_BIN);
      if candidate.is_file() {
        return Some(direct(format!("{TS_PACKAGE_BIN} ({})", candidate.display()), candidate));
      }
      dir = current.parent();
    }
  }

  if !env_flag("FERRIDRIVER_TS_DOWNLOAD") {
    return None;
  }
  // `npx --package <pkg> tsc` names the bin explicitly rather than
  // relying on "the package has exactly one bin".
  if let Ok(npx) = which::which("npx") {
    return Some(Checker {
      label: format!("{TS_PACKAGE_BIN} (npx {TS_PACKAGE})"),
      program: npx,
      leading: vec![
        "--yes".to_string(),
        "--registry".to_string(),
        PUBLIC_REGISTRY.to_string(),
        "--package".to_string(),
        TS_PACKAGE.to_string(),
        TS_PACKAGE_BIN.to_string(),
      ],
      env: Vec::new(),
      fetches: true,
    });
  }
  if let Ok(bunx) = which::which("bunx") {
    return Some(Checker {
      label: format!("{TS_PACKAGE_BIN} (bunx {TS_PACKAGE})"),
      program: bunx,
      leading: vec![
        "--package".to_string(),
        TS_PACKAGE.to_string(),
        TS_PACKAGE_BIN.to_string(),
      ],
      // bun takes the registry from config, not a flag.
      env: vec![("BUN_CONFIG_REGISTRY".to_string(), PUBLIC_REGISTRY.to_string())],
      fetches: true,
    });
  }
  None
}

/// Type-check `entries`. `package_dirs` are the packages the entries came
/// from, used to find a checker and to inherit an author `tsconfig.json`.
pub fn run(entries: &[PathBuf], package_dirs: &[PathBuf], scratch: &Path) -> TypecheckOutcome {
  let ts_entries: Vec<&PathBuf> = entries
    .iter()
    .filter(|p| {
      matches!(
        p.extension().and_then(|e| e.to_str()),
        Some("ts" | "tsx" | "mts" | "cts")
      )
    })
    .collect();
  if ts_entries.is_empty() {
    return TypecheckOutcome::skipped("no TypeScript entry files");
  }

  let mut roots: Vec<PathBuf> = package_dirs.to_vec();
  for e in &ts_entries {
    if let Some(parent) = e.parent() {
      roots.push(parent.to_path_buf());
    }
  }
  if let Ok(cwd) = std::env::current_dir() {
    roots.push(cwd);
  }

  let Some(checker) = find_checker(&roots) else {
    return TypecheckOutcome::skipped(format!(
      "no TypeScript compiler available: none of FERRIDRIVER_TSC, `tsc` on PATH, or in a \
       node_modules/.bin above the extension. Install one (`npm i -D {TS_PACKAGE}`), or set \
       FERRIDRIVER_TS_DOWNLOAD=1 to let `npx`/`bunx` fetch and run it from {PUBLIC_REGISTRY}"
    ));
  };
  if checker.fetches {
    // The first run downloads the compiler; without a line here it just
    // looks like the check hung.
    eprintln!("[ext] fetching {TS_PACKAGE} from {PUBLIC_REGISTRY} to type-check (first run only; cached afterwards)");
  }

  let types_root = scratch.join("types");
  if let Err(e) = ext_types::materialize(&types_root) {
    return TypecheckOutcome {
      checker: Some(checker.label),
      diagnostics: vec![format!("could not write the embedded type declarations: {e}")],
      passed: false,
      skipped: None,
    };
  }

  let config_path = scratch.join("tsconfig.json");
  if let Err(e) = std::fs::write(&config_path, tsconfig(&ts_entries, package_dirs, &types_root)) {
    return TypecheckOutcome {
      checker: Some(checker.label),
      diagnostics: vec![format!("could not write {}: {e}", config_path.display())],
      passed: false,
      skipped: None,
    };
  }

  let mut cmd = Command::new(&checker.program);
  cmd.args(&checker.leading);
  cmd.envs(checker.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
  cmd.arg("--noEmit").arg("-p").arg(&config_path);
  // Diagnostics are printed relative to the compiler's cwd; from the
  // scratch directory every path would come out as `../../private/...`.
  if let Some(dir) = package_dirs.first() {
    cmd.current_dir(dir);
  } else if let Ok(cwd) = std::env::current_dir() {
    cmd.current_dir(cwd);
  }
  let output = match run_bounded(cmd, CHECK_TIMEOUT) {
    Ok(o) => o,
    Err(e) => {
      return TypecheckOutcome {
        checker: Some(checker.label),
        diagnostics: vec![format!("could not run {}: {e}", checker.program.display())],
        passed: false,
        skipped: None,
      };
    },
  };

  let mut diagnostics: Vec<String> = String::from_utf8_lossy(&output.stdout)
    .lines()
    .chain(String::from_utf8_lossy(&output.stderr).lines())
    .map(str::trim_end)
    .filter(|l| !l.is_empty())
    .map(str::to_string)
    .collect();
  // A non-zero exit with nothing on either stream would otherwise read as
  // a pass.
  if !output.status.success() && diagnostics.is_empty() {
    diagnostics.push(format!("{} exited with {}", checker.label, output.status));
  }

  TypecheckOutcome {
    checker: Some(checker.label),
    passed: output.status.success() && diagnostics.is_empty(),
    diagnostics,
    skipped: None,
  }
}

/// Run the compiler to completion under `timeout`, killing it if it
/// overruns.
///
/// Both pipes are drained on their own threads: a compiler emitting more
/// than a pipe buffer of diagnostics (a package with hundreds of errors)
/// otherwise blocks in `write(2)` and would be reported as a timeout.
fn run_bounded(mut cmd: Command, timeout: Duration) -> std::io::Result<std::process::Output> {
  use std::io::Read as _;
  use std::process::Stdio;

  let mut child = cmd
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()?;
  let drain = |mut pipe: Option<std::process::ChildStdout>, mut err: Option<std::process::ChildStderr>| {
    std::thread::spawn(move || {
      let mut buf = Vec::new();
      if let Some(p) = pipe.as_mut() {
        let _ = p.read_to_end(&mut buf);
      }
      if let Some(p) = err.as_mut() {
        let _ = p.read_to_end(&mut buf);
      }
      buf
    })
  };
  let stdout = drain(child.stdout.take(), None);
  let stderr = drain(None, child.stderr.take());

  let started = std::time::Instant::now();
  let status = loop {
    if let Some(status) = child.try_wait()? {
      break status;
    }
    if started.elapsed() >= timeout {
      let _ = child.kill();
      let _ = child.wait();
      return Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("type check timed out after {}s", timeout.as_secs()),
      ));
    }
    std::thread::sleep(Duration::from_millis(25));
  };
  Ok(std::process::Output {
    status,
    stdout: stdout.join().unwrap_or_default(),
    stderr: stderr.join().unwrap_or_default(),
  })
}

/// The generated `tsconfig.json`.
///
/// `files` lists the entry files plus the embedded `@ferridriver/extension`
/// declaration: its `declare global` block is what makes a bare
/// `defineTool(...)` call type-check, and an ambient block only applies to
/// files in the program.
fn tsconfig(entries: &[&PathBuf], package_dirs: &[PathBuf], types_root: &Path) -> String {
  let quote = |p: &Path| serde_json::Value::String(p.display().to_string());

  let mut files: Vec<serde_json::Value> = vec![quote(&types_root.join("@ferridriver/extension/index.d.ts"))];
  files.extend(entries.iter().map(|p| quote(p.as_path())));

  let mut config = serde_json::json!({
    "compilerOptions": {
      "noEmit": true,
      "strict": true,
      "target": "ES2022",
      "module": "ESNext",
      "moduleResolution": "bundler",
      "lib": ["ES2022", "DOM", "DOM.Iterable"],
      // The extension runs in QuickJS with the host's own globals, so
      // @types packages an author happens to have installed (node, bun)
      // would describe an environment that is not there.
      "types": [],
      "typeRoots": [],
      "skipLibCheck": true,
      "verbatimModuleSyntax": true,
      "isolatedModules": true,
      "forceConsistentCasingInFileNames": true,
      // No `baseUrl`: TypeScript 7 removed it, and the mappings below are
      // absolute paths, so it was never needed.
      "paths": {
        "@ferridriver/extension": [types_root.join("@ferridriver/extension/index.d.ts").display().to_string()],
        "@ferridriver/test": [types_root.join("@ferridriver/test/index.d.ts").display().to_string()],
      },
    },
    "files": files,
    // Only `files`: a package tsconfig we extend may `include` a whole tree
    // (its own ambient types, its tests), and `files` alone does not stop
    // an inherited `include` from pulling those into the program.
    "include": [],
  });

  // Inherit the package's own options when it has a tsconfig, so an
  // author's `jsx`, `target` or stricter flags still apply. Ours are
  // applied on top and REPLACE the inherited value key by key (that is
  // how `extends` merges `compilerOptions`), so an author's own `paths`
  // does not survive — the two `@ferridriver/*` mappings below have to
  // win or nothing resolves without an install.
  if let Some(existing) = package_dirs
    .iter()
    .map(|d| d.join("tsconfig.json"))
    .find(|p| p.is_file())
  {
    config["extends"] = serde_json::Value::String(existing.display().to_string());
  }

  serde_json::to_string_pretty(&config).unwrap_or_else(|_| String::from("{}"))
}
