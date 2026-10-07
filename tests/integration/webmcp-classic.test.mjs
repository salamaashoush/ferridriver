import assert from 'node:assert/strict';
import {existsSync, readFileSync} from 'node:fs';
import {join} from 'node:path';
import {test} from '@ferridriver/test';
import {fixtureServer, passed, quote, repo, run, workspace} from './support.mjs';

for (const bidi of [false, true]) {
test(`WebDriver native WebMCP concurrency, results and traces, bidi=${bidi}`, async () => {
  const driver = process.env.FERRIDRIVER_CHROMEDRIVER ?? 'chromedriver';
  let browserBinary = process.env.FERRIDRIVER_WEBDRIVER_CHROME;
  if (!browserBinary) {
    const found = await commands.exec('probe', {command:'command -v chromium || command -v chromium-browser || command -v google-chrome'});
    if (found.exitCode === 0) browserBinary = found.stdout.trim();
  }
  const chromeOptions = {
    ...(browserBinary ? {binary:browserBinary} : {}),
    args:['--headless=new','--no-sandbox','--enable-features=WebMCP,WebMCPTesting,DevToolsWebMCPSupport'],
  };
  // ChromeDriver's own log is the only record of why a session did not
  // start; it is attached to the failure rather than kept on disk.
  const driverLog = test.info().outputPath('chromedriver.log');
  await commands.start('stdio', {command: `${quote(driver)} --port=0 --verbose --log-path=${quote(driverLog)}`});
  try {
    const output = await commands.waitForOutput('stdio', 'ChromeDriver was started successfully on port');
    const line = output.match(/started successfully on port (\d+)/)
      ?? (await commands.waitForOutput('stdio', '\n')).match(/started successfully on port (\d+)/);
    assert.ok(line, output);
    await fixtureServer(async base => {
      const cwd = await workspace({'main.ts': `
import assert from 'node:assert/strict';
import {verifyWebMcpConcurrency} from ${JSON.stringify(join(repo, 'tests/shared/webmcp-concurrency.ts'))};
const browser = await chromium().connect('http://127.0.0.1:${line[1]}', {
  timeout: 10000,
  capabilities: {webSocketUrl:${bidi}, 'goog:chromeOptions': ${JSON.stringify(chromeOptions)}},
});
const context = browser.contexts()[0];
await context.tracing.start({snapshots:false});
try {
  const pages = await browser.contexts()[0].pages();
  await verifyWebMcpConcurrency(pages[0], assert, ${JSON.stringify(base + '/fx/iframe')});
  const page = pages[0];
  await page.evaluate(() => {
    const model = document.modelContext ?? navigator.modelContext;
    model.registerTool({name:'echo', description:'Returns input', inputSchema:{type:'object'}, execute: value => value});
    model.registerTool({name:'empty', description:'Returns undefined', execute: () => undefined});
    model.registerTool({name:'failure', description:'Fails once', execute: () => {
      window.failureCalls = (window.failureCalls ?? 0) + 1; throw new Error('sashoush tool failure');
    }});
  });
  assert.deepEqual(await page.webmcp.callTool('echo', {message:'sashoush', nested:{values:[1,null,true]}}),
    {message:'sashoush', nested:{values:[1,null,true]}});
  assert.equal(await page.webmcp.callTool('empty'), undefined);
  await assert.rejects(page.webmcp.callTool('failure'), /invocation failed|sashoush tool failure/);
  assert.equal(Number(await page.evaluate(() => window.failureCalls)), 1);
  await assert.rejects(page.webmcp.callTool('gated', {}, {timeout:100}), /Timeout|timeout/);
  assert.ok((await page.webmcp.tools()).some(tool => tool.name === 'gated'));
  await page.click('#release-tool');
} finally {
  try { await context.tracing.stop({path:'webmcp.trace.zip'}); }
  finally { await browser.close(); }
}
export default 'passed';
`});
      const result = await run(['run', '--no-inherit', '--json', 'main.ts'], {cwd});
      if (result.code !== 0 && existsSync(driverLog)) {
        const lines = readFileSync(driverLog, 'utf8').split('\n');
        const notable = lines.filter(line => /Launching|DevToolsActivePort|ERROR|SEVERE|exited|binary/i.test(line));
        result.text += `\n--- chromedriver log ---\n${[...notable, '...', ...lines.slice(-20)].join('\n')}`;
      }
      passed(result);
      assert.equal(JSON.parse(result.stdout).value, 'passed');
      const trace = await run(['trace', 'show', 'webmcp.trace.zip', '--no-inherit', '--json'], {cwd});
      passed(trace);
      const actions = JSON.parse(trace.stdout).contexts.flatMap(context => context.actions);
      const calls = actions.filter(action => action.title?.endsWith('.callTool'));
      assert.equal(calls.length, 5, JSON.stringify(actions));
      assert.ok(actions.some(action => action.title?.endsWith('.tools')), JSON.stringify(actions));
      assert.equal(calls.filter(action => action.error).length, 2, JSON.stringify(calls));
    });
  } finally {
    await commands.stop('stdio');
  }
});
}
