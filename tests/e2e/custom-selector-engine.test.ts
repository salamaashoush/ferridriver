// `selectors.register(name, script)` — a user's own selector engine.
//
// The engine is JS by definition: matching needs the live DOM, so the
// script is evaluated inside every document ferridriver injects into.
// Two halves have to work for that to be true — the Rust selector
// parser must ROUTE `tag=h1` to the registered engine instead of
// treating it as CSS, and the injected script must have been told about
// the engine before the document was queried.
//
// Runs on all four backends: each injects the engine its own way.

import { test, describe, expect, selectors } from '@ferridriver/test';

// One function object, registered from module scope — which is how a
// suite would do it. Every worker in this process evaluates this file,
// so the second registration of the SAME script has to be a no-op; that
// is what `register_selector_engine` compares on, and why the retry
// below reuses this reference rather than retyping the body.
const tagNameEngine = () => ({
  queryAll(root: Element | Document, selector: string) {
    return Array.from(root.querySelectorAll(selector));
  },
});

await selectors.register('tagname', tagNameEngine);

describe('selectors.register', () => {
  test('a registered engine resolves', async ({ page }) => {
    await page.setContent('<h1 id="title">hello</h1><p id="body">text</p>');
    await expect(page.locator('tagname=h1')).toHaveAttribute('id', 'title');
    await expect(page.locator('tagname=p')).toHaveAttribute('id', 'body');
  });

  test('it composes in a chain', async ({ page }) => {
    await page.setContent(`
      <section id="one"><p>first</p></section>
      <section id="two"><p>second</p></section>
    `);
    await expect(page.locator('#two').locator('tagname=p')).toHaveText('second');
    await expect(page.locator('tagname=section').first()).toHaveAttribute('id', 'one');
  });

  test('it reaches a frame of its own', async ({ page }) => {
    // A separate document, injected separately — if the engine list
    // only reached the main frame this would find nothing.
    await page.setContent(`<iframe id="f" srcdoc="<h2 id='inner'>in frame</h2>"></iframe>`);
    await expect(page.frameLocator('#f').locator('tagname=h2')).toHaveText('in frame');
  });

  test('it survives a navigation', async ({ page }) => {
    // Each new document is injected from scratch, so the registration
    // has to be part of what gets injected, not a one-time call.
    await page.setContent('<h1>first document</h1>');
    await expect(page.locator('tagname=h1')).toHaveText('first document');
    await page.setContent('<h1>second document</h1>');
    await expect(page.locator('tagname=h1')).toHaveText('second document');
  });

  test('registering the same engine again is a no-op', async () => {
    // Same name, same script: ferridriver's workers share a process, so
    // this is exactly what the next worker evaluating this file does.
    await selectors.register('tagname', tagNameEngine);
  });

  test('registering the same name with a different script throws', async () => {
    let message = '';
    try {
      await selectors.register('tagname', () => ({
        queryAll() {
          return [];
        },
      }));
    } catch (error) {
      message = String((error as Error).message);
    }
    expect(message).toContain('selectors.register: "tagname" selector engine has been already registered');
  });
});
