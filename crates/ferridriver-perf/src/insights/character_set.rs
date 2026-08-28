//! Whether the document declared its character encoding early enough.
//!
//! Mirrors devtools-frontend `insights/CharacterSet.ts`. Without a
//! declaration in the `Content-Type` header or a `<meta charset>` inside
//! the first 1024 bytes, the parser has to guess, and may have to
//! restart once it finds out it guessed wrong.

use crate::handlers::network::NetworkRequest;
use crate::insights::{Check, Insight, Severity};

/// What the trace said about the meta tag.
///
/// `Unknown` is distinct from `NotFound`: Blink emits the check only on
/// recent Chrome, and a trace without it says nothing either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MetaCharset {
  Early,
  Late,
  NotFound,
  #[default]
  Unknown,
}

impl MetaCharset {
  #[must_use]
  pub fn from_disposition(disposition: &str) -> Self {
    match disposition {
      "found-in-first-1024-bytes" => Self::Early,
      "found-after-first-1024-bytes" => Self::Late,
      "not-found" => Self::NotFound,
      _ => Self::Unknown,
    }
  }

  fn detail(self) -> &'static str {
    match self {
      Self::Early => "Declares charset using a meta tag in the first 1024 bytes",
      Self::Late => "Declares charset using a meta tag after the first 1024 bytes",
      Self::NotFound => "Doesn't declare charset using a meta tag",
      Self::Unknown => "Couldn't determine meta charset declaration from trace",
    }
  }
}

#[must_use]
pub fn run(document: Option<&NetworkRequest>, meta: MetaCharset) -> Option<Insight> {
  let document = document?;

  // Only `Content-Type` counts; a `charset` elsewhere is not what the
  // parser reads.
  let http_charset = document
    .header("content-type")
    .is_some_and(|value| value.to_ascii_lowercase().contains("charset="));

  let checks = vec![
    Check {
      name: "httpCharset".into(),
      passed: http_charset,
      detail: if http_charset {
        "Declares charset in HTTP header".into()
      } else {
        "Doesn't declare charset in HTTP header".into()
      },
    },
    Check {
      name: "metaCharset".into(),
      passed: meta == MetaCharset::Early,
      detail: meta.detail().into(),
    },
  ];

  // Either declaration alone is enough for the parser.
  let declared = http_charset || meta == MetaCharset::Early;
  Some(Insight {
    key: "CharacterSet".into(),
    title: "Character encoding".into(),
    description: "A character encoding declaration is required. It can be done with a meta charset tag in the \
                  first 1024 bytes of the HTML, or in the Content-Type HTTP response header."
      .into(),
    severity: if declared { Severity::Pass } else { Severity::Fail },
    checks,
    metrics: Vec::new(),
    items: Vec::new(),
  })
}
