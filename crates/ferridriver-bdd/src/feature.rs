//! Feature file discovery and Gherkin parsing.

use std::path::PathBuf;

/// A parsed `.feature` file.
pub struct ParsedFeature {
  /// File path.
  pub path: PathBuf,
  /// Parsed Gherkin feature AST.
  pub feature: gherkin::Feature,
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
  pub fn discover(patterns: &[String], ignore: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();

    for raw_pattern in patterns {
      // If the pattern is a directory, expand to recursive glob.
      let pattern = if std::path::Path::new(raw_pattern).is_dir() {
        let trimmed = raw_pattern.trim_end_matches('/');
        format!("{trimmed}/**/*.feature")
      } else {
        raw_pattern.clone()
      };

      let entries =
        glob::glob(&pattern).map_err(|e| format!("invalid glob pattern \"{pattern}\": {e}"))?;

      for entry in entries {
        match entry {
          Ok(path) => {
            if path.extension().and_then(|e| e.to_str()) == Some("feature") {
              let should_ignore = ignore.iter().any(|ig| {
                glob::Pattern::new(ig)
                  .map(|p| p.matches_path(&path))
                  .unwrap_or(false)
              });
              if !should_ignore {
                files.push(path);
              }
            }
          }
          Err(e) => {
            tracing::warn!("glob error: {e}");
          }
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
  pub fn parse(files: Vec<PathBuf>) -> Result<Self, String> {
    Self::parse_with_language(files, None)
  }

  /// Parse feature files with an optional default language for i18n keyword support.
  pub fn parse_with_language(files: Vec<PathBuf>, language: Option<&str>) -> Result<Self, String> {
    let mut features = Vec::with_capacity(files.len());

    for path in files {
      let env = if let Some(lang) = language {
        gherkin::GherkinEnv::new(lang)
          .map_err(|e| format!("unsupported language \"{lang}\": {e}"))?
      } else {
        gherkin::GherkinEnv::default()
      };
      let mut feature = gherkin::Feature::parse_path(&path, env)
        .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;

      // parse_path may not set the path field, ensure it is set.
      if feature.path.is_none() {
        feature.path = Some(path.clone());
      }

      features.push(ParsedFeature { path, feature });
    }

    Ok(Self { features })
  }

  /// Discover and parse in one step.
  pub fn discover_and_parse(
    patterns: &[String],
    ignore: &[String],
  ) -> Result<Self, String> {
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
    .map(|t| {
      if t.starts_with('@') {
        t.clone()
      } else {
        format!("@{t}")
      }
    })
    .collect()
}

/// Convert a `gherkin::Table` into a `DataTable`.
pub fn table_to_vec(table: &gherkin::Table) -> crate::data_table::DataTable {
  crate::data_table::DataTable::new(table.rows.clone())
}
