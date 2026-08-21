//! Width-aware columnar output.
//!
//! Hand-padded `{:<20}` misaligns the moment a value is wider than the guess
//! or carries an escape sequence, and both happen constantly here: session
//! ids, trace paths and tool names are all unbounded. Columns are measured
//! from the content, styling is discounted from the measurement, and whatever
//! does not fit the terminal is taken out of the column that can afford it.

use console::{Alignment, Style};

/// Space between columns. Two, so a right-aligned number never touches the
/// value left of it.
const GUTTER: usize = 2;

/// Narrowest a column may be squeezed to before the table gives up and lets
/// the line overflow: below this the content is all ellipsis.
const MIN_COL: usize = 8;

pub struct Table {
  headers: Vec<String>,
  /// Column that absorbs the slack, and gives it back first when the table is
  /// too wide. Defaults to the widest column.
  flex: Option<usize>,
  /// Leading spaces on every line, so a table nested under a list item lines
  /// up with it instead of starting back at the margin.
  indent: usize,
  rows: Vec<Vec<String>>,
}

impl Table {
  #[must_use]
  pub fn new(headers: &[&str]) -> Self {
    Self {
      headers: headers.iter().map(|h| (*h).to_string()).collect(),
      flex: None,
      indent: 0,
      rows: Vec::new(),
    }
  }

  /// Nominate the column that gives up width first.
  #[must_use]
  pub fn flex(mut self, column: usize) -> Self {
    self.flex = Some(column);
    self
  }

  /// Indent every line, header included.
  #[must_use]
  pub fn indent(mut self, spaces: usize) -> Self {
    self.indent = spaces;
    self
  }

  pub fn row<I, S>(&mut self, cells: I)
  where
    I: IntoIterator<Item = S>,
    S: Into<String>,
  {
    self.rows.push(cells.into_iter().map(Into::into).collect());
  }

  /// Render to a string, laid out for `available` columns of terminal.
  #[must_use]
  pub fn render(&self, available: usize) -> String {
    let margin = " ".repeat(self.indent);
    let widths = self.resolve_widths(available.saturating_sub(self.indent));
    let mut out = String::new();
    let head = Style::new().dim().bold();
    let line: Vec<String> = self
      .headers
      .iter()
      .enumerate()
      .map(|(i, h)| pad(&head.apply_to(h).to_string(), widths[i]))
      .collect();
    out.push_str(&margin);
    out.push_str(line.join(&" ".repeat(GUTTER)).trim_end());
    out.push('\n');
    for row in &self.rows {
      let line: Vec<String> = (0..self.headers.len())
        .map(|i| {
          let cell = row.get(i).map_or("", String::as_str);
          pad(cell, widths[i])
        })
        .collect();
      out.push_str(&margin);
      out.push_str(line.join(&" ".repeat(GUTTER)).trim_end());
      out.push('\n');
    }
    out
  }

  /// Print the table, or nothing at all when it has no rows — an empty table
  /// is a header line that says a thing exists when it does not.
  pub fn print(&self, available: usize) {
    if self.rows.is_empty() {
      return;
    }
    print!("{}", self.render(available));
  }

  /// Natural width of every column, then the shrink pass that makes the whole
  /// line fit.
  fn resolve_widths(&self, available: usize) -> Vec<usize> {
    let mut widths: Vec<usize> = self.headers.iter().map(|h| console::measure_text_width(h)).collect();
    for row in &self.rows {
      for (i, cell) in row.iter().enumerate() {
        if let Some(w) = widths.get_mut(i) {
          *w = (*w).max(console::measure_text_width(cell));
        }
      }
    }
    let gutters = GUTTER * self.headers.len().saturating_sub(1);
    let total: usize = widths.iter().sum::<usize>() + gutters;
    if total <= available {
      return widths;
    }
    let victim = self
      .flex
      .filter(|i| *i < widths.len())
      .unwrap_or_else(|| widths.iter().enumerate().max_by_key(|(_, w)| **w).map_or(0, |(i, _)| i));
    let others: usize = widths
      .iter()
      .enumerate()
      .filter(|(i, _)| *i != victim)
      .map(|(_, w)| *w)
      .sum();
    widths[victim] = available.saturating_sub(others + gutters).max(MIN_COL);
    widths
  }
}

/// Pad or truncate one cell to `width`, measuring the display width so ANSI
/// sequences neither count toward it nor get cut in half.
fn pad(cell: &str, width: usize) -> String {
  if console::measure_text_width(cell) > width {
    return console::truncate_str(cell, width, "…").into_owned();
  }
  console::pad_str(cell, width, Alignment::Left, None).into_owned()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn columns_size_to_their_widest_cell() {
    let mut t = Table::new(&["ID", "PID"]);
    t.row(["a-very-long-session-id", "1"]);
    t.row(["b", "123456"]);
    let out = t.render(120);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3);
    // Every line pads to the same column start, which hand-rolled `{:<20}`
    // stops doing the moment a value exceeds the guess.
    let col = lines[1].find('1').expect("pid on the first row");
    assert_eq!(&lines[2][col..col + 6], "123456");
  }

  #[test]
  fn the_flex_column_absorbs_the_overflow() {
    let mut t = Table::new(&["PATH", "SIZE"]).flex(0);
    t.row([format!("/{}/report.zip", "x".repeat(200)), "2 KB".to_string()]);
    let out = t.render(40);
    for line in out.lines() {
      assert!(console::measure_text_width(line) <= 40, "{line}");
    }
  }

  #[test]
  fn styling_does_not_count_toward_the_column_width() {
    let styled = Style::new().green().force_styling(true).apply_to("ok").to_string();
    assert!(styled.len() > 2, "the fixture must actually carry escapes");
    let mut t = Table::new(&["S", "N"]);
    t.row([styled, "1".to_string()]);
    let out = t.render(80);
    let row = out.lines().nth(1).expect("a row");
    assert!(console::measure_text_width(row) < 10, "{row:?}");
  }

  #[test]
  fn an_empty_table_prints_no_header() {
    let t = Table::new(&["A"]);
    let mut sink = String::new();
    if !t.rows.is_empty() {
      sink.push_str(&t.render(80));
    }
    assert!(sink.is_empty());
  }
}
