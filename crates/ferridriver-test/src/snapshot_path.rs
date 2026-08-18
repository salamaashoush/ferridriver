//! Where a snapshot lives on disk.
//!
//! A faithful port of Playwright's `TestInfo._resolveSnapshotPaths` and
//! `TestInfo._applyPathTemplate` (`playwright/src/worker/testInfo.ts:560-642`),
//! because the path a matcher writes and the path `testInfo.snapshotPath()`
//! reports have to be the same string — that agreement is the whole
//! point of the template, and a second implementation of it would drift.
//!
//! Two rules are easy to get wrong and are the reason this is not a
//! `str::replace` chain:
//!
//! - A token may carry a SEPARATOR inside its braces
//!   (`{-projectName}`), which is emitted only when the value is
//!   non-empty. `{arg}{-projectName}{ext}` is how the legacy layout
//!   keeps `button.png` and `button-firefox.png` apart, and a plain
//!   replace produces a literal `{-projectName}` in the filename.
//! - The name argument is sanitized BEFORE its extension, indexed per
//!   test (so two `toMatchSnapshot('a.png')` calls in one test do not
//!   collide), and trimmed with a hash in the middle when it is long.

use std::path::{Path, PathBuf};

use rustc_hash::FxHashMap;

/// Playwright's `trimLongString` default budget.
const DEFAULT_TRIM_LENGTH: usize = 100;

/// Playwright's `windowsFilesystemFriendlyLength` — the output copy of a
/// name is trimmed harder than the baseline, because it is joined onto
/// the (much longer) output directory.
const WINDOWS_FRIENDLY_LENGTH: usize = 60;

/// The legacy layout, unchanged since Playwright 1.x: a `-snapshots`
/// directory beside the spec.
const LEGACY_TEMPLATE: &str =
  "{snapshotDir}/{testFileDir}/{testFileName}-snapshots/{arg}{-projectName}{-snapshotSuffix}{ext}";

/// Aria snapshots default to the same layout without the project /
/// suffix qualifiers.
const ARIA_TEMPLATE: &str = "{snapshotDir}/{testFileDir}/{testFileName}-snapshots/{arg}{ext}";

/// Which matcher is asking. Decides the default extension and which
/// `pathTemplate` applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotKind {
  /// `toMatchSnapshot` — text.
  Snapshot,
  /// `toHaveScreenshot` — PNG.
  Screenshot,
  /// `toMatchAriaSnapshot` — YAML.
  Aria,
}

impl SnapshotKind {
  /// The extension an unnamed snapshot of this kind gets.
  #[must_use]
  pub fn default_extension(self) -> &'static str {
    match self {
      Self::Snapshot => ".txt",
      Self::Screenshot => ".png",
      Self::Aria => ".aria.yml",
    }
  }

  /// Playwright's `testInfo.snapshotPath(name, { kind })` values.
  #[must_use]
  pub fn parse(kind: &str) -> Option<Self> {
    match kind {
      "snapshot" => Some(Self::Snapshot),
      "screenshot" => Some(Self::Screenshot),
      "aria" => Some(Self::Aria),
      _ => None,
    }
  }
}

/// The name a matcher was called with.
#[derive(Debug, Clone)]
pub enum SnapshotName {
  /// `toMatchSnapshot()` — the name comes from the test's title path
  /// plus a running index.
  Anonymous,
  /// `toMatchSnapshot('a.png')` — sanitized before its extension.
  One(String),
  /// `toMatchSnapshot(['dir', 'a.png'])` — segments are NOT sanitized;
  /// Playwright treats an array as a filesystem path the author chose
  /// (microsoft/playwright#9156).
  Segments(Vec<String>),
}

impl SnapshotName {
  /// Build from the variadic form `snapshotPath(...names)`: no names is
  /// anonymous, one name is a name, several are path segments.
  ///
  /// An EMPTY single name is anonymous too — upstream tests the name for
  /// JS falsiness (`if (!name)`, `worker/testInfo.ts:568`), so
  /// `snapshotPath('')` is the unnamed form, not a nameless file.
  #[must_use]
  pub fn from_parts(parts: &[String]) -> Self {
    match parts {
      [] => Self::Anonymous,
      [one] if one.is_empty() => Self::Anonymous,
      [one] => Self::One(one.clone()),
      many => Self::Segments(many.to_vec()),
    }
  }
}

