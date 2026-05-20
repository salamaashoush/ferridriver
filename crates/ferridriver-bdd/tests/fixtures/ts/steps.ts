// TypeScript step definitions: interfaces + typed params (stripped by
// rolldown/oxc), importing a typed helper from a sibling .ts module.
// Given/When/Then are ambient globals (installed by ferridriver-script);
// not type-checked, only transpiled.
//
// World is always the first argument — arrow or classic, same shape.

import { add } from "./math.ts";

interface Wallet {
  total: number;
}

Before((world: Wallet) => {
  world.total = 0;
});

Given("I start with {int}", (world: Wallet, n: number) => {
  world.total = n;
});

When("I add {int}", (world: Wallet, n: number) => {
  world.total = add(world.total, n);
});

Then("the total is {int}", (world: Wallet, n: number) => {
  if (world.total !== n) {
    throw new Error(`expected ${n}, got ${world.total}`);
  }
});
