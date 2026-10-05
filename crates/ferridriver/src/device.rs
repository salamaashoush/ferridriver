use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "camelCase")]
pub enum DeviceTarget {
  Android(crate::android::AndroidOptions),
  Ios(crate::ios::IosOptions),
}

impl DeviceTarget {
  #[must_use]
  pub fn browser(&self) -> crate::options::BrowserKind {
    match self {
      Self::Android(_) => crate::options::BrowserKind::Chromium,
      Self::Ios(_) => crate::options::BrowserKind::Safari,
    }
  }

  #[must_use]
  pub fn backend(&self) -> crate::backend::BackendKind {
    match self {
      Self::Android(_) => crate::backend::BackendKind::CdpWs,
      Self::Ios(_) => crate::backend::BackendKind::WebDriver,
    }
  }
}
