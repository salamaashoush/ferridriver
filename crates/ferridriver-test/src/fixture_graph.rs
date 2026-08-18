//! Custom-fixture dependency resolution, mirroring Playwright's
//! `FixturePool` (`packages/playwright/src/common/fixtures.ts`).
//!
//! The one rule everything else follows from: a dependency named the
//! same as the fixture declaring it resolves to that fixture's SUPER —
//! the previous registration of the name in the `test.extend` chain —
//! never to itself. That is what makes
//!
//! ```js
//! base.extend({ page: async ({ page }, use) => { …; await use(page); } })
//! ```
//!
//! a shadowing override rather than a cycle. When no earlier
//! registration exists the dependency falls through to the runtime's
//! built-in of that name.
//!
//! Core-owned, host-neutral: the QuickJS binding resolves a spec's
//! fixtures through it, the BDD host picks a scenario's chain with
//! [`dominant_fixture_set`], and the runner's own pool-request
//! computation reads the same order, so no two of them can disagree
//! about which registration a name means.

use crate::fixture::FixtureScope;

/// One fixture registration, in the order it was registered. `reg` is
/// the caller's own registration id (an index into the Rust pool's
/// definitions, or into the JS registry); this module only cares about
/// positions within the slice it is given.
///
/// A Rust `#[fixture]`, a `test.extend` entry and a BDD chain
/// registration all lower to this — which is what lets one set of rules
/// serve all three.
#[derive(Debug, Clone)]
pub struct FixtureSlot {
  pub reg: usize,
  pub name: String,
  pub deps: Vec<String>,
  pub auto: bool,
  pub scope: FixtureScope,
  /// Registered as `[value, { option: true }]` — the only kind of
  /// fixture a `use` block may set a value for. Inherited from the
  /// registration being overridden, so the topmost slot of a name
  /// already carries the chain's answer.
  pub option: bool,
}

/// Playwright's `FixturePool.resolve(name, forFixture)`. `from` is the
/// position of the fixture whose dependency list `dep` came from;
/// `None` for a test/hook body, which always sees the topmost
/// registration.
#[must_use]
pub fn resolve_dep(slots: &[FixtureSlot], dep: &str, from: Option<usize>) -> Option<usize> {
  if let Some(pos) = from
    && slots.get(pos).is_some_and(|s| s.name == dep)
  {
    return slots[..pos].iter().rposition(|s| s.name == dep);
  }
  slots.iter().rposition(|s| s.name == dep)
}

/// Positions of every `auto` fixture at or below `scope`, topmost
/// registration of each name (Playwright's name-keyed `autoFixtures()`).
///
/// A worker entering its scope resolves the worker-scoped autos; a test
/// resolves those plus its own — hence "at or below".
#[must_use]
pub fn auto_slots(slots: &[FixtureSlot], scope: FixtureScope) -> Vec<usize> {
  slots
    .iter()
    .enumerate()
    .filter(|(pos, slot)| {
      slot.auto && slot.scope.rank() <= scope.rank() && resolve_dep(slots, &slot.name, None) == Some(*pos)
    })
    .map(|(pos, _)| pos)
    .collect()
}

/// Positions to set up for a test: everything [`dependency_order`]
/// needs for `requested`, plus every auto fixture of the chain.
///
/// # Errors
///
/// See [`dependency_order`].
pub fn resolution_order(
  slots: &[FixtureSlot],
  requested: &[String],
  is_builtin: &dyn Fn(&str) -> bool,
) -> Result<Vec<usize>, String> {
  order_from(slots, requested, &auto_slots(slots, FixtureScope::Test), is_builtin)
}

/// Positions to set up for `requested` alone, in dependency order —
/// dependencies before dependents, with no auto fixtures pulled in.
///
/// This is the lazy single-request form the runner's pool resolves
/// through; a test entering its scope wants [`resolution_order`].
///
/// `is_builtin` answers whether the runtime provides a fixture of that
/// name without any registration; it is what distinguishes a legitimate
/// override of a built-in from Playwright's "references itself, but
/// does not have a base implementation".
///
/// # Errors
///
/// Dependency cycle, a self-reference with no base implementation, or a
/// fixture depending on one that does not outlive it (a worker fixture
/// on a test fixture).
pub fn dependency_order(
  slots: &[FixtureSlot],
  requested: &[String],
  is_builtin: &dyn Fn(&str) -> bool,
) -> Result<Vec<usize>, String> {
  order_from(slots, requested, &[], is_builtin)
}

