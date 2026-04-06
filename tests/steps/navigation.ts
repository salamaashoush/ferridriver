// Navigation step definitions for TS BDD runner.
// These replicate the built-in Rust steps to test TS step registration works.

import { Given, When, Then, Step } from '../../packages/ferridriver-test/src/bdd.js';

// Note: Built-in Rust steps already cover these patterns.
// These TS definitions will OVERRIDE the Rust ones since they're registered later.
// In a real project, you'd only define custom steps here.

// For this test, we DON'T register navigation steps -- let the Rust built-ins handle them.
// We only register custom steps that don't exist in Rust.

Given('I am on a blank page', async (page) => {
  await page.goto('about:blank');
});
