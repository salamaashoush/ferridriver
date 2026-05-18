// cucumber-js-shaped step definitions, run by the shared ferridriver
// QuickJS engine (no Node). Given/When/Then/Before/defineParameterType
// are globals installed by ferridriver-script. `this` is the
// per-scenario World built by the Rust core; it also carries
// `page`/`context` (the same bindings as `ferridriver run`) when
// fixtures are present, though these pure-logic steps don't use them.

Before(function () {
  this.count = 0;
});

Given("I have {int} cukes in my belly", function (n) {
  this.count = n;
});

When("I eat {int} cukes", function (n) {
  this.count -= n;
});

Then("I have {int} cukes left", function (n) {
  if (this.count !== n) {
    throw new Error("expected " + n + " cukes, had " + this.count);
  }
});

Then("the data table sums to {int}", function (n, table) {
  const sum = table.hashes().reduce((acc, row) => acc + Number(row.amount), 0);
  if (sum !== n) {
    throw new Error("data table sum " + sum + " != " + n);
  }
});

Then("this step always fails", function () {
  throw new Error("boom from js step");
});
