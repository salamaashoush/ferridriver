//! What can go wrong reading a snapshot.

/// A snapshot that could not be read.
#[derive(Debug, thiserror::Error)]
pub enum HeapError {
  /// The text is not JSON, or not the shape a snapshot has.
  #[error("heap snapshot is not readable: {0}")]
  Json(#[from] serde_json::Error),
  /// The text parses, but its `meta` omits something this reader needs
  /// or its arrays contradict the counts it declares.
  #[error("heap snapshot is malformed: {0}")]
  Format(String),
}

pub type Result<T> = std::result::Result<T, HeapError>;