/// Per-test snapshot-name bookkeeping. Playwright keeps one of these for
/// ordinary snapshots and a second for aria snapshots, so their indices
/// never interfere.
#[derive(Debug, Default, Clone)]
pub struct SnapshotNames {
  last_anonymous_index: usize,
  last_named_index: FxHashMap<String, usize>,
}

/// Everything about the run that a template can name.
#[derive(Debug, Clone)]
pub struct SnapshotPathContext {
  /// Directory of the config file — every resolved path is relative to
  /// it, as upstream's `path.resolve(configDir, …)`.
  pub config_dir: PathBuf,
  /// The PROJECT's `testDir` (`{testDir}`), and what a test file's path
  /// is made relative to.
  pub test_dir: PathBuf,
  /// The project's `snapshotDir` (`{snapshotDir}`).
  pub snapshot_dir: PathBuf,
  /// `{projectName}`, sanitized for the filesystem.
  pub project_name: String,
  /// The spec file this test came from.
  pub test_file: PathBuf,
  /// `testInfo.titlePath` — the file, then each suite, then the test.
  pub title_path: Vec<String>,
  /// `testInfo.snapshotSuffix` (`{snapshotSuffix}`).
  pub snapshot_suffix: String,
  /// `expect.toHaveScreenshot.pathTemplate`.
  pub screenshot_template: Option<String>,
  /// `expect.toMatchAriaSnapshot.pathTemplate`.
  pub aria_template: Option<String>,
  /// `snapshotPathTemplate` on the config / project.
  pub config_template: Option<String>,
}

/// Both paths a matcher needs: where the baseline lives, and what to
/// call the `-actual` / `-diff` files it writes into the output
/// directory.
#[derive(Debug, Clone)]
pub struct ResolvedSnapshotPaths {
  pub absolute_snapshot_path: PathBuf,
  pub relative_output_path: PathBuf,
}

/// Playwright's `_resolveSnapshotPaths`.
///
/// `update_index` mirrors the `'updateSnapshotIndex'` argument: a
/// matcher consumes an index, `testInfo.snapshotPath()` only reads one.
/// `anonymous_extension` overrides the kind's default for an unnamed
/// snapshot (`toMatchSnapshot(buffer)` naming its own).
pub fn resolve_snapshot_paths(
  cx: &SnapshotPathContext,
  kind: SnapshotKind,
  name: &SnapshotName,
  names: &mut SnapshotNames,
  update_index: bool,
  anonymous_extension: Option<&str>,
) -> ResolvedSnapshotPaths {
  let (sub_path, ext, mut relative_output_path) = match name {
    SnapshotName::Anonymous => {
      // Two anonymous snapshots in one test must not overwrite each
      // other, so the index is part of the name.
      let index = names.last_anonymous_index + 1;
      if update_index {
        names.last_anonymous_index = index;
      }
      let mut parts: Vec<String> = cx.title_path.iter().skip(1).cloned().collect();
      parts.push(index.to_string());
      let full_title = parts.join(" ");
      let ext = anonymous_extension
        .unwrap_or_else(|| kind.default_extension())
        .to_string();
      let sub_path =
        sanitize_file_path_before_extension(&(trim_long_string(&full_title, DEFAULT_TRIM_LENGTH) + &ext), &ext);
      let output =
        sanitize_file_path_before_extension(&(trim_long_string(&full_title, WINDOWS_FRIENDLY_LENGTH) + &ext), &ext);
      (sub_path, ext, output)
    },
    SnapshotName::Segments(segments) => {
      let joined = segments.iter().fold(PathBuf::new(), |acc, s| acc.join(s));
      let joined = joined.to_string_lossy().into_owned();
      let ext = aria_aware_extname(kind, &joined);
      (joined.clone(), ext, joined)
    },
    SnapshotName::One(one) => {
      let ext = aria_aware_extname(kind, one);
      let sub_path = sanitize_file_path_before_extension(one, &ext);
      let output = sanitize_file_path_before_extension(&trim_long_string(one, WINDOWS_FRIENDLY_LENGTH), &ext);
      (sub_path, ext, output)
    },
  };

  if !matches!(name, SnapshotName::Anonymous) {
    // A repeated NAME within one test gets `-1`, `-2`, … on its output
    // copy, so the diff of the second call does not clobber the first.
    let index = names.last_named_index.get(&relative_output_path).copied().unwrap_or(0) + 1;
    if update_index {
      names.last_named_index.insert(relative_output_path.clone(), index);
    }
    if index > 1 {
      relative_output_path = add_suffix_to_file_path(&relative_output_path, &format!("-{}", index - 1));
    }
  }

  let template = match kind {
    SnapshotKind::Screenshot => cx
      .screenshot_template
      .clone()
      .or_else(|| cx.config_template.clone())
      .unwrap_or_else(|| LEGACY_TEMPLATE.to_string()),
    SnapshotKind::Aria => cx
      .aria_template
      .clone()
      .or_else(|| cx.config_template.clone())
      .unwrap_or_else(|| ARIA_TEMPLATE.to_string()),
    SnapshotKind::Snapshot => cx
      .config_template
      .clone()
      .unwrap_or_else(|| LEGACY_TEMPLATE.to_string()),
  };

  // `{arg}` is the name without its extension, directory included.
  let sub = Path::new(&sub_path);
  let stem = sub
    .file_name()
    .map(|f| {
      let f = f.to_string_lossy();
      f.strip_suffix(&ext).map_or_else(|| f.to_string(), ToString::to_string)
    })
    .unwrap_or_default();
  let name_argument = match sub.parent() {
    Some(dir) if !dir.as_os_str().is_empty() => dir.join(stem).to_string_lossy().into_owned(),
    _ => stem,
  };

  ResolvedSnapshotPaths {
    absolute_snapshot_path: apply_path_template(cx, &template, &name_argument, &ext),
    relative_output_path: PathBuf::from(relative_output_path),
  }
}

