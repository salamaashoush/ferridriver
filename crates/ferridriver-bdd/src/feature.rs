//! Feature file discovery and Gherkin parsing.

use std::path::PathBuf;

use ferridriver::FerriError;
use ferridriver::error::Result;

/// A parsed `.feature` file.
pub struct ParsedFeature {
  /// File path.
  pub path: PathBuf,
  /// Parsed Gherkin feature AST.
  pub feature: gherkin::Feature,
  /// The file's own lines, 1-indexed by [`ParsedFeature::line`].
  ///
  /// The `gherkin` crate's AST carries neither comments nor tag
  /// positions, and a table knows only where it starts — so a
  /// `# title-format:` comment, a `tags[].line` in a cucumber-json
  /// document and an Examples ROW's line all have to be read back off
  /// the source.
  lines: Vec<String>,
}

/// Where a tag was written.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceTag {
  pub name: String,
  pub line: usize,
}

impl ParsedFeature {
  fn with_source(path: PathBuf, feature: gherkin::Feature, source: &str) -> Self {
    Self {
      path,
      feature,
      lines: source.lines().map(ToString::to_string).collect(),
    }
  }

  /// The file's `line`th line (1-indexed), if it has one.
  #[must_use]
  pub fn line(&self, line: usize) -> Option<&str> {
    line
      .checked_sub(1)
      .and_then(|index| self.lines.get(index))
      .map(String::as_str)
  }

  /// The comment on `line`, without its leading `#`-run trimmed — the
  /// text as `playwright-bdd` reads it, `# title-format: ...` included.
  #[must_use]
  pub fn comment_at(&self, line: usize) -> Option<&str> {
    let text = self.line(line)?.trim();
    text.starts_with('#').then_some(text)
  }

  /// The line each of `tags` was written on, searching upward from the
  /// keyword at `keyword_line` over the contiguous run of tag and
  /// comment lines above it. A tag the run does not mention is left
  /// without a line, which is what Cucumber itself emits for one it
  /// cannot place.
  #[must_use]
  pub fn tag_lines(&self, keyword_line: usize, tags: &[String]) -> Vec<SourceTag> {
    let mut found: rustc_hash::FxHashMap<&str, usize> = rustc_hash::FxHashMap::default();
    let mut line = keyword_line.saturating_sub(1);
    while line >= 1 {
      let Some(text) = self.line(line).map(str::trim) else {
        break;
      };
      if text.is_empty() || text.starts_with('#') {
        line -= 1;
        continue;
      }
      if !text.starts_with('@') {
        break;
      }
      for word in text.split_whitespace() {
        found.entry(word.trim_start_matches('@')).or_insert(line);
      }
      line -= 1;
    }
    tags
      .iter()
      .map(|tag| {
        let bare = tag.trim_start_matches('@');
        SourceTag {
          name: if tag.starts_with('@') {
            tag.clone()
          } else {
            format!("@{tag}")
          },
          line: found.get(bare).copied().unwrap_or_default(),
        }
      })
      .collect()
  }

  /// The line of each row of the table starting at `table_line`. Read
  /// off the source rather than counted, because Gherkin allows a
  /// comment or a blank line between two rows.
  #[must_use]
  pub fn table_row_lines(&self, table_line: usize, rows: usize) -> Vec<usize> {
    let mut lines = Vec::with_capacity(rows);
    let mut line = table_line;
    while lines.len() < rows {
      let Some(text) = self.line(line).map(str::trim) else {
        break;
      };
      if text.starts_with('|') {
        lines.push(line);
      } else if !text.is_empty() && !text.starts_with('#') {
        break;
      }
      line += 1;
    }
    // A table the source cannot account for (an inline feature built by
    // a test) still needs one line per row.
    while lines.len() < rows {
      lines.push(table_line + lines.len());
    }
    lines
  }
}

/// A collection of parsed features.
pub struct FeatureSet {
  pub features: Vec<ParsedFeature>,
}

