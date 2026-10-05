await page.goto(args[0]);
await page.getByLabel('Name').fill('sashoush');
await page.keyboard.press('Backspace');
await page.keyboard.press('h');
if (await page.getByLabel('Name').inputValue() !== 'sashoush') throw new Error('Native keyboard input did not edit the focused field');
await page.getByRole('button', {name: 'Save'}).click({position: {x: 20, y: 20}});
const result = await page.locator('#result').textContent();
if (result !== 'sashoush') throw new Error(`Form returned ${result}`);
const click = await page.evaluate(() => window.lastClick);
if (!click?.trusted || Math.abs(click.x - 20) > 1 || Math.abs(click.y - 20) > 1) {
  throw new Error(`Positioned native click failed: ${JSON.stringify(click)}`);
}
if (await page.getByRole('button', {name: 'Save'}).getAttribute('aria-label') != null) {
  throw new Error('Pointer lookup left an accessibility label behind');
}
await page.evaluate(() => document.querySelector('button').setAttribute('aria-label', 'Save'));
await page.getByRole('button', {name: 'Save'}).click({position: {x: 20, y: 20}});
if (await page.getByRole('button', {name: 'Save'}).getAttribute('aria-label') !== 'Save') {
  throw new Error('Pointer lookup changed the original accessibility label');
}
const promised = await page.evaluate(async () => ({answer: await Promise.resolve(42)}));
if (promised.answer !== 42) throw new Error('Promise result was not awaited');
let rejected = false;
try { await page.evaluate(() => Promise.reject(new Error('probe rejection'))); }
catch (error) { rejected = String(error).includes('probe rejection'); }
if (!rejected) throw new Error('Promise rejection was lost');
const handle = await page.evaluateHandle(async () => ({date: new Date(0), answer: 42}));
try {
  const value = await handle.jsonValue();
  if (value.date.toISOString() !== '1970-01-01T00:00:00.000Z' || value.answer !== 42) {
    throw new Error('Rich handle result was corrupted');
  }
} finally { await handle.dispose(); }
await page.screenshot({path: args[1]});
return {version: browser.version(), result, click, promised, rejected};
