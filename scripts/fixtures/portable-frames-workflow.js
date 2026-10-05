await page.goto(args[0] + 'frames');
const child = page.frame('child');
if (page.frames().length !== 2 || !child || child.url() !== 'about:srcdoc') {
  throw new Error('Navigation did not discover the child frame');
}
if (await child.locator('p').textContent() !== 'child') {
  throw new Error('Child-frame locator did not read the child document');
}
await page.goto(args[0] + 'empty');
if (page.frames().length !== 1 || !child.isDetached() || page.frame('child') != null) {
  throw new Error('Navigation retained the previous document frames');
}
await page.screenshot({path: args[1]});
return {version: browser.version(), frames: 2, after: 1, detached: child.isDetached()};
