// Two independent `test.extend` chains combined with `mergeTests` — the
// composition a real suite grows into, and the one the extension's own
// fixtures have to survive.

import { test as base, mergeTests } from '@ferridriver/test';

const withTenant = base.extend<{ tenant: string }>({
  tenant: async ({}, use) => {
    await use('acme');
  },
});

const withOperator = base.extend<{ operator: string }>({
  operator: async ({}, use) => {
    await use('ada');
  },
});

export const test = mergeTests(withTenant, withOperator);
