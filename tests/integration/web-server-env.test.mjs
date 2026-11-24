import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

// Playwright exports the webServer URL into the environment its workers and
// their children inherit. The runner cannot write its own environment while
// its threads read it, so it hands the URL to the spec realm and to the
// processes the spec starts instead; both must still see it.

const config = allowEnv => `[scripting]
allowEnv = ${JSON.stringify(allowEnv)}

[test]
testMatch = ["*.test.mjs"]
workers = 1
retries = 0

[[test.webServer]]
staticDir = "public"
`;

const spec = `import { test, expect } from '@ferridriver/test';
import { execFileSync } from 'node:child_process';

test('sees the webServer URL', async ({ baseURL }) => {
  const url = process.env.FERRIDRIVER_BASE_URL;
  expect(url).toMatch(/^http:\\/\\/127\\.0\\.0\\.1:\\d+$/);
  expect(url).toBe(baseURL);
  expect(await (await fetch(url + '/marker.txt')).text()).toBe('served by the webServer');
  const child = execFileSync('sh', ['-c', 'printf %s "$FERRIDRIVER_BASE_URL"'], { encoding: 'utf8' });
  expect(child).toBe(url);
});
`;

const grantless = `import { test, expect } from '@ferridriver/test';

test('does not see an ungranted variable', async ({ baseURL }) => {
  expect(baseURL).toMatch(/^http:\\/\\/127\\.0\\.0\\.1:\\d+$/);
  expect(process.env.FERRIDRIVER_BASE_URL).toBeUndefined();
});
`;

test('a spec and its child processes see the webServer URL as FERRIDRIVER_BASE_URL', async () => {
  const cwd = await workspace({
    'ferridriver.toml': config(['FERRIDRIVER_BASE_URL']),
    'url.test.mjs': spec,
    'public/marker.txt': 'served by the webServer',
  });
  passed(await run(['test', '--no-inherit', '--headless', '--reporter', 'line'], { cwd }));
});

test('the webServer URL stays within the env grant like any other variable', async () => {
  const cwd = await workspace({
    'ferridriver.toml': config([]),
    'url.test.mjs': grantless,
    'public/marker.txt': 'served by the webServer',
  });
  passed(await run(['test', '--no-inherit', '--headless', '--reporter', 'line'], { cwd }));
});
