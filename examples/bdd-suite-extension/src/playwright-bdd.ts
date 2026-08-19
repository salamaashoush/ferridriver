// The specifier this package claims.
//
// A suite written against playwright-bdd imports `createBdd` from here
// with no edit to its own source — the point of a package owning an
// import specifier. `createBdd(test)` binds the step registrars to a
// `test` object so a step body destructures that chain's fixtures, which
// is exactly what `bindSteps` does natively.

import { bindSteps } from 'ferridriver';

export function createBdd(test?: unknown) {
  return bindSteps(test);
}

export default { createBdd };
