//! `BiDi` `permissions` module.

use serde::Serialize;

use crate::{Command, EmptyResponse};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionDescriptor<'a> {
  pub name: &'a str,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPermission<'a> {
  pub descriptor: PermissionDescriptor<'a>,
  pub state: &'a str,
  pub origin: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub user_context: Option<&'a str>,
}

impl Command for SetPermission<'_> {
  const METHOD: &'static str = "permissions.setPermission";
  type Response = EmptyResponse;
}
