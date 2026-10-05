const url = 'data:text/html;charset=utf-8,' + encodeURIComponent(
  '<title>sashoush data page</title><h1>Data navigation</h1><iframe name="child" srcdoc="<p>child</p>"></iframe>'
);
await page.goto(url, {timeout: 10000});
if (await page.title() !== 'sashoush data page' || page.url() !== url) {
  throw new Error('Data navigation did not commit the requested document');
}
const child = page.frame('child');
if (!child || await child.locator('p').textContent() !== 'child') {
  throw new Error('Data navigation did not discover a usable child frame');
}
await page.goto(args[0]);
if (!child.isDetached()) throw new Error('Leaving a data URL retained its frame');
await page.goBack();
if (await page.title() !== 'sashoush data page' || !page.frame('child')) {
  throw new Error('History navigation did not restore the data document');
}
await page.screenshot({path: args[1]});
return {version: browser.version(), dataUrl: true, childFrame: true, history: true};