fn order_from(
  slots: &[FixtureSlot],
  requested: &[String],
  seeds: &[usize],
  is_builtin: &dyn Fn(&str) -> bool,
) -> Result<Vec<usize>, String> {
  let mut queue: Vec<usize> = requested
    .iter()
    .filter_map(|name| resolve_dep(slots, name, None))
    .collect();
  queue.extend_from_slice(seeds);

  let mut needed: Vec<usize> = Vec::new();
  while let Some(pos) = queue.pop() {
    if needed.contains(&pos) {
      continue;
    }
    needed.push(pos);
    for dep in &slots[pos].deps {
      if let Some(dep_pos) = resolve_dep(slots, dep, Some(pos)) {
        queue.push(dep_pos);
      }
    }
  }
  // Registration order, so the setup sequence does not depend on the
  // order names happened to come off the queue.
  needed.sort_unstable();

  let mut ordered: Vec<usize> = Vec::with_capacity(needed.len());
  let mut visiting: Vec<usize> = Vec::new();
  for &pos in &needed {
    visit(pos, slots, is_builtin, &mut ordered, &mut visiting)?;
  }
  Ok(ordered)
}

/// A dependency name that resolves to no registration: either the
/// runtime provides it (a built-in, an injected value), or it is one of
/// Playwright's two dependency errors, verbatim
/// (`common/fixtures.ts:196-200`) — a self-reference with nothing under
/// it, or an unknown parameter.
fn unresolved_dep(slot: &FixtureSlot, dep: &str, is_builtin: &dyn Fn(&str) -> bool) -> Result<(), String> {
  if is_builtin(dep) {
    return Ok(());
  }
  if dep == slot.name {
    return Err(format!(
      "Fixture \"{dep}\" references itself, but does not have a base implementation."
    ));
  }
  Err(format!("Fixture \"{}\" has unknown parameter \"{dep}\".", slot.name))
}

fn visit(
  pos: usize,
  slots: &[FixtureSlot],
  is_builtin: &dyn Fn(&str) -> bool,
  ordered: &mut Vec<usize>,
  visiting: &mut Vec<usize>,
) -> Result<(), String> {
  if ordered.contains(&pos) {
    return Ok(());
  }
  if let Some(start) = visiting.iter().position(|&p| p == pos) {
    let chain: Vec<String> = visiting[start..]
      .iter()
      .map(|&p| format!("\"{}\"", slots[p].name))
      .collect();
    return Err(format!(
      "Fixtures {} -> \"{}\" form a dependency cycle",
      chain.join(" -> "),
      slots[pos].name
    ));
  }
  visiting.push(pos);
  for dep in &slots[pos].deps {
    let Some(dep_pos) = resolve_dep(slots, dep, Some(pos)) else {
      unresolved_dep(&slots[pos], dep, is_builtin)?;
      continue;
    };
    // A fixture may only depend on one that outlives it: a worker
    // fixture reused across tests cannot hold a value torn down after
    // each one.
    if slots[pos].scope.rank() > slots[dep_pos].scope.rank() {
      return Err(format!(
        "{} fixture \"{}\" cannot depend on a {} fixture \"{dep}\"",
        slots[pos].scope.label(),
        slots[pos].name,
        slots[dep_pos].scope.label()
      ));
    }
    visit(dep_pos, slots, is_builtin, ordered, visiting)?;
  }
  visiting.pop();
  ordered.push(pos);
  Ok(())
}

