// TypeScript step definitions: interfaces + typed params (stripped by
// rolldown/oxc), importing a typed helper from a sibling .ts module.
// Given/When/Then are ambient globals (installed by ferridriver-script);
// not type-checked, only transpiled.

import { add } from "./math.ts";

interface Wallet {
  total: number;
}

Before(function (this: Wallet) {
  this.total = 0;
});

Given("I start with {int}", function (this: Wallet, n: number) {
  this.total = n;
});

When("I add {int}", function (this: Wallet, n: number) {
  this.total = add(this.total, n);
});

Then("the total is {int}", function (this: Wallet, n: number) {
  if (this.total !== n) {
    throw new Error(`expected ${n}, got ${this.total}`);
  }
});
