await page.goto(args[0] + 'frames');
await page.goto(args[0] + 'empty');
await page.goBack();
if (page.url() !== args[0] + 'frames' || page.frames().length !== 2 || !page.frame('child')) {
  throw new Error('Back navigation did not restore the frame tree');
}
if (await page.frame('child').locator('p').textContent() !== 'child') {
  throw new Error('Back navigation selected the wrong child document');
}
await page.reload();
const reloaded = page.frame('child');
if (page.frames().length !== 2 || !reloaded || await reloaded.locator('p').textContent() !== 'child') {
  throw new Error('Reload did not refresh the child document');
}
await page.goForward();
if (page.url() !== args[0] + 'empty' || page.frames().length !== 1 || !reloaded.isDetached()) {
  throw new Error('Forward navigation retained the previous frame tree');
}
await page.screenshot({path: args[1]});
return {version: browser.version(), backFrames: 2, reloadFrames: 2, forwardFrames: 1};