/// The one fixture set that covers `wanted`, or the reason none does.
///
/// A `test.extend` chain's set is its parent's set plus the new
/// registrations, and `mergeTests` unions the sets of its arguments, so
/// "one chain covers them all" is exactly "one set contains every other
/// set's registrations". Steps bound to unrelated chains have no such
/// set: nothing could give a scenario both bags at once, and silently
/// picking one would leave the other chain's fixtures undefined.
///
/// The empty request answers the base set (0), which is the chain the
/// ambient `Given`/`When`/`Then` and every unbound hook resolve from.
///
/// # Errors
///
/// The wanted sets are not totally ordered by containment.
pub fn dominant_fixture_set(sets: &[Vec<usize>], wanted: &[usize]) -> Result<usize, String> {
  let mut uniq: Vec<usize> = Vec::new();
  for &s in wanted {
    if !uniq.contains(&s) {
      uniq.push(s);
    }
  }
  let Some(&first) = uniq.first() else { return Ok(0) };
  if uniq.len() == 1 {
    return Ok(first);
  }
  let slots = |set: usize| sets.get(set).map(Vec::as_slice).unwrap_or_default();
  let widest = uniq.iter().copied().max_by_key(|&s| slots(s).len()).unwrap_or(first);
  for &other in &uniq {
    if !slots(other).iter().all(|reg| slots(widest).contains(reg)) {
      return Err(format!(
        "this scenario's steps are bound to unrelated `test` objects (fixture sets {other} and {widest}), \
         so no single fixture chain resolves them all.\n\
         Build one object with mergeTests(...) and pass it to bindSteps()."
      ));
    }
  }
  Ok(widest)
}

/// Fixtures the runtime provides without any registration and that
/// Playwright does NOT declare `{ option: true }` (`page`, `context`,
/// `request` and `browser` come out of the worker's pool). Naming one
/// in a `use` block is the error below, not an override.
pub const BUILTIN_NON_OPTION_FIXTURES: &[&str] = &["page", "context", "request", "browser"];

/// Every fixture name a host provides without a registration.
///
/// Playwright keeps its built-ins in the base pool's `_registrations`,
/// so `validateFunction` accepts them like any other name. ferridriver's
/// are properties the host puts on the fixtures object, and a given
/// world legitimately carries only some of them — a `beforeAll` world
/// has no `page` — so the set a requested name is CHECKED against has to
/// be stated rather than read off whichever world is in hand.
/// [`BUILTIN_NON_OPTION_FIXTURES`] is the subset a `use` block may not
/// override.
pub const BUILTIN_FIXTURES: &[&str] = &[
  "baseURL",
  "browser",
  "browserName",
  "context",
  "hasTouch",
  "headless",
  "isMobile",
  "page",
  "request",
  "testInfo",
];

/// Playwright's `FixturePool.validateFunction`
/// (`common/fixtures.ts:250-256`), run per test, hook and modifier from
/// `common/poolBuilder.ts:66-71`: a first-parameter name that resolves
/// to no registration and no built-in is an error, not an `undefined`
/// the body then compares against and fails on somewhere else.
///
/// `prefix` is what the message names the function: `"Test"`,
/// `"beforeEach hook"`, `"skip modifier"` — Playwright's own wording.
///
/// Scope is deliberately not consulted. Playwright's scope rule is
/// fixture-to-fixture ([`dependency_order`] carries it); a function
/// asking for a name of the wrong scope is a different failure from a
/// function asking for a name nobody registered.
///
/// # Errors
///
/// The verbatim load error for the first parameter naming nothing.
pub fn validate_requested(
  slots: &[FixtureSlot],
  requested: &[String],
  is_builtin: &dyn Fn(&str) -> bool,
  prefix: &str,
) -> Result<(), String> {
  for name in requested {
    if resolve_dep(slots, name, None).is_none() && !is_builtin(name) {
      return Err(format!("{prefix} has unknown parameter \"{name}\"."));
    }
  }
  Ok(())
}

/// `use` keys the runner itself consumes that are not context options:
/// `viewport` is applied when the context is created and `baseURL`
/// feeds both the `baseURL` fixture and the HTTP client.
pub const RUNTIME_USE_KEYS: &[&str] = &["viewport", "baseURL"];

