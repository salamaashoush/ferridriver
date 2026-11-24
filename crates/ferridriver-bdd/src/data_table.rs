//! `DataTable`: a structured Gherkin data table with utility methods.

use std::ops::{Deref, DerefMut};

use rustc_hash::FxHashMap;

/// A Gherkin data table (rows of string cells) with helper methods.
#[derive(Debug, Clone)]
pub struct DataTable {
  rows: Vec<Vec<String>>,
}

impl DataTable {
  #[must_use]
  pub fn new(rows: Vec<Vec<String>>) -> Self {
    Self { rows }
  }

  #[must_use]
  pub fn raw(&self) -> &[Vec<String>] {
    &self.rows
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.rows.is_empty()
  }

  #[must_use]
  pub fn len(&self) -> usize {
    self.rows.len()
  }

  /// First row as headers.
  #[must_use]
  pub fn headers(&self) -> Option<&[String]> {
    self.rows.first().map(std::vec::Vec::as_slice)
  }

  /// All rows except the header row.
  #[must_use]
  pub fn data_rows(&self) -> &[Vec<String>] {
    if self.rows.len() > 1 { &self.rows[1..] } else { &[] }
  }

  /// Convert to array of header→value maps (one per data row).
  #[must_use]
  pub fn hashes(&self) -> Vec<FxHashMap<&str, &str>> {
    let Some(headers) = self.headers() else {
      return Vec::new();
    };
    self
      .data_rows()
      .iter()
      .map(|row| {
        headers
          .iter()
          .zip(row.iter())
          .map(|(h, v)| (h.as_str(), v.as_str()))
          .collect()
      })
      .collect()
  }

  /// Convert two-column table to key→value map (first col = key, second col = value).
  #[must_use]
  pub fn rows_hash(&self) -> FxHashMap<&str, &str> {
    self
      .rows
      .iter()
      .filter(|r| r.len() >= 2)
      .map(|r| (r[0].as_str(), r[1].as_str()))
      .collect()
  }

  /// Transpose rows and columns.
  #[must_use]
  pub fn transpose(&self) -> DataTable {
    if self.rows.is_empty() {
      return DataTable::new(Vec::new());
    }
    let max_cols = self.rows.iter().map(std::vec::Vec::len).max().unwrap_or(0);
    let mut transposed = vec![Vec::with_capacity(self.rows.len()); max_cols];
    for row in &self.rows {
      for (col_idx, cell) in row.iter().enumerate() {
        transposed[col_idx].push(cell.clone());
      }
    }
    DataTable::new(transposed)
  }

  /// Get a specific cell value.
  pub fn cell(&self, row: usize, col: usize) -> Option<&str> {
    self.rows.get(row).and_then(|r| r.get(col)).map(String::as_str)
  }
}

impl Deref for DataTable {
  type Target = [Vec<String>];

  fn deref(&self) -> &[Vec<String>] {
    &self.rows
  }
}

impl DerefMut for DataTable {
  fn deref_mut(&mut self) -> &mut [Vec<String>] {
    &mut self.rows
  }
}

impl From<Vec<Vec<String>>> for DataTable {
  fn from(rows: Vec<Vec<String>>) -> Self {
    Self::new(rows)
  }
}

/// Trait for converting a `DataTable` into typed rows.
pub trait FromDataTable: Sized {
  /// Build one value from a data row and the table's header row.
  ///
  /// # Errors
  ///
  /// When `row` cannot be read as `Self`.
  fn from_row(headers: &[String], row: &[String]) -> ferridriver::error::Result<Self>;
}

impl DataTable {
  /// Convert data rows to typed structs using the `FromDataTable` trait.
  ///
  /// # Errors
  ///
  /// When the table has no header row, or any data row fails `T::from_row`.
  pub fn as_type<T: FromDataTable>(&self) -> ferridriver::error::Result<Vec<T>> {
    let headers = self
      .headers()
      .ok_or_else(|| ferridriver::FerriError::invalid_argument("data-table", "table has no header row"))?;
    self.data_rows().iter().map(|row| T::from_row(headers, row)).collect()
  }
}
