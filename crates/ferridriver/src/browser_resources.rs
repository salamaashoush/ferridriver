use std::path::Path;
use std::sync::{Arc, Mutex};

use tokio::sync::RwLock;

use crate::options::{ConnectOptions, ConnectOverCdpOptions, LaunchOptions, LaunchPersistentContextOptions};
use crate::state::BrowserState;
use crate::{Browser, BrowserType, ContextRef, FerriError, Result};

#[cfg(test)]
#[path = "browser_resources_tests.rs"]
mod tests;

/// Retains complete browsers and failed allocations across caller cancellation.
/// Closing permanently ends admission; failed cleanup remains available for retry.
#[derive(Default)]
pub struct BrowserResources {
  registry: Mutex<Registry>,
  cleanup: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct Registry {
  closing: bool,
  states: Vec<Arc<dyn BrowserResource>>,
}

// Keep downstream Send checks from expanding the recursive browser/page graph.
trait BrowserResource: Send + Sync {
  fn close(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>>;
}

impl BrowserResource for RwLock<BrowserState> {
  fn close(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
    Box::pin(async move { self.write().await.dispose_result().await })
  }
}

impl BrowserResources {
  /// # Errors
  /// Returns launch or admission errors while retaining allocated resources for cleanup.
  pub async fn launch(&self, browser_type: BrowserType, options: LaunchOptions) -> Result<Browser> {
    browser_type.launch_with_resources(options, Some(self)).await
  }

  /// # Errors
  /// Returns connection or admission errors while retaining allocated resources for cleanup.
  pub async fn connect(&self, browser_type: BrowserType, endpoint: &str, options: ConnectOptions) -> Result<Browser> {
    browser_type.connect_with_resources(endpoint, options, Some(self)).await
  }

  /// # Errors
  /// Returns CDP connection or admission errors while retaining allocated resources for cleanup.
  pub async fn connect_over_cdp(
    &self,
    browser_type: BrowserType,
    endpoint: &str,
    options: ConnectOverCdpOptions,
  ) -> Result<Browser> {
    browser_type
      .connect_over_cdp_with_resources(endpoint, options, Some(self))
      .await
  }

  /// # Errors
  /// Returns launch or admission errors. Caller-owned profile directories remain caller-owned.
  pub async fn launch_persistent_context(
    &self,
    browser_type: BrowserType,
    user_data_dir: &Path,
    options: LaunchPersistentContextOptions,
  ) -> Result<ContextRef> {
    browser_type
      .launch_persistent_context_with_resources(user_data_dir, options, Some(self))
      .await
  }

  pub(crate) fn retain(&self, state: Arc<RwLock<BrowserState>>) -> Result<()> {
    let mut registry = self.registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if registry.closing {
      return Err(FerriError::target_closed(Some(
        "browser resource owner is closing".into(),
      )));
    }
    registry.states.push(state);
    Ok(())
  }

  /// # Errors
  /// Reports cleanup failures after attempting every retained browser; another close retries failures.
  pub async fn close(&self) -> Result<()> {
    self.begin_close();
    let _cleanup = self.cleanup.lock().await;
    let states = self
      .registry
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .states
      .clone();
    let mut errors = Vec::new();
    for state in states {
      match state.close().await {
        Ok(()) => {
          self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .states
            .retain(|retained| !Arc::ptr_eq(retained, &state));
        },
        Err(error) => errors.push(error.to_string()),
      }
    }
    if errors.is_empty() {
      Ok(())
    } else {
      Err(FerriError::backend(errors.join("; ")))
    }
  }

  /// Stop admitting factories while preserving existing resources for ordered cleanup.
  pub fn begin_close(&self) {
    self
      .registry
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .closing = true;
  }
}

impl Drop for BrowserResources {
  fn drop(&mut self) {
    let registry = self
      .registry
      .get_mut()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    for state in registry.states.drain(..) {
      if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move {
          if let Err(error) = state.close().await {
            tracing::warn!(%error, "Final browser resource cleanup failed");
          }
        });
      } else {
        tracing::warn!("Browser resources dropped without a runtime for cleanup");
      }
    }
  }
}
