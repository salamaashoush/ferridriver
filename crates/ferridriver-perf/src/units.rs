//! The numeric conversions this crate needs, in one place.
//!
//! Trace arithmetic mixes integer microseconds, integer byte counts and
//! floating-point milliseconds, so the casts are unavoidable. Routing
//! them through named functions means each conversion is justified once
//! here rather than argued about at sixty call sites, and a reader can
//! check the bounds argument in a single place.

use crate::event::Micro;

/// Largest integer `f64` represents exactly. Every quantity converted
/// below is bounded well under it: a trace covers seconds, so
/// microsecond timestamps run to about 1e10 relative to the trace
/// origin, and transfer sizes to about 1e9.
const F64_EXACT_INT_LIMIT: i64 = 1 << 53;

/// Microseconds to milliseconds.
///
/// Saturates rather than wrapping if a timestamp ever exceeded the
/// exact-integer range, so a corrupt trace yields an absurd number
/// instead of a silently wrong small one.
#[must_use]
pub fn micros_to_ms(us: Micro) -> f64 {
  int_to_f64(us) / 1000.0
}

/// Milliseconds back to microseconds, for comparing a derived metric
/// against raw event timestamps.
#[must_use]
pub fn ms_to_micros(ms: f64) -> Micro {
  f64_to_int(ms * 1000.0)
}

/// A byte count or a cardinality as a float, for report fields that are
/// uniformly `f64`.
#[must_use]
pub fn count_to_f64(n: i64) -> f64 {
  int_to_f64(n)
}

/// `usize` cardinality as a float.
#[must_use]
pub fn len_to_f64(n: usize) -> f64 {
  int_to_f64(i64::try_from(n).unwrap_or(i64::MAX))
}

/// Integer microseconds as a float, for arithmetic against the
/// floating-point fields of `ResourceTiming`.
#[must_use]
pub fn micros_to_f64(us: Micro) -> f64 {
  int_to_f64(us)
}

/// A microsecond quantity computed in floating point back to an
/// integer. Distinct from [`ms_to_micros`], which also rescales:
/// confusing the two multiplies every timing by a thousand.
#[must_use]
pub fn f64_to_micros(v: f64) -> Micro {
  f64_to_int(v)
}

/// A computed byte count back to an integer.
#[must_use]
pub fn f64_to_count(v: f64) -> i64 {
  f64_to_int(v)
}

/// The one integer-to-float conversion, bounds-checked.
///
/// `as f64` past 2^53 rounds to the nearest representable value, which
/// for our inputs cannot happen; clamping first makes that an assertion
/// rather than an assumption.
#[expect(
  clippy::cast_precision_loss,
  reason = "input is clamped to the exact-integer range immediately above, so no precision is lost"
)]
fn int_to_f64(n: i64) -> f64 {
  n.clamp(-F64_EXACT_INT_LIMIT, F64_EXACT_INT_LIMIT) as f64
}

/// The one float-to-integer conversion.
///
/// Saturating, and NaN becomes zero: a missing timing field
/// deserializes to `0.0` and arithmetic on it can produce NaN, which
/// must not turn into a garbage timestamp.
#[expect(
  clippy::cast_possible_truncation,
  reason = "saturating and NaN-guarded on the line above, which is the whole point of the function"
)]
fn f64_to_int(v: f64) -> Micro {
  if v.is_nan() {
    return 0;
  }
  let limit = int_to_f64(F64_EXACT_INT_LIMIT);
  v.clamp(-limit, limit) as Micro
}
