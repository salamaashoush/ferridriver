defineParameterType({ name: 'color', regexp: /red|green|blue/, transformer: (s) => s.toUpperCase() });
defineParameterType({ name: 'coins', regexp: /\d+/ });

Given('a blank page', async (world) => {
  await world.page.goto('about:blank');
});

Then('the page is still blank', async (world) => {
  const url = world.page.url();
  if (url !== 'about:blank') throw new Error(`expected about:blank, got ${url}`);
});

When('I pick {color} paint', (world, color) => {
  world.order = color;
});

When('I pay {coins} coins', (world, coins) => {
  world.paid = coins;
});

When('I order {int} cans of {color} paint', (world, cans, color) => {
  world.cans = cans;
  world.order = color;
});

Then('the paint order is {string}', (world, expected) => {
  if (world.order !== expected) throw new Error(`paint order was ${JSON.stringify(world.order)}`);
});

Then('the payment is {string}', (world, expected) => {
  if (world.paid !== expected) throw new Error(`payment was ${JSON.stringify(world.paid)}`);
});

When('I trigger the ambiguous step', () => {});
When('I trigger the {word} step', () => {});

Then('the can count is {int}', (world, expected) => {
  if (world.cans !== expected) throw new Error(`can count was ${JSON.stringify(world.cans)}`);
});
