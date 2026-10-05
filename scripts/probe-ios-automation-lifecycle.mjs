import assert from 'node:assert/strict';
import {execFileSync, spawn} from 'node:child_process';
import {once} from 'node:events';
import {mkdtempSync, openSync, closeSync, writeFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {createServer} from 'node:net';
import {homedir, tmpdir} from 'node:os';
import {dirname, join} from 'node:path';
import {setTimeout as delay} from 'node:timers/promises';
import {pathToFileURL} from 'node:url';

const [installation, deviceType, runtime, inputBackend = 'web'] = process.argv.slice(2);
if (process.platform !== 'darwin' || !runtime) {
  throw new Error('Usage on macOS: node probe-ios-automation-lifecycle.mjs <installation> <device-type> <runtime> [web|native]');
}
assert.ok(['web', 'native'].includes(inputBackend), 'input backend must be web or native');
const root = mkdtempSync(join(tmpdir(), 'ferridriver-automation-lifecycle-'));
const devicesSetPath = join(homedir(), 'Library/Developer/CoreSimulator/Devices');
console.log(`Artifacts: ${root}`);
const simctl = (...args) => execFileSync('/usr/bin/xcrun', ['simctl', ...args], {
  encoding: 'utf8', timeout: 180000,
}).trim();
const own = (...args) => simctl('--set', devicesSetPath, ...args);
const inventory = () => Object.values(JSON.parse(simctl('list', 'devices', '--json')).devices)
  .flat().map(({udid, name, state}) => ({udid, name, state})).sort((a, b) => a.udid.localeCompare(b.udid));
const before = inventory();
const require = createRequire(join(installation, 'node_modules/appium-xcuitest-driver/package.json'));
const simulatorRoot = dirname(require.resolve('appium-ios-simulator/package.json'));
const {getSimulator} = await import(pathToFileURL(join(simulatorRoot, 'build/lib/index.js')).href);
let server;
let endpoint;
let sessionId;
let failure;
let udid;
const results = [];
const freePort = async () => {
  const socket = createServer();
  socket.listen(0, '127.0.0.1');
  await once(socket, 'listening');
  const port = socket.address().port;
  await new Promise(resolve => socket.close(resolve));
  return port;
};
const command = async (method, path, body, timeout = 30000) => {
  const started = performance.now();
  const response = await fetch(endpoint + path, {
    method, headers: {'content-type': 'application/json'},
    body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(timeout),
  });
  const result = await response.json();
  results.push({method, path, status: response.status, wallMs: performance.now() - started});
  if (!response.ok || result.value?.error) throw new Error(JSON.stringify(result));
  return result.value;
};
const sessionCommand = (method, path, body, timeout) => command(method, `/session/${sessionId}${path}`, body, timeout);
try {
  udid = own('create', 'ferridriver-automation-lifecycle', deviceType, runtime);
  const simulator = await getSimulator(udid, {devicesSetPath});
  await simulator.run({isHeadless: true, startupTimeout: 120000});
  const port = await freePort();
  endpoint = `http://127.0.0.1:${port}`;
  const log = openSync(join(root, 'appium.log'), 'wx');
  server = spawn(join(installation, 'node/bin/node'), [
    join(installation, 'node_modules/appium/index.js'), '--address', '127.0.0.1', '--port', String(port),
  ], {
    detached: true, stdio: ['ignore', log, log],
    env: {...process.env, PATH: join(installation, 'node/bin') + ':' + process.env.PATH,
      APPIUM_HOME: join(installation, 'home'), APPIUM_WDA_INHERIT_PROCESS_GROUP: '1'},
  });
  closeSync(log);
  const readyDeadline = performance.now() + 30000;
  for (;;) {
    try { await command('GET', '/status', undefined, 1000); break; }
    catch (error) {
      if (server.exitCode !== null || performance.now() > readyDeadline) throw error;
      await delay(100);
    }
  }
  const wdaPort = await freePort();
  let mjpegPort = await freePort();
  while (mjpegPort === wdaPort) mjpegPort = await freePort();
  for (let iteration = 0; iteration < 2; iteration++) {
    const session = await command('POST', '/session', {capabilities: {alwaysMatch: {
      browserName: 'safari', platformName: 'iOS', 'appium:automationName': 'XCUITest',
      'appium:udid': udid, 'appium:simulatorDevicesSetPath': devicesSetPath,
      'appium:isHeadless': true, 'appium:noReset': true, 'appium:showXcodeLog': true,
      'appium:safariGlobalPreferences': {
        'WBSOnboardingStatesDefaultsKeyV0.2': {TipForMoreButton: 3},
      },
      'appium:wdaLocalPort': wdaPort, 'appium:mjpegServerPort': mjpegPort,
      'appium:derivedDataPath': join(root, 'wda'),
    }}}, 180000);
    sessionId = session.sessionId;
    const context = await sessionCommand('GET', '/context');
    await sessionCommand('POST', '/execute/sync', {script: 'mobile: startAutomationSession', args: []});
    assert.equal(await sessionCommand('GET', '/context'), context);
    const handle = await sessionCommand('GET', '/window');
    const url = 'data:text/html,' + encodeURIComponent(
      '<title>sashoush automation</title><p>native navigation</p>' +
      '<button onclick="window.clicked=event.isTrusted">Save</button>',
    );
    await sessionCommand('POST', '/url', {url});
    const title = await sessionCommand('GET', '/title');
    assert.equal(title, 'sashoush automation');
    const document = await sessionCommand('POST', '/execute/sync', {
      script: 'return {url: location.href, text: document.querySelector("p").textContent}', args: [],
    });
    assert.ok(document.url.startsWith('data:text/html,'));
    assert.equal(document.text, 'native navigation');
    const created = await sessionCommand('POST', '/window/new', {type: 'tab'});
    assert.notEqual(created.handle, handle);
    assert.equal(await sessionCommand('GET', '/context'), context);
    await sessionCommand('POST', '/window', {handle: created.handle});
    await sessionCommand('POST', '/url', {url});
    assert.equal(await sessionCommand('GET', '/title'), title);
    const remaining = await sessionCommand('DELETE', '/window');
    assert.ok(remaining.includes(handle));
    assert.ok(!remaining.includes(created.handle));
    await sessionCommand('POST', '/window', {handle});
    assert.equal(await sessionCommand('GET', '/title'), title);
    if (inputBackend === 'native') {
      const marker = `ferridriver-native-button-${iteration}`;
      await sessionCommand('POST', '/execute/sync', {
        script: 'document.querySelector("button").setAttribute("aria-label", arguments[0])', args: [marker],
      });
      try {
        await sessionCommand('POST', '/context', {name: 'NATIVE_APP'});
        const button = await sessionCommand('POST', '/element', {using: 'accessibility id', value: marker});
        const rect = await sessionCommand('GET', `/element/${button['element-6066-11e4-a52e-4f735466cecf']}/rect`);
        await sessionCommand('POST', '/actions', {actions: [{type: 'pointer', id: 'touch',
          parameters: {pointerType: 'touch'}, actions: [
            {type: 'pointerMove', duration: 0, origin: 'viewport', x: rect.x + rect.width / 2, y: rect.y + rect.height / 2},
            {type: 'pointerDown', button: 0}, {type: 'pointerUp', button: 0},
          ]}]});
      } finally {
        await sessionCommand('POST', '/context', {name: context});
        await sessionCommand('POST', '/window', {handle});
        await sessionCommand('POST', '/execute/sync', {
          script: 'document.querySelector("button").removeAttribute("aria-label")', args: [],
        });
      }
    } else {
      const button = await sessionCommand('POST', '/element', {using: 'css selector', value: 'button'});
      await sessionCommand('POST', `/element/${button['element-6066-11e4-a52e-4f735466cecf']}/click`, {});
    }
    assert.equal(await sessionCommand('POST', '/execute/sync', {script: 'return window.clicked', args: []}), true);
    const screenshot = await sessionCommand('GET', '/screenshot');
    writeFileSync(join(root, `iteration-${iteration}.png`), Buffer.from(screenshot, 'base64'));
    console.log(JSON.stringify({iteration, handle, title}));
    await sessionCommand('POST', '/execute/sync', {
      script: 'mobile: stopAutomationSession', args: [{closeAllWindows: false}],
    });
    await sessionCommand('DELETE', '');
    sessionId = undefined;
  }
} catch (error) {
  failure = error;
  if (udid && sessionId) {
    try {
      own('io', udid, 'screenshot', join(root, 'failure-screen.png'));
      const document = await sessionCommand('POST', '/execute/sync', {
        script: 'return {url:location.href, visibility:document.visibilityState, clicked:window.clicked,' +
          'viewport:{width:innerWidth,height:innerHeight,scale:visualViewport.scale},' +
          'button:document.querySelector("button")?.getBoundingClientRect().toJSON()}', args: [],
      }, 5000);
      writeFileSync(join(root, 'failure-document.json'), JSON.stringify(document, null, 2));
    } catch (diagnosticError) {
      results.push({diagnosticError: String(diagnosticError)});
    }
  }
} finally {
  if (sessionId) {
    try { await sessionCommand('DELETE', '', undefined, 20000); }
    catch (error) { failure = new AggregateError([failure, error].filter(Boolean), 'Session cleanup failed'); }
  }
  if (server && server.exitCode === null) {
    const exited = once(server, 'exit');
    process.kill(-server.pid, 'SIGTERM');
    await Promise.race([exited, delay(10000)]);
    if (server.exitCode === null && server.signalCode === null) {
      process.kill(-server.pid, 'SIGKILL');
      await exited;
    }
  }
  try {
    // Xcode resolves devices in its default set; only this probe's UUID is owned.
    if (udid) {
      const device = inventory().find(device => device.udid === udid);
      if (device && device.state !== 'Shutdown') own('shutdown', udid);
      if (device) own('delete', udid);
    }
    assert.deepEqual(inventory(), before);
    writeFileSync(join(root, 'cleanup.json'), JSON.stringify({remaining: 0, originalDevicesUnchanged: true}));
  } catch (error) {
    failure = new AggregateError([failure, error].filter(Boolean), 'Device cleanup failed');
  }
  writeFileSync(join(root, 'results.json'), JSON.stringify({results, failure: failure?.stack}, null, 2));
}
if (failure) throw failure;
