import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

const defaults = "defineDefaults({ test: { testMatch: ['specs/from-extension.spec.ts'] } });";
async function project(extension = defaults, match, policy = '') {
  const files = { 'ext/plug.ts': extension,
    'ferridriver.toml': `[extensions]\npaths = ["./ext/plug.ts"]\n${policy}\n[test]\n${match ? `testMatch = ["${match}"]\n` : ''}reporter = [{ name = "list" }]\n[test.browser]\nheadless = true\n` };
  for (const name of ['extension', 'config', 'cli']) {
    files[`specs/from-${name}.spec.ts`] = "import { test, expect } from '@ferridriver/test'; test('runs', async ({}) => { expect(1).toBe(1); });";
  }
  return workspace(files);
}
const list = (cwd, ...extra) => run(['test', '--list', '--no-inherit', ...extra], { cwd });

test('extension defaults apply until overridden by a document and then the command line', async () => {
  const extension = await list(await project());
  passed(extension);
  assert.ok(extension.text.includes('from-extension.spec.ts'));
  assert.ok(!extension.text.includes('from-config.spec.ts'));
  const cwd = await project(defaults, 'specs/from-config.spec.ts');
  const config = await list(cwd);
  passed(config);
  assert.ok(config.text.includes('from-config.spec.ts'));
  assert.ok(!config.text.includes('from-extension.spec.ts'));
  const cli = await list(cwd, 'specs/from-cli.spec.ts');
  passed(cli);
  assert.ok(cli.text.includes('from-cli.spec.ts'));
  assert.ok(!cli.text.includes('from-config.spec.ts'));
  assert.ok(!cli.text.includes('from-extension.spec.ts'));
});

for (const [title, source, policy, messages] of [
  ['invalid defaults identify the key and contributing package', "defineDefaults({ test: { testMatchh: ['specs/*.spec.ts'] } });", '', ['testMatchh', 'plug.ts']],
  ['operator policy refuses config defaults explicitly', defaults, '[extensions.policy]\nconfigDefaults = false\n', ['configDefaults']],
  ['extensions cannot change the loader that compiled them', "defineDefaults({ bundler: { conditions: ['node'] } });", '', ['bundler', 'compiled this package']],
]) {
  test(title, async () => {
    const result = await list(await project(source, undefined, policy));
    assert.notEqual(result.code, 0, result.text);
    for (const message of messages) assert.ok(result.text.includes(message), result.text);
  });
}
