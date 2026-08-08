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
//! Shared by the VM-side resolver (`bindings::test`) and the glue crate's
//! pool-request computation, so the two can never disagree about which
//! registration a name means.

/// One entry of a `test.extend` chain, in extend order. `reg` is the
/// caller's own registration index; this module only cares about
/// positions within the chain.
#[derive(Debug, Clone)]
pub struct FixtureSlot {
  pub reg: usize,
  pub name: String,
  pub deps: Vec<String>,
  pub auto: bool,
  pub worker_scoped: bool,
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

/// Positions to set up for `requested` (plus every auto fixture), in
/// dependency order — dependencies before dependents.
///
/// `is_builtin` answers whether the runtime provides a fixture of that
/// name without any registration; it is what distinguishes a legitimate
/// override of a built-in from Playwright's "references itself, but
/// does not have a base implementation".
///
/// # Errors
///
/// Dependency cycle, a self-reference with no base implementation, or a
/// worker-scoped fixture depending on a test-scoped one.
pub fn resolution_order(
  slots: &[FixtureSlot],
  requested: &[String],
  is_builtin: &dyn Fn(&str) -> bool,
) -> Result<Vec<usize>, String> {
  let mut queue: Vec<usize> = requested
    .iter()
    .filter_map(|name| resolve_dep(slots, name, None))
    .collect();
  // Auto fixtures: the topmost registration of each auto name, matching
  // Playwright's name-keyed `autoFixtures()`.
  queue.extend(
    slots
      .iter()
      .enumerate()
      .filter(|(pos, slot)| slot.auto && resolve_dep(slots, &slot.name, None) == Some(*pos))
      .map(|(pos, _)| pos),
  );

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
    match resolve_dep(slots, dep, Some(pos)) {
      Some(dep_pos) => {
        if slots[pos].worker_scoped && !slots[dep_pos].worker_scoped {
          return Err(format!(
            "worker fixture \"{}\" cannot depend on a test fixture \"{dep}\"",
            slots[pos].name
          ));
        }
        visit(dep_pos, slots, is_builtin, ordered, visiting)?;
      },
      None => {
        if *dep == slots[pos].name && !is_builtin(dep) {
          return Err(format!(
            "Fixture \"{dep}\" references itself, but does not have a base implementation."
          ));
        }
      },
    }
  }
  visiting.pop();
  ordered.push(pos);
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::{FixtureSlot, resolution_order, resolve_dep};

  fn slot(reg: usize, name: &str, deps: &[&str]) -> FixtureSlot {
    FixtureSlot {
      reg,
      name: name.to_string(),
      deps: deps.iter().map(|d| (*d).to_string()).collect(),
      auto: false,
      worker_scoped: false,
    }
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
  fn genuine_cycles_are_still_rejected() {
    let slots = vec![slot(0, "a", &["b"]), slot(1, "b", &["a"])];
    let err = resolution_order(&slots, &["a".to_string()], &|_| false).expect_err("cycle");
    assert!(err.contains("form a dependency cycle"), "{err}");
  }

  #[test]
  fn worker_fixture_cannot_depend_on_a_test_fixture() {
    let mut slots = vec![slot(0, "seed", &[]), slot(1, "pool", &["seed"])];
    slots[1].worker_scoped = true;
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
}
