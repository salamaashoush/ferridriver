// Custom TypeScript step for the `custom_ts_steps.feature` smoke test.
// Verifies that user-supplied TS steps load via the rolldown bundle and
// coexist with the built-in Rust step registry.

interface World {
  page: { goto: (url: string) => Promise<unknown> };
}

Given("I am on a blank page", async (world: World) => {
  await world.page.goto("about:blank");
});