/// Node's `path.relative(base, path)`, which is what Playwright uses to
/// place a spec under `{testFileDir}`.
///
/// It never answers with an absolute path — the reason this is not a
/// bare `strip_prefix`. A failed strip used to fall back to the absolute
/// path, which the template then joined under `{snapshotDir}`, so every
/// baseline landed in a mirror of the whole filesystem. macOS reaches
/// that on its own: `/var/folders/...` is a symlink to
/// `/private/var/folders/...`, so a `testDir` and a spec path that name
/// the same directory through different sides of the link do not strip.
/// Hence the canonicalised second attempt before the `..`-walk.
fn relative_to(base: &Path, path: &Path) -> PathBuf {
  if let Ok(rel) = path.strip_prefix(base) {
    return rel.to_path_buf();
  }
  let (base, path) = match (base.canonicalize(), path.canonicalize()) {
    (Ok(b), Ok(p)) => {
      if let Ok(rel) = p.strip_prefix(&b) {
        return rel.to_path_buf();
      }
      (b, p)
    },
    _ => (base.to_path_buf(), path.to_path_buf()),
  };
  let mut b = base.components().peekable();
  let mut p = path.components().peekable();
  while b.peek().is_some() && b.peek() == p.peek() {
    b.next();
    p.next();
  }
  let mut out = PathBuf::new();
  for _ in b {
    out.push("..");
  }
  out.extend(p);
  out
}

/// Playwright's `_applyPathTemplate`.
#[must_use]
pub fn apply_path_template(cx: &SnapshotPathContext, template: &str, name_argument: &str, ext: &str) -> PathBuf {
  let relative_test_file = relative_to(&cx.test_dir, &cx.test_file);
  let file_dir = relative_test_file
    .parent()
    .map(|p| p.to_string_lossy().into_owned())
    .unwrap_or_default();
  let file_base = relative_test_file
    .file_stem()
    .map(|s| s.to_string_lossy().into_owned())
    .unwrap_or_default();
  let file_name = relative_test_file
    .file_name()
    .map(|s| s.to_string_lossy().into_owned())
    .unwrap_or_default();
  let project_segment = sanitize_for_file_path(&cx.project_name);

  let mut out = template.to_string();
  for (token, value, omit_when_empty) in [
    ("testDir", cx.test_dir.to_string_lossy().into_owned(), false),
    ("snapshotDir", cx.snapshot_dir.to_string_lossy().into_owned(), false),
    ("snapshotSuffix", cx.snapshot_suffix.clone(), true),
    ("testFileDir", file_dir, false),
    ("platform", node_platform().to_string(), false),
    ("projectName", project_segment, true),
    ("testName", fs_sanitized_test_name(&cx.title_path), false),
    ("testFileBaseName", file_base, false),
    ("testFileName", file_name, false),
    ("testFilePath", relative_test_file.to_string_lossy().into_owned(), false),
    ("arg", name_argument.to_string(), false),
    ("ext", ext.to_string(), true),
  ] {
    out = replace_token(&out, token, &value, omit_when_empty);
  }

  let resolved = if Path::new(&out).is_absolute() {
    PathBuf::from(out)
  } else {
    cx.config_dir.join(out)
  };
  ferridriver_config::layer::normalize_path(&resolved)
}

