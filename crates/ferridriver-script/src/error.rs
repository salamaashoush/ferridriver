//! Script execution errors: the runtime's, plus the one name ferridriver
//! reserves.

pub use ferrijs::{ScriptError, ScriptErrorKind};

/// `name` every operator-ceiling refusal carries, on the Rust error and
/// on the JS `Error` it is thrown as.
///
/// `install_extensions` reads it to tell a refusal by
/// `[extensions.policy]` -- which can never be skipped, because skipping
/// would silently run the deployment with authority the operator denied
/// -- apart from an extension whose own top level threw, which is
/// skippable while it left no registrations behind.
pub const EXTENSION_POLICY_ERROR: &str = "ExtensionPolicyError";

/// An operator-ceiling refusal: an `[extensions.policy]` key denied
/// something a package's manifest or contribution asked for. Carries
/// [`EXTENSION_POLICY_ERROR`] as its `name` so the extension loader can
/// refuse to skip past it.
#[must_use]
pub fn policy_error(message: impl Into<String>) -> ScriptError {
  ScriptError::named(EXTENSION_POLICY_ERROR, message)
}

/// Whether this is an `[extensions.policy]` refusal -- the one error an
/// extension loader may never skip past.
#[must_use]
pub fn is_policy_refusal(error: &ScriptError) -> bool {
  error.name.as_deref() == Some(EXTENSION_POLICY_ERROR)
}
