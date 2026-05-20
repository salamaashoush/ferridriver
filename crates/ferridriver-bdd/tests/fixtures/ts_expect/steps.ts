// TypeScript BDD steps exercising the QuickJS `expect()` global —
// Jest-style value matchers, asymmetric matchers (`expect.any`,
// `expect.objectContaining`, ...), `toThrow`, and Playwright web-first
// matchers (`toBeVisible`, `toHaveText`, `toHaveTitle`).
//
// World is always the FIRST argument. Same shape for arrow and classic
// `function` bodies — no `this` magic to remember.

interface World {
  page: { goto: (url: string) => Promise<unknown>; locator: (sel: string) => unknown };
  doc: { id: number; name: string; tags: string[]; address: { city: string; zip: string } };
}

Given("a fresh page is loaded", async (world: World) => {
  await world.page.goto("data:text/html,<title>fixture</title><h1>Hello World</h1>");
  world.doc = {
    id: 42,
    name: "Ada",
    tags: ["admin", "user"],
    address: { city: "London", zip: "EC1A" },
  };
});

Then("the synthetic JSON matches the expected shape", (world: World) => {
  expect(world.doc).toEqual({
    id: 42,
    name: "Ada",
    tags: ["admin", "user"],
    address: { city: "London", zip: "EC1A" },
  });
});

Then("the synthetic JSON satisfies asymmetric matchers", (world: World) => {
  expect(world.doc).toEqual({
    id: expect.any(Number),
    name: expect.stringContaining("Ad"),
    tags: expect.arrayContaining(["admin"]),
    address: expect.objectContaining({ city: "London" }),
  });
});

Then("toThrow captures a throwing closure", async () => {
  await expect(() => {
    throw new Error("boom: out of range");
  }).toThrow("out of range");
  await expect(() => 42).not.toThrow();
});

Given("the page is navigated to a fixture", async (world: World) => {
  await world.page.goto("data:text/html,<title>fixture</title><h1>Hello World</h1>");
});

Then("the heading element is visible", async (world: World) => {
  await expect(world.page.locator("h1")).toBeVisible();
});

Then("the heading has the expected text", async (world: World) => {
  await expect(world.page.locator("h1")).toHaveText("Hello World");
});

Then("the page has the expected title", async (world: World) => {
  await expect(world.page).toHaveTitle("fixture");
});

// Classic `function` body proving World binds as `this` in addition to
// arg[0]. The first parameter is identical to the arrow form above —
// no per-body branching at the runtime layer.
Given("the page is set up via a classic function step", async function (world: World) {
  await world.page.goto("data:text/html,<title>fixture</title><h1>Hello World</h1>");
});