/// Playwright's `_fsSanitizedTestName`: the title path minus the file,
/// joined with spaces and sanitized.
fn fs_sanitized_test_name(title_path: &[String]) -> String {
  let joined = title_path.iter().skip(1).cloned().collect::<Vec<_>>().join(" ");
  sanitize_for_file_path(&trim_long_string(&joined, DEFAULT_TRIM_LENGTH))
}

/// `process.platform` spelling, which is what a checked-in snapshot
/// directory is named after. Rust says `macos` / `windows` where Node
/// says `darwin` / `win32`.
#[must_use]
pub fn node_platform() -> &'static str {
  match std::env::consts::OS {
    "macos" => "darwin",
    "windows" => "win32",
    other => other,
  }
}

/// Substitute one `{token}` / `{<sep>token}` occurrence everywhere.
///
/// The separator form is what makes `{-projectName}` disappear entirely
/// for the unnamed project instead of leaving a trailing dash.
fn replace_token(input: &str, token: &str, value: &str, omit_when_empty: bool) -> String {
  let mut out = String::with_capacity(input.len());
  let mut rest = input;
  while let Some(open) = rest.find('{') {
    let Some(close_rel) = rest[open..].find('}') else {
      break;
    };
    let close = open + close_rel;
    let inner = &rest[open + 1..close];
    let matched = if inner == token {
      Some("")
    } else if inner.len() > token.len()
      && inner.ends_with(token)
      && inner[..inner.len() - token.len()].chars().count() == 1
    {
      Some(&inner[..inner.len() - token.len()])
    } else {
      None
    };
    out.push_str(&rest[..open]);
    match matched {
      Some(separator) => {
        if !(value.is_empty() && omit_when_empty) {
          out.push_str(separator);
          out.push_str(value);
        }
      },
      None => out.push_str(&rest[open..=close]),
    }
    rest = &rest[close + 1..];
  }
  out.push_str(rest);
  out
}

/// An `.aria.yml` name keeps both halves of its extension.
fn aria_aware_extname(kind: SnapshotKind, file_path: &str) -> String {
  if kind == SnapshotKind::Aria && file_path.ends_with(".aria.yml") {
    return ".aria.yml".to_string();
  }
  extname(file_path)
}

/// Node's `path.extname`: the last dot in the last segment, and nothing
/// for a dotfile or a name with no dot.
fn extname(file_path: &str) -> String {
  let base = file_path.rsplit(['/', '\\']).next().unwrap_or(file_path);
  match base.rfind('.') {
    Some(idx) if idx > 0 => base[idx..].to_string(),
    _ => String::new(),
  }
}

/// Playwright's `sanitizeForFilePath`: every ASCII character outside
/// `-`, `0-9`, `A-Z`, `_`, `a-z` collapses to a single `-`.
#[must_use]
pub fn sanitize_for_file_path(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  let mut in_run = false;
  for c in s.chars() {
    let keep = matches!(c, '-' | '_') || c.is_ascii_alphanumeric() || !c.is_ascii();
    if keep {
      out.push(c);
      in_run = false;
    } else if !in_run {
      out.push('-');
      in_run = true;
    }
  }
  out
}

/// Playwright's `sanitizeFilePathBeforeExtension`: the extension is left
/// alone so `a b.png` becomes `a-b.png`, not `a-b-png`.
fn sanitize_file_path_before_extension(file_path: &str, ext: &str) -> String {
  let base = file_path.strip_suffix(ext).unwrap_or(file_path);
  format!("{}{ext}", sanitize_for_file_path(base))
}

