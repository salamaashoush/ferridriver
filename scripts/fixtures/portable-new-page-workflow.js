await page.goto(args[0]);
const originalTitle = await page.title();
const initialCount = (await context.pages()).length;
for (let index = 0; index < 2; index++) {
  const created = await context.newPage();
  try {
    const initial = await created.evaluate(() => ({url: location.href, name: window.name, opener: window.opener === null}));
    if (initial.url !== 'about:blank' || initial.name !== '' || !initial.opener) {
      throw new Error('New page did not start as an unnamed blank tab without an opener: ' + JSON.stringify(initial));
    }
    if ((await context.pages()).length !== initialCount + 1) throw new Error('New page was not registered exactly once');
    await created.goto(args[0]);
    await created.getByRole('textbox', {name: 'Name', exact: true}).fill('sashoush');
    await created.getByRole('button', {name: 'Save', exact: true}).click();
    const result = await created.evaluate(() => ({result: document.querySelector('#result').textContent, click: window.lastClick}));
    if (result.result !== 'sashoush' || !result.click?.trusted) throw new Error('New tab did not receive trusted input');
    await created.screenshot({path: args[1]});
  } finally {
    await created.close();
  }
  await page.bringToFront();
  if (await page.title() !== originalTitle) throw new Error('Creating a tab changed the original title');
  const markers = await page.evaluate(() => Object.keys(window).filter(key => key.startsWith('__fd_window_')));
  if (markers.length) throw new Error('Creating a tab left window markers: ' + JSON.stringify(markers));
  if ((await context.pages()).length !== initialCount) throw new Error('Closing a tab left a registered page');
}
return {version: browser.version(), created: 2, trustedClicks: 2, remainingPages: initialCount};
