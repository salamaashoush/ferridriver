// Web-first assertions judged by the page's engine, the way Playwright's
// `frames.ts::expect` judges them. Cases and failure texts are ported
// from Playwright's tests/page/expect-*.spec.ts.

import { test, describe, expect } from '@ferridriver/test';

const failure = (pending: Promise<unknown>): Promise<string> =>
  pending.then(
    () => { throw new Error('assertion passed'); },
    (e: Error) => e.message,
  );

describe('toBeChecked', () => {
  test('fail', async ({ page }) => {
    await page.setContent('<input type=checkbox></input>');
    const message = await failure(expect(page.locator('input')).toBeChecked({ timeout: 1000 }));
    expect(message).toContain(`expect(locator).toBeChecked() failed

Locator:  locator('input')
Expected: checked
Received: unchecked
Timeout:  1000ms`);
    expect(message).toContain(`- Expect "toBeChecked" locator('input') with timeout 1000ms`);
  });

  test('fail with not', async ({ page }) => {
    await page.setContent('<input type=checkbox checked></input>');
    const message = await failure(expect(page.locator('input')).not.toBeChecked({ timeout: 1000 }));
    expect(message).toContain(`expect(locator).not.toBeChecked() failed

Locator:  locator('input')
Expected: not checked
Received: checked
Timeout:  1000ms`);
    expect(message).toContain(`- Expect "not toBeChecked" locator('input') with timeout 1000ms`);
    expect(message).toContain(`locator resolved to <input checked type="checkbox"/>`);
  });

  test('checked: false asserts unchecked, not a negation', async ({ page }) => {
    await page.setContent('<input type=checkbox checked></input>');
    const message = await failure(expect(page.locator('input')).toBeChecked({ checked: false, timeout: 1000 }));
    expect(message).toContain(`expect(locator).toBeChecked({ checked: false }) failed

Locator:  locator('input')
Expected: unchecked
Received: checked
Timeout:  1000ms`);
  });

  test('indeterminate', async ({ page }) => {
    await page.setContent('<input type=checkbox></input>');
    const message = await failure(expect(page.locator('input')).toBeChecked({ indeterminate: true, timeout: 1000 }));
    expect(message).toContain(`expect(locator).toBeChecked({ indeterminate: true }) failed

Locator:  locator('input')
Expected: indeterminate
Received: unchecked
Timeout:  1000ms`);
    await page.locator('input').evaluate((e: HTMLInputElement) => { e.indeterminate = true; });
    await expect(page.locator('input')).toBeChecked({ indeterminate: true });
    // Checked-ness ignores the mixed state, so the box still reads unchecked.
    await expect(page.locator('input')).toBeChecked({ checked: false });
  });

  test('indeterminate and checked together are refused at once', async ({ page }) => {
    await page.setContent('<input type=checkbox></input>');
    const started = Date.now();
    const message = await failure(
      expect(page.locator('input')).toBeChecked({ indeterminate: true, checked: false, timeout: 5000 }),
    );
    expect(message).toContain(`Can't assert indeterminate and checked at the same time`);
    expect(Date.now() - started).toBeLessThan(4000);
  });

  test('a missing element reads as not found', async ({ page }) => {
    await page.setContent('<input type=checkbox></input>');
    const message = await failure(expect(page.locator('input2')).not.toBeChecked({ timeout: 1000 }));
    expect(message).toContain(`expect(locator).not.toBeChecked() failed

Locator: locator('input2')
Expected: not checked
Timeout: 1000ms
Error: element(s) not found`);
  });
});