/// Playwright's `trimLongString`: keep both ends, put a short hash in
/// the middle, so two long names that share a prefix stay distinct.
fn trim_long_string(s: &str, length: usize) -> String {
  if s.chars().count() <= length {
    return s.to_string();
  }
  let hash = ferridriver::tracing::sha1_hex(s.as_bytes());
  let middle = format!("-{}-", &hash[..5]);
  let start = (length - middle.len()) / 2;
  let end = length - middle.len() - start;
  let chars: Vec<char> = s.chars().collect();
  let head: String = chars[..start].iter().collect();
  let tail: String = chars[chars.len() - end..].iter().collect();
  format!("{head}{middle}{tail}")
}

/// Playwright's `addSuffixToFilePath`: before the extension.
#[must_use]
pub fn add_suffix_to_file_path(file_path: &str, suffix: &str) -> String {
  let ext = extname(file_path);
  let base = &file_path[..file_path.len() - ext.len()];
  format!("{base}{suffix}{ext}")
}

#[cfg(test)]
mod tests {
  use super::*;

  fn context() -> SnapshotPathContext {
    SnapshotPathContext {
      config_dir: PathBuf::from("/repo"),
      test_dir: PathBuf::from("/repo/tests"),
      snapshot_dir: PathBuf::from("/repo/tests"),
      project_name: String::new(),
      test_file: PathBuf::from("/repo/tests/e2e/login.spec.ts"),
      title_path: vec![
        "tests/e2e/login.spec.ts".to_string(),
        "sign in".to_string(),
        "shows the form".to_string(),
      ],
      snapshot_suffix: String::new(),
      screenshot_template: None,
      aria_template: None,
      config_template: None,
    }
  }

  fn resolve(cx: &SnapshotPathContext, kind: SnapshotKind, name: &SnapshotName) -> ResolvedSnapshotPaths {
    let mut names = SnapshotNames::default();
    resolve_snapshot_paths(cx, kind, name, &mut names, true, None)
  }

  #[test]
  fn the_legacy_layout_puts_a_baseline_beside_its_spec() {
    let cx = context();
    let resolved = resolve(
      &cx,
      SnapshotKind::Screenshot,
      &SnapshotName::One("shot.png".to_string()),
    );
    assert_eq!(
      resolved.absolute_snapshot_path,
      PathBuf::from("/repo/tests/e2e/login.spec.ts-snapshots/shot.png")
    );
  }

  #[test]
  fn a_project_name_and_suffix_qualify_the_file_only_when_they_exist() {
    let mut cx = context();
    cx.project_name = "web kit".to_string();
    cx.snapshot_suffix = "linux".to_string();
    let with = resolve(
      &cx,
      SnapshotKind::Screenshot,
      &SnapshotName::One("shot.png".to_string()),
    );
    assert_eq!(
      with.absolute_snapshot_path,
      PathBuf::from("/repo/tests/e2e/login.spec.ts-snapshots/shot-web-kit-linux.png")
    );

    // Empty values take their separator with them — this is the whole
    // reason the template is not a plain replace.
    let plain = resolve(
      &context(),
      SnapshotKind::Screenshot,
      &SnapshotName::One("shot.png".to_string()),
    );
    assert_eq!(
      plain.absolute_snapshot_path,
      PathBuf::from("/repo/tests/e2e/login.spec.ts-snapshots/shot.png")
    );
  }

  #[test]
  fn a_custom_template_names_every_token() {
    let mut cx = context();
    cx.project_name = "chromium".to_string();
    cx.config_template =
      Some("{testDir}/../__screenshots__/{projectName}/{platform}/{testFilePath}/{arg}{ext}".to_string());
    let resolved = resolve(
      &cx,
      SnapshotKind::Screenshot,
      &SnapshotName::One("shot.png".to_string()),
    );
    assert_eq!(
      resolved.absolute_snapshot_path,
      PathBuf::from(format!(
        "/repo/__screenshots__/chromium/{}/e2e/login.spec.ts/shot.png",
        node_platform()
      ))
    );
  }

