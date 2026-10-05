await page.goto(args[0] + 'tabs/back');
await page.evaluate(url => {
  const link = document.createElement('a');
  link.href = url;
  link.target = '_blank';
  link.textContent = 'Open probe tab';
  document.body.prepend(link);
}, args[0] + 'tabs/front');
await page.getByRole('link', {name: 'Open probe tab', exact: true}).click();
let tabs = [];
const deadline = Date.now() + 5000;
while (Date.now() < deadline) {
  tabs = [];
  for (const candidate of await context.pages()) {
    const state = await candidate.evaluate(() => ({url: location.href, visible: document.visibilityState}));
    if (state.url === args[0] + 'tabs/back' || state.url === args[0] + 'tabs/front') {
      tabs.push({page: candidate, state});
    }
  }
  if (tabs.length === 2) break;
}
if (tabs.length !== 2) throw new Error('Expected two fixture tabs: ' + JSON.stringify(tabs.map(tab => tab.state)));
const back = tabs.find(tab => tab.state.url.endsWith('/back')).page;
const front = tabs.find(tab => tab.state.url.endsWith('/front')).page;
if (await back.evaluate(() => document.visibilityState) !== 'hidden') {
  throw new Error('Back tab must start hidden');
}
for (const tab of [back, front]) {
  await tab.evaluate(() => {
    document.title = 'Tab probe';
    window.activationClicks = [];
    document.querySelector('button').addEventListener('click', event => {
      window.activationClicks.push({trusted: event.isTrusted, visible: document.visibilityState});
    });
  });
}
await back.bringToFront();
if (await back.evaluate(() => document.visibilityState) !== 'visible') {
  throw new Error('bringToFront did not activate the back tab');
}
if (await front.evaluate(() => document.visibilityState) !== 'hidden') {
  throw new Error('bringToFront activated the wrong tab');
}
await back.getByRole('button', {name: 'Save', exact: true}).click();
await front.getByRole('button', {name: 'Save', exact: true}).click();
for (const tab of [back, front]) {
  const state = await tab.evaluate(() => ({
    title: document.title,
    clicks: window.activationClicks,
    markers: Object.keys(window).filter(key => key.startsWith('__fd_native_')),
    labels: document.querySelectorAll('[aria-label^="__fd_native_"]').length,
  }));
  if (state.title !== 'Tab probe' || state.markers.length || state.labels) {
    throw new Error('Native activation left document markers behind');
  }
  if (state.clicks.length !== 1 || !state.clicks[0].trusted || state.clicks[0].visible !== 'visible') {
    throw new Error('Input was not delivered to the visible requested tab');
  }
}
await front.screenshot({path: args[1]});
await front.close();
await back.bringToFront();
if ((await context.pages()).length !== 1) throw new Error('Closing the probe tab did not remove it');
return {version: browser.version(), tabs: 2, trustedClicks: 2, remainingTabs: 1};
