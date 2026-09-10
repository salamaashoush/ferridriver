import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import cases from './fixtures/use-option-cases.mjs';
import { passed, run, workspace } from './support.mjs';

const expectations = {
  use_precedence_is_spec_then_project_then_config_then_default: { passes: true, messages: ['15 passed'] },
  an_option_fixture_with_no_override_anywhere_keeps_its_declared_default: { passes: true, messages: ['1 passed'] },
  a_use_key_naming_a_non_option_fixture_is_refused: { passes: false, messages: ['Fixture "plain" cannot be overridden in the configuration "use" section. Only fixtures registered with { option: true } can be set in the config.'] },
  a_use_key_naming_a_built_in_fixture_is_refused: { passes: false, messages: ['Fixture "page" cannot be overridden in the configuration "use" section.'] },
  a_use_key_that_names_nothing_is_reported_and_ignored: { passes: true, messages: ['use.unknownKey', 'nobodyClaimsThisKey'] },
  a_project_use_key_is_validated_too: { passes: false, messages: ['Fixture "plain" cannot be overridden'] },
};

for (const fixture of cases) {
  test(`option fixtures: ${fixture.name.replaceAll('_', ' ')}`, async () => {
    const cwd = await workspace({ 'specs/use.spec.ts': fixture.spec, 'ferridriver.toml': `[test]
      testDir = "specs"
      testMatch = ["**/*.spec.ts"]
      workers = 16
      maxParallelProjects = 5
      retries = 0
      reporter = [{ name = "list" }]
      [test.browser]
      browser = "chromium"
      backend = "cdp-pipe"
      headless = true
      ${fixture.config}
    ` });
    const result = await run(['test', '--no-inherit', '--headless'], { cwd });
    const expected = expectations[fixture.name];
    assert.ok(expected, fixture.name);
    if (expected.passes) passed(result);
    else assert.notEqual(result.code, 0, result.text);
    for (const message of expected.messages) assert.ok(result.text.includes(message), result.text);
  });
}
