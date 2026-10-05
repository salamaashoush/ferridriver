import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {mkdirSync, mkdtempSync, writeFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {tmpdir} from 'node:os';
import {dirname, join} from 'node:path';
import {pathToFileURL} from 'node:url';

const [installation, originalDevice, originalUiPid, deviceType, runtime] = process.argv.slice(2);
if (process.platform !== 'darwin' || !runtime || !/^\d+$/.test(originalUiPid)) {
  throw new Error('Usage on macOS: node probe-ios-device-isolation.mjs <installation> <existing-device-udid> <existing-ui-pid> <device-type-id> <runtime-id>');
}
const require = createRequire(join(installation, 'node_modules/appium-xcuitest-driver/package.json'));
const packageRoot = dirname(require.resolve('appium-ios-simulator/package.json'));
const {getSimulator} = await import(pathToFileURL(join(packageRoot, 'build/lib/index.js')).href);
const root = mkdtempSync(join(tmpdir(), 'ferridriver-ios-isolation-'));
const devicesSetPath = join(root, 'devices');
mkdirSync(devicesSetPath);
console.log(`Artifacts: ${root}`);

const simctl = (...args) => execFileSync('/usr/bin/xcrun', ['simctl', ...args], {
  encoding: 'utf8', timeout: 180000,
}).trim();
const own = (...args) => simctl('--set', devicesSetPath, ...args);
const devices = listing => Object.values(JSON.parse(listing).devices).flat();
const original = () => ({
  device: devices(simctl('list', 'devices', '--json')).find(device => device.udid === originalDevice)?.state,
  ui: execFileSync('/bin/ps', ['-p', originalUiPid, '-o', 'args='], {encoding: 'utf8', timeout: 10000}).trim(),
});
const before = original();
assert.equal(before.device, 'Booted');
assert.ok(before.ui.includes('/Simulator '));
assert.ok(before.ui.includes(originalDevice));
let failure;
try {
  const udid = own('create', 'ferridriver-isolation-probe', deviceType, runtime);
  const simulator = await getSimulator(udid, {devicesSetPath});
  const started = performance.now();
  await simulator.run({isHeadless: true, startupTimeout: 120000});
  const coldMs = performance.now() - started;
  const warmStarted = performance.now();
  await simulator.run({isHeadless: true, startupTimeout: 120000});
  const warmMs = performance.now() - warmStarted;
  const after = original();
  assert.deepEqual(after, before);
  assert.equal(devices(own('list', 'devices', '--json')).find(device => device.udid === udid)?.state, 'Booted');
  own('io', udid, 'screenshot', join(root, 'headless.png'));
  writeFileSync(join(root, 'result.json'), JSON.stringify({before, after, udid, coldMs, warmMs}, null, 2));
} catch (error) {
  failure = error;
} finally {
  try {
    // This set was created by this probe; default-set devices are never shutdown targets.
    own('shutdown', 'all');
    own('delete', 'all');
    assert.equal(devices(own('list', 'devices', '--json')).length, 0);
    assert.deepEqual(original(), before);
    writeFileSync(join(root, 'cleanup.json'), JSON.stringify({remaining: 0, original: original()}, null, 2));
  } catch (error) {
    failure = failure ? new AggregateError([failure, error], 'Probe and cleanup failed') : error;
  }
}
if (failure) throw failure;