  #[test]
  fn an_empty_arg_leaves_no_literal_braces() {
    let mut cx = context();
    cx.config_template = Some("{testDir}/__screenshots__/{testFilePath}/{arg}{ext}".to_string());
    let resolved = resolve(&cx, SnapshotKind::Screenshot, &SnapshotName::Anonymous);
    let rendered = resolved.absolute_snapshot_path.to_string_lossy().into_owned();
    assert!(!rendered.contains('{'), "{rendered} kept a literal brace");
    assert!(
      rendered.ends_with("/e2e/login.spec.ts/sign-in-shows-the-form-1.png"),
      "unexpected anonymous name: {rendered}"
    );
  }

  #[test]
  fn file_name_keeps_its_extension_and_base_name_does_not() {
    let mut cx = context();
    cx.config_template = Some("{snapshotDir}/{testFileName}/{testFileBaseName}/{arg}{ext}".to_string());
    let resolved = resolve(&cx, SnapshotKind::Snapshot, &SnapshotName::One("a.txt".to_string()));
    assert_eq!(
      resolved.absolute_snapshot_path,
      PathBuf::from("/repo/tests/login.spec.ts/login.spec/a.txt")
    );
  }

  #[test]
  fn a_relative_template_resolves_against_the_config_dir() {
    let mut cx = context();
    cx.config_template = Some("__snapshots__/{arg}{ext}".to_string());
    let resolved = resolve(&cx, SnapshotKind::Snapshot, &SnapshotName::One("a.txt".to_string()));
    assert_eq!(
      resolved.absolute_snapshot_path,
      PathBuf::from("/repo/__snapshots__/a.txt")
    );
  }

  #[test]
  fn an_anonymous_snapshot_is_a_txt_file_named_after_the_test() {
    let cx = context();
    let mut names = SnapshotNames::default();
    let first = resolve_snapshot_paths(
      &cx,
      SnapshotKind::Snapshot,
      &SnapshotName::Anonymous,
      &mut names,
      true,
      None,
    );
    assert_eq!(
      first.absolute_snapshot_path,
      PathBuf::from("/repo/tests/e2e/login.spec.ts-snapshots/sign-in-shows-the-form-1.txt")
    );
    // The index advances, so a second anonymous snapshot is a new file.
    let second = resolve_snapshot_paths(
      &cx,
      SnapshotKind::Snapshot,
      &SnapshotName::Anonymous,
      &mut names,
      true,
      None,
    );
    assert_eq!(
      second.absolute_snapshot_path,
      PathBuf::from("/repo/tests/e2e/login.spec.ts-snapshots/sign-in-shows-the-form-2.txt")
    );
    // Reading a path must not consume an index.
    let peek = resolve_snapshot_paths(
      &cx,
      SnapshotKind::Snapshot,
      &SnapshotName::Anonymous,
      &mut names,
      false,
      None,
    );
    let peek_again = resolve_snapshot_paths(
      &cx,
      SnapshotKind::Snapshot,
      &SnapshotName::Anonymous,
      &mut names,
      false,
      None,
    );
    assert_eq!(peek.absolute_snapshot_path, peek_again.absolute_snapshot_path);
  }

  #[test]
  fn a_repeated_name_indexes_its_output_copy_only() {
    let cx = context();
    let mut names = SnapshotNames::default();
    let first = resolve_snapshot_paths(
      &cx,
      SnapshotKind::Screenshot,
      &SnapshotName::One("a.png".to_string()),
      &mut names,
      true,
      None,
    );
    let second = resolve_snapshot_paths(
      &cx,
      SnapshotKind::Screenshot,
      &SnapshotName::One("a.png".to_string()),
      &mut names,
      true,
      None,
    );
    assert_eq!(
      first.absolute_snapshot_path, second.absolute_snapshot_path,
      "the same name is the same baseline"
    );
    assert_eq!(first.relative_output_path, PathBuf::from("a.png"));
    assert_eq!(
      second.relative_output_path,
      PathBuf::from("a-1.png"),
      "the second call's actual/diff must not clobber the first's"
    );
  }

