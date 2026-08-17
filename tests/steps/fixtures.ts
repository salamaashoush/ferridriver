// Steps for `fixtures.feature`: the fixture graph on the BDD world.
//
// The chain is built with `test.extend` exactly as a spec would build
// it, and `bindSteps` binds the registrars to it, so a step body
// destructures the fixtures it needs from its first parameter — beside
// `page` and the rest of the scenario's world.

import { mergeTests, test } from "@ferridriver/test";

// Module-level counters: the VM is one per worker, so they observe how
// often each scope actually set up across the scenarios of a run.
let autoSetups = 0;
let workerSetups = 0;
let testSetups = 0;
const teardowns: string[] = [];

const bdd = test.extend<{
  greeting: string;
  autoMark: number;
  outer: string;
  inner: string;
}>({
  greeting: async ({}, use) => {
    testSetups += 1;
    await use("hello from a fixture");
  },
  autoMark: [
    async ({}, use) => {
      autoSetups += 1;
      await use(autoSetups);
    },
    { auto: true },
  ],
  outer: async ({}, use) => {
    await use("outer");
    teardowns.push("outer");
  },
  inner: async ({ outer }, use) => {
    await use(`${outer}/inner`);
    teardowns.push("inner");
  },
});

const worker = test.extend<{ workerToken: string }>({
  workerToken: [
    async ({}, use) => {
      workerSetups += 1;
      await use(`w${workerSetups}`);
    },
    { scope: "worker" },
  ],
});

const { Given, When, Then } = bindSteps(mergeTests(bdd, worker));

Given("a fixture-backed scenario", async function ({ page }) {
  await page.goto("about:blank");
});

Then("the greeting fixture reads {string}", async function ({ greeting }, expected: string) {
  if (greeting !== expected) {
    throw new Error(`greeting fixture is ${JSON.stringify(greeting)}, expected ${JSON.stringify(expected)}`);
  }
});

Then("the auto fixture has run {int} time(s)", async function ({ autoMark }, times: number) {
  if (autoMark !== times) {
    throw new Error(`auto fixture ran ${autoMark} time(s), expected ${times}`);
  }
});

Then("no step named the auto fixture and it still ran", async function () {
  if (autoSetups < 1) {
    throw new Error("auto fixture never ran");
  }
});

When("I use the nested fixtures", async function ({ inner }) {
  this.seenInner = inner;
});

Then("the nested fixture value is {string}", async function (world, expected: string) {
  if (world.seenInner !== expected) {
    throw new Error(`nested fixture is ${JSON.stringify(world.seenInner)}, expected ${JSON.stringify(expected)}`);
  }
});

Then("the previous scenario tore its fixtures down in LIFO order", async function () {
  const order = teardowns.join(",");
  if (order !== "inner,outer") {
    throw new Error(`teardown order was ${JSON.stringify(order)}, expected "inner,outer"`);
  }
});

Then("the worker fixture reads {string}", async function ({ workerToken }, expected: string) {
  if (workerToken !== expected) {
    throw new Error(`worker fixture is ${JSON.stringify(workerToken)}, expected ${JSON.stringify(expected)}`);
  }
});

Then("the worker fixture was set up {int} time(s)", async function (_world, times: number) {
  if (workerSetups !== times) {
    throw new Error(`worker fixture set up ${workerSetups} time(s), expected ${times}`);
  }
});

Then("the test fixture was set up {int} time(s)", async function (_world, times: number) {
  if (testSetups !== times) {
    throw new Error(`test-scoped fixture set up ${testSetups} time(s), expected ${times}`);
  }
});

Then("the world carries the browser bindings", async function (world) {
  for (const key of ["page", "context", "request", "browser"]) {
    if (world[key] === undefined) {
      throw new Error(`world is missing ${key}`);
    }
  }
  if (world !== this) {
    throw new Error("arg0 and `this` are not the same object");
  }
});
