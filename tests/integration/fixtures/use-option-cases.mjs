export default [
  {
    "name": "use_precedence_is_spec_then_project_then_config_then_default",
    "config": "\n[test.browser.use]\nprofile = \"from-config\"\ntheme = \"config-theme\"\n\n[[test.projects]]\nname = \"cdp-pipe\"\n[test.projects.browser]\nbrowser = \"chromium\"\nbackend = \"cdp-pipe\"\nheadless = true\n[test.projects.browser.use]\nprofile = \"cdp-pipe\"\n\n[[test.projects]]\nname = \"cdp-raw\"\n[test.projects.browser]\nbrowser = \"chromium\"\nbackend = \"cdp-raw\"\nheadless = true\n[test.projects.browser.use]\nprofile = \"cdp-raw\"\n\n[[test.projects]]\nname = \"bidi\"\n[test.projects.browser]\nbrowser = \"firefox\"\nbackend = \"bidi\"\nheadless = true\n[test.projects.browser.use]\nprofile = \"bidi\"\n\n[[test.projects]]\nname = \"webkit\"\n[test.projects.browser]\nbrowser = \"webkit\"\nbackend = \"webkit\"\nheadless = true\n[test.projects.browser.use]\nprofile = \"webkit\"\n\n[[test.projects]]\nname = \"inherits-config\"\n[test.projects.browser]\nbrowser = \"chromium\"\nbackend = \"cdp-pipe\"\nheadless = true\n",
    "spec": "\nimport { test as base, describe, expect } from '@ferridriver/test';\n\nconst test = base.extend<{ profile: string; theme: string }>({\n  profile: ['fixture-default', { option: true }],\n  theme: ['fixture-theme', { option: true }],\n});\n\nconst expected: Record<string, string> = {\n  'cdp-pipe': 'cdp-pipe',\n  'cdp-raw': 'cdp-raw',\n  bidi: 'bidi',\n  webkit: 'webkit',\n  'inherits-config': 'from-config',\n};\n\ntest('the project use block wins over the config one', async ({ profile }) => {\n  const name = test.info().project?.name ?? '';\n  expect(name).toBe(expected[name] === undefined ? 'a known project' : name);\n  expect(profile).toBe(expected[name]);\n});\n\ntest('a config key no project overrides still arrives', async ({ theme }) => {\n  expect(theme).toBe('config-theme');\n});\n\ndescribe('with a describe-level use', () => {\n  test.use({ profile: 'from-spec' });\n\n  test('the spec bag wins over the project one', async ({ profile, theme }) => {\n    expect(profile).toBe('from-spec');\n    // The key the spec did not name keeps the config's value.\n    expect(theme).toBe('config-theme');\n  });\n});\n"
  },
  {
    "name": "an_option_fixture_with_no_override_anywhere_keeps_its_declared_default",
    "config": "\n[[test.projects]]\nname = \"cdp-pipe\"\n[test.projects.browser]\nbrowser = \"chromium\"\nbackend = \"cdp-pipe\"\nheadless = true\n",
    "spec": "\nimport { test as base, expect } from '@ferridriver/test';\n\nconst test = base.extend<{ profile: string }>({\n  profile: ['fixture-default', { option: true }],\n});\n\ntest('nothing overrides it', async ({ profile }) => {\n  expect(profile).toBe('fixture-default');\n});\n"
  },
  {
    "name": "a_use_key_naming_a_non_option_fixture_is_refused",
    "config": "\n[test.browser.use]\nplain = \"from-config\"\n",
    "spec": "\nimport { test as base, expect } from '@ferridriver/test';\n\nconst test = base.extend<{ plain: string }>({\n  plain: async ({}, use: (v: string) => Promise<void>) => use('plain-value'),\n});\n\ntest('never runs', async ({ plain }) => {\n  expect(plain).toBe('plain-value');\n});\n"
  },
  {
    "name": "a_use_key_naming_a_built_in_fixture_is_refused",
    "config": "\n[test.browser.use]\npage = \"nope\"\n",
    "spec": "\nimport { test, expect } from '@ferridriver/test';\n\ntest('never runs', async ({ page }) => {\n  expect(page).toBeTruthy();\n});\n"
  },
  {
    "name": "a_use_key_that_names_nothing_is_reported_and_ignored",
    "config": "\n[test.browser.use]\nnobodyClaimsThisKey = 7\n",
    "spec": "\nimport { test, expect } from '@ferridriver/test';\n\ntest('still runs', async ({}) => {\n  expect(1).toBe(1);\n});\n"
  },
  {
    "name": "a_project_use_key_is_validated_too",
    "config": "\n[[test.projects]]\nname = \"one\"\n[test.projects.browser]\nbrowser = \"chromium\"\nbackend = \"cdp-pipe\"\nheadless = true\n[test.projects.browser.use]\nplain = \"from-project\"\n",
    "spec": "\nimport { test as base, expect } from '@ferridriver/test';\n\nconst test = base.extend<{ plain: string }>({\n  plain: async ({}, use: (v: string) => Promise<void>) => use('plain-value'),\n});\n\ntest('never runs', async ({ plain }) => {\n  expect(plain).toBe('plain-value');\n});\n"
  }
];
