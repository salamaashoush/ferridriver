//! What can go wrong reading a snapshot, or asking it a question.

/// A snapshot that could not be read, or a query it could not answer.
#[derive(Debug, thiserror::Error)]
pub enum HeapError {
  /// The text is not JSON, or not the shape a snapshot has.
  #[error("heap snapshot is not readable: {0}")]
  Json(#[from] serde_json::Error),
  /// The text parses, but its `meta` omits something this reader needs
  /// or its arrays contradict the counts it declares.
  #[error("heap snapshot is malformed: {0}")]
  Format(String),
  /// A query carried a pattern that is not a regular expression.
  #[error("{what} is not a valid regular expression: {source}")]
  Pattern {
    what: &'static str,
    #[source]
    source: Box<regex::Error>,
  },
}

pub type Result<T> = std::result::Result<T, HeapError>;
