// Step-scoped hooks: BeforeStep / AfterStep must fire around every
// EXECUTED step (cucumber-js semantics, mirroring the Rust executor:
// skipped steps after a failure get no step hooks; AfterStep always
// runs for an executed step, even a failing one, and sees its status).
// Counters live on globalThis so the assertion step can read them
// after the scenario (the World resets per scenario).

BeforeStep((world, info) => {
  globalThis.__beforeStep = (globalThis.__beforeStep || 0) + 1;
});

AfterStep((world, info) => {
  globalThis.__afterStep = (globalThis.__afterStep || 0) + 1;
  if (info && info.result && info.result.status === "FAILED") {
    globalThis.__afterStepSawFailure = true;
  }
});

Given("a counted step", () => {});

Given("a failing counted step", () => {
  throw new Error("deliberate step failure");
});

Given("hook counters are {int} and {int} with failure seen {word}", (_world, before, after, sawFailure) => {
  const b = globalThis.__beforeStep || 0;
  const a = globalThis.__afterStep || 0;
  const f = String(globalThis.__afterStepSawFailure === true);
  if (b !== before || a !== after || f !== sawFailure) {
    throw new Error(`hook counters before=${b} after=${a} sawFailure=${f}, expected ${before}/${after}/${sawFailure}`);
  }
});