describe('visibility and state', () => {
  test('not.toBeHidden on a missing element', async ({ page }) => {
    await page.setContent('<div></div>');
    const message = await failure(expect(page.locator('button')).not.toBeHidden({ timeout: 1000 }));
    expect(message).toContain(`expect(locator).not.toBeHidden() failed

Locator: locator('button')
Expected: not hidden
Timeout: 1000ms
Error: element(s) not found`);
  });

  test('visible: false is the hidden assertion under toBeVisible', async ({ page }) => {
    await page.setContent('<button>b</button>');
    const message = await failure(expect(page.locator('button')).toBeVisible({ visible: false, timeout: 500 }));
    expect(message).toContain(`expect(locator).toBeVisible({ visible: false }) failed`);
    expect(message).toContain('Expected: hidden');
    await expect(page.locator('span')).toBeVisible({ visible: false });
  });

  test('an element inside a hidden iframe is not visible', async ({ page }) => {
    await page.setContent('<iframe style="visibility:hidden" srcdoc="<button>in frame</button>"></iframe>');
    const button = page.frameLocator('iframe').locator('button');
    await expect(button).toBeAttached();
    await expect(button).toBeHidden();
  });

  test('editable: false holds for anything not editable', async ({ page }) => {
    await page.setContent('<input readonly id=r><input disabled id=d><input id=e>');
    await expect(page.locator('#r')).toBeEditable({ editable: false });
    await expect(page.locator('#d')).toBeEditable({ editable: false });
    const message = await failure(expect(page.locator('#e')).toBeEditable({ editable: false, timeout: 500 }));
    expect(message).toContain('expect(locator).toBeEditable({ editable: false }) failed');
    expect(message).toContain('Expected: readOnly');
    expect(message).toContain('Received: editable');
  });

  test('toBeEmpty reads an input by its value', async ({ page }) => {
    await page.setContent('<input id=i><div id=d>  </div>');
    await expect(page.locator('#i')).toBeEmpty();
    await expect(page.locator('#d')).toBeEmpty();
    await page.locator('#i').fill('x');
    await expect(page.locator('#i')).not.toBeEmpty();
  });

  test('attached: false asserts detached', async ({ page }) => {
    await page.setContent('<div></div>');
    await expect(page.locator('span')).toBeAttached({ attached: false });
    const message = await failure(expect(page.locator('div')).toBeAttached({ attached: false, timeout: 500 }));
    expect(message).toContain('expect(locator).toBeAttached({ attached: false }) failed');
  });

  test('a strict breach fails at once with the breach', async ({ page }) => {
    await page.setContent('<p>a</p><p>b</p>');
    const started = Date.now();
    const message = await failure(expect(page.locator('p')).toBeVisible({ timeout: 5000 }));
    expect(message).toContain('strict mode violation');
    expect(Date.now() - started).toBeLessThan(4000);
  });
});

describe('text', () => {
  test('a pattern that fails prints both labels aligned', async ({ page }) => {
    await page.setContent('<div id=node>Text content</div>');
    const message = await failure(expect(page.locator('#node')).toHaveText(/Text 2/, { timeout: 1000 }));
    expect(message).toContain(`expect(locator).toHaveText(expected) failed

Locator: locator('#node')
Expected pattern: /Text 2/
Received string:  "Text content"
Timeout: 1000ms
`);
  });

  test('a substring that fails', async ({ page }) => {
    await page.setContent('<div id=node>Text content</div>');
    const message = await failure(expect(page.locator('#node')).toContainText('foo', { timeout: 1000 }));
    expect(message).toContain(`expect(locator).toContainText(expected) failed

Locator: locator('#node')
Expected substring: "foo"
Received string:    "Text content"
Timeout: 1000ms
`);
  });

  test('whitespace is normalized and patterns run in the page', async ({ page }) => {
    await page.setContent('<div id=node>  Text\n   content&nbsp;</div>');
    await expect(page.locator('#node')).toHaveText('Text content');
    // Lookbehind is a JavaScript pattern feature the Rust engine lacks.
    await expect(page.locator('#node')).toHaveText(/(?<=Text\s+)content/);
    await expect(page.locator('#node')).toContainText('TEXT', { ignoreCase: true });
  });

  test('an array that fails diffs like Jest', async ({ page }) => {
    await page.setContent('<div>Text 1</div><div>Text 3</div>');
    const message = await failure(
      expect(page.locator('div')).toHaveText(['Text 1', /Text \d/, 'Extra'], { timeout: 1000 }),
    );
    expect(message).toContain(`expect(locator).toHaveText(expected) failed

Locator: locator('div')
Timeout: 1000ms
- Expected  - 1
+ Received  + 0

  Array [
    "Text 1",
    "Text 3",
-   "Extra",
  ]

Call log:
  - Expect "toHaveText" locator('div') with timeout 1000ms
  - waiting for locator('div')
`);
    expect(message).toContain('locator resolved to 2 elements');
  });

  test('toHaveTitle prints without a locator', async ({ page }) => {
    await page.setContent('<title>  Hello     world</title>');
    await expect(page).toHaveTitle('Hello  world');
    await page.setContent('<title>Bye</title>');
    const message = await failure(expect(page).toHaveTitle('Hello', { timeout: 1000 }));
    expect(message).toContain(`expect(page).toHaveTitle(expected) failed

Expected: "Hello"
Received: "Bye"
Timeout:  1000ms

Call log:
`);
    expect(message).toContain(`- Expect "toHaveTitle" with timeout 1000ms`);
  });
});

describe('values', () => {
  test('toHaveValue follows a label', async ({ page }) => {
    await page.setContent('<label><input></input></label>');
    await page.locator('label input').fill('Text content');
    await expect(page.locator('label')).toHaveValue('Text content');
  });

  test('toHaveValue on a non-input is refused', async ({ page }) => {
    await page.setContent('<div>x</div>');
    const message = await failure(expect(page.locator('div')).toHaveValue('x', { timeout: 5000 }));
    expect(message).toContain('Error: Not an input element');
  });

  test('toHaveValues takes patterns and diffs the selection', async ({ page }) => {
    await page.setContent(`<select multiple>
      <option value="R">Red</option><option value="G">Green</option><option value="B">Blue</option>
    </select>`);
    await page.locator('select').selectOption(['R', 'G']);
    await expect(page.locator('select')).toHaveValues([/R/, /G/]);
    await page.locator('select').selectOption(['B']);
    const message = await failure(expect(page.locator('select')).toHaveValues([/R/, /G/], { timeout: 1000 }));
    expect(message).toContain('+   "B"');
  });

  test('toHaveValues on a single select is refused', async ({ page }) => {
    await page.setContent('<select><option value="R">Red</option></select>');
    const message = await failure(expect(page.locator('select')).toHaveValues(['R'], { timeout: 1000 }));
    expect(message).toContain('Not a select element with a multiple attribute');
  });
});

