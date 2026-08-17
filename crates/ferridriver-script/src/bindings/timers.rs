//! The timer globals, with ferridriver's net policy carried from
//! registration to callback.
//!
//! The timers themselves are [`ferridriver_jsstd::web::timers`]; what is
//! ferridriver's is the ambient state they carry. Capability follows the
//! registrar: a timer armed (or a microtask queued) by a net-restricted
//! tool handler keeps that handler's `allow.net` when it later fires from
//! the executor or the job queue, where the resting policy would
//! otherwise be unrestricted.

use std::sync::Arc;

use ferridriver_jsstd::web::timers::CallbackPolicy;
use rquickjs::Ctx;

/// The registrar's `allow.net` list.
#[derive(Clone)]
pub struct NetGrant(Arc<[String]>);

impl CallbackPolicy for NetGrant {
  fn capture(ctx: &Ctx<'_>) -> Option<Self> {
    crate::bindings::fetch::active_net(ctx).map(Self)
  }

  fn enter<R>(ctx: &Ctx<'_>, policy: Option<&Self>, f: impl FnOnce() -> R) -> R {
    crate::bindings::fetch::call_with_net(ctx, policy.map(|p| &p.0), f)
  }
}

pub fn install(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  ferridriver_jsstd::web::timers::install::<NetGrant>(ctx)
}