/// What one key of a `use` block means once the fixture chain is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseKeyVerdict {
  /// Sets the value of an `{ option: true }` fixture.
  Option,
  /// Names a fixture that exists but was not declared an option —
  /// Playwright's load error, [`use_override_error`].
  NotAnOption,
  /// Names nothing the run knows about. Playwright ignores these
  /// silently; ferridriver reports them, because before `use` became
  /// open the config layer warned about every one of them.
  Unrecognized,
}

/// Playwright's `FixturePool` constructor rule for a config `use` key
/// (`packages/playwright/src/common/fixtures.ts:105-111`), against the
/// topmost registration of that name in one `test.extend` chain.
#[must_use]
pub fn classify_use_key(key: &str, slots: &[FixtureSlot]) -> UseKeyVerdict {
  if let Some(pos) = resolve_dep(slots, key, None) {
    return if slots[pos].option {
      UseKeyVerdict::Option
    } else {
      UseKeyVerdict::NotAnOption
    };
  }
  if BUILTIN_NON_OPTION_FIXTURES.contains(&key) {
    return UseKeyVerdict::NotAnOption;
  }
  UseKeyVerdict::Unrecognized
}

/// The verdict for a key across every chain a run collected: an option
/// anywhere makes it an option, otherwise the strictest verdict wins.
#[must_use]
pub fn classify_use_key_across(key: &str, chains: &[Vec<FixtureSlot>]) -> UseKeyVerdict {
  let mut verdict = UseKeyVerdict::Unrecognized;
  for slots in chains {
    match classify_use_key(key, slots) {
      UseKeyVerdict::Option => return UseKeyVerdict::Option,
      UseKeyVerdict::NotAnOption => verdict = UseKeyVerdict::NotAnOption,
      UseKeyVerdict::Unrecognized => {},
    }
  }
  if chains.is_empty() && BUILTIN_NON_OPTION_FIXTURES.contains(&key) {
    return UseKeyVerdict::NotAnOption;
  }
  verdict
}

/// Decide what every open `use` key means now that the fixture chains
/// are known, which is the earliest a host can: Playwright runs the
/// same check in the `FixturePool` constructor, after collection.
///
/// A key naming a non-option fixture is a load error. A key naming
/// nothing is reported and ignored — Playwright ignores it silently,
/// but before `use` accepted open keys ferridriver's config layer
/// warned about each one, and losing that signal would make a typo
/// invisible.
///
/// # Errors
///
/// [`use_override_error`] for the first key that names a fixture which
/// is not an option.
pub fn validate_use_keys<'a>(
  keys: impl IntoIterator<Item = &'a str>,
  chains: &[Vec<FixtureSlot>],
) -> Result<(), String> {
  for key in keys {
    if RUNTIME_USE_KEYS.contains(&key) {
      continue;
    }
    match classify_use_key_across(key, chains) {
      UseKeyVerdict::Option => {},
      UseKeyVerdict::NotAnOption => return Err(use_override_error(key)),
      UseKeyVerdict::Unrecognized => tracing::warn!(
        target: "ferridriver::test",
        key,
        "use.unknownKey: no fixture registered with {{ option: true }} claims this `use` key; it is ignored"
      ),
    }
  }
  Ok(())
}

/// Playwright's message, verbatim (`fixtures.ts:109`).
#[must_use]
pub fn use_override_error(key: &str) -> String {
  format!(
    "Fixture \"{key}\" cannot be overridden in the configuration \"use\" section. \
     Only fixtures registered with {{ option: true }} can be set in the config."
  )
}

#[cfg(test)]
mod tests {
  use super::{
    BUILTIN_FIXTURES, BUILTIN_NON_OPTION_FIXTURES, FixtureScope, FixtureSlot, UseKeyVerdict, classify_use_key,
    classify_use_key_across, dependency_order, dominant_fixture_set, resolution_order, resolve_dep, validate_requested,
  };

  fn slot(reg: usize, name: &str, deps: &[&str]) -> FixtureSlot {
    FixtureSlot {
      reg,
      name: name.to_string(),
      deps: deps.iter().map(|d| (*d).to_string()).collect(),
      auto: false,
      scope: FixtureScope::Test,
      option: false,
    }
  }