describe('counts, properties and classes', () => {
  test('toHaveCount', async ({ page }) => {
    await page.setContent('<span></span>');
    const message = await failure(expect(page.locator('span')).toHaveCount(0, { timeout: 1000 }));
    expect(message).toContain(`expect(locator).toHaveCount(expected) failed

Locator:  locator('span')
Expected: 0
Received: 1
Timeout:  1000ms`);
    const negated = await failure(expect(page.locator('span')).not.toHaveCount(1, { timeout: 1000 }));
    expect(negated).toContain(`expect(locator).not.toHaveCount(expected) failed

Locator:  locator('span')
Expected: not 1
Received: 1
Timeout:  1000ms`);
  });

  test('toHaveJSProperty', async ({ page }) => {
    await page.setContent('<div></div>');
    await page.locator('div').evaluate((e: any) => { e.foo = 'string'; e.bar = { nested: 2021 }; });
    await expect(page.locator('div')).toHaveJSProperty('bar.nested', 2021);
    const message = await failure(expect(page.locator('div')).toHaveJSProperty('foo', 'error', { timeout: 200 }));
    expect(message).toContain(`expect(locator).toHaveJSProperty(expected) failed

Locator:  locator('div')
Expected: "error"
Received: "string"
Timeout:  200ms`);
  });

  test('toHaveClass and toContainClass', async ({ page }) => {
    await page.setContent('<div class="bar baz"></div><div class="foo"></div>');
    const first = page.locator('div').first();
    const message = await failure(expect(first).toHaveClass('foo bar baz', { timeout: 1000 }));
    expect(message).toContain(`Expected: "foo bar baz"
Received: "bar baz"`);
    await expect(first).toContainClass('baz bar');
    await expect(page.locator('div')).toHaveClass(['bar baz', /fo+/]);
    await expect(page.locator('div')).toContainClass(['bar', 'foo']);
    const contained = await failure(expect(first).toContainClass('does-not-exist', { timeout: 1000 }));
    expect(contained).toContain(`Expected: "does-not-exist"
Received: "bar baz"`);
  });

  test('toHaveId and the implicit role', async ({ page }) => {
    await page.setContent('<button id="go">Go</button><h2>Title</h2>');
    await expect(page.locator('button')).toHaveId('go');
    await expect(page.locator('button')).toHaveRole('button');
    await expect(page.locator('h2')).toHaveRole('heading');
  });
});

describe('accessibility', () => {
  test('toHaveAccessibleName computes the name, with ignoreCase', async ({ page }) => {
    await page.setContent('<div role="button" aria-label="Hello"></div>');
    await expect(page.locator('div')).toHaveAccessibleName('Hello');
    await expect(page.locator('div')).not.toHaveAccessibleName('hello');
    await expect(page.locator('div')).toHaveAccessibleName('hello', { ignoreCase: true });
    await expect(page.locator('div')).toHaveAccessibleName(/hello/, { ignoreCase: true });
    await page.setContent('<button>foo&nbsp;bar\nbaz</button>');
    await expect(page.locator('button')).toHaveAccessibleName('foo bar baz');
    await page.setContent('<span id=a>Save</span><span id=b>draft</span><button aria-labelledby="a b"></button>');
    await expect(page.locator('button')).toHaveAccessibleName('Save draft');
  });

  test('toHaveAccessibleDescription', async ({ page }) => {
    await page.setContent('<div role="button" aria-describedby="desc"></div><span id="desc">foo&nbsp;bar\nbaz</span>');
    await expect(page.locator('div')).toHaveAccessibleDescription('foo bar baz');
    await expect(page.locator('div')).toHaveAccessibleDescription('FOO BAR BAZ', { ignoreCase: true });
  });

  test('toHaveAccessibleErrorMessage', async ({ page }) => {
    await page.setContent(`<form>
      <input role="textbox" aria-invalid="true" aria-errormessage="error-message" />
      <div id="error-message">Hello</div>
      <div id="irrelevant-error">This should not be considered.</div>
    </form>`);
    const locator = page.locator('input[role="textbox"]');
    await expect(locator).toHaveAccessibleErrorMessage('Hello');
    await expect(locator).not.toHaveAccessibleErrorMessage('hello');
    await expect(locator).toHaveAccessibleErrorMessage('hello', { ignoreCase: true });
    await expect(locator).not.toHaveAccessibleErrorMessage('This should not be considered.');
  });
});
