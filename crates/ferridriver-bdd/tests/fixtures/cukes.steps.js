// cucumber-shaped step definitions, run by the shared ferridriver
// QuickJS engine (no Node). Given/When/Then/Before/defineParameterType
// are globals installed by ferridriver-script. World is always the
// FIRST argument (cucumber params follow); also exposed as `this` for
// any body that prefers `function (world, ...) { this.x = ... }`.

Before((world) => {
  world.count = 0;
});

Given("I have {int} cukes in my belly", (world, n) => {
  world.count = n;
});

When("I eat {int} cukes", (world, n) => {
  world.count -= n;
});

Then("I have {int} cukes left", (world, n) => {
  if (world.count !== n) {
    throw new Error("expected " + n + " cukes, had " + world.count);
  }
});

Then("the data table sums to {int}", (_world, n, table) => {
  const sum = table.hashes().reduce((acc, row) => acc + Number(row.amount), 0);
  if (sum !== n) {
    throw new Error("data table sum " + sum + " != " + n);
  }
});

Then("this step always fails", () => {
  throw new Error("boom from js step");
});