  #[test]
  fn an_array_name_is_a_path_the_author_chose() {
    let cx = context();
    let resolved = resolve(
      &cx,
      SnapshotKind::Screenshot,
      &SnapshotName::Segments(vec!["some dir".to_string(), "a b.png".to_string()]),
    );
    assert_eq!(
      resolved.absolute_snapshot_path,
      PathBuf::from("/repo/tests/e2e/login.spec.ts-snapshots/some dir/a b.png"),
      "segments are not sanitized (microsoft/playwright#9156)"
    );
  }

  #[test]
  fn an_aria_name_keeps_both_halves_of_its_extension() {
    let cx = context();
    let resolved = resolve(&cx, SnapshotKind::Aria, &SnapshotName::One("form.aria.yml".to_string()));
    assert_eq!(
      resolved.absolute_snapshot_path,
      PathBuf::from("/repo/tests/e2e/login.spec.ts-snapshots/form.aria.yml"),
      "`.aria.yml` is one extension, not `.yml` after a `form.aria` stem"
    );
    let anonymous = resolve(&cx, SnapshotKind::Aria, &SnapshotName::Anonymous);
    assert!(
      anonymous
        .absolute_snapshot_path
        .to_string_lossy()
        .ends_with("sign-in-shows-the-form-1.aria.yml")
    );
  }

  #[test]
  fn a_name_is_sanitized_before_its_extension() {
    let cx = context();
    let resolved = resolve(
      &cx,
      SnapshotKind::Screenshot,
      &SnapshotName::One("a b/c:d.png".to_string()),
    );
    assert_eq!(
      resolved.absolute_snapshot_path,
      PathBuf::from("/repo/tests/e2e/login.spec.ts-snapshots/a-b-c-d.png")
    );
  }

  #[test]
  fn an_empty_name_is_the_anonymous_form() {
    // `if (!name)` upstream: an empty string is not a nameless file.
    assert!(matches!(
      SnapshotName::from_parts(&[String::new()]),
      SnapshotName::Anonymous
    ));
    let cx = context();
    let resolved = resolve(&cx, SnapshotKind::Snapshot, &SnapshotName::from_parts(&[String::new()]));
    assert_eq!(
      resolved.absolute_snapshot_path,
      PathBuf::from("/repo/tests/e2e/login.spec.ts-snapshots/sign-in-shows-the-form-1.txt")
    );
    // A real single name still names the file.
    assert!(matches!(
      SnapshotName::from_parts(&["a.png".to_string()]),
      SnapshotName::One(_)
    ));
  }

  #[test]
  fn a_long_name_keeps_both_ends_around_a_hash() {
    let long = "x".repeat(200);
    let trimmed = trim_long_string(&long, 100);
    assert_eq!(trimmed.chars().count(), 100);
    assert!(trimmed.contains('-'), "the hash separator is missing: {trimmed}");
    assert_eq!(trim_long_string("short", 100), "short");
  }

  #[test]
  fn a_spec_outside_test_dir_relativizes_instead_of_going_absolute() {
    // The bug this replaced: `strip_prefix` fails, the fallback is the
    // ABSOLUTE path, and `{snapshotDir}/{testFileDir}` becomes a mirror
    // of the whole filesystem under the snapshot dir.
    let rel = super::relative_to(Path::new("/repo/tests"), Path::new("/repo/other/login.spec.ts"));
    assert_eq!(rel, PathBuf::from("../other/login.spec.ts"));
    assert!(!rel.is_absolute(), "a relative path is the whole point");
  }

  #[test]
  fn a_spec_under_test_dir_strips_to_its_own_tail() {
    assert_eq!(
      super::relative_to(Path::new("/repo/tests"), Path::new("/repo/tests/e2e/login.spec.ts")),
      PathBuf::from("e2e/login.spec.ts")
    );
  }

  #[test]
  fn the_two_sides_of_a_symlinked_temp_dir_still_strip() {
    // macOS: `std::env::temp_dir()` answers `/var/folders/...` while a
    // canonicalised spec path is `/private/var/folders/...`. Both name
    // the same directory, so the spec must relativize to its own name
    // and not drag `private/var/folders/...` into the template.
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = dir.path().join("snap.test.ts");
    std::fs::write(&spec, "").expect("write spec");
    let canonical = spec.canonicalize().expect("canonicalize");
    assert_eq!(
      super::relative_to(dir.path(), &canonical),
      PathBuf::from("snap.test.ts")
    );
  }
}