impl FeatureSet {
  /// Discover `.feature` files matching the given glob patterns.
  ///
  /// If a pattern is a directory path (no glob chars, exists as dir), it is
  /// automatically expanded to `<dir>/**/*.feature` so users can pass bare
  /// directory paths like `tests/features/` or `tests/features`.
  pub fn discover(patterns: &[String], ignore: &[String]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for raw_pattern in patterns {
      // If the pattern is a directory, expand to recursive glob.
      let pattern = if std::path::Path::new(raw_pattern).is_dir() {
        let trimmed = raw_pattern.trim_end_matches('/');
        format!("{trimmed}/**/*.feature")
      } else {
        raw_pattern.clone()
      };

      let entries = glob::glob(&pattern)
        .map_err(|e| FerriError::invalid_argument("pattern", format!("invalid glob pattern \"{pattern}\": {e}")))?;

      for entry in entries {
        match entry {
          Ok(path) => {
            if path.extension().and_then(|e| e.to_str()) == Some("feature") {
              let should_ignore = ignore
                .iter()
                .any(|ig| glob::Pattern::new(ig).map(|p| p.matches_path(&path)).unwrap_or(false));
              if !should_ignore {
                files.push(path);
              }
            }
          },
          Err(e) => {
            tracing::warn!("glob error: {e}");
          },
        }
      }
    }

    files.sort();
    files.dedup();
    Ok(files)
  }

  /// Parse a list of feature files into a `FeatureSet`.
  ///
  /// When `language` is `Some("fr")`, all features default to that language's keywords.
  /// Individual features can still override via `# language: xx` comments.
  pub fn parse(files: Vec<PathBuf>) -> Result<Self> {
    Self::parse_with_language(files, None)
  }

  /// Parse feature files with an optional default language for i18n keyword support.
  pub fn parse_with_language(files: Vec<PathBuf>, language: Option<&str>) -> Result<Self> {
    let mut features = Vec::with_capacity(files.len());

    for path in files {
      let env = if let Some(lang) = language {
        gherkin::GherkinEnv::new(lang)
          .map_err(|e| FerriError::unsupported(format!("unsupported language \"{lang}\": {e}")))?
      } else {
        gherkin::GherkinEnv::default()
      };
      let mut feature = gherkin::Feature::parse_path(&path, env)
        .map_err(|e| FerriError::backend(format!("failed to parse {}: {e}", path.display())))?;

      // parse_path may not set the path field, ensure it is set.
      if feature.path.is_none() {
        feature.path = Some(path.clone());
      }

      let source = std::fs::read_to_string(&path).unwrap_or_default();
      features.push(ParsedFeature::with_source(path, feature, &source));
    }

    Ok(Self { features })
  }

  /// Parse inline Gherkin text into a `FeatureSet`.
  pub fn parse_text(text: &str) -> Result<Self> {
    let env = gherkin::GherkinEnv::default();
    let feature = gherkin::Feature::parse(text, env)
      .map_err(|e| FerriError::backend(format!("failed to parse Gherkin text: {e}")))?;
    Ok(Self {
      features: vec![ParsedFeature::with_source(PathBuf::from("<inline>"), feature, text)],
    })
  }

  /// Discover and parse in one step.
  pub fn discover_and_parse(patterns: &[String], ignore: &[String]) -> Result<Self> {
    let files = Self::discover(patterns, ignore)?;
    if files.is_empty() {
      tracing::warn!("no .feature files found matching patterns: {patterns:?}");
    }
    Self::parse(files)
  }
}

/// Extract tags from a Gherkin feature/scenario as `@tag` strings.
pub fn extract_tags(tags: &[String]) -> Vec<String> {
  tags
    .iter()
    .map(|t| if t.starts_with('@') { t.clone() } else { format!("@{t}") })
    .collect()
}

/// Convert a `gherkin::Table` into a `DataTable`.
pub fn table_to_vec(table: &gherkin::Table) -> crate::data_table::DataTable {
  crate::data_table::DataTable::new(table.rows.clone())
}
