//! Shared backend-agnostic driver primitives for ferridriver.

pub mod common;
pub mod disposable;
pub mod frame_cache;
pub mod json_scan;
pub mod keyboard;
pub mod operation_budget;
pub mod pause;
pub mod permissions;
pub mod screencast;
pub mod totp;
pub mod trace_categories;
pub mod transport_tasks;
pub mod url_matcher;

/// Frame metadata (backend-agnostic).
#[derive(Debug, Clone, serde::Serialize)]
pub struct FrameInfo {
  pub frame_id: String,
  pub parent_frame_id: Option<String>,
  pub name: String,
  pub url: String,
}