  fn option_slot(reg: usize, name: &str) -> FixtureSlot {
    FixtureSlot {
      option: true,
      ..slot(reg, name, &[])
    }
  }

  #[test]
  fn a_use_key_is_an_override_only_for_an_option_fixture() {
    let slots = vec![option_slot(0, "profile"), slot(1, "helper", &[])];
    assert_eq!(classify_use_key("profile", &slots), UseKeyVerdict::Option);
    assert_eq!(classify_use_key("helper", &slots), UseKeyVerdict::NotAnOption);
    assert_eq!(classify_use_key("nope", &slots), UseKeyVerdict::Unrecognized);
    // A built-in the runtime provides is a registration too, and none
    // of them is an option.
    assert_eq!(classify_use_key("page", &slots), UseKeyVerdict::NotAnOption);
  }

  #[test]
  fn an_override_of_an_option_reads_as_an_option() {
    // base.extend({ profile: ['a', {option:true}] }).extend({ profile: 'b' })
    // — the second registration inherits `option` at registration time,
    // which is what the topmost slot carries.
    let slots = vec![option_slot(0, "profile"), option_slot(1, "profile")];
    assert_eq!(classify_use_key("profile", &slots), UseKeyVerdict::Option);
  }

  #[test]
  fn one_chain_declaring_the_option_settles_the_key() {
    let with_option = vec![option_slot(0, "profile")];
    let without = vec![slot(0, "helper", &[])];
    assert_eq!(
      classify_use_key_across("profile", &[without.clone(), with_option]),
      UseKeyVerdict::Option
    );
    assert_eq!(
      classify_use_key_across("profile", &[without]),
      UseKeyVerdict::Unrecognized
    );
    assert_eq!(classify_use_key_across("page", &[]), UseKeyVerdict::NotAnOption);
  }

  #[test]
  fn same_name_dependency_resolves_to_the_super() {
    // base.extend({ page }).extend({ page })
    let slots = vec![slot(0, "page", &["page"]), slot(1, "page", &["page"])];
    assert_eq!(resolve_dep(&slots, "page", Some(1)), Some(0));
    // The first override's `page` falls through to the built-in.
    assert_eq!(resolve_dep(&slots, "page", Some(0)), None);
    // A test body always sees the topmost.
    assert_eq!(resolve_dep(&slots, "page", None), Some(1));

    let order = resolution_order(&slots, &["page".to_string()], &|n| n == "page").expect("resolves");
    assert_eq!(order, vec![0, 1], "super runs before the override");
  }

  #[test]
  fn self_reference_without_a_base_is_named_as_such() {
    let slots = vec![slot(0, "todoPage", &["todoPage"])];
    let err = resolution_order(&slots, &["todoPage".to_string()], &|_| false).expect_err("no base");
    assert_eq!(
      err,
      "Fixture \"todoPage\" references itself, but does not have a base implementation."
    );
  }

  #[test]
  fn an_unregistered_dependency_is_named_as_an_unknown_parameter() {
    let slots = vec![slot(0, "todoPage", &["db"])];
    let err = dependency_order(&slots, &["todoPage".to_string()], &|_| false).expect_err("unknown dep");
    assert_eq!(err, "Fixture \"todoPage\" has unknown parameter \"db\".");
    // …unless the runtime provides it without a registration.
    assert!(dependency_order(&slots, &["todoPage".to_string()], &|n| n == "db").is_ok());
  }

  #[test]
  fn genuine_cycles_are_still_rejected() {
    let slots = vec![slot(0, "a", &["b"]), slot(1, "b", &["a"])];
    let err = resolution_order(&slots, &["a".to_string()], &|_| false).expect_err("cycle");
    assert!(err.contains("form a dependency cycle"), "{err}");
  }

  #[test]
  fn worker_fixture_cannot_depend_on_a_test_fixture() {
    let mut slots = vec![slot(0, "seed", &[]), slot(1, "pool", &["seed"])];
    slots[1].scope = FixtureScope::Worker;
    let err = resolution_order(&slots, &["pool".to_string()], &|_| false).expect_err("scope order");
    assert_eq!(err, "worker fixture \"pool\" cannot depend on a test fixture \"seed\"");
  }

