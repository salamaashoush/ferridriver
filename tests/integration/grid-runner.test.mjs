import assert from 'node:assert/strict';
import { access, mkdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { binary, quote, run, workspace } from './support.mjs';

// Listing, selection by stable id, sharding with an explicit assignment,
// the `stream` reporter, merging attempts across shards, and the exit
// statuses that tell a test failure from everything else: what a remote
// orchestrator needs to plan a run, hand out parts of it and fold the
// results back together.

const config = (extra = '') => `[test]
testDir = "."
testMatch = ["tests/*.test.mjs"]
workers = 2
retries = 0
features = ["features/*.feature"]
steps = ["steps/*.mjs"]
${extra}`;

const files = {
  'tests/cart.test.mjs': `import { test } from '@ferridriver/test';
test('adds an item @smoke', async () => {});
test('removes an item', { tag: '@slow', annotation: { type: 'issue', description: 'https://example.com/issues/7' } }, async () => {});
test.describe.serial('checkout', () => {
  test('fills the address', async () => {});
  test.describe('payment', () => {
    test('pays', async () => {});
  });
  test('confirms', async () => {});
});
test.describe('budgeted', () => {
  test.describe.configure({ retries: 2, timeout: 5000 });
  test('retried', async () => {});
});
`,
  'features/login.feature': `Feature: Login
  Scenario Outline: signing in as <user>
    Given a user named "<user>"
    When they sign in
    Then they see the dashboard

    Examples:
      | user  |
      | alice |
      | bob   |

  Scenario: an undefined step
    Given a user named "carol"
    When they do something nobody defined
`,
  'features/orders.feature': `@serial
Feature: Orders
  Scenario: first
    Given a user named "dave"
  Scenario: second
    Given a user named "erin"
`,
  'steps/steps.mjs': `import { Given, When, Then } from '@cucumber/cucumber';
Given('a user named {string}', function (name) { this.user = name; });
When('they sign in', function () {});
Then('they see the dashboard', function () {});
`,
};

const suite = (extra = {}, configExtra = '') => workspace({ 'ferridriver.toml': config(configExtra), ...files, ...extra });

async function listing(cwd, command, args = []) {
  const result = await run([command, '--no-inherit', '--list', '--format', 'json', ...args], { cwd });
  let doc;
  try {
    doc = JSON.parse(result.stdout);
  } catch {
    assert.fail(`not a JSON document (exit ${result.code}):\n${result.text}`);
  }
  return { result, doc };
}

const byTitle = (doc, title) => {
  const record = doc.tests.find(test => test.titlePath.at(-1) === title);
  assert.ok(record, `no record titled ${title}: ${JSON.stringify(doc.tests.map(t => t.titlePath))}`);
  return record;
};

// Every spec of a JSON report, flattened: title -> its results.
async function report(path) {
  const doc = JSON.parse(await readFile(path, 'utf8'));
  const specs = new Map();
  const walk = suites => {
    for (const suite of suites ?? []) {
      for (const spec of suite.specs) specs.set(spec.title, { id: spec.id, test: spec.tests[0] });
      walk(suite.suites);
    }
  };
  walk(doc.suites);
  return { stats: doc.stats, specs };
}

test('test --list --format json describes every test without starting anything', async () => {
  // A web server that cannot start and a browser that does not exist:
  // listing touches neither.
  const cwd = await suite({}, '[[test.webServer]]\ncommand = "exit 3"\nurl = "http://127.0.0.1:9"\ntimeout = 1000\n');
  const { result, doc } = await listing(cwd, 'test', ['--executable-path', '/nonexistent/browser']);
  assert.equal(result.code, 0, result.text);
  assert.equal(doc.schema, 1);
  assert.deepEqual(doc.errors, []);
  assert.equal(doc.tests.length, 6);

  const pays = byTitle(doc, 'pays');
  assert.equal(pays.file, 'tests/cart.test.mjs');
  assert.equal(pays.line, 7);
  assert.equal(pays.project, '');
  assert.deepEqual(pays.titlePath, ['tests/cart.test.mjs', 'checkout', 'payment', 'pays']);
  assert.match(pays.stableId, /^[0-9a-f]{20}-[0-9a-f]{20}$/);
  // A describe nested in a serial one belongs to the same group.
  for (const title of ['fills the address', 'pays', 'confirms'])
    assert.equal(byTitle(doc, title).groupKey, 'tests/cart.test.mjs > checkout');
  assert.equal(byTitle(doc, 'adds an item @smoke').groupKey, null);
  assert.deepEqual(byTitle(doc, 'adds an item @smoke').tags, ['@smoke']);

  const removes = byTitle(doc, 'removes an item');
  assert.deepEqual(removes.tags, ['@slow']);
  assert.deepEqual(removes.annotations, [{ type: 'issue', description: 'https://example.com/issues/7' }]);
  assert.equal(removes.retries, 0);
  assert.equal(removes.timeout, 30000);
  const retried = byTitle(doc, 'retried');
  assert.equal(retried.retries, 2);
  assert.equal(retried.timeout, 5000);

  // The text form is still there.
  const text = await run(['test', '--no-inherit', '--list'], { cwd });
  assert.equal(text.code, 0, text.text);
  assert.match(text.stdout, /tests\/cart\.test\.mjs > checkout > payment > pays/);
  assert.match(text.stdout, /6 test\(s\) found/);
});

test('stable ids and files do not depend on the working directory', async () => {
  const cwd = await suite();
  const fromRoot = (await listing(cwd, 'test')).doc.tests;
  const fromSub = await run(['test', '--no-inherit', '--config', '../ferridriver.toml', '--list', '--format', 'json'], { cwd: join(cwd, 'tests') });
  assert.equal(fromSub.code, 0, fromSub.text);
  const sub = JSON.parse(fromSub.stdout).tests;
  const key = tests => tests.map(t => `${t.stableId} ${t.file} ${t.groupKey}`).sort();
  assert.deepEqual(key(sub), key(fromRoot));

  // And on another machine: the same suite checked out somewhere else.
  const elsewhere = (await listing(await suite(), 'test')).doc.tests;
  assert.deepEqual(key(elsewhere), key(fromRoot));
});

test('bdd --list reports outline rows, the @serial feature and undefined steps', async () => {
  const cwd = await suite();
  const { result, doc } = await listing(cwd, 'bdd', ['--executable-path', '/nonexistent/browser']);
  assert.equal(result.code, 1, `an undefined step fails a strict listing\n${result.text}`);
  assert.equal(doc.tests.length, 5);

  const alice = byTitle(doc, 'signing in as alice');
  assert.deepEqual(alice.titlePath, ['features/login.feature', 'Login', 'signing in as <user>', 'signing in as alice']);
  assert.equal(alice.line, 9);
  assert.equal(byTitle(doc, 'signing in as bob').line, 10);
  assert.equal(alice.groupKey, null);
  assert.equal(alice.stepIssues, undefined);

  for (const title of ['first', 'second']) assert.equal(byTitle(doc, title).groupKey, 'features/orders.feature > Orders');

  const undefinedStep = byTitle(doc, 'an undefined step');
  assert.equal(undefinedStep.stepIssues.length, 1);
  assert.equal(undefinedStep.stepIssues[0].kind, 'undefined');
  assert.equal(undefinedStep.stepIssues[0].line, 14);
  assert.equal(undefinedStep.stepIssues[0].text, 'they do something nobody defined');
  assert.equal(doc.errors.length, 1);
  assert.equal(doc.errors[0].stableId, undefinedStep.stableId);

  const lenient = await listing(cwd, 'bdd', ['--no-strict']);
  assert.equal(lenient.result.code, 0, lenient.result.text);
});

test('bdd --dry-run matches every step and launches nothing', async () => {
  const cwd = await suite();
  const strict = await run(['bdd', '--no-inherit', '--dry-run', '--executable-path', '/nonexistent/browser'], { cwd });
  assert.equal(strict.code, 1, strict.text);
  assert.match(strict.text, /undefined step: "they do something nobody defined"/);
  assert.match(strict.text, /4 passed/);

  const lenient = await run(['bdd', '--no-inherit', '--dry-run', '--no-strict', '--executable-path', '/nonexistent/browser'], { cwd });
  assert.equal(lenient.code, 0, lenient.text);

  const ambiguous = await suite({ 'steps/again.mjs': `import { Given } from '@cucumber/cucumber';
Given('a user named {string}', function () {});
` });
  const clash = await run(['bdd', '--no-inherit', '--dry-run', '--no-strict', 'features/orders.feature'], { cwd: ambiguous });
  assert.equal(clash.code, 1, clash.text);
  assert.match(clash.text, /ambiguous step/);
});

test('--test-id, --test-list and file:line select exactly those tests', async () => {
  const cwd = await suite();
  const { doc } = await listing(cwd, 'test');
  const removes = byTitle(doc, 'removes an item').stableId;
  const pays = byTitle(doc, 'pays').stableId;

  const one = await run(['test', '--no-inherit', '--test-id', removes, '--reporter', 'json=one.json'], { cwd });
  assert.equal(one.code, 0, one.text);
  const ran = await report(join(cwd, 'one.json'));
  assert.deepEqual([...ran.specs.keys()], ['removes an item']);
  // The report and the listing name the test by the same id.
  assert.equal(ran.specs.get('removes an item').id, removes);

  await writeFile(join(cwd, 'ids.txt'), `# from a planner\n${removes}\n\n${pays}\n`);
  const listed = await run(['test', '--no-inherit', '--test-list', 'ids.txt', '--reporter', 'json=two.json'], { cwd });
  assert.equal(listed.code, 0, listed.text);
  assert.deepEqual([...(await report(join(cwd, 'two.json'))).specs.keys()].sort(), ['pays', 'removes an item']);

  const unknown = await run(['test', '--no-inherit', '--test-id', 'ffffffffffffffffffff-ffffffffffffffffffff'], { cwd });
  assert.equal(unknown.code, 2, unknown.text);
  assert.match(unknown.text, /match no test/);

  const located = await run(['test', '--no-inherit', 'tests/cart.test.mjs:2', '--list', '--format', 'json'], { cwd });
  assert.equal(located.code, 0, located.text);
  assert.deepEqual(JSON.parse(located.stdout).tests.map(t => t.titlePath.at(-1)), ['adds an item @smoke']);

  const scenario = await run(['bdd', '--no-inherit', 'features/login.feature:10', '--list', '--format', 'json'], { cwd });
  assert.equal(scenario.code, 0, scenario.text);
  assert.deepEqual(JSON.parse(scenario.stdout).tests.map(t => t.titlePath.at(-1)), ['signing in as bob']);
});

test('--shard-assignment places tests, keeps serial groups whole and names the blob', async () => {
  const cwd = await suite();
  const { doc } = await listing(cwd, 'test');
  const id = title => byTitle(doc, title).stableId;
  // Assigning one test of a serial group moves the whole group.
  await writeFile(join(cwd, 'assignment.json'), JSON.stringify({ [id('pays')]: 2, [id('adds an item @smoke')]: 1 }));

  const shards = [];
  for (const index of [1, 2]) {
    const shard = await listing(cwd, 'test', ['--shard', `${index}/2`, '--shard-assignment', 'assignment.json']);
    assert.equal(shard.result.code, 0, shard.result.text);
    shards.push(shard.doc.tests.map(t => t.titlePath.at(-1)));
  }
  assert.ok(shards[0].includes('adds an item @smoke'), JSON.stringify(shards));
  for (const title of ['fills the address', 'pays', 'confirms']) assert.ok(shards[1].includes(title), JSON.stringify(shards));
  assert.deepEqual([...shards[0], ...shards[1]].sort(), doc.tests.map(t => t.titlePath.at(-1)).sort());

  const blob = await run(['test', '--no-inherit', '--shard', '2/2', '--shard-assignment', 'assignment.json', '--reporter', 'blob'], { cwd });
  assert.equal(blob.code, 0, blob.text);
  await access(join(cwd, 'test-results', 'report-2.zip'));

  await writeFile(join(cwd, 'split.json'), JSON.stringify({ [id('pays')]: 1, [id('confirms')]: 2 }));
  const split = await run(['test', '--no-inherit', '--shard', '1/2', '--shard-assignment', 'split.json'], { cwd });
  assert.equal(split.code, 2, split.text);
  assert.match(split.text, /splits the serial group/);

  await writeFile(join(cwd, 'range.json'), JSON.stringify({ [id('pays')]: 3 }));
  const range = await run(['test', '--no-inherit', '--shard', '1/2', '--shard-assignment', 'range.json'], { cwd });
  assert.equal(range.code, 2, range.text);
});

test('the stream reporter writes blob event lines that merge-reports reads back', async () => {
  const cwd = await suite({
    'tests/attach.test.mjs': `import { test } from '@ferridriver/test';
test('attaches a note', async () => { await test.info().attach('note', { body: 'hello', contentType: 'text/plain' }); });
`,
  });
  const streamed = await run(['test', '--no-inherit', '--reporter', 'stream'], { cwd });
  assert.equal(streamed.code, 0, streamed.text);
  const lines = streamed.stdout.trim().split('\n').map(line => JSON.parse(line));
  assert.deepEqual(lines[0], { kind: 'header', schema: 4, shard_index: null, shard_total: null });
  assert.equal(lines[1].kind, 'run-started');
  assert.equal(lines.at(-1).kind, 'run-finished');
  assert.equal(lines.filter(line => line.kind === 'test-finished').length, 7);
  const attached = lines.find(line => line.kind === 'test-finished' && line.outcome.test_id.name === 'attaches a note');
  const note = attached.outcome.attachments.find(a => a.name === 'note');
  assert.equal(await readFile(note.path, 'utf8'), 'hello');
  assert.equal(attached.outcome.test_id.root_path, 'tests/attach.test.mjs');

  await writeFile(join(cwd, 'events.jsonl'), streamed.stdout);
  const merged = await run(['merge-reports', '--no-inherit', 'events.jsonl', '--reporter', 'json=merged.json'], { cwd });
  assert.equal(merged.code, 0, merged.text);
  const { stats, specs } = await report(join(cwd, 'merged.json'));
  assert.equal(stats.expected, 7);
  assert.equal(specs.size, 7);

  // To a file, the same lines.
  const toFile = await run(['test', '--no-inherit', '--reporter', 'stream=out/events.jsonl', '--reporter', 'dot'], { cwd });
  assert.equal(toFile.code, 0, toFile.text);
  const written = (await readFile(join(cwd, 'out/events.jsonl'), 'utf8')).trim().split('\n').map(line => JSON.parse(line));
  assert.equal(written[0].kind, 'header');
  assert.equal(written.filter(line => line.kind === 'test-finished').length, 7);
});

test('merge-reports folds a test retried on another shard into one flaky test', async () => {
  const cwd = await suite({
    'tests/flaky.test.mjs': `import { test } from '@ferridriver/test';
import { access } from 'node:fs/promises';
test('needs the marker', async () => { await access('marker'); });
`,
    'tally.mjs': `export default class Tally {
  onTestEnd(test, result) { console.log('tally-test ' + test.title + ' retry=' + result.retry + ' ' + result.status); }
  onEnd(result) { console.log('tally-end ' + result.status); }
}
`,
  });
  const { doc } = await listing(cwd, 'test');
  const flaky = byTitle(doc, 'needs the marker').stableId;
  const stable = byTitle(doc, 'adds an item @smoke').stableId;

  // Shard 1 runs it and it fails; the grid re-queues it on shard 2, which
  // runs it from another working directory, where it passes.
  await writeFile(join(cwd, 'ids.txt'), `${flaky}\n${stable}\n`);
  const first = await run(['test', '--no-inherit', '--test-list', 'ids.txt', '--reporter', 'blob=shards/report-1.zip'], { cwd });
  assert.equal(first.code, 1, first.text);
  await mkdir(join(cwd, 'tests', 'marker'), { recursive: true });
  const second = await run(['test', '--no-inherit', '--config', '../ferridriver.toml', '--test-id', flaky, '--reporter', 'blob=../shards/report-2.zip'], { cwd: join(cwd, 'tests') });
  assert.equal(second.code, 0, second.text);

  const merged = await run(['merge-reports', '--no-inherit', 'shards', '--reporter', 'json=merged.json', '--reporter', './tally.mjs'], { cwd });
  assert.equal(merged.code, 0, `a flaky test does not fail the merged run\n${merged.text}`);
  // A JS reporter replays the merged run too.
  assert.match(merged.stdout, /tally-test needs the marker retry=0 failed/);
  assert.match(merged.stdout, /tally-test needs the marker retry=1 passed/);
  assert.match(merged.stdout, /tally-end passed/);
  const { stats, specs } = await report(join(cwd, 'merged.json'));
  assert.equal(stats.flaky, 1);
  assert.equal(stats.unexpected, 0);
  assert.equal(stats.expected, 1);
  const results = specs.get('needs the marker').test.results;
  assert.deepEqual(results.map(r => [r.retry, r.status]), [[0, 'failed'], [1, 'passed']]);
  assert.equal(specs.get('needs the marker').test.status, 'flaky');
});

async function retried(strategy) {
  const cwd = await workspace({
    'ferridriver.toml': `[test]\ntestMatch = ["*.test.mjs"]\nworkers = 2\nretries = 1\nretryStrategy = "${strategy}"\n`,
    'retry.test.mjs': `import { test } from '@ferridriver/test';
import { appendFile } from 'node:fs/promises';
const mark = name => appendFile('ran.txt', name + ':' + test.info().retry + '\\n');
test.fail('declared to fail', async () => {
  await mark('fail');
  if (test.info().retry === 1) throw new Error('fails as declared');
});
test.describe('budgeted', () => {
  test.describe.configure({ timeout: 400 });
  test('times out on its retry', async () => {
    await mark('timeout');
    if (test.info().retry === 0) throw new Error('first attempt');
    await new Promise(resolve => setTimeout(resolve, 2000));
  });
});
test.describe.serial('group', () => {
  test('one', async () => { await mark('one'); });
  test('two', async () => {
    await mark('two');
    if (test.info().retry === 0) throw new Error('first attempt');
  });
  test('three', async () => { await mark('three'); });
});
`,
  });
  const result = await run(['test', '--no-inherit', '--reporter', 'json=retry.json'], { cwd });
  assert.equal(result.code, 1, result.text);
  const { specs } = await report(join(cwd, 'retry.json'));
  const statuses = title => specs.get(title).test.results.map(r => r.status);

  // `test.fail()` survives the retry: the failing second attempt is the
  // expected outcome, so the test is flaky rather than failed.
  assert.deepEqual(statuses('declared to fail'), ['passed', 'failed']);
  assert.equal(specs.get('declared to fail').test.status, 'flaky');
  // The 400 ms budget applies to the retry too.
  assert.deepEqual(statuses('times out on its retry'), ['failed', 'timedOut']);
  // The rest of the serial group runs after the retry instead of being
  // reported skipped.
  assert.deepEqual(statuses('two'), ['failed', 'passed']);
  assert.deepEqual(statuses('three'), ['passed']);
  const ran = (await readFile(join(cwd, 'ran.txt'), 'utf8')).trim().split('\n');
  const group = ran.filter(line => /^(one|two|three):/.test(line));
  assert.deepEqual(group, ['one:0', 'two:0', 'two:1', 'three:0']);
}

test('a retry keeps the test.fail() expectation, the timeout and the serial group', async () => {
  await retried('immediate');
});

test('an isolated retry of a serial test still runs the rest of its group', async () => {
  await retried('isolated');
});

test('exit statuses tell usage, interruption and infrastructure apart', async () => {
  const cwd = await suite({
    'tests/slow.test.mjs': `import { test } from '@ferridriver/test';
test('takes a while', async () => { await new Promise(resolve => setTimeout(resolve, 3000)); });
`,
  });
  const usage = await run(['test', '--no-inherit', '--shard', '3/2'], { cwd });
  assert.equal(usage.code, 2, usage.text);

  const server = await workspace({
    'ferridriver.toml': '[test]\ntestMatch = ["*.test.mjs"]\n[[test.webServer]]\ncommand = "exit 3"\nurl = "http://127.0.0.1:9"\ntimeout = 1000\n',
    'a.test.mjs': `import { test } from '@ferridriver/test';\ntest('never runs', async () => {});\n`,
  });
  const infra = await run(['test', '--no-inherit'], { cwd: server });
  assert.equal(infra.code, 4, infra.text);

  const browser = await workspace({
    'ferridriver.toml': '[test]\ntestMatch = ["*.test.mjs"]\n',
    'a.test.mjs': `import { test } from '@ferridriver/test';\ntest('needs a page', async ({ page }) => { await page.goto('about:blank'); });\n`,
  });
  const launch = await run(['test', '--no-inherit', '--executable-path', '/nonexistent/browser'], { cwd: browser });
  assert.equal(launch.code, 4, launch.text);

  const command = `cd ${quote(cwd)} && { ${quote(binary)} test --no-inherit tests/slow.test.mjs --reporter stream=events.jsonl & pid=$!; sleep 1; kill -INT $pid; wait $pid; echo "status=$?"; }`;
  const interrupted = await commands.exec('probe', { command });
  assert.match(interrupted.stdout, /status=3/, interrupted.stdout + interrupted.stderr);
  const last = JSON.parse((await readFile(join(cwd, 'events.jsonl'), 'utf8')).trim().split('\n').at(-1));
  assert.equal(last.kind, 'run-finished');
  assert.equal(last.status, 'interrupted');
});
