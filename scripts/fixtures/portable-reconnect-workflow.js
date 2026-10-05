await page.goto(args[0] + 'tabs/back');
await page.evaluate(url => {
  const link = document.createElement('a');
  link.href = url;
  link.target = '_blank';
  link.textContent = 'Open reconnect tab';
  document.body.prepend(link);
}, args[0] + 'tabs/front');
await page.getByRole('link', {name: 'Open reconnect tab', exact: true}).click();
let original = [];
const deadline = Date.now() + 5000;
while (Date.now() < deadline) {
  original = await context.pages();
  if (original.length === 2) break;
  await page.evaluate(() => document.readyState);
}
if (original.length !== 2) throw new Error('Expected two tabs before reconnect');
for (const candidate of original) {
  await candidate.evaluate(() => {window.reconnectToken = 'sashoush:' + location.pathname;});
}
await browser.close();
if (original.some(candidate => !candidate.isClosed())) throw new Error('Disconnected page handles remained open');
const resumed = await safari().connect(args[2], {timeout: 30000, capabilities: JSON.parse(args[3])});
try {
  const resumedContext = resumed.contexts()[0];
  const pages = await resumedContext.pages();
  if (pages.length !== 2) throw new Error('Reconnect lost an existing tab');
  const tabs = {};
  for (const candidate of pages) {
    const state = await candidate.evaluate(() => ({url: location.href, token: window.reconnectToken}));
    const path = state.url.slice(args[0].length);
    if (!['tabs/back', 'tabs/front'].includes(path) || state.token !== 'sashoush:/' + path) {
      throw new Error('Reconnect selected or replaced the wrong document');
    }
    tabs[path] = candidate;
    await candidate.bringToFront();
    await candidate.evaluate(() => {
      window.reconnectClick = null;
      document.querySelector('button').addEventListener('click', event => {
        window.reconnectClick = {trusted: event.isTrusted, visible: document.visibilityState};
      });
    });
    await candidate.getByRole('button', {name: 'Save', exact: true}).click();
    const click = await candidate.evaluate(() => window.reconnectClick);
    if (!click || !click.trusted || click.visible !== 'visible') {
      throw new Error('Reconnected input missed the visible requested document');
    }
  }
  await tabs['tabs/front'].screenshot({path: args[1]});
  await tabs['tabs/front'].close();
  await tabs['tabs/back'].bringToFront();
  if ((await resumedContext.pages()).length !== 1) throw new Error('Reconnect cleanup retained the extra tab');
  return {version: resumed.version(), preservedDocuments: 2, trustedClicks: 2, remainingTabs: 1};
} finally {
  await resumed.close();
}