  #[test]
  fn auto_fixtures_seed_only_their_topmost_registration() {
    let mut slots = vec![slot(0, "seeded", &[]), slot(1, "seeded", &["seeded"])];
    slots[0].auto = true;
    slots[1].auto = true;
    let order = resolution_order(&slots, &[], &|_| false).expect("resolves");
    assert_eq!(order, vec![0, 1]);
  }

  #[test]
  fn three_deep_override_chain_runs_bottom_up() {
    let slots = vec![
      slot(0, "page", &["page"]),
      slot(1, "page", &["page"]),
      slot(2, "page", &["page"]),
    ];
    let order = resolution_order(&slots, &["page".to_string()], &|n| n == "page").expect("resolves");
    assert_eq!(order, vec![0, 1, 2]);
  }

  #[test]
  fn the_base_set_answers_an_empty_request() {
    let sets = vec![Vec::new(), vec![0, 1]];
    assert_eq!(dominant_fixture_set(&sets, &[]), Ok(0));
  }

  #[test]
  fn an_extend_chain_covers_the_base_it_grew_from() {
    // base, base.extend({a}), that.extend({b})
    let sets = vec![Vec::new(), vec![0], vec![0, 1]];
    assert_eq!(dominant_fixture_set(&sets, &[0, 2]), Ok(2));
    assert_eq!(dominant_fixture_set(&sets, &[2, 1, 0]), Ok(2));
  }

  #[test]
  fn unrelated_chains_are_refused_by_name() {
    // base.extend({a}) and base.extend({b}) — neither covers the other.
    let sets = vec![Vec::new(), vec![0], vec![1]];
    let err = dominant_fixture_set(&sets, &[1, 2]).expect_err("unrelated");
    assert!(err.contains("unrelated `test` objects"), "{err}");
    assert!(err.contains("mergeTests"), "{err}");
  }

  #[test]
  fn a_merged_chain_covers_both_arguments() {
    let sets = vec![Vec::new(), vec![0], vec![1], vec![0, 1]];
    assert_eq!(dominant_fixture_set(&sets, &[1, 2, 3]), Ok(3));
  }

  #[test]
  fn an_unregistered_parameter_name_is_playwrights_load_error() {
    let slots = [slot(0, "todoPage", &[])];
    let err = validate_requested(&slots, &["deployment".to_string()], &|_| false, "Test")
      .expect_err("nothing registers `deployment`");
    assert_eq!(err, "Test has unknown parameter \"deployment\".");
  }

  #[test]
  fn the_prefix_names_the_function_the_way_playwright_does() {
    let err = validate_requested(&[], &["nope".to_string()], &|_| false, "beforeEach hook").expect_err("unknown");
    assert_eq!(err, "beforeEach hook has unknown parameter \"nope\".");
  }

  #[test]
  fn a_registered_or_builtin_name_passes() {
    let slots = [slot(0, "todoPage", &[])];
    let is_builtin = |n: &str| BUILTIN_FIXTURES.contains(&n);
    validate_requested(&slots, &["todoPage".to_string()], &is_builtin, "Test").expect("registered");
    // `page` is a built-in even though no world in hand carries it —
    // a `beforeAll` world has none, and that is a scope question, not
    // an unknown-name one.
    validate_requested(&slots, &["page".to_string()], &is_builtin, "beforeAll hook").expect("built-in");
  }

  #[test]
  fn a_shadowed_registration_still_counts_as_registered() {
    let slots = [slot(0, "page", &[]), slot(1, "page", &["page"])];
    validate_requested(&slots, &["page".to_string()], &|_| false, "Test").expect("override chain resolves");
  }

  #[test]
  fn every_non_option_builtin_is_a_builtin() {
    for name in BUILTIN_NON_OPTION_FIXTURES {
      assert!(
        BUILTIN_FIXTURES.contains(name),
        "`{name}` may not be overridden in `use`, so it must also be a name a test may request"
      );
    }
  }
}
