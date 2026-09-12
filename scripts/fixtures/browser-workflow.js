if (await page.locator('body').count() !== 1) throw new Error('adopted tab has no usable main frame');
const version = browser.version();
if (!version || version === 'Unknown') throw new Error('browser version was not bound to the target');
if (context.browser()?.version() !== version) throw new Error('context belongs to a different browser');
await page.goto(args[0], {timeout:10000});
let listed = false;
for (const current of browser.contexts()) {
  for (const candidate of await current.pages()) {
    if (await candidate.url() === args[0]) listed = true;
  }
}
if (!listed) throw new Error('selected page is missing from browser.contexts()');
await page.getByLabel('Name').fill('sashoush');
await page.locator('#choice').selectOption('two');
await page.getByRole('button', {name:'Save'}).click();
const text = await page.locator('#result').textContent();
if (text !== 'Verified: sashoush / two') throw new Error(text);
const input = page.frameLocator('#frame').getByLabel('Inside');
await input.fill('frame verified');
if (await input.inputValue() !== 'frame verified') throw new Error('frame input failed');
await page.route('**/api/value', route => route.fulfill({json:{value:'intercepted'}}));
const network = await page.evaluate(async () => (await fetch('/api/value')).json());
if (network.value !== 'intercepted') throw new Error('network interception failed');
const handle = await page.evaluateHandle(() => ({value:42}));
const handleValue = await handle.evaluate(object => object.value);
if (handleValue !== 42) throw new Error('handle evaluation failed');
await handle.dispose();
const device = await page.evaluate(() => ({userAgent:navigator.userAgent,width:innerWidth,
  ratio:devicePixelRatio,touch:navigator.maxTouchPoints,trusted:window.lastTrusted}));
if (!device.trusted) throw new Error('click did not produce trusted input');
await page.screenshot({path:args[1]});
return {text,device,network,handleValue,version};
